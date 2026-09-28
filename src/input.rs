//! Keyboard and pointer input turned into actions, and actions into sway
//! commands. Pure, no Wayland.

use crate::layout::{Dir, Scene};
use crate::model::{ConId, Tree};

/// A window on one of the overview's surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sel {
    pub surface: usize,
    pub window: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key<'a> {
    Escape,
    Enter,
    Tab {
        back: bool,
    },
    Arrow(Dir),
    /// Text the key produced, if any.
    Text(&'a str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Select(Sel),
    Close,
    Focus(ConId),
    WorkspaceNumber(i32),
}

/// `scenes` is indexed by surface; `sel` is the current selection.
pub fn key(scenes: &[&Scene], sel: Option<Sel>, key: Key<'_>) -> Action {
    let select = |s: Option<Sel>| s.map_or(Action::Nothing, Action::Select);
    match key {
        Key::Escape => Action::Close,
        Key::Enter => sel.map_or(Action::Nothing, |s| Action::Focus(scenes[s.surface].windows[s.window].id)),
        Key::Tab { back } => select(cycle(scenes, sel, back)),
        Key::Arrow(dir) => select(match sel {
            Some(s) => step(scenes, s, dir),
            None => cycle(scenes, None, false),
        }),
        Key::Text(t) => match t.as_bytes() {
            &[d @ b'0'..=b'9'] => Action::WorkspaceNumber(if d == b'0' { 10 } else { i32::from(d - b'0') }),
            _ => Action::Nothing,
        },
    }
}

/// A left click at (`x`, `y`) on `scene`: focuses the window there, or closes.
pub fn click(scene: &Scene, x: f32, y: f32) -> Action {
    scene.hit(x, y).map_or(Action::Close, |w| Action::Focus(scene.windows[w].id))
}

/// Moving the pointer over a window selects it.
pub fn motion(scene: &Scene, surface: usize, x: f32, y: f32) -> Action {
    scene.hit(x, y).map_or(Action::Nothing, |window| Action::Select(Sel { surface, window }))
}

/// The selection sway's focus corresponds to, if it is shown.
pub fn focused(scenes: &[&Scene]) -> Option<Sel> {
    scenes
        .iter()
        .enumerate()
        .find_map(|(surface, s)| s.focused_window().map(|window| Sel { surface, window }))
}

/// Where window `id` is shown, if anywhere.
pub fn find(scenes: &[&Scene], id: ConId) -> Option<Sel> {
    scenes
        .iter()
        .enumerate()
        .find_map(|(surface, s)| s.window_by_id(id).map(|window| Sel { surface, window }))
}

/// Next (or previous) window, going through outputs left to right.
fn cycle(scenes: &[&Scene], sel: Option<Sel>, back: bool) -> Option<Sel> {
    let mut surfaces: Vec<usize> = (0..scenes.len()).collect();
    surfaces.sort_by(|&a, &b| {
        let (a, b) = (scenes[a].output, scenes[b].output);
        (a.x, a.y).partial_cmp(&(b.x, b.y)).unwrap_or(std::cmp::Ordering::Equal)
    });
    let order: Vec<Sel> = surfaces
        .into_iter()
        .flat_map(|surface| (0..scenes[surface].windows.len()).map(move |window| Sel { surface, window }))
        .collect();
    let n = order.len();
    if n == 0 {
        return None;
    }
    let i = sel.and_then(|s| order.iter().position(|o| *o == s));
    Some(
        order[match (i, back) {
            (None, false) => 0,
            (None, true) => n - 1,
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
        }],
    )
}

