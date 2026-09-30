//! The fonts, colors and thumbnail setting swayview draws with.
//!
//! Window colors default to sway's `client.*` colors, everything else to
//! built-in values. Anything set in `$XDG_CONFIG_HOME/swayview/config.yaml`
//! (or `~/.config/swayview/config.yaml`) replaces the default. Unknown keys are
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

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub fonts: Fonts,
    pub colors: Colors,
    /// Show windows' contents where sway can capture them.
    pub thumbnails: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fonts {
    /// The app name line; also, in bold, the workspace numbers.
    pub app: Font,
    /// The title line; also the output name.
    pub title: Font,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    /// A fontconfig family, like `Inter` or `monospace`.
    pub family: String,
    /// In logical pixels.
    pub size: f32,
}

/// Font sizes outside this range are clamped to it.
const FONT_SIZES: std::ops::RangeInclusive<f32> = 4.0..=72.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Colors {
    pub backdrop: Rgba,
    pub output_name: Rgba,
    pub workspace: WorkspaceColors,
    pub window: WindowColors,
}

impl Config {
    fn defaults(clients: sway_config::Clients) -> Config {
        let font = |size| Font { family: "sans-serif".into(), size };
        Config {
            fonts: Fonts { app: font(14.0), title: font(12.0) },
            colors: Colors::defaults(clients),
            thumbnails: true,
        }
    }

    /// Defaults from the sway config at `sway_config`, overridden by
    /// `config.yaml` if it exists.
    pub fn load(sway_config: Option<&Path>) -> Config {
        Config::load_from(sway_config, default_path().as_deref())
    }

    fn load_from(sway_config: Option<&Path>, file: Option<&Path>) -> Config {
        let mut config = Config::defaults(sway_config::load(sway_config));
        if let Some(path) = file {
            match std::fs::read_to_string(path) {
                Ok(text) => match serde_saphyr::from_str::<ConfigFile>(&text) {
                    Ok(f) => {
                        for key in f.unknown_keys() {
                            warn(format_args!("{}: unknown key `{key}` ignored", path.display()));
                        }
                        f.fonts.app.apply(&mut config.fonts.app);
                        f.fonts.title.apply(&mut config.fonts.title);
                        config.colors.apply(f.colors);
                        config.thumbnails = f.thumbnails.unwrap_or(config.thumbnails);
                    }
                    Err(e) => warn(format_args!("{}: {e}", path.display())),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn(format_args!("{}: {e}", path.display())),
            }
        }
        config
    }
}

impl Colors {
    /// Built-in colors, with window colors from sway's `client.*` colors.
    fn defaults(clients: sway_config::Clients) -> Colors {
        Colors {
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

    fn apply(&mut self, f: ColorsFile) {
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

/// `$XDG_CONFIG_HOME/swayview/config.yaml`, or `~/.config/swayview/config.yaml`.
fn default_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("swayview/config.yaml"))
}

fn set(dst: &mut Rgba, value: Option<Rgba>) {
    if let Some(v) = value {
        *dst = v;
    }
}

/// Keys a `config.yaml` section does not know. They are reported and ignored,
/// so a leftover or misspelled key does not discard the rest of the file.
type Unknown = BTreeMap<String, IgnoredAny>;

/// `config.yaml`: every key is optional.
#[derive(Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    fonts: FontsFile,
    #[serde(default)]
    colors: ColorsFile,
    thumbnails: Option<bool>,
    #[serde(flatten)]
    unknown: Unknown,
}

impl ConfigFile {
    /// Unknown keys, as dotted paths like `colors.workspace.border`.
    fn unknown_keys(&self) -> Vec<String> {
        let (f, c, w) = (&self.fonts, &self.colors, &self.colors.window);
        let sections: [(&str, &Unknown); 10] = [
            ("", &self.unknown),
            ("fonts.", &f.unknown),
            ("fonts.app.", &f.app.unknown),
            ("fonts.title.", &f.title.unknown),
            ("colors.", &c.unknown),
            ("colors.workspace.", &c.workspace.unknown),
            ("colors.window.", &w.unknown),
            ("colors.window.normal.", &w.normal.unknown),
            ("colors.window.selected.", &w.selected.unknown),
            ("colors.window.urgent.", &w.urgent.unknown),
        ];
        sections.iter().flat_map(|(prefix, keys)| keys.keys().map(move |k| format!("{prefix}{k}"))).collect()
    }
}

#[derive(Deserialize, Default)]
struct FontsFile {
    #[serde(default)]
    app: FontFile,
    #[serde(default)]
    title: FontFile,
    #[serde(flatten)]
    unknown: Unknown,
}

#[derive(Deserialize, Default)]
struct FontFile {
    family: Option<String>,
    size: Option<f32>,
    #[serde(flatten)]
    unknown: Unknown,
}

impl FontFile {
    fn apply(self, font: &mut Font) {
        if let Some(family) = self.family.filter(|f| !f.trim().is_empty()) {
            font.family = family.trim().to_string();
        }
        if let Some(size) = self.size.filter(|s| !s.is_nan()) {
            font.size = size.clamp(*FONT_SIZES.start(), *FONT_SIZES.end());
        }
    }
}

#[derive(Deserialize, Default)]
struct ColorsFile {
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

