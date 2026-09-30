//! A window's thumbnail and what is drawn over it, as two subsurfaces of the
//! overlay, stacked in the scene's drawing order. The overlay itself then only
//! draws the workspaces.

use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::reexports::client::{
    QueueHandle,
    protocol::{wl_buffer::WlBuffer, wl_subsurface::WlSubsurface, wl_surface::WlSurface},
};
use smithay_client_toolkit::shell::WaylandSurface;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use super::capture::Capture;
use super::{App, NoEvents};
use crate::model::Rect;

pub(super) struct Tile {
    thumb: Part,
    deco: Part,
    /// Logical x, y, width and height on the overlay.
    rect: (i32, i32, i32, i32),
    /// Size of the frame shown by `thumb`, once there is one.
    shown: Option<(u32, u32)>,
}

struct Part {
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
}

impl Part {
    fn new(
        capture: &Capture,
        compositor: &CompositorState,
        parent: &WlSurface,
        qh: &QueueHandle<App>,
    ) -> Self {
        let (subsurface, surface) = capture.subcompositor.create_subsurface(parent.clone(), qh);
        // Clicks go through to the overlay, which knows the layout.
        if let Ok(nothing) = Region::new(compositor) {
            surface.set_input_region(Some(nothing.wl_region()));
        }
        let viewport = capture.viewporter.get_viewport(&surface, qh, NoEvents);
        Part { surface, subsurface, viewport }
    }
}

impl Drop for Part {
    fn drop(&mut self) {
        self.viewport.destroy();
        self.subsurface.destroy();
        self.surface.destroy();
    }
}

impl Tile {
    pub(super) fn has_frame(&self) -> bool {
        self.shown.is_some()
    }

    pub(super) fn size(&self) -> (i32, i32) {
        (self.rect.2, self.rect.3)
    }

    pub(super) fn deco(&self) -> (&WlSurface, &WpViewport) {
        (&self.deco.surface, &self.deco.viewport)
    }

    fn show(&mut self, buffer: &WlBuffer, size: (u32, u32)) {
        let t = &self.thumb;
        t.surface.attach(Some(buffer), 0, 0);
        t.surface.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
        self.shown = Some(size);
        self.fit();
    }

    /// Scales the frame to cover the tile, cutting off what sticks out.
    fn fit(&self) {
        let Some(size) = self.shown else { return };
        let (x, y, w, h) = cover(size, self.size());
        let t = &self.thumb;
        t.viewport.set_source(x, y, w, h);
        t.viewport.set_destination(self.rect.2, self.rect.3);
        t.surface.commit();
    }
}

/// `r` on whole logical pixels, as subsurfaces are placed.
fn snap(r: Rect) -> (i32, i32, i32, i32) {
    let (x0, y0) = (r.x.round(), r.y.round());
    let (x1, y1) = ((r.x + r.w).round(), (r.y + r.h).round());
    (x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32)
}

/// The middle of a `bw`×`bh` buffer with the shape of a `w`×`h` box, as x, y,
/// width and height, in the 1/256 steps a viewport takes, rounded to stay
/// inside the buffer.
fn cover((bw, bh): (u32, u32), (w, h): (i32, i32)) -> (f64, f64, f64, f64) {
    let (bw, bh, w, h) = (f64::from(bw), f64::from(bh), f64::from(w), f64::from(h));
    let s = (w / bw).max(h / bh);
    let q = |v: f64| (v * 256.0).floor() / 256.0;
    let (sw, sh) = (q((w / s).min(bw)).max(1.0 / 256.0), q((h / s).min(bh)).max(1.0 / 256.0));
    (q((bw - sw) / 2.0), q((bh - sh) / 2.0), sw, sh)
}

impl App {
    /// Gives each window of surface `i` a tile, in its place and stacked in
    /// drawing order, and drops the tiles of windows that are gone.
    pub(super) fn sync_tiles(&mut self, i: usize) {
        let Some(capture) = &mut self.capture else { return };
        let s = &mut self.surfaces[i];
        let parent = s.layer.wl_surface().clone();
        s.tiles.retain(|id, _| s.scene.windows.iter().any(|w| w.id == *id));
        let mut below = parent.clone();
        for win in &s.scene.windows {
            let rect = snap(win.rect);
            if rect.2 < 1 || rect.3 < 1 {
                s.tiles.remove(&win.id);
                continue;
            }
            let tile = s.tiles.entry(win.id).or_insert_with(|| {
                let thumb = Part::new(capture, &self.compositor, &parent, &self.qh);
                // Frames arrive on their own time, not with the overlay's commits.
                thumb.subsurface.set_desync();
                let deco = Part::new(capture, &self.compositor, &parent, &self.qh);
                Tile { thumb, deco, rect, shown: None }
            });
            for part in [&tile.thumb, &tile.deco] {
                part.subsurface.set_position(rect.0, rect.1);
                part.subsurface.place_above(&below);
                below = part.surface.clone();
            }
            let moved = tile.rect != rect;
            tile.rect = rect;
            match win.toplevel.as_deref().and_then(|id| Some((id, capture.latest(id)?))) {
                Some((id, (buffer, size))) if tile.shown.is_none() => {
                    let buffer = buffer.clone();
                    tile.show(&buffer, size);
                    capture.shown_latest(id, true);
                }
                _ if moved => tile.fit(),
                _ => {}
            }
        }
    }

    /// Shows the latest frame of window `id` on its tile.
    pub(super) fn show_capture(&mut self, id: &str) {
        let Some(capture) = &mut self.capture else { return };
        let Some((buffer, size)) = capture.latest(id).map(|(b, s)| (b.clone(), s)) else { return };
        let found = self.surfaces.iter_mut().enumerate().find_map(|(i, s)| {
            let win = s.scene.windows.iter().find(|w| w.toplevel.as_deref() == Some(id))?;
            Some((i, s.tiles.get_mut(&win.id)?))
        });
        let Some((i, tile)) = found else {
            capture.shown_latest(id, false);
            return;
        };
        let first = !tile.has_frame();
        tile.show(&buffer, size);
        capture.shown_latest(id, true);
        // Drawn as a plain box until now; what goes over a frame differs.
        if first {
            self.redraw(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_to_logical_pixels() {
        assert_eq!(snap(Rect::new(10.3, 20.7, 101.1, 60.3)), (10, 21, 101, 60));
        assert_eq!(snap(Rect::new(5.0, 5.0, 0.2, 0.2)).2, 0);
    }

    #[test]
    fn frame_covers_the_tile() {
        // Same shape: all of it.
        assert_eq!(cover((1920, 1080), (192, 108)), (0.0, 0.0, 1920.0, 1080.0));
        // A narrow tile: the middle columns.
        assert_eq!(cover((1920, 1080), (100, 200)), (690.0, 0.0, 540.0, 1080.0));
        // A wide tile: the middle rows.
        assert_eq!(cover((1000, 1000), (200, 100)), (0.0, 250.0, 1000.0, 500.0));
        // Never past the buffer's edge.
        for (buf, tile) in [((1531, 977), (333, 211)), ((7, 3), (1, 999)), ((1, 1), (5, 3))] {
            let (x, y, w, h) = cover(buf, tile);
            assert!(x >= 0.0 && y >= 0.0 && w > 0.0 && h > 0.0);
            assert!(x + w <= f64::from(buf.0) && y + h <= f64::from(buf.1), "{buf:?} {tile:?}");
        }
    }
}