/// Moves in `dir` within the surface, or on to the next output in that direction.
fn step(scenes: &[&Scene], sel: Sel, dir: Dir) -> Option<Sel> {
    let scene = scenes[sel.surface];
    if let Some(window) = scene.neighbor(sel.window, dir) {
        return Some(Sel { surface: sel.surface, window });
    }
    let origin = scene.output.center();
    let surface = scenes
        .iter()
        .enumerate()
        .filter(|(i, s)| *i != sel.surface && !s.windows.is_empty())
        .filter_map(|(i, s)| {
            let (along, across) = dir.project(origin, s.output.center());
            (along > 0.0).then_some((i, along + 2.0 * across.abs()))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))?
        .0;

    // Enter at the near edge, keeping the relative position across it.
    let from = scene.relative_center(sel.window);
    let target = scenes[surface];
    let score = |w: usize| {
        let (u, v) = target.relative_center(w);
        let (edge, across) = match dir {
            Dir::Right => (u, v - from.1),
            Dir::Left => (1.0 - u, v - from.1),
            Dir::Down => (v, u - from.0),
            Dir::Up => (1.0 - v, u - from.0),
        };
        edge + 2.0 * across.abs()
    };
    let window = (0..target.windows.len()).min_by(|&a, &b| score(a).total_cmp(&score(b)))?;
    Some(Sel { surface, window })
}

