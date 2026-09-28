//! Places workspace boxes in a grid and windows inside them. Pure, no Wayland.

use crate::model::{ConId, Rect, Workspace};

pub const HEADER: f32 = 28.0;
const WINDOW_GAP: f32 = 3.0;
/// A workspace box never gets larger than this fraction of the surface.
const MAX_BOX: f32 = 0.5;

#[derive(Debug)]
pub struct WsItem {
    pub name: String,
    pub header: Rect,
    pub rect: Rect,
    pub focused: bool,
    pub visible: bool,
}

#[derive(Debug)]
pub struct WinItem {
    pub id: ConId,
    pub app: String,
    pub title: String,
    pub rect: Rect,
    pub focused: bool,
}

#[derive(Debug, Default)]
pub struct Scene {
    /// The sway output this scene is shown on, in global layout coordinates.
    pub output: Rect,
    /// Surface size the scene was laid out for.
    pub size: (f32, f32),
    pub workspaces: Vec<WsItem>,
    /// Drawing order; the last one is on top.
    pub windows: Vec<WinItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Window(usize),
    Workspace(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    /// Offset from `from` to `to` as (distance along `self`, distance across it).
    pub fn project(self, from: (f32, f32), to: (f32, f32)) -> (f32, f32) {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        match self {
            Dir::Left => (-dx, dy),
            Dir::Right => (dx, dy),
            Dir::Up => (-dy, dx),
            Dir::Down => (dy, dx),
        }
    }
}

/// Lays out the `workspaces` of `output` on a `w`×`h` surface.
pub fn build(workspaces: &[Workspace], output: Rect, w: f32, h: f32) -> Scene {
    let mut scene = Scene { output, size: (w, h), ..Scene::default() };
    let n = workspaces.len();
    if n == 0 || output.is_empty() {
        return scene;
    }
    let pad = (w.min(h) * 0.03).clamp(12.0, 48.0);
    // Workspace boxes are miniatures of the output.
    let aspect = output.w / output.h;

    // Pick the column count that gives the largest boxes.
    let (mut cols, mut box_w) = (1, 0.0);
    for c in 1..=n {
        let rows = n.div_ceil(c);
        let by_w = (w - pad * (c as f32 + 1.0)) / c as f32;
        let by_h = ((h - pad * (rows as f32 + 1.0)) / rows as f32 - HEADER) * aspect;
        let bw = by_w.min(by_h);
        if bw > box_w {
            (cols, box_w) = (c, bw);
        }
    }
    box_w = box_w.min(w * MAX_BOX).min((h * MAX_BOX - HEADER) * aspect).max(1.0);
    let box_h = box_w / aspect;
    let cell_h = HEADER + box_h;
    let rows = n.div_ceil(cols);
    let grid_h = rows as f32 * cell_h + (rows as f32 - 1.0) * pad;
    let top = (h - grid_h) / 2.0;

    for (i, ws) in workspaces.iter().enumerate() {
        let (row, col) = (i / cols, i % cols);
        let in_row = if row == rows - 1 { n - row * cols } else { cols };
        let row_w = in_row as f32 * box_w + (in_row as f32 - 1.0) * pad;
        let x = (w - row_w) / 2.0 + col as f32 * (box_w + pad);
        let y = top + row as f32 * (cell_h + pad);
        let rect = Rect::new(x, y + HEADER, box_w, box_h);

        for win in &ws.windows {
            let r = output.map_into(win.rect, rect).inset(WINDOW_GAP / 2.0);
            scene.windows.push(WinItem {
                id: win.id,
                app: win.app.clone(),
                title: win.title.clone(),
                rect: clip(r, rect),
                focused: win.focused,
            });
        }
        scene.workspaces.push(WsItem {
            name: ws.name.clone(),
            header: Rect::new(x, y, box_w, HEADER),
            rect,
            focused: ws.focused,
            visible: ws.visible,
        });
    }
    scene
}

fn clip(r: Rect, to: Rect) -> Rect {
    let x0 = r.x.max(to.x);
    let y0 = r.y.max(to.y);
    let x1 = (r.x + r.w).min(to.x + to.w);
    let y1 = (r.y + r.h).min(to.y + to.h);
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

impl Scene {
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        // The gap around a window belongs to it, so the pointer never falls
        // through to the workspace between windows.
        let grown = |r: Rect| r.inset(-WINDOW_GAP / 2.0);
        if let Some(i) = self.windows.iter().rposition(|w| grown(w.rect).contains(x, y)) {
            return Some(Hit::Window(i));
        }
        self.workspaces
            .iter()
            .position(|ws| ws.rect.contains(x, y) || ws.header.contains(x, y))
            .map(Hit::Workspace)
    }

    pub fn focused_window(&self) -> Option<usize> {
        self.windows.iter().position(|w| w.focused)
    }

    pub fn window_by_id(&self, id: ConId) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    /// Center of window `i` as a fraction of the surface size.
    pub fn relative_center(&self, i: usize) -> (f32, f32) {
        let (x, y) = self.windows[i].rect.center();
        (x / self.size.0.max(1.0), y / self.size.1.max(1.0))
    }

    /// Nearest window from `from` in direction `dir`, judged by box centers.
    pub fn neighbor(&self, from: usize, dir: Dir) -> Option<usize> {
        let origin = self.windows[from].rect.center();
        self.windows
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != from)
            .filter_map(|(i, w)| {
                let (along, across) = dir.project(origin, w.rect.center());
                (along > 1.0).then_some((i, along + 2.0 * across.abs()))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::tree;

    fn scene() -> Scene {
        let t = tree();
        let o = &t.outputs[0];
        build(&o.workspaces, o.rect, 1920.0, 1080.0)
    }

    #[test]
    fn boxes_fit_and_do_not_overlap() {
        let s = scene();
        assert_eq!(s.workspaces.len(), 3);
        for (i, a) in s.workspaces.iter().enumerate() {
            assert!(a.header.y >= 0.0 && a.rect.y + a.rect.h <= 1080.0);
            assert!(a.rect.x >= 0.0 && a.rect.x + a.rect.w <= 1920.0);
            for b in &s.workspaces[i + 1..] {
                let apart = a.rect.x + a.rect.w <= b.rect.x
                    || b.rect.x + b.rect.w <= a.rect.x
                    || a.rect.y + a.rect.h <= b.header.y
                    || b.rect.y + b.rect.h <= a.header.y;
                assert!(apart, "{} overlaps {}", a.name, b.name);
            }
        }
    }

    #[test]
    fn hit_prefers_floating_window() {
        let s = scene();
        let float = s.windows.iter().position(|w| w.app == "pavucontrol").unwrap();
        let (x, y) = s.windows[float].rect.center();
        assert_eq!(s.hit(x, y), Some(Hit::Window(float)));
        let ws = &s.workspaces[0];
        assert_eq!(s.hit(ws.header.x + 1.0, ws.header.y + 1.0), Some(Hit::Workspace(0)));
        assert_eq!(s.hit(1.0, 1.0), None);
        // Between two adjacent windows is still a window, not the workspace.
        let (a, b) = (&s.windows[0].rect, &s.windows[1].rect);
        let gap_x = (a.x + a.w + b.x) / 2.0;
        assert!(matches!(s.hit(gap_x, a.y + a.h / 2.0), Some(Hit::Window(_))));
    }

    #[test]
    fn neighbor_moves_spatially() {
        let s = scene();
        let id = |app: &str| s.windows.iter().position(|w| w.app == app).unwrap();
        let htop = s.windows.iter().position(|w| w.title == "htop").unwrap();
        assert_eq!(s.neighbor(htop, Dir::Up), Some(id("firefox")));
        assert_eq!(s.neighbor(id("firefox"), Dir::Right), Some(id("code")));
    }

    #[test]
    fn empty_scene_keeps_output_and_size() {
        let s = build(&[], Rect::new(10.0, 0.0, 5.0, 5.0), 100.0, 50.0);
        assert!(s.windows.is_empty() && s.workspaces.is_empty());
        assert_eq!((s.output.x, s.size), (10.0, (100.0, 50.0)));
    }
}
