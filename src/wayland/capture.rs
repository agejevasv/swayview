//! Live window thumbnails with `ext-image-copy-capture-v1` (sway 1.12 and
//! later). Windows without one are drawn as plain boxes.
//!
//! Every window gets a capture session that stays open while the overview is.
//! Its frames go into two shm buffers, used in turn, which `tiles` shows on a
//! subsurface as they are, for sway to scale: the pixels never pass through
//! this process. After the first frame, a request is only answered once the
//! window changes, so idle windows cost nothing.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use smithay_client_toolkit::{
    dispatch2::Dispatch2,
    foreign_toplevel_list::{ForeignToplevelList, ForeignToplevelListHandler},
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        client::{
            Connection, QueueHandle, WEnum,
            globals::GlobalList,
            protocol::{
                wl_buffer::{self, WlBuffer},
                wl_compositor::WlCompositor,
                wl_shm,
            },
        },
    },
    shm::{Shm, raw::RawPool},
    subcompositor::SubcompositorState,
};
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    image_capture_source::v1::client::{
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
        ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use super::{App, NoEvents};
use crate::warn;

/// Larger windows are not captured; keeps buffer sizes well within `i32`.
const MAX_SIDE: u32 = 16384;
/// Live updates per window per second, at most.
const LIVE_FPS: u64 = 15;
/// Failed frames in a row after which a window is not asked again.
const MAX_FAILURES: u32 = 3;

/// Formats whose pixels sway can show again as they are, in order of
/// preference: with alpha first, so translucent windows stay translucent.
const FORMATS: [wl_shm::Format; 4] =
    [wl_shm::Format::Argb8888, wl_shm::Format::Abgr8888, wl_shm::Format::Xrgb8888, wl_shm::Format::Xbgr8888];

pub struct Capture {
    toplevels: ForeignToplevelList,
    sources: ExtForeignToplevelImageCaptureSourceManagerV1,
    copier: ExtImageCopyCaptureManagerV1,
    pub(super) subcompositor: SubcompositorState,
    pub(super) viewporter: WpViewporter,
    /// By window identifier.
    streams: HashMap<String, Stream>,
}

/// One window's capture session and the buffers its frames go to.
struct Stream {
    source: ExtImageCaptureSourceV1,
    session: ExtImageCopyCaptureSessionV1,
    /// Buffer constraints, complete once `constrained`.
    size: (u32, u32),
    formats: Vec<wl_shm::Format>,
    constrained: bool,
    /// Two buffers of `size`, so one can be filled while the other is shown.
    slots: Vec<Slot>,
    /// A frame being captured, and the slot it goes to.
    frame: Option<(ExtImageCopyCaptureFrameV1, usize)>,
    /// The slot with the latest frame.
    latest: Option<usize>,
    /// The latest frame went to a tile; only then are more asked for.
    on_screen: bool,
    failures: u32,
    /// Stopped by sway, or failing; no more frames are asked for.
    dead: bool,
}

struct Slot {
    buffer: WlBuffer,
    size: (u32, u32),
    /// Being filled, or shown and not released by sway yet.
    busy: bool,
}

/// User data of a stream's session and frames: the window's identifier.
struct StreamId(String);

/// User data of a slot's buffer.
struct SlotId(String, usize);

impl Capture {
    /// `None` if the compositor cannot capture single windows or show them scaled.
    pub fn new(
        globals: &GlobalList,
        qh: &QueueHandle<App>,
        compositor: &WlCompositor,
        viewporter: Option<&WpViewporter>,
    ) -> Option<Self> {
        let sources = globals.bind(qh, 1..=1, NoEvents).ok()?;
        let copier = globals.bind(qh, 1..=1, NoEvents).ok()?;
        Some(Capture {
            subcompositor: SubcompositorState::bind(compositor.clone(), globals, qh).ok()?,
            viewporter: viewporter?.clone(),
            toplevels: ForeignToplevelList::new(globals, qh),
            sources,
            copier,
            streams: HashMap::new(),
        })
    }

    fn handle(&self, id: &str) -> Option<ExtForeignToplevelHandleV1> {
        let list = &self.toplevels;
        list.toplevels().iter().find(|h| list.info(h).is_some_and(|i| i.identifier == id)).cloned()
    }

    /// Whether every window has its first frame, or will not get one.
    pub fn settled(&self) -> bool {
        self.streams.values().all(|s| s.latest.is_some() || s.dead)
    }

    /// The buffer with window `id`'s latest frame, and its size.
    pub fn latest(&self, id: &str) -> Option<(&WlBuffer, (u32, u32))> {
        let stream = self.streams.get(id)?;
        let slot = &stream.slots[stream.latest?];
        Some((&slot.buffer, slot.size))
    }

    /// Frees the buffer with window `id`'s latest frame if it was not shown;
    /// a shown one stays busy until sway releases it.
    pub fn shown_latest(&mut self, id: &str, shown: bool) {
        if let Some(stream) = self.streams.get_mut(id)
            && let Some(slot) = stream.latest
        {
            stream.slots[slot].busy = shown;
            stream.on_screen = shown;
        }
    }
}

impl Stream {
    /// Asks for the next frame into a free buffer, making new buffers first
    /// if the window changed size.
    fn request(&mut self, id: &str, shm: &Shm, qh: &QueueHandle<App>) -> Result<()> {
        if self.dead || !self.constrained || self.frame.is_some() {
            return Ok(());
        }
        if self.slots.first().is_none_or(|s| s.size != self.size) {
            self.make_slots(id, shm, qh)?;
        }
        let Some(slot) = self.slots.iter().position(|s| !s.busy) else { return Ok(()) };
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let frame = self.session.create_frame(qh, StreamId(id.to_owned()));
        frame.attach_buffer(&self.slots[slot].buffer);
        frame.damage_buffer(0, 0, w, h);
        frame.capture();
        self.slots[slot].busy = true;
        self.frame = Some((frame, slot));
        Ok(())
    }

    fn make_slots(&mut self, id: &str, shm: &Shm, qh: &QueueHandle<App>) -> Result<()> {
        let (w, h) = self.size;
        let sides = 1..=MAX_SIDE;
        ensure!(sides.contains(&w) && sides.contains(&h), "unusable window size {w}×{h}");
        let format = FORMATS
            .into_iter()
            .find(|f| self.formats.contains(f))
            .with_context(|| format!("no supported pixel format in {:?}", self.formats))?;
        let len = w as usize * h as usize * 4;
        let mut pool = RawPool::new(2 * len, shm)?;
        // The buffers keep the pool's memory. A replaced buffer may still be
        // shown; destroying it leaves sway's copy.
        self.slots = (0..2)
            .map(|i| {
                let offset = (i * len) as i32;
                let data = SlotId(id.to_owned(), i);
                let buffer = pool.create_buffer(offset, w as i32, h as i32, w as i32 * 4, format, data, qh);
                Slot { buffer, size: (w, h), busy: false }
            })
            .collect();
        self.latest = None;
        Ok(())
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some((frame, _)) = self.frame.take() {
            frame.destroy();
        }
        self.session.destroy();
        self.source.destroy();
    }
}

impl App {
    /// Opens a capture session for every window in the tree that has none.
    pub(super) fn capture_windows(&mut self) {
        let Some(capture) = &mut self.capture else { return };
        for win in self.tree.workspaces().flat_map(|w| &w.windows) {
            let Some(id) = win.toplevel.as_deref().filter(|id| !capture.streams.contains_key(*id)) else {
                continue;
            };
            // Tried again when its handle is announced.
            let Some(handle) = capture.handle(id) else { continue };
            let source = capture.sources.create_source(&handle, &self.qh, NoEvents);
            let session =
                capture.copier.create_session(&source, Options::empty(), &self.qh, StreamId(id.into()));
            let stream = Stream {
                source,
                session,
                size: (0, 0),
                formats: Vec::new(),
                constrained: false,
                slots: Vec::new(),
                frame: None,
                latest: None,
                on_screen: false,
                failures: 0,
                dead: false,
            };
            capture.streams.insert(id.to_owned(), stream);
        }
    }

    /// Asks every window on screen for its next frame, `LIVE_FPS` times a second.
    pub(super) fn start_live_updates(&self) {
        if self.capture.is_none() {
            return;
        }
        let interval = Duration::from_millis(1000 / LIVE_FPS);
        let inserted = self.loop_handle.insert_source(Timer::from_duration(interval), move |_, (), app| {
            if let Some(capture) = &mut app.capture {
                for (id, stream) in capture.streams.iter_mut().filter(|(_, s)| s.on_screen) {
                    if let Err(e) = stream.request(id, &app.shm, &app.qh) {
                        warn(e.context("window capture"));
                        stream.dead = true;
                    }
                }
            }
            TimeoutAction::ToDuration(interval)
        });
        if let Err(e) = inserted {
            warn(format_args!("live update timer: {}", e.error));
        }
    }
}

impl ForeignToplevelListHandler for App {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelList {
        // The list is only bound, and so only sends events, as part of `Capture`.
        &mut self.capture.as_mut().expect("toplevel list events without a capture").toplevels
    }

    fn new_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: ExtForeignToplevelHandleV1) {
        self.capture_windows();
    }

    fn update_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: ExtForeignToplevelHandleV1) {}

    fn toplevel_closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: ExtForeignToplevelHandleV1) {}
}