/// The sway command for `action`, or `None` if it needs none.
///
/// Sway warps the cursor on focus changes made by key bindings but not by
/// IPC commands, so when the target is on another output than the pointer
/// (`pointer_output`), the command moves the cursor there as well.
pub fn command(action: &Action, tree: &Tree, pointer_output: Option<&str>) -> Option<String> {
    let (cmd, target) = match action {
        Action::Focus(id) => {
            // Sway will not focus a window behind a fullscreen one; end that first.
            let unblock =
                tree.fullscreen_blocker(*id).map(|fs| format!("[con_id={fs}] fullscreen disable; "));
            (format!("{}[con_id={id}] focus", unblock.unwrap_or_default()), tree.window_target(*id))
        }
        Action::WorkspaceNumber(n) => {
            (format!("workspace number {n}"), tree.workspace_target(|w| w.num == Some(*n)))
        }
        Action::Nothing | Action::Select(_) | Action::Close => return None,
    };
    Some(match target {
        Some(t) if pointer_output != Some(t.output) => {
            let (x, y) = (t.point.0 - tree.root.x, t.point.1 - tree.root.y);
            format!("{cmd}; seat - cursor set {} {}", x as i32, y as i32)
        }
        _ => cmd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::build;
    use crate::model::{Tree, tests::tree};

    /// The fixture's output, and a copy of it placed to its right.
    fn two_scenes() -> (Scene, Scene) {
        let t = tree();
        let left = t.outputs[0].clone();
        let mut right = left.clone();
        right.rect.x += left.rect.w;
        for w in right.workspaces.iter_mut().flat_map(|ws| &mut ws.windows) {
            w.rect.x += left.rect.w;
        }
        (build(&left, 1920.0, 1080.0), build(&right, 1920.0, 1080.0))
    }

    fn index(s: &Scene, title: &str) -> usize {
        s.windows.iter().position(|w| w.title == title).unwrap()
    }

    #[test]
    fn keys() {
        let (a, _) = two_scenes();
        let scenes = [&a];
        let htop = Sel { surface: 0, window: index(&a, "htop") };
        assert_eq!(key(&scenes, Some(htop), Key::Escape), Action::Close);
        assert_eq!(key(&scenes, Some(htop), Key::Enter), Action::Focus(a.windows[htop.window].id));
        assert_eq!(key(&scenes, None, Key::Enter), Action::Nothing);
        assert_eq!(key(&scenes, None, Key::Text("3")), Action::WorkspaceNumber(3));
        assert_eq!(key(&scenes, None, Key::Text("0")), Action::WorkspaceNumber(10));
        assert_eq!(key(&scenes, None, Key::Text("x")), Action::Nothing);
        assert_eq!(key(&scenes, None, Key::Text("12")), Action::Nothing);
    }

    #[test]
    fn tab_cycles_across_outputs_left_to_right() {
        let (a, b) = two_scenes();
        // Surface 0 is the right-hand output, to check ordering by position.
        let scenes = [&b, &a];
        let last_left = Sel { surface: 1, window: a.windows.len() - 1 };
        assert_eq!(
            key(&scenes, None, Key::Tab { back: false }),
            Action::Select(Sel { surface: 1, window: 0 })
        );
        assert_eq!(
            key(&scenes, Some(last_left), Key::Tab { back: false }),
            Action::Select(Sel { surface: 0, window: 0 })
        );
        assert_eq!(
            key(&scenes, Some(Sel { surface: 1, window: 0 }), Key::Tab { back: true }),
            Action::Select(Sel { surface: 0, window: b.windows.len() - 1 })
        );
    }

    #[test]
    fn arrows_continue_on_the_next_output() {
        let (a, b) = two_scenes();
        let scenes = [&a, &b];
        // Rightmost window on the left output: nothing further right there.
        let discord = Sel { surface: 0, window: index(&a, "Discord | #rust") };
        let Action::Select(next) = key(&scenes, Some(discord), Key::Arrow(Dir::Right)) else {
            panic!("expected a selection");
        };
        assert_eq!(next.surface, 1);
        // Enters on the left edge of the other output.
        assert_eq!(b.windows[next.window].app, "foot");
        // Nothing to the left of the left output.
        let foot = Sel { surface: 0, window: 0 };
        assert_eq!(key(&scenes, Some(foot), Key::Arrow(Dir::Left)), Action::Nothing);
    }

    #[test]
    fn pointer() {
        let (a, _) = two_scenes();
        let w = index(&a, "htop");
        let (x, y) = a.windows[w].rect.center();
        assert_eq!(click(&a, x, y), Action::Focus(a.windows[w].id));
        assert_eq!(motion(&a, 0, x, y), Action::Select(Sel { surface: 0, window: w }));
        let header = a.workspaces[1].header;
        assert_eq!(click(&a, header.x + 1.0, header.y + 1.0), Action::Close);
        assert_eq!(click(&a, 1.0, 1.0), Action::Close);
        assert_eq!(motion(&a, 0, 1.0, 1.0), Action::Nothing);
    }

    #[test]
    fn commands_warp_only_across_outputs() {
        let t = tree();
        let htop = t.workspaces().flat_map(|w| &w.windows).find(|w| w.title == "htop").unwrap();
        let (cx, cy) = htop.rect.center();
        let focus = Action::Focus(htop.id);
        assert_eq!(command(&focus, &t, Some("HEADLESS-1")), Some(format!("[con_id={}] focus", htop.id)));
        assert_eq!(
            command(&focus, &t, None),
            Some(format!("[con_id={}] focus; seat - cursor set {} {}", htop.id, cx as i32, cy as i32))
        );
        assert_eq!(
            command(&Action::WorkspaceNumber(3), &t, Some("OTHER")),
            Some("workspace number 3; seat - cursor set 960 540".into())
        );
        // A workspace that does not exist yet is created where sway decides.
        assert_eq!(command(&Action::WorkspaceNumber(7), &t, None), Some("workspace number 7".into()));
        assert_eq!(command(&Action::Close, &t, None), None);
    }

    #[test]
    fn focus_behind_fullscreen_ends_it_first() {
        let t = Tree::from_json(include_bytes!("../tests/fixtures/sway-1.4-fullscreen-nested.json")).unwrap();
        let id = |title: &str| {
            t.workspaces().flat_map(|w| &w.windows).find(|w| w.title.starts_with(title)).unwrap().id
        };
        let (htop, github) = (id("htop"), id("GitHub"));
        assert_eq!(
            command(&Action::Focus(github), &t, Some("HEADLESS-1")),
            Some(format!("[con_id={htop}] fullscreen disable; [con_id={github}] focus"))
        );
        assert_eq!(
            command(&Action::Focus(htop), &t, Some("HEADLESS-1")),
            Some(format!("[con_id={htop}] focus"))
        );
    }

    #[test]
    fn focus_and_find() {
        let (a, b) = two_scenes();
        let scenes = [&a, &b];
        let f = focused(&scenes).unwrap();
        assert_eq!(f.surface, 0);
        assert_eq!(a.windows[f.window].title, "htop");
        assert_eq!(find(&scenes, a.windows[3].id), Some(Sel { surface: 0, window: 3 }));
    }
}
