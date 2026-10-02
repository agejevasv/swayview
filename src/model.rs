//! Sway tree (as returned by `get_tree`) reduced to what the overview draws.
//!
//! Not shown on purpose: the scratchpad (sway's hidden `__i3` output) and
//! outputs without workspaces.

use std::fmt;

use serde::Deserialize;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Side by side.
    Horizontal,
    /// On top of each other.
    Vertical,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// Shrinks by `d` on every side; a negative `d` grows.
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, (self.w - 2.0 * d).max(0.0), (self.h - 2.0 * d).max(0.0))
    }

    /// Maps `r`, given in the coordinate space of `self`, into `to`.
    pub fn map_into(&self, r: Rect, to: Rect) -> Rect {
        let sx = to.w / self.w;
        let sy = to.h / self.h;
        Rect::new(to.x + (r.x - self.x) * sx, to.y + (r.y - self.y) * sy, r.w * sx, r.h * sy)
    }

    /// Splits along `axis` into consecutive parts, one per fraction of the whole.
    fn split(&self, fractions: &[f32], axis: Axis) -> Vec<Rect> {
        let mut offset = 0.0;
        fractions
            .iter()
            .map(|f| {
                let r = match axis {
                    Axis::Horizontal => Rect::new(self.x + self.w * offset, self.y, self.w * f, self.h),
                    Axis::Vertical => Rect::new(self.x, self.y + self.h * offset, self.w, self.h * f),
                };
                offset += f;
                r
            })
            .collect()
    }

    /// Splits into a grid of `n` equal cells, with about as many rows as
    /// columns, so the cells come close to `self`'s shape. When the counts
    /// differ, there are more cells along `axis`. A short last row is centered.
    fn grid(&self, n: usize, axis: Axis) -> Vec<Rect> {
        let long = (1..=n).find(|k| k * k >= n).unwrap_or(1);
        let short = n.div_ceil(long);
        let (cols, rows) = match axis {
            Axis::Horizontal => (long, short),
            Axis::Vertical => (short, long),
        };
        let (w, h) = (self.w / cols as f32, self.h / rows as f32);
        (0..n)
            .map(|i| {
                let (row, col) = (i / cols, i % cols);
                let in_row = cols.min(n - row * cols);
                let x = self.x + (self.w - in_row as f32 * w) / 2.0 + col as f32 * w;
                Rect::new(x, self.y + row as f32 * h, w, h)
            })
            .collect()
    }
}

/// A sway container id, as used in `[con_id=…]` criteria.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
pub struct ConId(pub i64);

impl fmt::Display for ConId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone)]
pub struct Tree {
    /// Bounding box of all outputs, the origin of `seat cursor set`.
    pub root: Rect,
    pub outputs: Vec<Output>,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub name: String,
    pub rect: Rect,
    pub workspaces: Vec<Workspace>,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub name: String,
    pub num: Option<i32>,
    /// Contains the focused window, or is itself focused (empty workspace).
    pub focused: bool,
    /// In drawing order: tiled, then floating.
    pub windows: Vec<Window>,
}

#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools, reason = "independent flags, as sway reports them")]
pub struct Window {
    pub id: ConId,
    pub app: String,
    pub title: String,
    pub rect: Rect,
    pub floating: bool,
    /// The fullscreen container the window is in, or is.
    pub fullscreen: Option<Fullscreen>,
    pub sticky: bool,
    pub urgent: bool,
    pub focused: bool,
    /// Its `ext_foreign_toplevel_list_v1` identifier; sway 1.11 and later.
    pub toplevel: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fullscreen {
    pub id: ConId,
    /// Fullscreen on all outputs (`fullscreen toggle global`), not only its workspace.
    pub global: bool,
}

/// What sway had focused before the overview took over the keyboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Focus {
    pub workspace: String,
    pub window: Option<ConId>,
}

/// Where to put the cursor for a focus change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WarpTarget<'a> {
    pub output: &'a str,
    pub point: (f32, f32),
}

impl Tree {
    pub fn from_json(json: &[u8]) -> anyhow::Result<Tree> {
        let root: Node = serde_json::from_slice(json)?;
        Ok(Tree::from_root(&root))
    }

