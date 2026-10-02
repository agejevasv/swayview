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

pub(super) struct Tile {
    thumb: Part,
    deco: Part,
    place: Place,
    /// Whether `thumb` shows a frame yet.
    shown: bool,
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
    fn new(g: &Globals<'_>, parent: &WlSurface, place: Place) -> Self {
        let thumb = Part::new(g, parent);
        // Frames arrive on their own time, not with the overlay's commits.
        thumb.subsurface.set_desync();
        let deco = Part::new(g, parent);
        Tile { thumb, deco, place, shown: false, drawn: None, moved: false }
    }

    fn show(&mut self, buffer: &WlBuffer, size: (u32, u32)) {
        let t = &self.thumb;
        t.surface.attach(Some(buffer), 0, 0);
        t.surface.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
        self.shown = true;
        self.fit();
    }

    /// Stretches the frame over the tile, whatever their shapes. They differ
    /// a little as the tile includes the title bar and borders, and more for
    /// tabbed, stacked and fullscreen windows, whose tile is not their shape.
    fn fit(&self) {
        if !self.shown {
            return;
        }
        let t = &self.thumb;
        t.viewport.set_destination(self.place.w, self.place.h);
        t.surface.commit();
    }

    fn moved_to(&mut self, place: Place) {
        self.place = place;
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
        let behind = if self.shown { Behind::Thumbnail } else { Behind::Nothing };
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
            let tile = s.tiles.entry(win.id).or_insert_with(|| Tile::new(&g, &parent, place));
            tile.drawn = None;
            for part in [&tile.thumb, &tile.deco] {
                part.subsurface.set_position(place.x, place.y);
                part.subsurface.place_above(&below);
                below = part.surface.clone();
            }
            if !tile.shown
                && let Some((buffer, size)) = win.toplevel.as_deref().and_then(|id| capture.take_latest(id))
            {
                tile.place = place;
                tile.show(&buffer, size);
            } else if tile.place != place {
                tile.moved_to(place);
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
        let first = !tile.shown;
        tile.show(&buffer, size);
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
}
