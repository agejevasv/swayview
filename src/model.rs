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
    pub fn split(&self, fractions: &[f32], axis: Axis) -> Vec<Rect> {
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
    pub fullscreen: bool,
    pub sticky: bool,
    /// Asks for attention.
    pub urgent: bool,
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

    /// The fullscreen window that stops sway from focusing window `id`, if any.
    /// Sway refuses to focus windows hidden behind a fullscreen one.
    pub fn fullscreen_blocker(&self, id: ConId) -> Option<ConId> {
        let ws = self.workspaces().find(|ws| ws.windows.iter().any(|w| w.id == id))?;
        let target = ws.windows.iter().find(|w| w.id == id)?;
        if target.fullscreen {
            return None;
        }
        ws.windows.iter().find(|w| w.fullscreen).map(|w| w.id)
    }

    /// The output holding the focused workspace.
    pub fn focused_output(&self) -> Option<&Output> {
        self.outputs.iter().find(|o| o.workspaces.iter().any(|w| w.focused))
    }
}

fn workspace(node: &Node, output_rect: Rect) -> Workspace {
    let mut c = Collector { floating: false, in_fullscreen: false, windows: Vec::new() };
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
    /// As equal slices: tabbed and stacked children all share one rect in
    /// sway, and missing rects fall back to this too.
    Slices(Axis),
    /// By their share of the container. A fullscreen child reports the whole
    /// output as its rect; this puts it back in its place in the layout.
    Shares(Axis),
}

/// Collects the leaf windows of one workspace.
struct Collector {
    floating: bool,
    /// Inside a fullscreen container.
    in_fullscreen: bool,
    windows: Vec<Window>,
}

impl Collector {
    /// Collects `node`'s windows, drawing it into `target`.
    fn node(&mut self, node: &Node, target: Rect) {
        // Workspace (1) or global (2), of the window or a container holding it;
        // either way drawn in its tiled place.
        let fullscreen = self.in_fullscreen || node.fullscreen_mode != 0;
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
            });
            return;
        }
        let outer = std::mem::replace(&mut self.in_fullscreen, fullscreen);
        self.children(node, target);
        self.in_fullscreen = outer;
    }

    /// Collects the windows of `node`'s tiled children, drawing it into `target`.
    fn children(&mut self, node: &Node, target: Rect) {
        let own: Rect = node.rect.into();
        let axis = if node.layout == Layout::Splitv { Axis::Vertical } else { Axis::Horizontal };
        let missing = own.is_empty() || node.nodes.iter().any(|c| c.outer_rect().is_empty());
        let arrange = match node.layout {
            Layout::Tabbed => Arrange::Slices(Axis::Horizontal),
            Layout::Stacked => Arrange::Slices(Axis::Vertical),
            _ if node.nodes.iter().any(|c| c.fullscreen_mode != 0) => Arrange::Shares(axis),
            _ if missing => Arrange::Slices(axis),
            _ => Arrange::Mapped,
        };
        let n = node.nodes.len();
        let slots = match arrange {
            Arrange::Slices(axis) => target.split(&vec![1.0 / n.max(1) as f32; n], axis),
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
    let equal = vec![1.0 / children.len().max(1) as f32; children.len()];
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

    fn workspace_1(json: &[u8]) -> Vec<(String, Rect, bool)> {
        let t = Tree::from_json(json).unwrap();
        ws(&t, "1").windows.iter().map(|w| (w.title.clone(), w.rect, w.fullscreen)).collect()
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
                  "urgent":true,"sticky":true,
                  "rect":{"x":0,"y":0,"width":5,"height":5}}]}]}]}"#;
        let t = Tree::from_json(json).unwrap();
        let w = &t.outputs[0].workspaces[0].windows[0];
        assert!(w.fullscreen && w.urgent && w.sticky);
        assert_eq!(w.rect, Rect::new(0.0, 0.0, 10.0, 10.0));
    }
}