impl Dispatch2<ExtImageCopyCaptureSessionV1, App> for StreamId {
    fn event(
        &self,
        app: &mut App,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        let Some(stream) = app.capture.as_mut().and_then(|c| c.streams.get_mut(&self.0)) else { return };
        match event {
            Event::BufferSize { width, height } => {
                stream.size = (width, height);
                stream.formats.clear();
            }
            Event::ShmFormat { format: WEnum::Value(format) } => stream.formats.push(format),
            // Sent again when the window changes size; a frame in flight then
            // fails, and the next one is asked for at the new size.
            Event::Done => {
                stream.constrained = true;
                if let Err(e) = stream.request(&self.0, &app.shm, qh) {
                    warn(e.context("window capture"));
                    stream.dead = true;
                }
            }
            Event::Stopped => stream.dead = true,
            _ => {}
        }
    }
}

impl Dispatch2<ExtImageCopyCaptureFrameV1, App> for StreamId {
    fn event(
        &self,
        app: &mut App,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        let Some(stream) = app.capture.as_mut().and_then(|c| c.streams.get_mut(&self.0)) else { return };
        match event {
            Event::Ready => {
                let Some((frame, slot)) = stream.frame.take() else { return };
                frame.destroy();
                stream.latest = Some(slot);
                stream.failures = 0;
                app.show_capture(&self.0);
            }
            Event::Failed { .. } => {
                let Some((frame, slot)) = stream.frame.take() else { return };
                frame.destroy();
                stream.slots[slot].busy = false;
                stream.failures += 1;
                stream.dead |= stream.failures >= MAX_FAILURES;
                // Before the first frame, live updates have not started to ask again.
                if stream.latest.is_none()
                    && let Err(e) = stream.request(&self.0, &app.shm, qh)
                {
                    warn(e.context("window capture"));
                    stream.dead = true;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch2<WlBuffer, App> for SlotId {
    fn event(
        &self,
        app: &mut App,
        buffer: &WlBuffer,
        event: wl_buffer::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wl_buffer::Event::Release = event
            && let Some(stream) = app.capture.as_mut().and_then(|c| c.streams.get_mut(&self.0))
            && let Some(slot) = stream.slots.get_mut(self.1).filter(|s| s.buffer == *buffer)
        {
            slot.busy = false;
        }
    }
}
