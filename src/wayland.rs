//! Layer-shell overlay: one surface per output, redrawn live from sway events.
//!
//! Input is translated by `input` into actions; this module only applies them.
//! Redraws are throttled to the compositor's frame callbacks. Each surface is
//! rendered at its output's scale, fractional where the compositor supports
//! `wp_fractional_scale_v1` and `wp_viewporter`, integer otherwise.

use anyhow::{Context, Result, ensure};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputInfo, OutputState},
    reexports::{
        calloop::{
            EventLoop,
            channel::{self, Channel},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Proxy, QueueHandle,
            globals::{GlobalList, registry_queue_init},
            protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use wayland_protocols::wp::{
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::{self, WpFractionalScaleV1},
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

use crate::input::{self, Action, Key, Sel};
use crate::layout::{self, Dir, Hit, Scene};
use crate::model::{Focus, Tree};
use crate::render::{Renderer, View};
use crate::sway::Ipc;
use crate::theme::Theme;
use crate::warn;

const BTN_LEFT: u32 = 0x110;
/// The fractional-scale protocol counts scale in 120ths: 120 is 1×, 180 is 1.5×.
const SCALE_UNIT: u32 = 120;

struct Surf {
    layer: LayerSurface,
    /// Sway name of the output the surface is on.
    output: String,
    /// Logical size, known once configured.
    size: Option<(u32, u32)>,
    /// Scale in `SCALE_UNIT`s; a whole multiple without a viewport.
    scale: u32,
    /// Present when rendering at fractional scale.
    viewport: Option<WpViewport>,
    _fractional_scale: Option<WpFractionalScaleV1>,
    scene: Scene,
    pointer: Option<(f32, f32)>,
    hover: Option<Hit>,
    /// Needs a redraw.
    dirty: bool,
    /// A frame callback is outstanding; draw when it arrives.
    frame_pending: bool,
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    /// Both present, or fractional scaling is not used.
    fractional_scale: Option<(WpFractionalScaleManagerV1, WpViewporter)>,
    shm: Shm,
    pool: SlotPool,
    qh: QueueHandle<App>,
    renderer: Renderer,
    ipc: Ipc,
    tree: Tree,
    initial_focus: Option<Focus>,
    surfaces: Vec<Surf>,
    selected: Option<Sel>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    /// Output under the pointer, known while it is over one of our surfaces.
    pointer_output: Option<String>,
    tree_dirty: bool,
    exit: bool,
}

pub fn run() -> Result<()> {
    let conn = Connection::connect_to_env().context("connecting to Wayland")?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let mut app = App::new(&globals, &queue.handle())?;
    // Learn output names and positions before creating surfaces.
    queue.roundtrip(&mut app)?;
    app.create_surfaces();
    ensure!(!app.surfaces.is_empty(), "no Wayland output matches one of sway's outputs");

    let mut event_loop: EventLoop<'_, App> = EventLoop::try_new()?;
    WaylandSource::new(conn, queue).insert(event_loop.handle()).map_err(|e| e.error)?;
    event_loop
        .handle()
        .insert_source(watch_sway()?, |event, (), app| {
            if let channel::Event::Msg(()) = event {
                app.tree_dirty = true;
            }
        })
        .map_err(|e| e.error)?;

    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
        if std::mem::take(&mut app.tree_dirty) {
            app.refresh();
        }
    }
    Ok(())
}

/// Sends a message for every sway event that may change what is shown.
fn watch_sway() -> Result<Channel<()>> {
    let (tx, rx) = channel::channel();
    let ipc = Ipc::connect()?;
    std::thread::spawn(move || {
        let result = ipc.subscribe(&["window", "workspace"], || {
            let _ = tx.send(());
        });
        if let Err(e) = result {
            warn(e.context("event stream ended"));
        }
    });
    Ok(rx)
}

/// Pairs a `wl_output` with a sway output, by name (`wl_output` v4) or by position.
fn sway_output_name(info: &OutputInfo, tree: &Tree) -> Option<String> {
    if let Some(name) = info.name.as_ref().filter(|n| tree.outputs.iter().any(|o| &o.name == *n)) {
        return Some(name.clone());
    }
    let pos = info.logical_position.unwrap_or(info.location);
    tree.outputs.iter().find(|o| (o.rect.x as i32, o.rect.y as i32) == pos).map(|o| o.name.clone())
}

/// Render scale and buffer size for a logical size at `scale` `SCALE_UNIT`s,
/// rounded half away from zero as `wp_fractional_scale_v1` specifies.
fn buffer_size((w, h): (u32, u32), scale: u32) -> (f32, (u32, u32)) {
    let f = scale.max(1) as f32 / SCALE_UNIT as f32;
    (f, ((w as f32 * f).round() as u32, (h as f32 * f).round() as u32))
}

fn key_of(event: &KeyEvent) -> Option<Key<'_>> {
    Some(match event.keysym {
        Keysym::Escape => Key::Escape,
        Keysym::Return | Keysym::KP_Enter => Key::Enter,
        Keysym::Tab => Key::Tab { back: false },
        Keysym::ISO_Left_Tab => Key::Tab { back: true },
        Keysym::Left | Keysym::h => Key::Arrow(Dir::Left),
        Keysym::Right | Keysym::l => Key::Arrow(Dir::Right),
        Keysym::Up | Keysym::k => Key::Arrow(Dir::Up),
        Keysym::Down | Keysym::j => Key::Arrow(Dir::Down),
        _ => Key::Text(event.utf8.as_deref()?),
    })
}

impl App {
    fn new(globals: &GlobalList, qh: &QueueHandle<Self>) -> Result<Self> {
        let mut ipc = Ipc::connect()?;
        let tree = ipc.get_tree()?;
        let theme = Theme::load(ipc.config_path().ok().as_deref());
        let shm = Shm::bind(globals, qh).context("wl_shm")?;
        Ok(App {
            registry_state: RegistryState::new(globals),
            seat_state: SeatState::new(globals, qh),
            output_state: OutputState::new(globals, qh),
            compositor: CompositorState::bind(globals, qh).context("wl_compositor")?,
            layer_shell: LayerShell::bind(globals, qh).context("wlr-layer-shell")?,
            fractional_scale: globals
                .bind(qh, 1..=1, NoEvents)
                .ok()
                .zip(globals.bind(qh, 1..=1, NoEvents).ok()),
            pool: SlotPool::new(1920 * 1080 * 4, &shm)?,
            shm,
            qh: qh.clone(),
            renderer: Renderer::new(theme),
            ipc,
            initial_focus: tree.focus(),
            tree,
            surfaces: Vec::new(),
            selected: None,
            keyboard: None,
            pointer: None,
            pointer_output: None,
            tree_dirty: false,
            exit: false,
        })
    }

    /// One fullscreen overlay per output.
    fn create_surfaces(&mut self) {
        for wl_output in self.output_state.outputs() {
            let Some(info) = self.output_state.info(&wl_output) else { continue };
            let Some(name) = sway_output_name(&info, &self.tree) else { continue };
            let surface = self.compositor.create_surface(&self.qh);
            let layer = self.layer_shell.create_layer_surface(
                &self.qh,
                surface,
                Layer::Overlay,
                Some("swayview"),
                Some(&wl_output),
            );
            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            let surface = layer.wl_surface();
            let (fractional_scale, viewport) = if let Some((manager, viewporter)) = &self.fractional_scale {
                let data = FractionalScale(surface.clone());
                (
                    Some(manager.get_fractional_scale(surface, &self.qh, data)),
                    Some(viewporter.get_viewport(surface, &self.qh, NoEvents)),
                )
            } else {
                surface.set_buffer_scale(info.scale_factor);
                (None, None)
            };
            layer.commit();
            self.surfaces.push(Surf {
                layer,
                output: name,
                size: None,
                // Until the preferred fractional scale arrives, the output's integer one.
                scale: info.scale_factor.max(1).unsigned_abs() * SCALE_UNIT,
                viewport,
                _fractional_scale: fractional_scale,
                scene: Scene::default(),
                pointer: None,
                hover: None,
                dirty: false,
                frame_pending: false,
            });
        }
    }

    fn scenes(&self) -> Vec<&Scene> {
        self.surfaces.iter().map(|s| &s.scene).collect()
    }

    /// Lays out every configured surface again, keeping the selection by window.
    fn rebuild(&mut self) {
        let selected_id = self.selected.map(|s| self.surfaces[s.surface].scene.windows[s.window].id);
        for s in &mut self.surfaces {
            let Some((w, h)) = s.size else { continue };
            s.scene =
                self.tree.output(&s.output).map(|o| layout::build(o, w as f32, h as f32)).unwrap_or_default();
            s.hover = s.pointer.and_then(|(x, y)| s.scene.hit(x, y));
        }
        let scenes = self.scenes();
        self.selected =
            selected_id.and_then(|id| input::find(&scenes, id)).or_else(|| input::focused(&scenes));
        self.redraw_all();
    }

    fn refresh(&mut self) {
        match self.ipc.get_tree() {
            Ok(mut tree) => {
                if let Some(focus) = &self.initial_focus {
                    tree.restore_focus(focus);
                }
                self.tree = tree;
                self.rebuild();
            }
            Err(e) => {
                warn(e.context("get_tree"));
                self.exit = true;
            }
        }
    }

    /// Draws now, or on the next frame callback if one is outstanding.
    fn redraw(&mut self, i: usize) {
        let s = &mut self.surfaces[i];
        s.dirty = true;
        if !s.frame_pending {
            self.draw(i);
        }
    }

    fn redraw_all(&mut self) {
        for i in 0..self.surfaces.len() {
            self.redraw(i);
        }
    }

    fn draw(&mut self, i: usize) {
        let s = &mut self.surfaces[i];
        let Some((w, h)) = s.size else { return };
        let (scale, (pw, ph)) = buffer_size((w, h), s.scale);
        let view = View {
            selected: self.selected.filter(|sel| sel.surface == i).map(|sel| sel.window),
            hover: s.hover,
        };
        let Some(pix) = self.renderer.draw(&s.scene, &view, pw, ph, scale) else { return };

        let (buffer, canvas) =
            match self.pool.create_buffer(pw as i32, ph as i32, pw as i32 * 4, wl_shm::Format::Argb8888) {
                Ok(b) => b,
                Err(e) => return warn(format_args!("buffer: {e}")),
            };
        // tiny-skia is premultiplied RGBA; ARGB8888 little-endian is BGRA in memory.
        for (dst, src) in canvas.as_chunks_mut::<4>().0.iter_mut().zip(pix.data().as_chunks::<4>().0) {
            *dst = [src[2], src[1], src[0], src[3]];
        }
        let surface = s.layer.wl_surface();
        match &s.viewport {
            // The buffer is pw×ph; the viewport shows it at the logical size.
            Some(viewport) => viewport.set_destination(w as i32, h as i32),
            None => surface.set_buffer_scale((s.scale / SCALE_UNIT).max(1) as i32),
        }
        surface.damage_buffer(0, 0, pw as i32, ph as i32);
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        if let Err(e) = buffer.attach_to(surface) {
            return warn(format_args!("attach: {e}"));
        }
        s.layer.commit();
        s.dirty = false;
        s.frame_pending = true;
    }

    fn surface_index(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.surfaces.iter().position(|s| s.layer.wl_surface() == surface)
    }

    fn set_scale(&mut self, surface: &wl_surface::WlSurface, scale: u32) {
        if let Some(i) = self.surface_index(surface)
            && self.surfaces[i].scale != scale
        {
            self.surfaces[i].scale = scale;
            self.redraw(i);
        }
    }

    fn select(&mut self, sel: Sel) {
        let old = self.selected.replace(sel);
        if old == Some(sel) {
            return;
        }
        if let Some(old) = old
            && old.surface != sel.surface
        {
            self.redraw(old.surface);
        }
        self.redraw(sel.surface);
    }

    fn apply(&mut self, action: &Action) {
        match action {
            Action::Nothing => {}
            Action::Select(sel) => self.select(*sel),
            Action::Close => self.exit = true,
            Action::Focus(_) | Action::Workspace(_) | Action::WorkspaceNumber(_) => {
                if let Some(cmd) = input::command(action, &self.tree, self.pointer_output.as_deref())
                    && let Err(e) = self.ipc.command(&cmd)
                {
                    warn(e);
                }
                self.exit = true;
            }
        }
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(i) = self.surface_index(layer.wl_surface()) else { return };
        self.surfaces[i].size = Some((configure.new_size.0.max(1), configure.new_size.1.max(1)));
        self.rebuild();
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        // With a viewport, the fractional scale takes precedence.
        if let Some(i) = self.surface_index(surface)
            && self.surfaces[i].viewport.is_none()
        {
            self.set_scale(surface, new_factor.max(1).unsigned_abs() * SCALE_UNIT);
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        if let Some(i) = self.surface_index(surface) {
            self.surfaces[i].frame_pending = false;
            if self.surfaces[i].dirty {
                self.draw(i);
            }
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(k) = self.keyboard.take()
        {
            k.release();
        }
        if capability == Capability::Pointer
            && let Some(p) = self.pointer.take()
        {
            p.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if let Some(key) = key_of(&event) {
            let action = input::key(&self.scenes(), self.selected, key);
            self.apply(&action);
        }
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if self.exit {
                break;
            }
            let Some(i) = self.surface_index(&event.surface) else { continue };
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    let s = &mut self.surfaces[i];
                    self.pointer_output = Some(s.output.clone());
                    s.pointer = Some((x, y));
                    let hover = s.scene.hit(x, y);
                    if hover != s.hover {
                        s.hover = hover;
                        self.redraw(i);
                    }
                    // Moving over a window selects it. Enter alone does not, so a
                    // resting mouse leaves the focused window selected on open.
                    if matches!(event.kind, PointerEventKind::Motion { .. }) {
                        let action = input::motion(&self.surfaces[i].scene, i, x, y);
                        self.apply(&action);
                    }
                }
                PointerEventKind::Leave { .. } => {
                    self.pointer_output = None;
                    let s = &mut self.surfaces[i];
                    s.pointer = None;
                    if s.hover.take().is_some() {
                        self.redraw(i);
                    }
                }
                PointerEventKind::Press { button: BTN_LEFT, .. } => {
                    let action = input::click(&self.surfaces[i].scene, x, y);
                    self.apply(&action);
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(App);

/// User data for the fractional-scale manager, viewporter and viewports,
/// none of which send events.
struct NoEvents;

impl<I: Proxy> Dispatch2<I, App> for NoEvents {
    fn event(&self, _: &mut App, _: &I, _: I::Event, _: &Connection, _: &QueueHandle<App>) {}
}

/// User data of a surface's fractional-scale object.
struct FractionalScale(wl_surface::WlSurface);

impl Dispatch2<WpFractionalScaleV1, App> for FractionalScale {
    fn event(
        &self,
        app: &mut App,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            app.set_scale(&self.0, scale);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_sizes() {
        assert_eq!(buffer_size((2560, 1440), 180), (1.5, (3840, 2160)));
        assert_eq!(buffer_size((1920, 1080), 120), (1.0, (1920, 1080)));
        assert_eq!(buffer_size((1920, 1080), 240), (2.0, (3840, 2160)));
        // 1.25× of an odd size rounds up.
        assert_eq!(buffer_size((1001, 3), 150).1, (1251, 4));
    }
}
