//! Window colors from the sway config (`client.*`), falling back to sway's defaults.
//!
//! Sway does not expose these over IPC, so the config it loaded is read here,
//! following `set $var` and `include`. Anything unreadable or malformed is
//! skipped, leaving the defaults in place.

use std::path::{Path, PathBuf};

use crate::color::Rgba;

/// Colors of one `client.<class>` line. The fifth color, `child_border`, is
/// accepted but not drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Class {
    pub border: Rgba,
    pub background: Rgba,
    pub text: Rgba,
    pub indicator: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub focused: Class,
    pub focused_inactive: Class,
    pub unfocused: Class,
}

impl Default for Theme {
    /// Sway's built-in colors.
    fn default() -> Self {
        let class = |border, background, text, indicator| Class {
            border: Rgba(border),
            background: Rgba(background),
            text: Rgba(text),
            indicator: Rgba(indicator),
        };
        Theme {
            focused: class(0x4c7899ff, 0x285577ff, 0xffffffff, 0x2e9ef4ff),
            focused_inactive: class(0x333333ff, 0x5f676aff, 0xffffffff, 0x484e50ff),
            unfocused: class(0x333333ff, 0x222222ff, 0x888888ff, 0x292d2eff),
        }
    }
}

const MAX_INCLUDE_DEPTH: usize = 8;

impl Theme {
    /// Theme from the sway config at `path`, or the defaults.
    pub fn load(path: Option<&Path>) -> Theme {
        let mut parser = Parser { theme: Theme::default(), vars: Vec::new() };
        if let Some(path) = path {
            parser.file(path, 0);
        }
        parser.theme
    }
}

struct Parser {
    theme: Theme,
    /// `$name` → value, in definition order.
    vars: Vec<(String, String)>,
}

impl Parser {
    fn file(&mut self, path: &Path, depth: usize) {
        if depth > MAX_INCLUDE_DEPTH {
            return;
        }
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let dir = path.parent().unwrap_or(Path::new("/"));
        // A trailing backslash continues the line.
        let mut line = String::new();
        for part in text.lines() {
            if let Some(head) = part.strip_suffix('\\') {
                line.push_str(head);
            } else {
                line.push_str(part);
                self.line(line.trim(), dir, depth);
                line.clear();
            }
        }
        self.line(line.trim(), dir, depth);
    }

    fn line(&mut self, line: &str, dir: &Path, depth: usize) {
        if line.is_empty() || line.starts_with('#') {
            return;
        }
        let (cmd, rest) = split_word(line);
        match cmd {
            "set" => {
                let (name, value) = split_word(rest);
                if name.starts_with('$') {
                    let value = self.expand(value);
                    self.vars.retain(|(n, _)| n != name);
                    self.vars.push((name.to_string(), value));
                }
            }
            "include" => {
                for path in resolve(&expand_env(&self.expand(rest)), dir) {
                    self.file(&path, depth + 1);
                }
            }
            _ => {
                if !cmd.starts_with("client.") {
                    return;
                }
                let args = self.expand(rest);
                let class = match cmd {
                    "client.focused" => &mut self.theme.focused,
                    "client.focused_inactive" => &mut self.theme.focused_inactive,
                    "client.unfocused" => &mut self.theme.unfocused,
                    _ => return,
                };
                let Some(colors) = args.split_whitespace().map(Rgba::parse).collect::<Option<Vec<_>>>()
                else {
                    return;
                };
                if let &[border, background, text, ref rest @ ..] = colors.as_slice() {
                    let indicator = rest.first().copied().unwrap_or(class.indicator);
                    *class = Class { border, background, text, indicator };
                }
            }
        }
    }

    /// Replaces `$name` variables, longest names first as sway does.
    fn expand(&self, s: &str) -> String {
        let mut vars: Vec<_> = self.vars.iter().collect();
        vars.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
        let mut out = s.to_string();
        for (name, value) in vars {
            out = out.replace(name.as_str(), value);
        }
        out
    }
}

fn split_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (s, ""),
    }
}

