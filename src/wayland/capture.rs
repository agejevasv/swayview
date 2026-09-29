//! Window thumbnails, captured once per window with `ext-image-copy-capture-v1`
//! (sway 1.11 and later), each shown as soon as it arrives. Windows without
//! one are drawn as plain boxes.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, ensure};
use smithay_client_toolkit::{
    dispatch2::Dispatch2,
    foreign_toplevel_list::{ForeignToplevelList, ForeignToplevelListHandler},
    reexports::client::{
        Connection, QueueHandle, WEnum,
        globals::GlobalList,
        protocol::{wl_buffer::WlBuffer, wl_shm},
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

/// Larger windows are not captured; keeps buffer sizes well within `i32`.
const MAX_SIDE: u32 = 16384;

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
        let job =
            Job { source, session, target, size: (0, 0), formats: Vec::new(), frame: None, retried: false };
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
        thumbnail_of(data, w, h, f.format, self.target)
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
    /// Starts capturing the windows shown on any surface, all at once.
    pub(super) fn capture_windows(&mut self) {
        let Some(capture) = &mut self.capture else { return };
        for s in &self.surfaces {
            let scale = render_scale(s.scale);
            for win in &s.scene.windows {
                let Some(id) = win.toplevel.as_deref().filter(|id| !capture.started.contains(*id)) else {
                    continue;
                };
                // Tried again when its handle is announced.
                if let Some(handle) = capture.handle(id) {
                    capture.start(id, &handle, (win.rect.w * scale, win.rect.h * scale), &self.qh);
                }
            }
        }
    }

    fn end_capture(&mut self, id: &str, thumb: Option<Pixmap>) {
        if let Some(capture) = &mut self.capture {
            capture.jobs.remove(id);
        }
        if let Some(thumb) = thumb {
            self.thumbs.insert(id.to_owned(), Thumb::new(thumb));
            self.redraw_all();
        }
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

/// Captured pixels, `w`×`h` in a little-endian 32-bit shm `format`, as a
/// pixmap shrunk near `target`, see `shrink`.
fn thumbnail_of(data: &[u8], w: u32, h: u32, format: wl_shm::Format, target: (f32, f32)) -> Option<Pixmap> {
    // In memory, ARGB8888 is B, G, R, A and ABGR8888 is R, G, B, A.
    match format {
        wl_shm::Format::Argb8888 => shrink(data, w, h, target, |[b, g, r, a]| premultiplied(r, g, b, a)),
        wl_shm::Format::Xrgb8888 => shrink(data, w, h, target, |[b, g, r, _]| [r, g, b, 255]),
        wl_shm::Format::Abgr8888 => shrink(data, w, h, target, |[r, g, b, a]| premultiplied(r, g, b, a)),
        wl_shm::Format::Xbgr8888 => shrink(data, w, h, target, |[r, g, b, _]| [r, g, b, 255]),
        _ => None,
    }
}

/// No channel may exceed alpha; a bad client could break that.
fn premultiplied(r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
    [r.min(a), g.min(a), b.min(a), a]
}

/// Converts `w`×`h` pixels with `px` into a pixmap, halving it for as long as
/// it still covers `target`, so fitting it to its box reduces it at most 2×,
/// which bilinear filtering handles well. The first halving converts as it
/// goes, sparing a full-size copy.
fn shrink(
    data: &[u8],
    w: u32,
    h: u32,
    (tw, th): (f32, f32),
    px: impl Fn([u8; 4]) -> [u8; 4],
) -> Option<Pixmap> {
    if data.len() != w as usize * h as usize * 4 {
        return None;
    }
    let halvable = |w: u32, h: u32| w as f32 / 2.0 >= tw.max(1.0) && h as f32 / 2.0 >= th.max(1.0);
    if !halvable(w, h) {
        let rgba = data.as_chunks::<4>().0.iter().flat_map(|p| px(*p)).collect();
        return Pixmap::from_vec(rgba, IntSize::from_wh(w, h)?);
    }
    let mut pix = halve(data, w, h, px)?;
    while halvable(pix.width(), pix.height()) {
        let Some(half) = halve(pix.data(), pix.width(), pix.height(), |p| p) else { break };
        pix = half;
    }
    Some(pix)
}

/// Averages each 2×2 block of `w`×`h` pixels, converted with `px`, into one;
/// an odd last row or column is dropped.
fn halve(src: &[u8], w: u32, h: u32, px: impl Fn([u8; 4]) -> [u8; 4]) -> Option<Pixmap> {
    let (w, h) = (w as usize, h as usize);
    let mut out = Pixmap::new((w / 2) as u32, (h / 2) as u32)?;
    let rows = out.data_mut().chunks_exact_mut(w / 2 * 4);
    for (row, pair) in rows.zip(src.chunks_exact(w * 8)) {
        let (top, bottom) = pair.split_at(w * 4);
        let top = top.as_chunks::<4>().0.as_chunks::<2>().0;
        let bottom = bottom.as_chunks::<4>().0.as_chunks::<2>().0;
        for ((d, &[t0, t1]), &[b0, b1]) in row.as_chunks_mut::<4>().0.iter_mut().zip(top).zip(bottom) {
            let [t0, t1, b0, b1] = [px(t0), px(t1), px(b0), px(b1)];
            for c in 0..4 {
                let sum = u16::from(t0[c]) + u16::from(t1[c]) + u16::from(b0[c]) + u16::from(b1[c]);
                d[c] = ((sum + 2) / 4) as u8;
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
        let one = |data: [u8; 4], format| thumbnail_of(&data, 1, 1, format, (1.0, 1.0)).unwrap();
        // One translucent red pixel, premultiplied: R=0x80, A=0x80.
        let argb = one([0x00, 0x00, 0x80, 0x80], wl_shm::Format::Argb8888);
        assert_eq!(argb.data(), [0x80, 0x00, 0x00, 0x80]);
        assert_eq!(one([0x80, 0x00, 0x00, 0x80], wl_shm::Format::Abgr8888).data(), argb.data());
        // X formats ignore the fourth byte.
        assert_eq!(one([0x10, 0x20, 0x30, 0x00], wl_shm::Format::Xrgb8888).data(), [0x30, 0x20, 0x10, 0xff]);
        // Colors brighter than alpha are clamped to stay premultiplied.
        assert_eq!(one([0xff, 0xff, 0xff, 0x40], wl_shm::Format::Argb8888).data(), [0x40; 4]);
        assert!(thumbnail_of(&[0; 4], 1, 1, wl_shm::Format::Rgb565, (1.0, 1.0)).is_none());
        assert!(thumbnail_of(&[0; 4], 2, 1, wl_shm::Format::Argb8888, (1.0, 1.0)).is_none());
    }

    #[test]
    fn halving_averages_blocks() {
        // Left 2×2 block: two white and two black opaque pixels; the third column is dropped.
        let src: Vec<u8> = [255u8, 0, 99, 0, 255, 99].into_iter().flat_map(|v| [v, v, v, 255]).collect();
        let half = halve(&src, 3, 2, |p| p).unwrap();
        assert_eq!((half.width(), half.height()), (1, 1));
        assert_eq!(half.data(), [128, 128, 128, 255]);
    }

    #[test]
    fn first_halving_converts() {
        // 2×2 XRGB, opaque: blue 0x40 and 0x80, in memory B, G, R, X.
        let data = [0x40, 0, 0, 0, 0x80, 0, 0, 0, 0x40, 0, 0, 0, 0x80, 0, 0, 0];
        let half = thumbnail_of(&data, 2, 2, wl_shm::Format::Xrgb8888, (1.0, 1.0)).unwrap();
        assert_eq!(half.data(), [0, 0, 0x60, 0xff]);
    }

    #[test]
    fn shrinks_while_covering_the_target() {
        let size = |w: u32, h: u32, target| {
            let data = vec![0; w as usize * h as usize * 4];
            let p = shrink(&data, w, h, target, |p| p).unwrap();
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
