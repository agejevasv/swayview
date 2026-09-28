//! Plain test window with a chosen `app_id` and title, for faking sway layouts.
//!
//!     cargo run -p fake-window -- firefox "GitHub - Mozilla Firefox"

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::client::{
        Connection, QueueHandle,
        globals::registry_queue_init,
        protocol::{wl_output, wl_shm, wl_surface},
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};

struct Dummy {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    _window: Window,
    color: [u8; 4],
    exit: bool,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let app_id = args.next().unwrap_or_else(|| "dummy".into());
    let title = args.next().unwrap_or_else(|| app_id.clone());

    let conn = Connection::connect_to_env().expect("wayland");
    let (globals, mut queue) = registry_queue_init(&conn).unwrap();
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).unwrap();
    let xdg = XdgShell::bind(&globals, &qh).unwrap();
    let shm = Shm::bind(&globals, &qh).unwrap();

    let window = xdg.create_window(compositor.create_surface(&qh), WindowDecorations::None, &qh);
    window.set_app_id(app_id.clone());
    window.set_title(title);
    window.commit();

    let h = app_id.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    let mut dummy = Dummy {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(64 * 64 * 4, &shm).unwrap(),
        shm,
        _window: window,
        color: [(h & 0x7f) as u8 + 64, (h >> 8 & 0x7f) as u8 + 64, (h >> 16 & 0x7f) as u8 + 64, 255],
        exit: false,
    };
    while !dummy.exit {
        queue.blocking_dispatch(&mut dummy).unwrap();
    }
}

impl WindowHandler for Dummy {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        window: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        let w = configure.new_size.0.map_or(400, std::num::NonZero::get) as i32;
        let h = configure.new_size.1.map_or(300, std::num::NonZero::get) as i32;
        let (buffer, canvas) = self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888).unwrap();
        canvas.as_chunks_mut::<4>().0.fill(self.color);
        window.wl_surface().damage_buffer(0, 0, w, h);
        buffer.attach_to(window.wl_surface()).unwrap();
        window.commit();
    }
}

impl CompositorHandler for Dummy {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
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

impl OutputHandler for Dummy {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Dummy {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(Dummy);

impl ProvidesRegistryState for Dummy {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

smithay_client_toolkit::delegate_dispatch2!(Dummy);
