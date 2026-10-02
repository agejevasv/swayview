//! A window's thumbnail and what is drawn over it, as two subsurfaces of the
//! overlay, stacked in the scene's drawing order. The overlay itself then only
//! draws the workspaces.

use anyhow::Result;
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::reexports::client::{
    QueueHandle,
    protocol::{wl_buffer::WlBuffer, wl_subsurface::WlSubsurface, wl_surface::WlSurface},
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::subcompositor::SubcompositorState;
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};

use super::{App, NoEvents, attach, buffer_size};
use crate::layout::WinItem;
use crate::model::Rect;
use crate::render::{Behind, Renderer};
use crate::warn;

pub(super) struct Tile {
    thumb: Part,
    deco: Part,
    place: Place,
    /// See `WinItem::crop`.
    crop: (f32, f32),
    /// Size of the frame shown by `thumb`, once there is one.
    shown: Option<(u32, u32)>,
    /// What `deco` was last drawn for, to skip drawing it the same again.
    drawn: Option<Deco>,
    /// Moved since the overlay's last commit. Until that commit, which moves
    /// it, the thumbnail waits for it too, so its new size shows in its new
    /// place.
    moved: bool,
}

/// Where a tile is, in whole logical pixels, as subsurfaces are placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Place {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Deco {
    selected: bool,
    behind: Behind,
    size: (u32, u32),
}

struct Part {
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
}

/// What creating a tile's subsurfaces needs.
pub(super) struct Globals<'a> {
    pub compositor: &'a CompositorState,
    pub subcompositor: &'a SubcompositorState,
    pub viewporter: &'a WpViewporter,
    pub qh: &'a QueueHandle<App>,
}

impl Part {
    fn new(g: &Globals<'_>, parent: &WlSurface) -> Self {
        let (subsurface, surface) = g.subcompositor.create_subsurface(parent.clone(), g.qh);
        // Clicks go through to the overlay, which knows the layout.
        if let Ok(nothing) = Region::new(g.compositor) {
            surface.set_input_region(Some(nothing.wl_region()));
        }
        let viewport = g.viewporter.get_viewport(&surface, g.qh, NoEvents);
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
    fn new(g: &Globals<'_>, parent: &WlSurface, place: Place, crop: (f32, f32)) -> Self {
        let thumb = Part::new(g, parent);
        // Frames arrive on their own time, not with the overlay's commits.
        thumb.subsurface.set_desync();
        let deco = Part::new(g, parent);
        Tile { thumb, deco, place, crop, shown: None, drawn: None, moved: false }
    }

    /// Shows a frame, unless the tile's part of it is not inside it.
    fn show(&mut self, buffer: &WlBuffer, size: (u32, u32)) -> bool {
        if source(size, self.crop).is_none() {
            bad_source(size, self.crop);
            return false;
        }
        let t = &self.thumb;
        t.surface.attach(Some(buffer), 0, 0);
        t.surface.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
        self.shown = Some(size);
        self.fit();
        true
    }

    /// Stretches the tile's part of the frame over the tile. Their shapes
    /// differ a little, as the tile includes the title bar and borders, and
    /// more for fullscreen windows, whose tile is not their shape.
    ///
    /// A source outside the frame would make sway disconnect swayview, so
    /// rather than that, the frame is hidden.
    fn fit(&mut self) {
        let Some(size) = self.shown else { return };
        let t = &self.thumb;
        match source(size, self.crop) {
            Some((w, h)) if (w, h) == (f64::from(size.0), f64::from(size.1)) => {
                t.viewport.set_source(-1.0, -1.0, -1.0, -1.0);
            }
            Some((w, h)) => t.viewport.set_source(0.0, 0.0, w, h),
            None => {
                bad_source(size, self.crop);
                t.surface.attach(None, 0, 0);
                self.shown = None;
            }
        }
        t.viewport.set_destination(self.place.w, self.place.h);
        t.surface.commit();
    }

    fn moved_to(&mut self, place: Place, crop: (f32, f32)) {
        (self.place, self.crop) = (place, crop);
        self.moved = true;
        self.thumb.subsurface.set_sync();
        self.fit();
    }

    /// Draws what goes over the thumbnail, at `scale` `SCALE_UNIT`s, unless
    /// it is drawn so already. Shows with the overlay's next commit.
    pub(super) fn draw_deco(
        &mut self,
        renderer: &mut Renderer,
        pool: &mut SlotPool,
        win: &WinItem,
        selected: bool,
        scale: u32,
    ) -> Result<()> {
        let Place { w, h, .. } = self.place;
        let (f, size) = buffer_size((w as u32, h as u32), scale);
        let behind = if self.shown.is_some() { Behind::Thumbnail } else { Behind::Nothing };
        let deco = Deco { selected, behind, size };
        if self.drawn == Some(deco) {
            return Ok(());
        }
        let Some(pix) = renderer.draw_tile(win, selected, behind, size, f) else { return Ok(()) };
        self.deco.viewport.set_destination(w, h);
        attach(pool, &self.deco.surface, &pix)?;
        self.deco.surface.commit();
        self.drawn = Some(deco);
        Ok(())
    }

    /// Lets the thumbnail of a moved tile update on its own again, once the
    /// overlay committed its move.
    pub(super) fn committed(&mut self) {
        if std::mem::take(&mut self.moved) {
            self.thumb.subsurface.set_desync();
        }
    }
}

fn snap(r: Rect) -> Place {
    let (x0, y0) = (r.x.round(), r.y.round());
    let (x1, y1) = ((r.x + r.w).round(), (r.y + r.h).round());
    Place { x: x0 as i32, y: y0 as i32, w: (x1 - x0) as i32, h: (y1 - y0) as i32 }
}

/// The `crop` part of a `bw`×`bh` buffer from its top-left, as a viewport
/// source's width and height, in the 1/256 steps it takes; `None` unless it
/// is inside the buffer and not empty. With no buffer scale set, the source
/// is in buffer pixels.
fn source((bw, bh): (u32, u32), (fx, fy): (f32, f32)) -> Option<(f64, f64)> {
    let part = |b: u32, f: f32| {
        let v = (f64::from(b) * f64::from(f) * 256.0).floor() / 256.0;
        (v > 0.0 && v <= f64::from(b)).then_some(v)
    };
    Some((part(bw, fx)?, part(bh, fy)?))
}

fn bad_source((w, h): (u32, u32), crop: (f32, f32)) {
    warn(format_args!("not showing a thumbnail: part {crop:?} of its {w}×{h} frame is not inside it"));
}

impl App {
    /// Gives each window of surface `i` a tile, in its place and stacked in
    /// drawing order, and drops the tiles of windows that are gone. Every
    /// deco is drawn again, as the windows may have changed.
    pub(super) fn sync_tiles(&mut self, i: usize) {
        let (Some(capture), Some(subcompositor), Some(viewporter)) =
            (&mut self.capture, &self.subcompositor, &self.viewporter)
        else {
            return;
        };
        let g = Globals { compositor: &self.compositor, subcompositor, viewporter, qh: &self.qh };
        let s = &mut self.surfaces[i];
        let parent = s.layer.wl_surface().clone();
        s.tiles.retain(|id, _| s.scene.windows.iter().any(|w| w.id == *id));
        let mut below = parent.clone();
        for win in &s.scene.windows {
            let place = snap(win.rect);
            if place.w < 1 || place.h < 1 {
                s.tiles.remove(&win.id);
                continue;
            }
            let tile = s.tiles.entry(win.id).or_insert_with(|| Tile::new(&g, &parent, place, win.crop));
            tile.drawn = None;
            for part in [&tile.thumb, &tile.deco] {
                part.subsurface.set_position(place.x, place.y);
                part.subsurface.place_above(&below);
                below = part.surface.clone();
            }
            if tile.shown.is_none()
                && let Some(id) = win.toplevel.as_deref()
                && let Some((buffer, size)) = capture.take_latest(id)
            {
                (tile.place, tile.crop) = (place, win.crop);
                if !tile.show(&buffer, size) {
                    capture.skip_latest(id);
                }
            } else if (tile.place, tile.crop) != (place, win.crop) {
                tile.moved_to(place, win.crop);
            }
        }
    }