    fn with_file(name: &str, yaml: &str) -> Config {
        let path = std::env::temp_dir().join(format!("swayview-config-{name}-{}.yaml", std::process::id()));
        std::fs::write(&path, yaml).unwrap();
        let config = Config::load_from(None, Some(&path));
        let _ = std::fs::remove_file(&path);
        config
    }

    fn defaults() -> Config {
        Config::defaults(sway_config::Clients::default())
    }

    #[test]
    fn no_file_gives_defaults() {
        assert_eq!(Config::load_from(None, None), defaults());
        assert_eq!(Config::load_from(None, Some(Path::new("/nonexistent/config.yaml"))), defaults());
    }

    #[test]
    fn selected_workspace_defaults_to_the_active_color() {
        let c = defaults().colors;
        assert_eq!(c.workspace.selected, c.window.selected.background);
    }

    #[test]
    fn file_overrides_only_what_it_sets() {
        let c = with_file(
            "partial",
            r##"
fonts:
  title:
    family: "Inter"
colors:
  backdrop: "#000000cc"
  workspace:
    selected: "#00ffff"
  window:
    selected:
      background: "#123456"
"##,
        );
        let d = defaults();
        assert_eq!(c.fonts.title, Font { family: "Inter".into(), size: d.fonts.title.size });
        assert_eq!(c.fonts.app, d.fonts.app);
        assert!(c.thumbnails);
        let (c, d) = (c.colors, d.colors);
        assert_eq!(c.backdrop, Rgba(0x000000cc));
        assert_eq!(c.workspace.selected, Rgba(0x00ffffff));
        assert_eq!(c.workspace.label, d.workspace.label);
        assert_eq!(c.window.selected.background, Rgba(0x123456ff));
        assert_eq!(c.window.selected.text, d.window.selected.text);
        assert_eq!(c.window.normal, d.window.normal);
    }

    #[test]
    fn thumbnails_can_be_turned_off() {
        assert!(!with_file("thumbnails", "thumbnails: false\n").thumbnails);
    }

    #[test]
    fn font_sizes_are_clamped() {
        let yaml = "fonts:\n  app:\n    size: 1000\n    family: \"  \"\n  title:\n    size: -3\n";
        let f = with_file("sizes", yaml).fonts;
        assert_eq!(f.app, Font { family: "sans-serif".into(), size: 72.0 });
        assert_eq!(f.title, Font { family: "sans-serif".into(), size: 4.0 });
        assert_eq!(with_file("nan", "fonts:\n  app:\n    size: .nan\n").fonts, defaults().fonts);
    }

    #[test]
    fn bad_files_are_ignored() {
        assert_eq!(with_file("color", "colors:\n  backdrop: \"#12\"\n"), defaults());
        assert_eq!(with_file("syntax", "colors: [unclosed\n"), defaults());
        assert_eq!(with_file("empty", ""), defaults());
        // Unquoted, `#` starts a comment and the value is empty.
        assert_eq!(with_file("unquoted", "colors:\n  backdrop: #000000\n"), defaults());
    }

    #[test]
    fn unknown_keys_are_skipped_not_fatal() {
        let yaml = "colors:\n  workspace:\n    border: \"#3a3f4b\"\n    selected: \"#00d7d7\"\n\
                    backdrop: \"#000000\"\nfonts:\n  app:\n    weight: bold\n";
        let c = with_file("unknown", yaml);
        assert_eq!(c.colors.workspace.selected, Rgba(0x00d7d7ff));
        let f: ConfigFile = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(f.unknown_keys(), ["backdrop", "fonts.app.weight", "colors.workspace.border"]);
    }
}
