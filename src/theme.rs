//! Every color swayview draws with.
//!
//! Window colors default to sway's `client.*` colors, the rest to built-in
//! values. Anything set in `$XDG_CONFIG_HOME/swayview/theme.yaml` (or
//! `~/.config/swayview/theme.yaml`) replaces the default. A file that cannot
//! be read or parsed is reported and ignored.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::color::Rgba;
use crate::sway_config;
use crate::warn;

/// Colors of one kind of window box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Class {
    pub border: Rgba,
    pub background: Rgba,
    pub text: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceColors {
    pub fill: Rgba,
    pub border: Rgba,
    /// Border of a workspace currently shown on its output.
    pub visible: Rgba,
    /// The workspace number.
    pub label: Rgba,
    /// Number and border of the workspace holding the selection.
    pub selected: Rgba,
    /// Number and border of a workspace with an urgent window.
    pub urgent: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowColors {
    pub normal: Class,
    pub selected: Class,
    pub urgent: Class,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub backdrop: Rgba,
    pub output_name: Rgba,
    pub workspace: WorkspaceColors,
    pub window: WindowColors,
    /// Stripe colors apps are hashed into.
    pub app_colors: Vec<Rgba>,
}

impl Theme {
    /// Built-in colors, with window colors from sway's `client.*` colors.
    fn defaults(clients: sway_config::Clients) -> Theme {
        Theme {
            backdrop: Rgba(0x101216e0),
            output_name: Rgba(0x8a93a5ff),
            workspace: WorkspaceColors {
                fill: Rgba(0x16181dff),
                border: Rgba(0x3a3f4bff),
                visible: Rgba(0x6b7385ff),
                label: Rgba(0xdde1e8ff),
                selected: clients.focused.background,
                urgent: clients.urgent.background,
            },
            window: WindowColors {
                normal: clients.unfocused,
                selected: clients.focused,
                urgent: clients.urgent,
            },
            // About 30° apart in hue: red, orange, yellow, lime, green, teal,
            // cyan, blue, indigo, purple, magenta, pink.
            app_colors: [
                0xe06c75ff, 0xe8915aff, 0xe5c07bff, 0xb5d468ff, 0x98c379ff, 0x5fc9a4ff, 0x56b6c2ff,
                0x61afefff, 0x8a8cf0ff, 0xc678ddff, 0xe87fd0ff, 0xf78fb3ff,
            ]
            .map(Rgba)
            .to_vec(),
        }
    }

    /// The theme: defaults from the sway config at `sway_config`, overridden by
    /// the YAML file at `file` if it exists.
    pub fn load(sway_config: Option<&Path>, file: Option<&Path>) -> Theme {
        let mut theme = Theme::defaults(sway_config::load(sway_config));
        if let Some(path) = file {
            match std::fs::read_to_string(path) {
                Ok(text) => match serde_saphyr::from_str::<ThemeFile>(&text) {
                    Ok(f) => theme.apply(f),
                    Err(e) => warn(format_args!("{}: {e}", path.display())),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn(format_args!("{}: {e}", path.display())),
            }
        }
        theme
    }

    /// `$XDG_CONFIG_HOME/swayview/theme.yaml`, or `~/.config/swayview/theme.yaml`.
    pub fn default_path() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(config.join("swayview/theme.yaml"))
    }

    /// The stripe color of `app`, the same on every run; ignores case.
    pub fn app_color(&self, app: &str) -> Rgba {
        // FNV-1a: stable across runs and builds, unlike `DefaultHasher`.
        let hash = app
            .bytes()
            .fold(0x811c_9dc5_u32, |h, b| (h ^ u32::from(b.to_ascii_lowercase())).wrapping_mul(0x0100_0193));
        self.app_colors[hash as usize % self.app_colors.len()]
    }

    fn apply(&mut self, f: ThemeFile) {
        set(&mut self.backdrop, f.backdrop);
        set(&mut self.output_name, f.output_name);
        let (w, fw) = (&mut self.workspace, f.workspace);
        set(&mut w.fill, fw.fill);
        set(&mut w.border, fw.border);
        set(&mut w.visible, fw.visible);
        set(&mut w.label, fw.label);
        set(&mut w.selected, fw.selected);
        set(&mut w.urgent, fw.urgent);
        let (w, fw) = (&mut self.window, f.window);
        fw.normal.apply(&mut w.normal);
        fw.selected.apply(&mut w.selected);
        fw.urgent.apply(&mut w.urgent);
        match f.app_colors {
            Some(colors) if colors.is_empty() => warn("theme: app_colors is empty, keeping the defaults"),
            Some(colors) => self.app_colors = colors,
            None => {}
        }
    }
}

fn set(dst: &mut Rgba, value: Option<Rgba>) {
    if let Some(v) = value {
        *dst = v;
    }
}

/// `theme.yaml`: every key is optional.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    #[serde(default, deserialize_with = "color")]
    backdrop: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    output_name: Option<Rgba>,
    #[serde(default)]
    workspace: WorkspaceFile,
    #[serde(default)]
    window: WindowFile,
    app_colors: Option<Vec<Rgba>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct WorkspaceFile {
    #[serde(default, deserialize_with = "color")]
    fill: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    border: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    visible: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    label: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    selected: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    urgent: Option<Rgba>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct WindowFile {
    #[serde(default)]
    normal: ClassFile,
    #[serde(default)]
    selected: ClassFile,
    #[serde(default)]
    urgent: ClassFile,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ClassFile {
    #[serde(default, deserialize_with = "color")]
    border: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    background: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    text: Option<Rgba>,
}

/// A color that, if the key is present, must be set; see `Rgba`'s `Deserialize`.
fn color<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Rgba>, D::Error> {
    Rgba::deserialize(d).map(Some)
}

impl ClassFile {
    fn apply(self, class: &mut Class) {
        set(&mut class.border, self.border);
        set(&mut class.background, self.background);
        set(&mut class.text, self.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_file(name: &str, yaml: &str) -> Theme {
        let path = std::env::temp_dir().join(format!("swayview-theme-{name}-{}.yaml", std::process::id()));
        std::fs::write(&path, yaml).unwrap();
        let theme = Theme::load(None, Some(&path));
        let _ = std::fs::remove_file(&path);
        theme
    }

    fn defaults() -> Theme {
        Theme::defaults(sway_config::Clients::default())
    }

    #[test]
    fn no_file_gives_defaults() {
        assert_eq!(Theme::load(None, None), defaults());
        assert_eq!(Theme::load(None, Some(Path::new("/nonexistent/theme.yaml"))), defaults());
    }

    #[test]
    fn selected_workspace_defaults_to_the_active_color() {
        let t = defaults();
        assert_eq!(t.workspace.selected, t.window.selected.background);
    }

    #[test]
    fn file_overrides_only_what_it_sets() {
        let t = with_file(
            "partial",
            r##"
backdrop: "#000000cc"
workspace:
  selected: "#00ffff"
window:
  selected:
    background: "#123456"
app_colors: ["#ff0000", "#00ff00"]
"##,
        );
        let d = defaults();
        assert_eq!(t.backdrop, Rgba(0x000000cc));
        assert_eq!(t.workspace.selected, Rgba(0x00ffffff));
        assert_eq!(t.workspace.label, d.workspace.label);
        assert_eq!(t.window.selected.background, Rgba(0x123456ff));
        assert_eq!(t.window.selected.text, d.window.selected.text);
        assert_eq!(t.window.normal, d.window.normal);
        assert_eq!(t.app_colors, [Rgba(0xff0000ff), Rgba(0x00ff00ff)]);
    }

    #[test]
    fn bad_files_are_ignored() {
        assert_eq!(with_file("color", "backdrop: \"#12\"\n"), defaults());
        assert_eq!(with_file("unknown", "backdrop: \"#123456\"\nbackground: \"#000000\"\n"), defaults());
        assert_eq!(with_file("syntax", "workspace: [unclosed\n"), defaults());
        assert_eq!(with_file("empty-colors", "app_colors: []\n").app_colors, defaults().app_colors);
        assert_eq!(with_file("empty", ""), defaults());
        // Unquoted, `#` starts a comment and the value is empty.
        assert_eq!(with_file("unquoted", "backdrop: #000000\n"), defaults());
        assert_eq!(with_file("unquoted-list", "app_colors:\n  - #ff0000\n"), defaults());
    }

    #[test]
    fn app_colors_are_stable_and_ignore_case() {
        let t = defaults();
        assert_eq!(t.app_color("Alacritty"), t.app_color("alacritty"));
        // Apps that shared a color with a smaller palette.
        assert_ne!(t.app_color("Alacritty"), t.app_color("brave-browser"));
        let apps = ["foot", "firefox", "code", "Slack", "discord", "Alacritty", "pavucontrol"];
        let distinct: std::collections::HashSet<_> = apps.iter().map(|a| t.app_color(a).0).collect();
        assert!(distinct.len() > 2);
    }
}