    fn from_root(root: &Node) -> Tree {
        let outputs = root
            .nodes
            .iter()
            .filter(|o| o.ty == NodeType::Output && !o.name_str().starts_with("__"))
            .map(|o| {
                let workspaces = o
                    .nodes
                    .iter()
                    .filter(|w| w.ty == NodeType::Workspace)
                    .map(|w| workspace(w, o.rect.into()))
                    .collect();
                Output { name: o.name_str().to_string(), rect: o.rect.into(), workspaces }
            })
            .collect();
        Tree { root: root.rect.into(), outputs }
    }

    pub fn workspaces(&self) -> impl Iterator<Item = &Workspace> {
        self.outputs.iter().flat_map(|o| o.workspaces.iter())
    }

    pub fn output(&self, name: &str) -> Option<&Output> {
        self.outputs.iter().find(|o| o.name == name)
    }

    pub fn focus(&self) -> Option<Focus> {
        let ws = self.workspaces().find(|w| w.focused)?;
        Some(Focus {
            workspace: ws.name.clone(),
            window: ws.windows.iter().find(|w| w.focused).map(|w| w.id),
        })
    }

    /// Marks `focus` as focused. While the overview holds the keyboard, sway
    /// reports nothing as focused, so it is carried over from the first tree.
    pub fn restore_focus(&mut self, focus: &Focus) {
        if self.focus().is_some() {
            return;
        }
        for ws in self.outputs.iter_mut().flat_map(|o| o.workspaces.iter_mut()) {
            ws.focused = ws.name == focus.workspace;
            for w in &mut ws.windows {
                w.focused = Some(w.id) == focus.window;
            }
        }
    }

    /// The center of window `id`.
    pub fn window_target(&self, id: ConId) -> Option<WarpTarget<'_>> {
        self.outputs.iter().find_map(|o| {
            let win = o.workspaces.iter().flat_map(|w| &w.windows).find(|w| w.id == id)?;
            Some(WarpTarget { output: &o.name, point: win.rect.center() })
        })
    }

    /// The center of the output holding the first workspace matching `pred`.
    pub fn workspace_target(&self, pred: impl Fn(&Workspace) -> bool) -> Option<WarpTarget<'_>> {
        let o = self.outputs.iter().find(|o| o.workspaces.iter().any(&pred))?;
        Some(WarpTarget { output: &o.name, point: o.rect.center() })
    }

    /// The fullscreen container that stops sway from focusing window `id`, if
    /// any: one on the same workspace, or a global one anywhere. Sway refuses
    /// to focus windows hidden behind a fullscreen container.
    pub fn fullscreen_blocker(&self, id: ConId) -> Option<ConId> {
        let (ws, target) =
            self.workspaces().find_map(|ws| Some((ws, ws.windows.iter().find(|w| w.id == id)?)))?;
        let own = target.fullscreen.map(|f| f.id);
        let blocks = |f: &Fullscreen| Some(f.id) != own;
        let local = ws.windows.iter().filter_map(|w| w.fullscreen).find(|f| !f.global && blocks(f));
        let global = || {
            self.workspaces()
                .flat_map(|w| &w.windows)
                .filter_map(|w| w.fullscreen)
                .find(|f| f.global && blocks(f))
        };
        local.or_else(global).map(|f| f.id)
    }

    pub fn focused_output(&self) -> Option<&Output> {
        self.outputs.iter().find(|o| o.workspaces.iter().any(|w| w.focused))
    }
}

fn workspace(node: &Node, output_rect: Rect) -> Workspace {
    let mut c = Collector { floating: false, fullscreen: None, windows: Vec::new() };
    c.children(node, node.rect.into());
    c.floating = true;
    for child in &node.floating_nodes {
        // A fullscreen floating window's own geometry is not reported; it is
        // shown in the middle of the output instead of covering everything.
        let target = if child.fullscreen_mode == 0 {
            child.outer_rect()
        } else {
            let o = output_rect;
            Rect::new(o.x + o.w / 4.0, o.y + o.h / 4.0, o.w / 2.0, o.h / 2.0)
        };
        c.node(child, target);
    }
    let mut windows = c.windows;
    windows.sort_by_key(|w| w.floating);
    Workspace {
        name: node.name_str().to_string(),
        num: node.num.filter(|n| *n >= 0),
        focused: node.focused || node.any_focused(),
        windows,
    }
}

