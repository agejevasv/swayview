//! Layer-shell overlay: one surface per output, redrawn live from sway events.
//!
//! Input is translated by `input` into actions; this module only applies them.
//! Redraws are throttled to the compositor's frame callbacks. Each surface is
//! rendered at its output's scale, fractional where the compositor supports
//! `wp_fractional_scale_v1` and `wp_viewporter`, integer otherwise.
//!
//! With thumbnails, each window is a tile of two subsurfaces instead, see
//! `tiles`, showing frames from `capture`.

mod capture;
mod tiles;

use std::collections::HashMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, ensure};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
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
    subcompositor::SubcompositorState,
};
use wayland_protocols::wp::{
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::{self, WpFractionalScaleV1},
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

use crate::config::Config;
use crate::input::{self, Action, Key, Sel};
use crate::layout::{self, Dir, Scene};
use crate::model::{ConId, Focus, Tree};
use crate::render::{Renderer, View};
use crate::sway::Ipc;
use crate::warn;
use capture::Capture;
use tiles::Tile;
use tiny_skia::Pixmap;

const BTN_LEFT: u32 = 0x110;
/// The fractional-scale protocol counts scale in 120ths: 120 is 1×, 180 is 1.5×.
const SCALE_UNIT: u32 = 120;
/// How long the overview waits for the windows' first frames before it shows
/// up; windows still without one are drawn as boxes until theirs comes.
const CAPTURE_WAIT: Duration = Duration::from_millis(200);

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
    /// With thumbnails, one per window with room for it.
    tiles: HashMap<ConId, Tile>,
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
    viewporter: Option<WpViewporter>,
    /// Used only with a viewporter.
    fractional_scale: Option<WpFractionalScaleManagerV1>,
    /// Bound only for thumbnails.
    subcompositor: Option<SubcompositorState>,
    shm: Shm,
    pool: SlotPool,
    /// To send each surface as soon as it is drawn.
    conn: Connection,
    qh: QueueHandle<App>,
    /// For timers.
    loop_handle: LoopHandle<'static, App>,
    /// Present once the fonts are loaded, before the surfaces are made.
    renderer: Option<Renderer>,
    /// Present when thumbnails are on and the compositor can capture windows.
    capture: Option<Capture>,
    ipc: Ipc,
    tree: Tree,
    initial_focus: Option<Focus>,
    surfaces: Vec<Surf>,
    selected: Option<Sel>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    /// Surface under the pointer, known while it is over one of ours.
    pointer_surface: Option<usize>,
    tree_dirty: bool,
    exit: bool,
}

pub fn run() -> Result<()> {
    // Subscribe before the first `get_tree`, so no change after it is missed.
    let sway_events = watch_sway()?;
    let conn = Connection::connect_to_env().context("connecting to Wayland")?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new()?;
    let (mut app, fonts) = App::new(&conn, &globals, &queue.handle(), event_loop.handle())?;
    // Learn output names and positions, and the windows to capture.
    queue.roundtrip(&mut app)?;
    if app.capture.is_some() {
        // For what each window handle announced after its creation.
        queue.roundtrip(&mut app)?;
    }
    app.capture_windows();

    WaylandSource::new(conn, queue).insert(event_loop.handle()).map_err(|e| e.error)?;
    event_loop
        .handle()
        .insert_source(sway_events, |event, (), app| {
            if let channel::Event::Msg(()) = event {
                app.tree_dirty = true;
            }
        })
        .map_err(|e| e.error)?;

    let deadline = Instant::now() + CAPTURE_WAIT;
    while !app.capture.as_ref().is_none_or(Capture::settled)
        && let Some(left) = deadline.checked_duration_since(Instant::now())
    {
        event_loop.dispatch(left, &mut app)?;
    }
    app.renderer = Some(fonts.join().map_err(|_| anyhow!("loading fonts failed"))?);
    app.create_surfaces();
    ensure!(!app.surfaces.is_empty(), "no Wayland output matches one of sway's outputs");
    app.start_live_updates();

    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
        if std::mem::take(&mut app.tree_dirty) && !app.exit {
            app.refresh();
        }
    }
    Ok(())
}