    /// Shows the latest frame of window `id` on its tile.
    pub(super) fn show_capture(&mut self, id: &str) {
        let Some(capture) = &mut self.capture else { return };
        let found = self.surfaces.iter_mut().enumerate().find_map(|(i, s)| {
            let win = s.scene.windows.iter().find(|w| w.toplevel.as_deref() == Some(id))?;
            Some((i, s.tiles.get_mut(&win.id)?))
        });
        let Some((i, tile)) = found else {
            capture.skip_latest(id);
            return;
        };
        let Some((buffer, size)) = capture.take_latest(id) else { return };
        let first = tile.shown.is_none();
        if !tile.show(&buffer, size) {
            capture.skip_latest(id);
            return;
        }
        // Its deco was drawn as a plain box until now.
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
        assert_eq!(snap(Rect::new(10.3, 20.7, 101.1, 60.3)), Place { x: 10, y: 21, w: 101, h: 60 });
        assert_eq!(snap(Rect::new(5.0, 5.0, 0.2, 0.2)).w, 0);
    }

    #[test]
    fn source_is_inside_the_buffer() {
        assert_eq!(source((1920, 1080), (1.0, 1.0)), Some((1920.0, 1080.0)));
        assert_eq!(source((1920, 1080), (0.5, 1.0)), Some((960.0, 1080.0)));
        // Rounded down to 1/256.
        assert_eq!(source((1001, 3), (1.0 / 3.0, 0.5)), Some((333.6640625, 1.5)));
        for (buf, crop) in [((1531, 977), (0.999_999, 1.0)), ((7, 3), (0.37, 0.91)), ((1, 1), (0.01, 1.0))] {
            let (w, h) = source(buf, crop).unwrap();
            assert!(w > 0.0 && h > 0.0 && w <= f64::from(buf.0) && h <= f64::from(buf.1), "{buf:?} {crop:?}");
        }
        // Outside or empty: nothing.
        for crop in
            [(1.01, 1.0), (0.0, 1.0), (1.0, -0.5), (f32::NAN, 1.0), (f32::INFINITY, 1.0), (0.00001, 1.0)]
        {
            assert_eq!(source((100, 100), crop), None, "{crop:?}");
        }
        assert_eq!(source((0, 100), (1.0, 1.0)), None);
    }
}