/// Expands `~`, `$VAR` and `${VAR}` from the environment, as sway's wordexp does.
fn expand_env(s: &str) -> String {
    let s = s.trim().trim_matches('"');
    let mut out = String::new();
    let mut rest = match s.strip_prefix('~') {
        Some(r) => {
            out.push_str(&std::env::var("HOME").unwrap_or_default());
            r
        }
        None => s,
    };
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (name, tail) = if let Some(b) = after.strip_prefix('{') {
            b.split_once('}').unwrap_or((b, ""))
        } else {
            let end = after.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(after.len());
            after.split_at(end)
        };
        out.push_str(&std::env::var(name).unwrap_or_default());
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// Paths for an include argument, relative to `dir`, with `*`/`?` in the file name.
fn resolve(pattern: &str, dir: &Path) -> Vec<PathBuf> {
    let path = dir.join(pattern);
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return Vec::new() };
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let parent = path.parent().unwrap_or(Path::new("/"));
    let Ok(entries) = std::fs::read_dir(parent) else { return Vec::new() };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_str().is_some_and(|n| wildcard(name.as_bytes(), n.as_bytes())))
        .map(|e| e.path())
        .collect();
    paths.sort();
    paths
}

fn wildcard(pattern: &[u8], s: &[u8]) -> bool {
    match (pattern.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => wildcard(&pattern[1..], s) || (!s.is_empty() && wildcard(pattern, &s[1..])),
        (Some(b'?'), Some(_)) => wildcard(&pattern[1..], &s[1..]),
        (Some(p), Some(c)) if p == c => wildcard(&pattern[1..], &s[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("swayview-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_config_gives_defaults() {
        assert_eq!(Theme::load(None), Theme::default());
        assert_eq!(Theme::load(Some(Path::new("/nonexistent/config"))), Theme::default());
    }

    #[test]
    fn reads_variables_includes_and_short_forms() {
        let dir = temp_dir("theme");
        std::fs::create_dir(dir.join("config.d")).unwrap();
        std::fs::write(
            dir.join("config"),
            "set $bg #101010\nset $bg2 #202020cc\n\
             client.focused #111111 $bg #eeeeee #ff0000 #00ff00\n\
             client.unfocused not-a-color #000000 #ffffff\n\
             include config.d/*\n",
        )
        .unwrap();
        std::fs::write(dir.join("config.d/10-colors"), "client.unfocused #222222 $bg2 #999999\n").unwrap();
        std::fs::write(dir.join("config.d/20-other"), "# nothing\nbindsym x exec y\n").unwrap();

        let t = Theme::load(Some(&dir.join("config")));
        let d = Theme::default();
        assert_eq!(
            t.focused,
            Class {
                border: Rgba(0x111111ff),
                background: Rgba(0x101010ff),
                text: Rgba(0xeeeeeeff),
                indicator: Rgba(0xff0000ff),
            }
        );
        // Short form: indicator kept from the default.
        assert_eq!(
            t.unfocused,
            Class {
                border: Rgba(0x222222ff),
                background: Rgba(0x202020cc),
                text: Rgba(0x999999ff),
                indicator: d.unfocused.indicator,
            }
        );
        assert_eq!(t.focused_inactive, d.focused_inactive);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn include_cycles_terminate() {
        let dir = temp_dir("cycle");
        std::fs::write(dir.join("config"), "include config\nclient.focused #010101 #020202 #030303\n")
            .unwrap();
        let t = Theme::load(Some(&dir.join("config")));
        assert_eq!(t.focused.border, Rgba(0x010101ff));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn line_continuation() {
        let dir = temp_dir("continuation");
        std::fs::write(dir.join("config"), "client.focused #010101 \\\n  #020202 #030303\n").unwrap();
        let t = Theme::load(Some(&dir.join("config")));
        assert_eq!(t.focused.background, Rgba(0x020202ff));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_expansion() {
        let home = std::env::var("HOME").unwrap_or_default();
        assert_eq!(expand_env("$HOME/a"), format!("{home}/a"));
        assert_eq!(expand_env("${HOME}b/*"), format!("{home}b/*"));
        assert_eq!(expand_env("~/c"), format!("{home}/c"));
        assert!(wildcard(b"*.conf", b"50-colors.conf"));
        assert!(!wildcard(b"*.conf", b"colors.ini"));
    }
}