/// Sends a message for every sway event that may change what is shown.
fn watch_sway() -> Result<Channel<()>> {
    let (tx, rx) = channel::channel();
    let mut ipc = Ipc::connect()?;
    ipc.subscribe(&["window", "workspace"])?;
    std::thread::spawn(move || {
        loop {
            if let Err(e) = ipc.wait_event() {
                return warn(e.context("event stream ended"));
            }
            if tx.send(()).is_err() {
                return;
            }
        }
    });
    Ok(rx)
}

/// The sway output a `wl_output` is: by its `name` (`wl_output` v4), or else
/// by its position.
fn sway_output_name(name: Option<&str>, pos: (i32, i32), tree: &Tree) -> Option<String> {
    if let Some(name) = name.filter(|n| tree.output(n).is_some()) {
        return Some(name.to_owned());
    }
    tree.outputs.iter().find(|o| (o.rect.x as i32, o.rect.y as i32) == pos).map(|o| o.name.clone())
}

/// What surface `i` marks: the selected window and its workspace, or with no
/// selection anywhere, sway's focused workspace.
fn view(scene: &Scene, selected: Option<Sel>, i: usize) -> View {
    let window = selected.filter(|sel| sel.surface == i).map(|sel| sel.window);
    let workspace = match selected {
        Some(_) => window.map(|w| scene.windows[w].workspace),
        None => scene.selected_workspace(None),
    };
    View { selected: window, selected_workspace: workspace }
}

/// Render scale and buffer size for a logical size at `scale` `SCALE_UNIT`s,
/// rounded half away from zero as `wp_fractional_scale_v1` specifies.
fn buffer_size((w, h): (u32, u32), scale: u32) -> (f32, (u32, u32)) {
    let f = scale.max(1) as f32 / SCALE_UNIT as f32;
    (f, ((w as f32 * f).round() as u32, (h as f32 * f).round() as u32))
}

/// Copies `pix` into a new buffer and attaches it to `surface`, all damaged.
fn attach(pool: &mut SlotPool, surface: &wl_surface::WlSurface, pix: &Pixmap) -> Result<()> {
    let (w, h) = (pix.width() as i32, pix.height() as i32);
    let (buffer, canvas) = pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888).context("buffer")?;
    // tiny-skia is premultiplied RGBA; ARGB8888 little-endian is BGRA in memory.
    for (dst, src) in canvas.as_chunks_mut::<4>().0.iter_mut().zip(pix.data().as_chunks::<4>().0) {
        *dst = [src[2], src[1], src[0], src[3]];
    }
    surface.damage_buffer(0, 0, w, h);
    buffer.attach_to(surface).context("attach")?;
    Ok(())
}