/// How a container's children are placed.
enum Arrange {
    /// At their own rects, scaled into the container's target.
    Mapped,
    /// As equal slices, when rects are missing.
    Slices(Axis),
    /// In a grid: tabbed and stacked children all share one rect in sway.
    Grid(Axis),
    /// By their share of the container. A fullscreen child reports the whole
    /// output as its rect; this puts it back in its place in the layout.
    Shares(Axis),
}

/// Collects the leaf windows of one workspace.
struct Collector {
    floating: bool,
    /// The fullscreen container being collected, if any.
    fullscreen: Option<Fullscreen>,
    windows: Vec<Window>,
}

impl Collector {
    /// Collects `node`'s windows, drawing it into `target`.
    fn node(&mut self, node: &Node, target: Rect) {
        // Of the window or a container holding it; either way drawn in its tiled place.
        let fullscreen = self.fullscreen.or_else(|| node.fullscreen());
        if node.nodes.is_empty() {
            self.windows.push(Window {
                id: node.id,
                app: node.app_name(),
                title: node.name_str().to_string(),
                rect: target,
                floating: self.floating,
                fullscreen,
                sticky: node.sticky,
                urgent: node.urgent,
                focused: node.focused,
                toplevel: node.foreign_toplevel_identifier.clone(),
            });
            return;
        }
        let outer = std::mem::replace(&mut self.fullscreen, fullscreen);
        self.children(node, target);
        self.fullscreen = outer;
    }

    /// Collects the windows of `node`'s tiled children, drawing it into `target`.
    fn children(&mut self, node: &Node, target: Rect) {
        let own: Rect = node.rect.into();
        let axis = if node.layout == Layout::Splitv { Axis::Vertical } else { Axis::Horizontal };
        let missing = own.is_empty() || node.nodes.iter().any(|c| c.outer_rect().is_empty());
        let arrange = match node.layout {
            Layout::Tabbed => Arrange::Grid(Axis::Horizontal),
            Layout::Stacked => Arrange::Grid(Axis::Vertical),
            _ if node.nodes.iter().any(|c| c.fullscreen_mode != 0) => Arrange::Shares(axis),
            _ if missing => Arrange::Slices(axis),
            _ => Arrange::Mapped,
        };
        let n = node.nodes.len();
        let slots = match arrange {
            Arrange::Slices(axis) => target.split(&equal_shares(n), axis),
            Arrange::Grid(axis) => target.grid(n, axis),
            Arrange::Shares(axis) => target.split(&shares(&node.nodes), axis),
            Arrange::Mapped => node.nodes.iter().map(|c| own.map_into(c.outer_rect(), target)).collect(),
        };
        for (child, slot) in node.nodes.iter().zip(slots) {
            self.node(child, slot);
        }
    }
}

/// Fractions of their container for `children` along its split axis. A
/// fullscreen child's own share is meaningless, so it gets what its siblings
/// leave. Falls back to equal shares when the numbers do not add up.
fn shares(children: &[Node]) -> Vec<f32> {
    let equal = equal_shares(children.len());
    let known = |c: &Node| c.percent.filter(|p| c.fullscreen_mode == 0 && *p > 0.0 && *p <= 1.0);
    let taken: f32 = children.iter().filter_map(known).sum();
    let unknown = children.iter().filter(|c| known(c).is_none()).count();
    let rest = (1.0 - taken) / unknown.max(1) as f32;
    if unknown > 0 && rest <= 0.0 {
        return equal;
    }
    let fractions: Vec<f32> = children.iter().map(|c| known(c).unwrap_or(rest)).collect();
    let total: f32 = fractions.iter().sum();
    if total <= 0.0 {
        return equal;
    }
    fractions.iter().map(|f| f / total).collect()
}

