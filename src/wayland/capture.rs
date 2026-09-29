//! Window thumbnails, captured once per window with `ext-image-copy-capture-v1`
//! (sway 1.11 and later). Windows without one are drawn as plain boxes.
//!
//! Thumbnails are held back until every window is captured, then fade in
//! together. After `WAIT_LIMIT`, they show as they arrive, and a capture sway
//! has not answered by then is given up.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use smithay_client_toolkit::{
    dispatch2::Dispatch2,
    foreign_toplevel_list::{ForeignToplevelList, ForeignToplevelListHandler},
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        client::{
            Connection, QueueHandle, WEnum,
            globals::GlobalList,
            protocol::{wl_buffer::WlBuffer, wl_shm},
        },
    },
    shm::{Shm, raw::RawPool},
};
use tiny_skia::{IntSize, Pixmap};
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

use super::{App, NoEvents, render_scale};
use crate::render::Thumb;
use crate::warn;

/// Captures running at once; each holds a full-size copy of its window.
const MAX_JOBS: usize = 4;
/// Larger windows are not captured; keeps buffer sizes well within `i32`.
const MAX_SIDE: u32 = 16384;
const WAIT_LIMIT: Duration = Duration::from_secs(1);
const FADE: Duration = Duration::from_millis(200);

/// In order of preference: with alpha first, so translucent windows stay translucent.
const FORMATS: [wl_shm::Format; 4] =
    [wl_shm::Format::Argb8888, wl_shm::Format::Abgr8888, wl_shm::Format::Xrgb8888, wl_shm::Format::Xbgr8888];

pub struct Capture {
    toplevels: ForeignToplevelList,
    sources: ExtForeignToplevelImageCaptureSourceManagerV1,
    copier: ExtImageCopyCaptureManagerV1,
    /// Windows captured or being captured, by identifier; each is captured once.
    started: HashSet<String>,
    jobs: HashMap<String, Job>,
    /// Captured, not shown yet.
    ready: Vec<String>,
    /// Until `WAIT_LIMIT` passes; after that, thumbnails show as they arrive.
    waiting: bool,
    /// Thumbnails fading in, with when they started.
    fading: Vec<(String, Instant)>,
}

/// One window being captured.
struct Job {
    source: ExtImageCaptureSourceV1,
    session: ExtImageCopyCaptureSessionV1,
    /// Physical size the thumbnail is drawn at, as laid out when it was requested.
    target: (f32, f32),
    /// Buffer constraints, sent before the session's `done`.
    size: (u32, u32),
    formats: Vec<wl_shm::Format>,
    frame: Option<Frame>,
    /// A frame failed because the window changed size, and was asked for again.
    retried: bool,
    since: Instant,
}

struct Frame {
    proxy: ExtImageCopyCaptureFrameV1,
    pool: RawPool,
    buffer: WlBuffer,
    size: (u32, u32),
    format: wl_shm::Format,
}

/// User data of a job's session and frame: the window's identifier.
struct JobId(String);

impl Capture {
    /// `None` if the compositor cannot capture single windows.
    pub fn new(globals: &GlobalList, qh: &QueueHandle<App>) -> Option<Self> {
        let sources = globals.bind(qh, 1..=1, NoEvents).ok()?;
        let copier = globals.bind(qh, 1..=1, NoEvents).ok()?;
        Some(Capture {
            toplevels: ForeignToplevelList::new(globals, qh),
            sources,
            copier,
            started: HashSet::new(),
            jobs: HashMap::new(),
            ready: Vec::new(),
            waiting: true,
            fading: Vec::new(),
        })
    }

    fn handle(&self, id: &str) -> Option<ExtForeignToplevelHandleV1> {
        let list = &self.toplevels;
        list.toplevels().iter().find(|h| list.info(h).is_some_and(|i| i.identifier == id)).cloned()
    }

