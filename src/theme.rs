//! Every color swayview draws with.
//!
//! Window colors default to sway's `client.*` colors, the rest to built-in
//! values. Anything set in `$XDG_CONFIG_HOME/swayview/theme.yaml` (or
//! `~/.config/swayview/theme.yaml`) replaces the default. Unknown keys are
//! reported and skipped; a file that cannot be read or parsed is reported and
//! ignored.

// Unknown keys are collected as a map to `IgnoredAny`, serde's way to skip values.
#![allow(clippy::zero_sized_map_values)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::IgnoredAny;

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
    /// The workspace number.
    pub label: Rgba,
    /// Number of the workspace holding the selection.
    pub selected: Rgba,
    /// Number of a workspace with an urgent window.
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
}

impl Theme {
    /// Built-in colors, with window colors from sway's `client.*` colors.
    fn defaults(clients: sway_config::Clients) -> Theme {
        Theme {
            backdrop: Rgba(0x101216e0),
            output_name: Rgba(0x8a93a5ff),
            workspace: WorkspaceColors {
                fill: Rgba(0x16181dff),
                label: Rgba(0xdde1e8ff),
                selected: clients.focused.background,
                urgent: clients.urgent.background,
            },
            window: WindowColors {
                normal: clients.unfocused,
                selected: clients.focused,
                urgent: clients.urgent,
            },
        }
    }

    /// The theme: defaults from the sway config at `sway_config`, overridden by
    /// the YAML file at `file` if it exists.
    pub fn load(sway_config: Option<&Path>, file: Option<&Path>) -> Theme {
        let mut theme = Theme::defaults(sway_config::load(sway_config));
        if let Some(path) = file {
            match std::fs::read_to_string(path) {
                Ok(text) => match serde_saphyr::from_str::<ThemeFile>(&text) {
                    Ok(f) => {
                        for key in f.unknown_keys() {
                            warn(format_args!("{}: unknown key `{key}` ignored", path.display()));
                        }
                        theme.apply(f);
                    }
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

    fn apply(&mut self, f: ThemeFile) {
        set(&mut self.backdrop, f.backdrop);
        set(&mut self.output_name, f.output_name);
        let (w, fw) = (&mut self.workspace, f.workspace);
        set(&mut w.fill, fw.fill);
        set(&mut w.label, fw.label);
        set(&mut w.selected, fw.selected);
        set(&mut w.urgent, fw.urgent);
        let (w, fw) = (&mut self.window, f.window);
        fw.normal.apply(&mut w.normal);
        fw.selected.apply(&mut w.selected);
        fw.urgent.apply(&mut w.urgent);
    }
}

fn set(dst: &mut Rgba, value: Option<Rgba>) {
    if let Some(v) = value {
        *dst = v;
    }
}

/// Keys a `theme.yaml` section does not know. They are reported and ignored,
/// so a leftover or misspelled key does not discard the rest of the file.
type Unknown = BTreeMap<String, IgnoredAny>;

/// `theme.yaml`: every key is optional.
#[derive(Deserialize, Default)]
struct ThemeFile {
    #[serde(default, deserialize_with = "color")]
    backdrop: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    output_name: Option<Rgba>,
    #[serde(default)]
    workspace: WorkspaceFile,
    #[serde(default)]
    window: WindowFile,
    #[serde(flatten)]
    unknown: Unknown,
}

impl ThemeFile {
    /// Unknown keys, as dotted paths like `workspace.border`.
    fn unknown_keys(&self) -> Vec<String> {
        let w = &self.window;
        let sections: [(&str, &Unknown); 6] = [
            ("", &self.unknown),
            ("workspace.", &self.workspace.unknown),
            ("window.", &w.unknown),
            ("window.normal.", &w.normal.unknown),
            ("window.selected.", &w.selected.unknown),
            ("window.urgent.", &w.urgent.unknown),
        ];
        sections.iter().flat_map(|(prefix, keys)| keys.keys().map(move |k| format!("{prefix}{k}"))).collect()
    }
}

#[derive(Deserialize, Default)]
struct WorkspaceFile {
    #[serde(default, deserialize_with = "color")]
    fill: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    label: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    selected: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    urgent: Option<Rgba>,
    #[serde(flatten)]
    unknown: Unknown,
}

#[derive(Deserialize, Default)]
struct WindowFile {
    #[serde(default)]
    normal: ClassFile,
    #[serde(default)]
    selected: ClassFile,
    #[serde(default)]
    urgent: ClassFile,
    #[serde(flatten)]
    unknown: Unknown,
}

#[derive(Deserialize, Default)]
struct ClassFile {
    #[serde(default, deserialize_with = "color")]
    border: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    background: Option<Rgba>,
    #[serde(default, deserialize_with = "color")]
    text: Option<Rgba>,
    #[serde(flatten)]
    unknown: Unknown,
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
"##,
        );
        let d = defaults();
        assert_eq!(t.backdrop, Rgba(0x000000cc));
        assert_eq!(t.workspace.selected, Rgba(0x00ffffff));
        assert_eq!(t.workspace.label, d.workspace.label);
        assert_eq!(t.window.selected.background, Rgba(0x123456ff));
        assert_eq!(t.window.selected.text, d.window.selected.text);
        assert_eq!(t.window.normal, d.window.normal);
    }

    #[test]
    fn bad_files_are_ignored() {
        assert_eq!(with_file("color", "backdrop: \"#12\"\n"), defaults());
        assert_eq!(with_file("syntax", "workspace: [unclosed\n"), defaults());
        assert_eq!(with_file("empty", ""), defaults());
        // Unquoted, `#` starts a comment and the value is empty.
        assert_eq!(with_file("unquoted", "backdrop: #000000\n"), defaults());
        assert_eq!(with_file("unquoted-nested", "window:\n  normal:\n    text: #ffffff\n"), defaults());
    }

    #[test]
    fn unknown_keys_are_skipped_not_fatal() {
        // `workspace.border` was removed; a file that still has it keeps working.
        let yaml = "workspace:\n  border: \"#3a3f4b\"\n  selected: \"#00d7d7\"\nbackground: \"#000000\"\n";
        let t = with_file("unknown", yaml);
        assert_eq!(t.workspace.selected, Rgba(0x00d7d7ff));
        let f: ThemeFile = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(f.unknown_keys(), ["background", "workspace.border"]);
        let nested: ThemeFile =
            serde_saphyr::from_str("window:\n  normal:\n    outline: \"#000000\"\n").unwrap();
        assert_eq!(nested.unknown_keys(), ["window.normal.outline"]);
    }
}
