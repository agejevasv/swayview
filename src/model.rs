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

    /// Splits into `n` equal slices along `axis`.
    pub fn slices(&self, n: usize, axis: Axis) -> Vec<Rect> {
        let n = n.max(1) as f32;
        (0..n as usize)
            .map(|i| {
                let i = i as f32;
                match axis {
                    Axis::Horizontal => Rect::new(self.x + self.w * i / n, self.y, self.w / n, self.h),
                    Axis::Vertical => Rect::new(self.x, self.y + self.h * i / n, self.w, self.h / n),
                }
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
    /// Currently shown on its output.
    pub visible: bool,
    /// In drawing order: tiled, then floating, then fullscreen.
    pub windows: Vec<Window>,
}

#[derive(Debug, Clone)]
pub struct Window {
    pub id: ConId,
    pub app: String,
    pub title: String,
    pub rect: Rect,
    pub floating: bool,
    pub fullscreen: bool,
    pub focused: bool,
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
                let current = o.current_workspace.as_deref();
                let workspaces = o
                    .nodes
                    .iter()
                    .filter(|w| w.ty == NodeType::Workspace)
                    .map(|w| workspace(w, o.rect.into(), current))
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

    /// The output holding the focused workspace.
    pub fn focused_output(&self) -> Option<&Output> {
        self.outputs.iter().find(|o| o.workspaces.iter().any(|w| w.focused))
    }
}

fn workspace(node: &Node, output_rect: Rect, current: Option<&str>) -> Workspace {
    let mut c = Collector { output_rect, floating: false, windows: Vec::new() };
    for child in &node.nodes {
        c.node(child, child.outer_rect());
    }
    c.floating = true;
    for child in &node.floating_nodes {
        c.node(child, child.outer_rect());
    }
    let mut windows = c.windows;
    windows.sort_by_key(|w| (w.fullscreen, w.floating));
    Workspace {
        name: node.name_str().to_string(),
        num: node.num.filter(|n| *n >= 0),
        focused: node.focused || node.any_focused(),
        visible: current == Some(node.name_str()),
        windows,
    }
}

/// How a container's children are placed.
enum Arrange {
    /// At their own rects, scaled into the container's target.
    Mapped,
    /// As equal slices: tabbed and stacked children all share one rect in
    /// sway, and missing rects fall back to this too.
    Slices(Axis),
}

/// Collects the leaf windows of one workspace.
struct Collector {
    output_rect: Rect,
    floating: bool,
    windows: Vec<Window>,
}

impl Collector {
    /// Collects `node`'s windows, drawing it into `target`.
    fn node(&mut self, node: &Node, target: Rect) {
        // Global fullscreen (mode 2) spans all outputs; it is drawn like
        // workspace fullscreen, filling its own output.
        let fullscreen = node.fullscreen_mode != 0;
        let target = if fullscreen { self.output_rect } else { target };

        if node.nodes.is_empty() {
            self.windows.push(Window {
                id: node.id,
                app: node.app_name(),
                title: node.name_str().to_string(),
                rect: target,
                floating: self.floating,
                fullscreen,
                focused: node.focused,
            });
            return;
        }

        let own: Rect = node.rect.into();
        let missing = own.is_empty() || node.nodes.iter().any(|c| c.outer_rect().is_empty());
        let arrange = match node.layout {
            Layout::Tabbed => Arrange::Slices(Axis::Horizontal),
            Layout::Stacked => Arrange::Slices(Axis::Vertical),
            Layout::Splitv if missing => Arrange::Slices(Axis::Vertical),
            _ if missing => Arrange::Slices(Axis::Horizontal),
            _ => Arrange::Mapped,
        };
        match arrange {
            Arrange::Slices(axis) => {
                for (child, slice) in node.nodes.iter().zip(target.slices(node.nodes.len(), axis)) {
                    self.node(child, slice);
                }
            }
            Arrange::Mapped => {
                for child in &node.nodes {
                    self.node(child, own.map_into(child.outer_rect(), target));
                }
            }
        }
    }
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
    nodes: Vec<Node>,
    #[serde(default)]
    floating_nodes: Vec<Node>,
    #[serde(default)]
    layout: Layout,
    app_id: Option<String>,
    window_properties: Option<WindowProperties>,
    num: Option<i32>,
    /// 0 none, 1 workspace, 2 global.
    #[serde(default)]
    fullscreen_mode: u8,
    current_workspace: Option<String>,
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
    fn tabbed_children_become_slices() {
        let t = tree();
        let rects: Vec<_> = ws(&t, "2").windows.iter().map(|w| w.rect).collect();
        assert_eq!(
            rects,
            [
                Rect::new(0.0, 0.0, 640.0, 1080.0),
                Rect::new(640.0, 0.0, 640.0, 1080.0),
                Rect::new(1280.0, 0.0, 640.0, 1080.0),
            ]
        );
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

    #[test]
    fn unknown_node_types_and_layouts_parse() {
        let json = br#"{"id":1,"name":"root","type":"root","rect":{"x":0,"y":0,"width":10,"height":10},
            "nodes":[{"id":2,"name":"X","type":"output","layout":"brand-new","current_workspace":"1",
              "rect":{"x":0,"y":0,"width":10,"height":10},
              "nodes":[{"id":3,"name":"1","num":1,"type":"workspace","rect":{"x":0,"y":0,"width":10,"height":10},
                "nodes":[{"id":4,"name":"t","type":"future_type","app_id":"a","fullscreen_mode":2,
                  "rect":{"x":0,"y":0,"width":5,"height":5}}]}]}]}"#;
        let t = Tree::from_json(json).unwrap();
        let w = &t.outputs[0].workspaces[0].windows[0];
        assert!(w.fullscreen);
        assert_eq!(w.rect, Rect::new(0.0, 0.0, 10.0, 10.0));
    }
}
