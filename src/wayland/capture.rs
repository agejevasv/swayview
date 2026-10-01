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
                wl_shm,
            },
        },
    },
    shm::{Shm, raw::RawPool},
};
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    image_capture_source::v1::client::{
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
        ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};

use super::{App, NoEvents};
use crate::warn;

/// Live updates per window per second, at most.
const LIVE_FPS: u64 = 15;
/// Failed frames in a row after which a window is not asked again.
const MAX_FAILURES: u32 = 3;

pub(super) struct Capture {
    toplevels: ForeignToplevelList,
    sources: ExtForeignToplevelImageCaptureSourceManagerV1,
    copier: ExtImageCopyCaptureManagerV1,
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
    /// Two buffers of `size`: the latest frame's, and one to capture into.
    slots: Vec<Slot>,
    /// A frame being captured, and the slot it goes to.
    frame: Option<(ExtImageCopyCaptureFrameV1, usize)>,
    latest: Option<usize>,
    /// The latest frame went to a tile; only then are more asked for.
    on_screen: bool,
    failures: u32,
    /// Failing, or unusable; no more frames are asked for.
    dead: bool,
}

struct Slot {
    buffer: WlBuffer,
    size: (u32, u32),
    /// Being captured into, or shown and not yet released by sway.
    busy: bool,
}

/// User data of a stream's session and frames: the window's identifier.
struct StreamId(String);

/// User data of a slot's buffer.
struct SlotId(String, usize);

impl Capture {
    /// `None` if the compositor cannot capture single windows.
    pub(super) fn new(globals: &GlobalList, qh: &QueueHandle<App>) -> Option<Self> {
        Some(Capture {
            sources: globals.bind(qh, 1..=1, NoEvents).ok()?,
            copier: globals.bind(qh, 1..=1, NoEvents).ok()?,
            toplevels: ForeignToplevelList::new(globals, qh),
            streams: HashMap::new(),
        })
    }

    fn handle(&self, id: &str) -> Option<ExtForeignToplevelHandleV1> {
        let list = &self.toplevels;
        list.toplevels().iter().find(|h| list.info(h).is_some_and(|i| i.identifier == id)).cloned()
    }

    /// Whether every window has its first frame, or will not get one.
    pub(super) fn settled(&self) -> bool {
        self.streams.values().all(|s| s.latest.is_some() || s.dead)
    }

    /// The buffer with window `id`'s latest frame, and its size, to be shown:
    /// it is kept until sway releases it.
    pub(super) fn take_latest(&mut self, id: &str) -> Option<(WlBuffer, (u32, u32))> {
        let stream = self.streams.get_mut(id)?;
        let slot = &mut stream.slots[stream.latest?];
        slot.busy = true;
        stream.on_screen = true;
        Some((slot.buffer.clone(), slot.size))
    }

    /// Frees the buffer with window `id`'s latest frame, which no tile shows.
    pub(super) fn skip_latest(&mut self, id: &str) {
        if let Some(stream) = self.streams.get_mut(id)
            && let Some(slot) = stream.latest
        {
            stream.slots[slot].busy = false;
            stream.on_screen = false;
        }
    }
}

impl Stream {
    /// Asks for the next frame; on an error, gives up on the window.
    fn request(&mut self, id: &str, shm: &Shm, qh: &QueueHandle<App>) {
        if let Err(e) = self.try_request(id, shm, qh) {
            warn(e.context("window capture"));
            self.dead = true;
        }
    }

    /// Asks for the next frame into the buffer not holding the latest one,
    /// making new buffers first if the window changed size.
    fn try_request(&mut self, id: &str, shm: &Shm, qh: &QueueHandle<App>) -> Result<()> {
        if self.dead || !self.constrained || self.frame.is_some() {
            return Ok(());
        }
        if self.slots.first().is_none_or(|s| s.size != self.size) {
            self.make_slots(id, shm, qh)?;
        }
        let free = |i: &usize| !self.slots[*i].busy && Some(*i) != self.latest;
        let Some(slot) = (0..self.slots.len()).find(free) else { return Ok(()) };
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
        let (format, bpp) = pick_format(&self.formats, shm.formats())
            .with_context(|| format!("no supported pixel format in {:?}", self.formats))?;
        let stride = stride(w, bpp);
        let len = stride as usize * h as usize;
        // Sizes and offsets in wl_shm are `i32`.
        ensure!(len > 0 && i32::try_from(2 * len).is_ok(), "unusable window size {w}×{h}");
        let mut pool = RawPool::new(2 * len, shm)?;
        // The buffers keep the pool's memory. A replaced buffer may still be
        // shown; destroying it leaves sway's copy.
        self.slots = (0..2)
            .map(|i| {
                let offset = (i * len) as i32;
                let data = SlotId(id.to_owned(), i);
                let buffer = pool.create_buffer(offset, w as i32, h as i32, stride as i32, format, data, qh);
                Slot { buffer, size: (w, h), busy: false }
            })
            .collect();
        self.latest = None;
        Ok(())
    }
}

