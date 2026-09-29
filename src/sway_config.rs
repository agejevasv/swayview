//! Window colors from the sway config (`client.*`), falling back to sway's defaults.
//!
//! Sway does not expose these over IPC, so the config it loaded is read here,
//! following `set $var` and `include`. Anything unreadable or malformed is
//! skipped, leaving the defaults in place.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::color::Rgba;
use crate::theme::Class;

/// The `client.*` colors swayview uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clients {
    pub focused: Class,
    pub unfocused: Class,
    pub urgent: Class,
}

impl Default for Clients {
    /// Sway's built-in colors.
    fn default() -> Self {
        let class = |border, background, text| Class {
            border: Rgba(border),
            background: Rgba(background),
            text: Rgba(text),
        };
        Clients {
            focused: class(0x4c7899ff, 0x285577ff, 0xffffffff),
            unfocused: class(0x333333ff, 0x222222ff, 0x888888ff),
            urgent: class(0x2f343aff, 0x900000ff, 0xffffffff),
        }
    }
}

pub fn load(path: Option<&Path>) -> Clients {
    let mut parser = Parser { clients: Clients::default(), vars: Vec::new(), loaded: HashSet::new() };
    if let Some(path) = path {
        parser.file(path);
    }
    parser.clients
}

struct Parser {
    clients: Clients,
    /// `$name` → value, in definition order.
    vars: Vec<(String, String)>,
    /// Files read so far; as in sway, each is read only once, which also ends include cycles.
    loaded: HashSet<PathBuf>,
}

impl Parser {
    fn file(&mut self, path: &Path) {
        let Ok(real) = path.canonicalize() else { return };
        if !self.loaded.insert(real) {
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
                self.line(line.trim(), dir);
                line.clear();
            }
        }
        self.line(line.trim(), dir);
    }

    fn line(&mut self, line: &str, dir: &Path) {
        if line.is_empty() || line.starts_with('#') {
            return;
        }
        let (cmd, rest) = split_word(line);
        match cmd {
            "set" => {
                let (name, value) = split_word(rest);
                if name.starts_with('$') {
                    let value = unquote(&self.expand(value)).to_string();
                    self.vars.retain(|(n, _)| n != name);
                    self.vars.push((name.to_string(), value));
                }
            }
            "include" => {
                for path in resolve(&expand_env(&self.expand(rest)), dir) {
                    self.file(&path);
                }
            }
            "client.focused" | "client.unfocused" | "client.urgent" => {
                let args = self.expand(rest);
                let colors: Option<Vec<_>> =
                    args.split_whitespace().map(|a| Rgba::parse(unquote(a))).collect();
                // The indicator and child border that may follow are not drawn.
                let Some(&[border, background, text, ..]) = colors.as_deref() else { return };
                let class = match cmd {
                    "client.focused" => &mut self.clients.focused,
                    "client.unfocused" => &mut self.clients.unfocused,
                    _ => &mut self.clients.urgent,
                };
                *class = Class { border, background, text };
            }
            _ => {}
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

/// `s` without surrounding quotes, which sway strips from arguments.
fn unquote(s: &str) -> &str {
    ['"', '\''].into_iter().find_map(|q| s.strip_prefix(q)?.strip_suffix(q)).unwrap_or(s)
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

/// Paths for an include argument, relative to `dir`, with `*`/`?` in the file
/// name. As in a shell, wildcards do not match a leading `.`.
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
        .filter(|e| {
            e.file_name().to_str().is_some_and(|n| {
                (!n.starts_with('.') || name.starts_with('.')) && wildcard(name.as_bytes(), n.as_bytes())
            })
        })
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
        assert_eq!(load(None), Clients::default());
        assert_eq!(load(Some(Path::new("/nonexistent/config"))), Clients::default());
    }

    #[test]
    fn reads_variables_includes_and_short_forms() {
        let dir = temp_dir("theme");
        std::fs::create_dir(dir.join("config.d")).unwrap();
        std::fs::write(
            dir.join("config"),
            "set $bg #101010\nset $bg2 \"#202020cc\"\n\
             client.focused #111111 $bg #eeeeee #ff0000 #00ff00\n\
             client.unfocused not-a-color #000000 #ffffff\n\
             include config.d/*\n",
        )
        .unwrap();
        std::fs::write(dir.join("config.d/10-colors"), "client.unfocused '#222222' $bg2 #999999\n").unwrap();
        std::fs::write(dir.join("config.d/20-other"), "# nothing\nbindsym x exec y\n").unwrap();
        // Hidden files are not matched by `*`.
        std::fs::write(dir.join("config.d/.30-hidden"), "client.urgent #010101 #010101 #010101\n").unwrap();

        let t = load(Some(&dir.join("config")));
        let class = |border, background, text| Class {
            border: Rgba(border),
            background: Rgba(background),
            text: Rgba(text),
        };
        assert_eq!(t.focused, class(0x111111ff, 0x101010ff, 0xeeeeeeff));
        // The later, valid short form replaces the malformed line.
        assert_eq!(t.unfocused, class(0x222222ff, 0x202020cc, 0x999999ff));
        assert_eq!(t.urgent, Clients::default().urgent);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_are_included_once() {
        let dir = temp_dir("cycle");
        std::fs::write(dir.join("colors"), "client.focused #010101 #020202 #030303\n").unwrap();
        let config =
            "include config\ninclude colors\nclient.focused #0a0a0a #0b0b0b #0c0c0c\ninclude colors\n";
        std::fs::write(dir.join("config"), config).unwrap();
        let t = load(Some(&dir.join("config")));
        assert_eq!(t.focused.border, Rgba(0x0a0a0aff));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn line_continuation() {
        let dir = temp_dir("continuation");
        let config = "client.focused #010101 \\\n  #020202 #030303\nclient.urgent #0a0a0a #0b0b0b #0c0c0c\n";
        std::fs::write(dir.join("config"), config).unwrap();
        let t = load(Some(&dir.join("config")));
        assert_eq!(t.focused.background, Rgba(0x020202ff));
        assert_eq!(t.urgent.background, Rgba(0x0b0b0bff));
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