    fn start(
        &mut self,
        id: &str,
        handle: &ExtForeignToplevelHandleV1,
        target: (f32, f32),
        qh: &QueueHandle<App>,
    ) {
        let source = self.sources.create_source(handle, qh, NoEvents);
        let session = self.copier.create_session(&source, Options::empty(), qh, JobId(id.to_owned()));
        let job = Job {
            source,
            session,
            target,
            size: (0, 0),
            formats: Vec::new(),
            frame: None,
            retried: false,
            since: Instant::now(),
        };
        self.started.insert(id.to_owned());
        self.jobs.insert(id.to_owned(), job);
    }
}

impl Job {
    /// Asks for a frame into a new shm buffer, once the constraints are known.
    fn capture(&mut self, shm: &Shm, qh: &QueueHandle<App>, id: &str) -> Result<()> {
        let (w, h) = self.size;
        let sides = 1..=MAX_SIDE;
        ensure!(sides.contains(&w) && sides.contains(&h), "unusable window size {w}×{h}");
        let format = FORMATS
            .into_iter()
            .find(|f| self.formats.contains(f))
            .with_context(|| format!("no supported pixel format in {:?}", self.formats))?;
        let (wi, hi) = (w as i32, h as i32);
        let mut pool = RawPool::new(w as usize * h as usize * 4, shm)?;
        let buffer = pool.create_buffer(0, wi, hi, wi * 4, format, NoEvents, qh);
        let frame = self.session.create_frame(qh, JobId(id.to_owned()));
        frame.attach_buffer(&buffer);
        frame.damage_buffer(0, 0, wi, hi);
        frame.capture();
        self.frame = Some(Frame { proxy: frame, pool, buffer, size: (w, h), format });
        Ok(())
    }

    /// The captured frame, shrunk near the size it is drawn at.
    fn thumbnail(&mut self) -> Option<Pixmap> {
        let f = self.frame.as_mut()?;
        let (w, h) = f.size;
        let data = f.pool.mmap().get(..w as usize * h as usize * 4)?;
        Some(shrink(to_pixmap(data, w, h, f.format)?, self.target))
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        self.proxy.destroy();
        self.buffer.destroy();
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.frame = None;
        self.session.destroy();
        self.source.destroy();
    }
}

impl App {
    /// Stops holding thumbnails back once `WAIT_LIMIT` has passed, and from
    /// then on every `WAIT_LIMIT` gives up captures older than that, which
    /// would otherwise hold their slot forever.
    pub(super) fn limit_capture_wait(&self) {
        if self.capture.is_none() {
            return;
        }
        let timer = Timer::from_duration(WAIT_LIMIT);
        let inserted = self.loop_handle.insert_source(timer, |_, (), app| {
            if let Some(capture) = &mut app.capture {
                capture.waiting = false;
                capture.jobs.retain(|_, job| job.since.elapsed() < WAIT_LIMIT);
            }
            app.capture_windows();
            TimeoutAction::ToDuration(WAIT_LIMIT)
        });
        if let Err(e) = inserted {
            warn(format_args!("capture timer: {}", e.error));
        }
    }

    /// Starts capturing the windows shown on any surface, a few at a time,
    /// and shows the thumbnails once all are in.
    pub(super) fn capture_windows(&mut self) {
        let Some(capture) = &mut self.capture else { return };
        // Windows are known once their surface is configured and laid out.
        let mut all_started = self.surfaces.iter().all(|s| s.size.is_some());
        for s in &self.surfaces {
            let scale = render_scale(s.scale);
            for win in &s.scene.windows {
                let Some(id) = win.toplevel.as_deref().filter(|id| !capture.started.contains(*id)) else {
                    continue;
                };
                let handle = if capture.jobs.len() < MAX_JOBS { capture.handle(id) } else { None };
                // Tried again when its handle is announced, or when a capture ends.
                match handle {
                    Some(handle) => {
                        capture.start(id, &handle, (win.rect.w * scale, win.rect.h * scale), &self.qh);
                    }
                    None => all_started = false,
                }
            }
        }
        if !capture.waiting || (all_started && capture.jobs.is_empty()) {
            self.show_thumbs();
        }
    }