/// The first `offered` capture format that sway can also show, as a buffer
/// of that format, and its bytes per pixel. Sway offers just one: whichever
/// its GPU driver reads fastest. ARGB8888 and XRGB8888 are always shown;
/// others only when `wl_shm` lists them as `shown`.
fn pick_format(offered: &[wl_shm::Format], shown: &[wl_shm::Format]) -> Option<(wl_shm::Format, u32)> {
    use wl_shm::Format as F;
    let bytes = |f: F| match f {
        F::Argb8888
        | F::Xrgb8888
        | F::Abgr8888
        | F::Xbgr8888
        | F::Rgba8888
        | F::Rgbx8888
        | F::Bgra8888
        | F::Bgrx8888
        | F::Argb2101010
        | F::Xrgb2101010
        | F::Abgr2101010
        | F::Xbgr2101010 => Some(4),
        F::Rgb888 | F::Bgr888 => Some(3),
        F::Rgb565
        | F::Bgr565
        | F::Argb4444
        | F::Xrgb4444
        | F::Abgr4444
        | F::Xbgr4444
        | F::Rgba4444
        | F::Rgbx4444
        | F::Bgra4444
        | F::Bgrx4444
        | F::Argb1555
        | F::Xrgb1555
        | F::Abgr1555
        | F::Xbgr1555
        | F::Rgba5551
        | F::Rgbx5551
        | F::Bgra5551
        | F::Bgrx5551 => Some(2),
        F::Argb16161616
        | F::Xrgb16161616
        | F::Abgr16161616
        | F::Xbgr16161616
        | F::Argb16161616f
        | F::Xrgb16161616f
        | F::Abgr16161616f
        | F::Xbgr16161616f => Some(8),
        F::Rgb161616 | F::Bgr161616 | F::Bgr161616f => Some(6),
        _ => None,
    };
    let displayable = |f: &F| matches!(f, F::Argb8888 | F::Xrgb8888) || shown.contains(f);
    offered.iter().copied().filter(displayable).find_map(|f| Some((f, bytes(f)?)))
}

/// Bytes per row of a `width` wide buffer: a multiple of both the pixel
/// size and 4, as sway reads rows with OpenGL's default 4-byte alignment.
fn stride(width: u32, bpp: u32) -> u32 {
    let unit = (1..=4).map(|k| k * bpp).find(|n| n.is_multiple_of(4)).unwrap_or(4 * bpp);
    (width * bpp).next_multiple_of(unit)
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
                    stream.request(id, &app.shm, &app.qh);
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
        let Some(capture) = &mut app.capture else { return };
        // The window is gone; a tile showing it keeps sway's copy of its last frame.
        if let Event::Stopped = event {
            capture.streams.remove(&self.0);
            return;
        }
        let Some(stream) = capture.streams.get_mut(&self.0) else { return };
        match event {
            // Starts the constraints, sent again whenever the window changes size.
            Event::BufferSize { width, height } => {
                stream.size = (width, height);
                stream.formats.clear();
                stream.constrained = false;
            }
            Event::ShmFormat { format: WEnum::Value(format) } => stream.formats.push(format),
            Event::Done => {
                stream.constrained = true;
                stream.request(&self.0, &app.shm, qh);
            }
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
            Event::Failed { reason } => {
                let Some((frame, slot)) = stream.frame.take() else { return };
                frame.destroy();
                stream.slots[slot].busy = false;
                match reason {
                    // The window changed size, and the new size came just
                    // before; the next frame is asked for at that size.
                    WEnum::Value(FailureReason::BufferConstraints) => {}
                    // Followed by the session's `stopped`.
                    WEnum::Value(FailureReason::Stopped) => return,
                    _ => {
                        stream.failures += 1;
                        stream.dead |= stream.failures >= MAX_FAILURES;
                    }
                }
                // Before the first frame, live updates do not ask again.
                if stream.latest.is_none() {
                    stream.request(&self.0, &app.shm, qh);
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

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shm::Format as F;

    #[test]
    fn picks_the_first_format_sway_can_show() {
        assert_eq!(pick_format(&[F::Xbgr8888], &[F::Xbgr8888]), Some((F::Xbgr8888, 4)));
        // A 4K desktop's driver may read three bytes a pixel.
        assert_eq!(pick_format(&[F::Bgr888], &[F::Argb8888, F::Bgr888]), Some((F::Bgr888, 3)));
        // Not shown, or an unknown size: nothing.
        assert_eq!(pick_format(&[F::Bgr888], &[F::Argb8888]), None);
        assert_eq!(pick_format(&[F::Yuyv], &[F::Yuyv]), None);
        assert_eq!(pick_format(&[F::Rgb565], &[F::Rgb565]), Some((F::Rgb565, 2)));
        assert_eq!(pick_format(&[F::Abgr16161616f], &[F::Abgr16161616f]), Some((F::Abgr16161616f, 8)));
        assert_eq!(pick_format(&[F::Bgr161616], &[F::Bgr161616]), Some((F::Bgr161616, 6)));
        // ARGB8888 is always shown, listed or not.
        assert_eq!(pick_format(&[F::Bgr888, F::Argb8888], &[]), Some((F::Argb8888, 4)));
    }

    #[test]
    fn rows_are_whole_pixels_and_four_byte_aligned() {
        assert_eq!(stride(1920, 4), 7680);
        assert_eq!(stride(1921, 4), 7684);
        assert_eq!(stride(1920, 3), 5760);
        // 1001 × 3 = 3003 bytes, padded to the next multiple of 12.
        assert_eq!(stride(1001, 3), 3012);
        assert_eq!(stride(1, 3), 12);
        assert_eq!(stride(1001, 2), 2004);
        assert_eq!(stride(1001, 8), 8008);
        assert_eq!(stride(1001, 6), 6012);
    }
}