fn equal_shares(n: usize) -> Vec<f32> {
    vec![1.0 / n.max(1) as f32; n]
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum NodeType {
    Root,
    Output,
    Workspace,
    Con,
    FloatingCon,
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Layout {
    Splith,
    Splitv,
    Stacked,
    Tabbed,
    /// `none` on leaves, `output`, `dockarea`.
    #[default]
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Node {
    id: ConId,
    name: Option<String>,
    #[serde(rename = "type")]
    ty: NodeType,
    rect: RawRect,
    deco_rect: Option<RawRect>,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    urgent: bool,
    #[serde(default)]
    sticky: bool,
    #[serde(default)]
    nodes: Vec<Node>,
    #[serde(default)]
    floating_nodes: Vec<Node>,
    #[serde(default)]
    layout: Layout,
    app_id: Option<String>,
    foreign_toplevel_identifier: Option<String>,
    window_properties: Option<WindowProperties>,
    num: Option<i32>,
    /// 0 none, 1 workspace, 2 global.
    #[serde(default)]
    fullscreen_mode: u8,
    /// Share of the parent container along its split axis.
    percent: Option<f32>,
}

#[derive(Deserialize)]
struct WindowProperties {
    class: Option<String>,
    instance: Option<String>,
}

#[derive(Deserialize, Clone, Copy)]
struct RawRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl From<RawRect> for Rect {
    fn from(r: RawRect) -> Self {
        Rect::new(r.x as f32, r.y as f32, r.width as f32, r.height as f32)
    }
}

impl Node {
    fn name_str(&self) -> &str {
        self.name.as_deref().unwrap_or("")
    }

    /// `rect` grown to include the title bar, which sway reports separately.
    fn outer_rect(&self) -> Rect {
        let mut r = Rect::from(self.rect);
        let deco = self.deco_rect.map_or(0.0, |d| d.height.max(0) as f32);
        r.y -= deco;
        r.h += deco;
        r
    }

    fn fullscreen(&self) -> Option<Fullscreen> {
        (self.fullscreen_mode != 0).then_some(Fullscreen { id: self.id, global: self.fullscreen_mode == 2 })
    }

    fn any_focused(&self) -> bool {
        self.nodes.iter().chain(&self.floating_nodes).any(|n| n.focused || n.any_focused())
    }

    fn app_name(&self) -> String {
        let props = self.window_properties.as_ref();
        [
            self.app_id.as_deref(),
            props.and_then(|p| p.class.as_deref()),
            props.and_then(|p| p.instance.as_deref()),
        ]
        .into_iter()
        .flatten()
        .find(|s| !s.is_empty())
        .unwrap_or("?")
        .to_string()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn tree() -> Tree {
        Tree::from_json(include_bytes!("../tests/fixtures/sway-1.4-tree.json")).unwrap()
    }

    fn ws<'a>(t: &'a Tree, name: &str) -> &'a Workspace {
        t.workspaces().find(|w| w.name == name).unwrap()
    }

    #[test]
    fn skips_scratchpad_output() {
        let t = tree();
        assert_eq!(t.outputs.len(), 1);
        let names: Vec<_> = t.workspaces().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["1", "2", "3"]);
    }

    #[test]
    fn output_by_name() {
        let t = tree();
        assert_eq!(t.output("HEADLESS-1").map(|o| o.workspaces.len()), Some(3));
        assert!(t.output("nope").is_none());
    }

    #[test]
    fn window_rects_include_title_bar() {
        let t = tree();
        let foot = &ws(&t, "1").windows[0];
        assert_eq!(foot.app, "foot");
        assert_eq!(foot.rect, Rect::new(0.0, 0.0, 960.0, 1080.0));
    }

    #[test]
    fn tabbed_children_go_in_a_grid() {
        let t = tree();
        let rects: Vec<_> = ws(&t, "2").windows.iter().map(|w| w.rect).collect();
        assert_eq!(
            rects,
            [
                Rect::new(0.0, 0.0, 960.0, 540.0),
                Rect::new(960.0, 0.0, 960.0, 540.0),
                Rect::new(480.0, 540.0, 960.0, 540.0),
            ]
        );
    }

    #[test]
    fn grid_cells_come_close_to_the_shape() {
        let r = Rect::new(0.0, 0.0, 600.0, 400.0);
        let sizes = |n, axis| r.grid(n, axis).iter().map(|c| (c.w, c.h)).collect::<Vec<_>>();
        assert_eq!(sizes(1, Axis::Horizontal), [(600.0, 400.0)]);
        // Two: side by side for tabbed, on top of each other for stacked.
        assert_eq!(sizes(2, Axis::Horizontal), [(300.0, 400.0); 2]);
        assert_eq!(sizes(2, Axis::Vertical), [(600.0, 200.0); 2]);
        assert_eq!(sizes(4, Axis::Vertical), [(300.0, 200.0); 4]);
        // Five stacked: three rows of two, the last one centered.
        let cells = r.grid(5, Axis::Vertical);
        assert_eq!(cells[4], Rect::new(150.0, 800.0 / 3.0, 300.0, 400.0 / 3.0));
    }

    #[test]
    fn grid_cells_tile_the_container_in_reading_order() {
        let r = Rect::new(10.0, 20.0, 600.0, 400.0);
        let level = |a: &Rect, b: &Rect| (a.y - b.y).abs() < 0.01;
        for axis in [Axis::Horizontal, Axis::Vertical] {
            for n in 1..=12 {
                let cells = r.grid(n, axis);
                assert_eq!(cells.len(), n);
                for (i, a) in cells.iter().enumerate() {
                    assert!(a.x >= r.x && a.y >= r.y, "{n} {axis:?} {a:?}");
                    assert!(
                        a.x + a.w <= r.x + r.w + 0.01 && a.y + a.h <= r.y + r.h + 0.01,
                        "{n} {axis:?} {a:?}"
                    );
                    for b in &cells[i + 1..] {
                        // Later cells come after: further right on the same row, or lower.
                        let after = b.y >= a.y + a.h - 0.01 || (level(a, b) && b.x >= a.x + a.w - 0.01);
                        assert!(after, "{n} {axis:?} {a:?} then {b:?}");
                    }
                }
                // The last row is centered.
                let last = cells[n - 1];
                let first = cells.iter().find(|c| level(c, &last)).unwrap();
                let (left, right) = (first.x - r.x, r.x + r.w - (last.x + last.w));
                assert!((left - right).abs() < 0.01, "{n} {axis:?}");
            }
        }
        // More columns for tabbed, more rows for stacked.
        let cols = |n, axis| r.grid(n, axis).iter().filter(|c| level(c, &r)).count();
        assert_eq!(cols(6, Axis::Horizontal), 3);
        assert_eq!(cols(6, Axis::Vertical), 2);
    }

    #[test]
    fn stacked_and_tabbed_children_and_their_splits() {
        // L beside a container of A, a split of B and C, and `more`; off the origin.
        let json = |layout, more| {
            format!(
                r#"{{"id":1,"type":"root","rect":{{"x":0,"y":0,"width":400,"height":300}},
                "nodes":[{{"id":2,"name":"X","type":"output","rect":{{"x":0,"y":100,"width":400,"height":200}},"nodes":[
                  {{"id":3,"name":"1","type":"workspace","layout":"splith","rect":{{"x":0,"y":100,"width":400,"height":200}},"nodes":[
                    {{"id":10,"name":"L","type":"con","rect":{{"x":0,"y":100,"width":200,"height":200}}}},
                    {{"id":11,"type":"con","layout":"{layout}","rect":{{"x":200,"y":100,"width":200,"height":200}},"nodes":[
                      {{"id":12,"name":"A","type":"con","rect":{{"x":200,"y":140,"width":200,"height":160}}}},
                      {{"id":13,"type":"con","layout":"splith","rect":{{"x":200,"y":140,"width":200,"height":160}},"nodes":[
                        {{"id":14,"name":"B","type":"con","rect":{{"x":200,"y":140,"width":150,"height":160}}}},
                        {{"id":15,"name":"C","type":"con","rect":{{"x":350,"y":140,"width":50,"height":160}}}}]}}{more}]}}]}}]}}]}}"#
            )
        };
        let rects = |layout, more| {
            let t = Tree::from_json(json(layout, more).as_bytes()).unwrap();
            ws(&t, "1").windows.iter().map(|w| (w.title.clone(), w.rect)).collect::<Vec<_>>()
        };
        let named = |v: &[(&str, Rect)]| v.iter().map(|(t, r)| (t.to_string(), *r)).collect::<Vec<_>>();
        let l = ("L", Rect::new(0.0, 100.0, 200.0, 200.0));
        assert_eq!(
            rects("stacked", ""),
            named(&[
                l,
                ("A", Rect::new(200.0, 100.0, 200.0, 100.0)),
                ("B", Rect::new(200.0, 200.0, 150.0, 100.0)),
                ("C", Rect::new(350.0, 200.0, 50.0, 100.0)),
            ])
        );
        assert_eq!(
            rects("tabbed", ""),
            named(&[
                l,
                ("A", Rect::new(200.0, 100.0, 100.0, 200.0)),
                ("B", Rect::new(300.0, 100.0, 75.0, 200.0)),
                ("C", Rect::new(375.0, 100.0, 25.0, 200.0)),
            ])
        );
        // A third one: a grid, not slices, the last one centered below.
        let d = r#",{"id":16,"name":"D","type":"con","rect":{"x":200,"y":140,"width":200,"height":160}}"#;
        assert_eq!(rects("stacked", d)[4], ("D".to_string(), Rect::new(250.0, 200.0, 100.0, 100.0)));
    }

    #[test]
    fn warp_targets() {
        let t = tree();
        let htop = ws(&t, "1").windows.iter().find(|w| w.title == "htop").unwrap();
        assert_eq!(
            t.window_target(htop.id),
            Some(WarpTarget { output: "HEADLESS-1", point: htop.rect.center() })
        );
        assert_eq!(
            t.workspace_target(|w| w.num == Some(3)),
            Some(WarpTarget { output: "HEADLESS-1", point: (960.0, 540.0) })
        );
        assert_eq!(t.workspace_target(|w| w.num == Some(9)), None);
    }

    #[test]
    fn floating_is_drawn_last() {
        let t = tree();
        let w = &ws(&t, "3").windows;
        assert_eq!(w.iter().map(|w| w.floating).collect::<Vec<_>>(), [false, true]);
    }

    #[test]
    fn focus_is_restored_when_missing() {
        let t = tree();
        let focus = t.focus().unwrap();
        assert_eq!(focus.workspace, "1");
        let mut unfocused = t.clone();
        for w in &mut unfocused.outputs[0].workspaces {
            w.focused = false;
            w.windows.iter_mut().for_each(|w| w.focused = false);
        }
        unfocused.restore_focus(&focus);
        assert_eq!(unfocused.focus(), Some(focus));
    }

    fn workspace_1(json: &[u8]) -> Vec<(String, Rect, bool)> {
        let t = Tree::from_json(json).unwrap();
        ws(&t, "1").windows.iter().map(|w| (w.title.clone(), w.rect, w.fullscreen.is_some())).collect()
    }

    #[test]
    fn fullscreen_window_keeps_its_tiled_place() {
        // htop is fullscreen inside the right-hand split; GitHub is below it.
        let w = workspace_1(include_bytes!("../tests/fixtures/sway-1.4-fullscreen-nested.json"));
        let rect = |title: &str| w.iter().find(|x| x.0.starts_with(title)).unwrap();
        assert_eq!(rect("htop").1, Rect::new(960.0, 0.0, 960.0, 540.0));
        assert!(rect("htop").2);
        assert_eq!(rect("GitHub").1, Rect::new(960.0, 540.0, 960.0, 540.0));
        assert_eq!(rect("~/src").1, Rect::new(0.0, 0.0, 960.0, 1080.0));

        // The left window is fullscreen directly on the workspace.
        let w = workspace_1(include_bytes!("../tests/fixtures/sway-1.4-fullscreen-top.json"));
        let rect = |title: &str| w.iter().find(|x| x.0.starts_with(title)).unwrap();
        assert_eq!(rect("~/src").1, Rect::new(0.0, 0.0, 960.0, 1080.0));
        assert!(rect("~/src").2);
        assert_eq!(rect("htop").1, Rect::new(960.0, 0.0, 960.0, 540.0));
        assert_eq!(rect("GitHub").1, Rect::new(960.0, 540.0, 960.0, 540.0));
    }

    #[test]
    fn fullscreen_blocks_focus_of_its_siblings() {
        let t = Tree::from_json(include_bytes!("../tests/fixtures/sway-1.4-fullscreen-nested.json")).unwrap();
        let id = |title: &str| {
            t.workspaces().flat_map(|w| &w.windows).find(|w| w.title.starts_with(title)).unwrap().id
        };
        assert_eq!(t.fullscreen_blocker(id("GitHub")), Some(id("htop")));
        assert_eq!(t.fullscreen_blocker(id("htop")), None);
        // Other workspaces are not affected.
        assert_eq!(t.fullscreen_blocker(id("Downloads")), None);
        assert_eq!(tree().fullscreen_blocker(id("GitHub")), None);
    }

    #[test]
    fn fullscreen_container_and_global_block_focus() {
        // Workspace 1: split 10 (A, B) and C; workspace 2: D.
        let json = |split_mode, d_mode| {
            format!(
                r#"{{"id":1,"type":"root","rect":{{"x":0,"y":0,"width":10,"height":10}},
                "nodes":[{{"id":2,"name":"X","type":"output","rect":{{"x":0,"y":0,"width":10,"height":10}},"nodes":[
                  {{"id":3,"name":"1","type":"workspace","rect":{{"x":0,"y":0,"width":10,"height":10}},"nodes":[
                    {{"id":10,"type":"con","fullscreen_mode":{split_mode},"rect":{{"x":0,"y":0,"width":5,"height":10}},"nodes":[
                      {{"id":11,"type":"con","rect":{{"x":0,"y":0,"width":5,"height":5}}}},
                      {{"id":12,"type":"con","rect":{{"x":0,"y":5,"width":5,"height":5}}}}]}},
                    {{"id":13,"type":"con","rect":{{"x":5,"y":0,"width":5,"height":10}}}}]}},
                  {{"id":4,"name":"2","type":"workspace","rect":{{"x":0,"y":0,"width":10,"height":10}},"nodes":[
                    {{"id":14,"type":"con","fullscreen_mode":{d_mode},"rect":{{"x":0,"y":0,"width":10,"height":10}}}}]}}]}}]}}"#
            )
        };
        let t = Tree::from_json(json(1, 0).as_bytes()).unwrap();
        // The container is disabled, not a window inside it.
        assert_eq!(t.fullscreen_blocker(ConId(13)), Some(ConId(10)));
        assert_eq!(t.fullscreen_blocker(ConId(12)), None);
        assert_eq!(t.fullscreen_blocker(ConId(14)), None);

        let t = Tree::from_json(json(0, 2).as_bytes()).unwrap();
        assert_eq!(t.fullscreen_blocker(ConId(11)), Some(ConId(14)));
        assert_eq!(t.fullscreen_blocker(ConId(14)), None);
    }

    #[test]
    fn shares_fill_the_gap_left_for_fullscreen() {
        let node = |fullscreen_mode, percent| Node {
            id: ConId(0),
            name: None,
            ty: NodeType::Con,
            rect: RawRect { x: 0, y: 0, width: 0, height: 0 },
            deco_rect: None,
            focused: false,
            urgent: false,
            sticky: false,
            nodes: Vec::new(),
            floating_nodes: Vec::new(),
            layout: Layout::Other,
            app_id: None,
            foreign_toplevel_identifier: None,
            window_properties: None,
            num: None,
            fullscreen_mode,
            percent,
        };
        assert_eq!(
            shares(&[node(0, Some(0.25)), node(1, Some(2.0)), node(0, Some(0.25))]),
            [0.25, 0.5, 0.25]
        );
        assert_eq!(shares(&[node(1, Some(1.0)), node(0, None)]), [0.5, 0.5]);
        // Siblings claiming everything: equal shares instead of a zero-width window.
        assert_eq!(shares(&[node(0, Some(0.5)), node(0, Some(0.5)), node(1, None)]), [1.0 / 3.0; 3]);
    }

    #[test]
    fn unknown_node_types_and_layouts_parse() {
        let json = br#"{"id":1,"name":"root","type":"root","rect":{"x":0,"y":0,"width":10,"height":10},
            "nodes":[{"id":2,"name":"X","type":"output","layout":"brand-new","current_workspace":"1",
              "rect":{"x":0,"y":0,"width":10,"height":10},
              "nodes":[{"id":3,"name":"1","num":1,"type":"workspace","rect":{"x":0,"y":0,"width":10,"height":10},
                "nodes":[{"id":4,"name":"t","type":"future_type","app_id":"a","fullscreen_mode":2,
                  "urgent":true,"sticky":true,"foreign_toplevel_identifier":"4756f54d",
                  "rect":{"x":0,"y":0,"width":5,"height":5}}]}]}]}"#;
        let t = Tree::from_json(json).unwrap();
        let w = &t.outputs[0].workspaces[0].windows[0];
        assert!(w.fullscreen.is_some_and(|f| f.global) && w.urgent && w.sticky);
        assert_eq!(w.rect, Rect::new(0.0, 0.0, 10.0, 10.0));
        assert_eq!(w.toplevel.as_deref(), Some("4756f54d"));
    }

    #[test]
    fn no_toplevel_identifier_before_sway_1_11() {
        assert!(tree().workspaces().flat_map(|w| &w.windows).all(|w| w.toplevel.is_none()));
    }
}