    fn end_capture(&mut self, id: &str, thumb: Option<Pixmap>) {
        if let Some(capture) = &mut self.capture {
            capture.jobs.remove(id);
            if let Some(thumb) = thumb {
                capture.ready.push(id.to_owned());
                self.thumbs.insert(id.to_owned(), Thumb::new(thumb));
                // Now, while still hidden, so the fade does not wait for it.
                self.fit_thumbs();
            }
        }
        self.capture_windows();
    }

    pub(super) fn fit_thumbs(&mut self) {
        for s in &self.surfaces {
            for win in &s.scene.windows {
                if let Some(thumb) = win.toplevel.as_ref().and_then(|id| self.thumbs.get_mut(id)) {
                    thumb.fit(win.rect, render_scale(s.scale));
                }
            }
        }
    }

    fn show_thumbs(&mut self) {
        let Some(capture) = &mut self.capture else { return };
        if capture.ready.is_empty() {
            return;
        }
        let now = Instant::now();
        capture.fading.extend(capture.ready.drain(..).map(|id| (id, now)));
        self.redraw_all();
    }

    /// Brings the opacity of fading thumbnails up to date; true while any is still fading.
    pub(super) fn step_fades(&mut self) -> bool {
        let Some(capture) = &mut self.capture else { return false };
        let now = Instant::now();
        capture.fading.retain(|(id, start)| {
            let t = (now - *start).as_secs_f32() / FADE.as_secs_f32();
            if let Some(thumb) = self.thumbs.get_mut(id) {
                thumb.opacity = t.min(1.0);
            }
            t < 1.0
        });
        !capture.fading.is_empty()
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

impl Dispatch2<ExtImageCopyCaptureSessionV1, App> for JobId {
    fn event(
        &self,
        app: &mut App,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        let Some(job) = app.capture.as_mut().and_then(|c| c.jobs.get_mut(&self.0)) else { return };
        match event {
            Event::BufferSize { width, height } => {
                job.size = (width, height);
                job.formats.clear();
            }
            Event::ShmFormat { format: WEnum::Value(format) } => job.formats.push(format),
            // A later `done` means the window changed size, and the frame fails.
            Event::Done if job.frame.is_none() => {
                if let Err(e) = job.capture(&app.shm, qh, &self.0) {
                    warn(e.context("window capture"));
                    app.end_capture(&self.0, None);
                }
            }
            Event::Stopped => app.end_capture(&self.0, None),
            _ => {}
        }
    }
}

impl Dispatch2<ExtImageCopyCaptureFrameV1, App> for JobId {
    fn event(
        &self,
        app: &mut App,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        let job = app.capture.as_mut().and_then(|c| c.jobs.get_mut(&self.0));
        match event {
            Event::Ready => {
                let thumb = job.and_then(Job::thumbnail);
                app.end_capture(&self.0, thumb);
            }
            Event::Failed { reason } => {
                // The window changed size; its new size came with the `done` before this.
                let resized = reason == WEnum::Value(FailureReason::BufferConstraints);
                match job {
                    Some(job) if resized && !job.retried => {
                        job.retried = true;
                        job.frame = None;
                        if let Err(e) = job.capture(&app.shm, qh, &self.0) {
                            warn(e.context("window capture"));
                            app.end_capture(&self.0, None);
                        }
                    }
                    _ => app.end_capture(&self.0, None),
                }
            }
            _ => {}
        }
    }
}

/// Converts shm pixels in a little-endian 32-bit `format` to a pixmap.
fn to_pixmap(data: &[u8], w: u32, h: u32, format: wl_shm::Format) -> Option<Pixmap> {
    // In memory, ARGB8888 is B, G, R, A and ABGR8888 is R, G, B, A.
    let (bgr, opaque) = match format {
        wl_shm::Format::Argb8888 => (true, false),
        wl_shm::Format::Xrgb8888 => (true, true),
        wl_shm::Format::Abgr8888 => (false, false),
        wl_shm::Format::Xbgr8888 => (false, true),
        _ => return None,
    };
    let mut rgba = Vec::with_capacity(data.len());
    for &[p0, p1, p2, p3] in data.as_chunks::<4>().0 {
        let (r, g, b) = if bgr { (p2, p1, p0) } else { (p0, p1, p2) };
        let a = if opaque { 255 } else { p3 };
        // Premultiplied, so no channel may exceed alpha; a bad client could break that.
        rgba.extend_from_slice(&[r.min(a), g.min(a), b.min(a), a]);
    }
    Pixmap::from_vec(rgba, IntSize::from_wh(w, h)?)
}

/// Halves `pix` for as long as it still covers `target`, so drawing it
/// reduces it at most 2×, which the draw-time filter handles well.
fn shrink(mut pix: Pixmap, (tw, th): (f32, f32)) -> Pixmap {
    while pix.width() as f32 / 2.0 >= tw.max(1.0) && pix.height() as f32 / 2.0 >= th.max(1.0) {
        let Some(half) = halve(&pix) else { break };
        pix = half;
    }
    pix
}

/// Averages each 2×2 block into one pixel; an odd last row or column is dropped.
fn halve(pix: &Pixmap) -> Option<Pixmap> {
    let (w, h) = (pix.width() as usize / 2, pix.height() as usize / 2);
    let mut out = Pixmap::new(w as u32, h as u32)?;
    let (src, stride) = (pix.data(), pix.width() as usize * 4);
    let dst = out.data_mut();
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let i = 2 * y * stride + 8 * x + c;
                let sum: u16 =
                    [i, i + 4, i + stride, i + stride + 4].iter().map(|&j| u16::from(src[j])).sum();
                dst[(y * w + x) * 4 + c] = ((sum + 2) / 4) as u8;
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_formats() {
        // One translucent red pixel, premultiplied: R=0x80, A=0x80.
        let argb = to_pixmap(&[0x00, 0x00, 0x80, 0x80], 1, 1, wl_shm::Format::Argb8888).unwrap();
        assert_eq!(argb.data(), [0x80, 0x00, 0x00, 0x80]);
        let abgr = to_pixmap(&[0x80, 0x00, 0x00, 0x80], 1, 1, wl_shm::Format::Abgr8888).unwrap();
        assert_eq!(abgr.data(), argb.data());
        // X formats ignore the fourth byte.
        let xrgb = to_pixmap(&[0x10, 0x20, 0x30, 0x00], 1, 1, wl_shm::Format::Xrgb8888).unwrap();
        assert_eq!(xrgb.data(), [0x30, 0x20, 0x10, 0xff]);
        // Colors brighter than alpha are clamped to stay premultiplied.
        let bad = to_pixmap(&[0xff, 0xff, 0xff, 0x40], 1, 1, wl_shm::Format::Argb8888).unwrap();
        assert_eq!(bad.data(), [0x40, 0x40, 0x40, 0x40]);
        assert!(to_pixmap(&[0; 4], 1, 1, wl_shm::Format::Rgb565).is_none());
        assert!(to_pixmap(&[0; 4], 2, 1, wl_shm::Format::Argb8888).is_none());
    }

    #[test]
    fn halving_averages_blocks() {
        let mut pix = Pixmap::new(3, 2).unwrap();
        // Left 2×2 block: two white and two black opaque pixels; the third column is dropped.
        for (i, v) in [255u8, 0, 99, 0, 255, 99].into_iter().enumerate() {
            pix.data_mut()[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, 255]);
        }
        let half = halve(&pix).unwrap();
        assert_eq!((half.width(), half.height()), (1, 1));
        assert_eq!(half.data(), [128, 128, 128, 255]);
    }

    #[test]
    fn shrinks_while_covering_the_target() {
        let size = |w, h, target| {
            let p = shrink(Pixmap::new(w, h).unwrap(), target);
            (p.width(), p.height())
        };
        assert_eq!(size(1920, 1080, (200.0, 100.0)), (240, 135));
        // A narrow tab slot is covered by height.
        assert_eq!(size(1920, 1080, (50.0, 300.0)), (960, 540));
        // Never below one pixel, and never enlarged.
        assert_eq!(size(4, 4, (0.0, 0.0)), (1, 1));
        assert_eq!(size(100, 100, (500.0, 500.0)), (100, 100));
    }
}