/// An integer output scale in `SCALE_UNIT`s.
fn integer_scale(factor: i32) -> u32 {
    factor.max(1).unsigned_abs() * SCALE_UNIT
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
    /// The app, and its renderer loading on a thread meanwhile.
    fn new(
        conn: &Connection,
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        loop_handle: LoopHandle<'static, Self>,
    ) -> Result<(Self, JoinHandle<Renderer>)> {
        let mut ipc = Ipc::connect()?;
        let tree = ipc.get_tree()?;
        let config = Config::load(ipc.config_path().ok().as_deref());
        let compositor = CompositorState::bind(globals, qh).context("wl_compositor")?;
        let viewporter: Option<WpViewporter> = globals.bind(qh, 1..=1, NoEvents).ok();
        // Thumbnails are shown on subsurfaces, scaled by a viewport.
        let subcompositor = config
            .thumbnails
            .then(|| SubcompositorState::bind(compositor.wl_compositor().clone(), globals, qh).ok())
            .flatten()
            .filter(|_| viewporter.is_some());
        let capture = subcompositor.as_ref().and_then(|_| Capture::new(globals, qh));
        let shm = Shm::bind(globals, qh).context("wl_shm")?;
        let fonts = std::thread::spawn(move || Renderer::new(config));
        let app = App {
            registry_state: RegistryState::new(globals),
            seat_state: SeatState::new(globals, qh),
            output_state: OutputState::new(globals, qh),
            compositor,
            layer_shell: LayerShell::bind(globals, qh).context("wlr-layer-shell")?,
            fractional_scale: globals.bind(qh, 1..=1, NoEvents).ok(),
            viewporter,
            subcompositor,
            pool: SlotPool::new(1920 * 1080 * 4, &shm)?,
            shm,
            conn: conn.clone(),
            qh: qh.clone(),
            loop_handle,
            renderer: None,
            capture,
            ipc,
            initial_focus: tree.focus(),
            tree,
            surfaces: Vec::new(),
            selected: None,
            keyboard: None,
            pointer: None,
            pointer_surface: None,
            tree_dirty: false,
            exit: false,
        };
        Ok((app, fonts))
    }

    /// One fullscreen overlay per output.
    fn create_surfaces(&mut self) {
        for wl_output in self.output_state.outputs() {
            let Some(info) = self.output_state.info(&wl_output) else { continue };
            let pos = info.logical_position.unwrap_or(info.location);
            let Some(name) = sway_output_name(info.name.as_deref(), pos, &self.tree) else { continue };
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
            let (fractional_scale, viewport) = if let Some((manager, viewporter)) =
                self.fractional_scale.as_ref().zip(self.viewporter.as_ref())
            {
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
                scale: integer_scale(info.scale_factor),
                viewport,
                _fractional_scale: fractional_scale,
                scene: Scene::default(),
                tiles: HashMap::new(),
                dirty: false,
                frame_pending: false,
            });
        }
    }

    fn scenes(&self) -> Vec<&Scene> {
        self.surfaces.iter().map(|s| &s.scene).collect()
    }

    /// Lays out every configured surface again, keeping the selection by
    /// window, and redraws those that changed.
    fn rebuild(&mut self) {
        let old = self.selected;
        let selected_id = old.map(|s| self.surfaces[s.surface].scene.windows[s.window].id);
        let mut changed = Vec::new();
        for (i, s) in self.surfaces.iter_mut().enumerate() {
            let Some((w, h)) = s.size else { continue };
            let scene =
                self.tree.output(&s.output).map(|o| layout::build(o, w as f32, h as f32)).unwrap_or_default();
            if scene != s.scene {
                s.scene = scene;
                changed.push(i);
            }
        }
        let scenes = self.scenes();
        self.selected =
            selected_id.and_then(|id| input::find(&scenes, id)).or_else(|| input::focused(&scenes));
        for &i in &changed {
            self.sync_tiles(i);
        }
        // A selection that moved changes what every surface marks.
        if self.selected == old {
            for i in changed {
                self.redraw(i);
            }
        } else {
            self.redraw_all();
        }
        self.capture_windows();
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
        let Some(renderer) = &mut self.renderer else { return };
        let (scale, (pw, ph)) = buffer_size((w, h), s.scale);
        let view = view(&s.scene, self.selected, i);
        let pix = if self.capture.is_some() {
            renderer.draw_workspaces(&s.scene, &view, pw, ph, scale)
        } else {
            renderer.draw(&s.scene, &view, pw, ph, scale)
        };
        let Some(pix) = pix else { return };
        for (k, win) in s.scene.windows.iter().enumerate() {
            if let Some(tile) = s.tiles.get_mut(&win.id)
                && let Err(e) =
                    tile.draw_deco(renderer, &mut self.pool, win, view.selected == Some(k), s.scale)
            {
                warn(e.context("window overlay"));
            }
        }

        let surface = s.layer.wl_surface();
        match &s.viewport {
            // The buffer is pw×ph; the viewport shows it at the logical size.
            Some(viewport) => viewport.set_destination(w as i32, h as i32),
            None => surface.set_buffer_scale((s.scale / SCALE_UNIT).max(1) as i32),
        }
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        if let Err(e) = attach(&mut self.pool, surface, &pix) {
            return warn(e);
        }
        s.layer.commit();
        s.tiles.values_mut().for_each(Tile::committed);
        s.dirty = false;
        s.frame_pending = true;
        // Without waiting for the other surfaces to be drawn.
        if let Err(e) = self.conn.flush() {
            warn(format_args!("flush: {e}"));
        }
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

    /// Handles a key press, or its repeat. Only moving the selection repeats,
    /// so a held key cannot run a sway command twice.
    fn key(&mut self, event: &KeyEvent, repeat: bool) {
        let Some(key) = key_of(event) else { return };
        if repeat && !matches!(key, Key::Arrow(_) | Key::Tab { .. }) {
            return;
        }
        let action = input::key(&self.scenes(), self.selected, key);
        self.apply(&action);
    }

    fn apply(&mut self, action: &Action) {
        match action {
            Action::Nothing => {}
            Action::Select(sel) => self.select(*sel),
            Action::Close => self.exit = true,
            Action::Focus(_) | Action::WorkspaceNumber(_) => {
                let pointer_output = self.pointer_surface.map(|i| self.surfaces[i].output.as_str());
                if let Some(cmd) = input::command(action, &self.tree, pointer_output)
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
            self.set_scale(surface, integer_scale(new_factor));
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
            let repeat = Box::new(|app: &mut App, _: &wl_keyboard::WlKeyboard, event| app.key(&event, true));
            self.keyboard = self
                .seat_state
                .get_keyboard_with_repeat(qh, &seat, None, self.loop_handle.clone(), repeat)
                .ok();
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
        // The keyboard is not released: destroying it drops its key repeat
        // timer from inside the event loop's dispatch, which calloop panics on.
        // The dead object lives until exit.
        if capability == Capability::Keyboard {
            self.keyboard = None;
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
        self.key(&event, false);
    }

    /// Sent by compositors that repeat keys themselves; others are repeated by a timer.
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(&event, true);
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
                PointerEventKind::Enter { .. } => self.pointer_surface = Some(i),
                // Moving over a window selects it. Enter alone does not, so a
                // resting mouse leaves the focused window selected on open.
                PointerEventKind::Motion { .. } => {
                    self.pointer_surface = Some(i);
                    let action = input::motion(&self.surfaces[i].scene, i, x, y);
                    self.apply(&action);
                }
                PointerEventKind::Leave { .. } => self.pointer_surface = None,
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

/// User data for objects that send no events: the viewporter, viewports,
/// the fractional-scale manager, and the capture managers and sources.
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

    #[test]
    fn outputs_pair_by_name_then_position() {
        let tree = crate::model::tests::tree();
        assert_eq!(sway_output_name(Some("HEADLESS-1"), (500, 500), &tree).as_deref(), Some("HEADLESS-1"));
        assert_eq!(sway_output_name(Some("DP-9"), (0, 0), &tree).as_deref(), Some("HEADLESS-1"));
        assert_eq!(sway_output_name(None, (0, 0), &tree).as_deref(), Some("HEADLESS-1"));
        assert_eq!(sway_output_name(None, (1920, 0), &tree), None);
    }

    #[test]
    fn surfaces_mark_the_selection_or_else_the_focus() {
        let tree = crate::model::tests::tree();
        let scene = layout::build(&tree.outputs[0], 1920.0, 1080.0);
        let slack = scene.windows.iter().position(|w| w.app == "Slack").unwrap();
        let v = view(&scene, Some(Sel { surface: 0, window: slack }), 0);
        assert_eq!((v.selected, v.selected_workspace), (Some(slack), Some(1)));
        // Selected elsewhere: nothing marked here.
        let v = view(&scene, Some(Sel { surface: 1, window: slack }), 0);
        assert_eq!((v.selected, v.selected_workspace), (None, None));
        // Nothing selected: sway's focused workspace, "1" in the fixture.
        let v = view(&scene, None, 0);
        assert_eq!((v.selected, v.selected_workspace), (None, Some(0)));
    }
}
