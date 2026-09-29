//! Font loading.
//!
//! cosmic-text's default scans every installed font, which takes hundreds of
//! milliseconds with thousands of fonts. Instead only the faces fontconfig
//! picks for the configured families are loaded, plus its pruned fallback
//! chain (`fc-match -s`), which keeps coverage for other scripts and emoji in
//! window titles. Without `fc-match`, it falls back to the full scan.

use std::collections::HashSet;
use std::process::Command;
use std::thread;

use cosmic_text::{FontSystem, fontdb};

use crate::warn;

/// Loads `faces`, given as (fontconfig family, bold), and returns the family
/// name to draw each one with.
pub fn load<const N: usize>(faces: [(&str, bool); N]) -> (FontSystem, [String; N]) {
    fontconfig(faces).unwrap_or_else(|| (FontSystem::new(), faces.map(|(family, _)| family.to_string())))
}

fn fontconfig<const N: usize>(faces: [(&str, bool); N]) -> Option<(FontSystem, [String; N])> {
    let (matches, fallback) = thread::scope(|s| {
        let join = |h: thread::ScopedJoinHandle<'_, Option<String>>| h.join().ok().flatten();
        let matches = faces.map(|(family, bold)| {
            let pattern = format!("{}{}", escape(family), if bold { ":bold" } else { "" });
            s.spawn(move || fc_match(&["-f", "%{family[0]}\n%{file}\n", &pattern]))
        });
        let fallback = s.spawn(|| fc_match(&["-s", "-f", "%{file}\n", "sans-serif"]));
        (matches.map(join), join(fallback))
    });

    let mut names = Vec::with_capacity(N);
    let mut files = Vec::new();
    for ((family, _), m) in faces.iter().zip(&matches) {
        let (name, file) = m.as_deref()?.split_once('\n')?;
        if name.is_empty() {
            return None;
        }
        // fontconfig always picks something, so a typo would go unnoticed.
        if !["sans-serif", "serif", "monospace"].contains(family) && !name.eq_ignore_ascii_case(family) {
            warn(format_args!("font {family:?} not found, using {name:?}"));
        }
        names.push(name.to_string());
        files.push(file.trim_end());
    }
    files.extend(fallback.as_deref().unwrap_or("").lines());

    let mut db = fontdb::Database::new();
    let mut seen = HashSet::new();
    for file in files {
        if !file.is_empty() && seen.insert(file) {
            // An unreadable font only costs its coverage.
            let _ = db.load_font_file(file);
        }
    }
    if db.is_empty() {
        return None;
    }
    let names = names.try_into().ok()?;
    Some((FontSystem::new_with_locale_and_db(locale(|v| std::env::var(v).ok()), db), names))
}

/// Escapes the characters that end a family name in a fontconfig pattern.
fn escape(family: &str) -> String {
    let mut out = String::with_capacity(family.len());
    for c in family.chars() {
        if matches!(c, '\\' | '-' | ':' | ',') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn fc_match(args: &[&str]) -> Option<String> {
    let out = Command::new("fc-match").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// BCP 47 tag from the POSIX locale variables, e.g. `en_US.UTF-8` → `en-US`.
fn locale(var: impl Fn(&str) -> Option<String>) -> String {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .filter_map(var)
        .find(|v| !v.is_empty())
        .and_then(|v| {
            let tag = v.split(['.', '@']).next().unwrap_or_default().replace('_', "-");
            (!tag.is_empty() && tag != "C" && tag != "POSIX").then_some(tag)
        })
        .unwrap_or_else(|| "en-US".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locale_of(pairs: &[(&str, &str)]) -> String {
        locale(|v| pairs.iter().find(|(k, _)| *k == v).map(|(_, x)| (*x).to_string()))
    }

    #[test]
    fn locales() {
        assert_eq!(locale_of(&[("LANG", "en_US.UTF-8")]), "en-US");
        assert_eq!(locale_of(&[("LC_ALL", "lt_LT.UTF-8"), ("LANG", "en_US.UTF-8")]), "lt-LT");
        assert_eq!(locale_of(&[("LC_ALL", ""), ("LANG", "de_DE@euro")]), "de-DE");
        assert_eq!(locale_of(&[("LANG", "C.UTF-8")]), "en-US");
        assert_eq!(locale_of(&[]), "en-US");
    }

    #[test]
    fn loads_fontconfig_faces() {
        // Needs fc-match and at least one font, as on any desktop.
        let Some((fs, [regular, bold])) = fontconfig([("sans-serif", false), ("sans-serif", true)]) else {
            return;
        };
        assert!(!fs.db().is_empty());
        assert!(!regular.is_empty() && !bold.is_empty());
    }

    #[test]
    fn escapes_patterns() {
        assert_eq!(escape("Fira Code-Retina"), "Fira Code\\-Retina");
        assert_eq!(escape("a:b,c\\"), "a\\:b\\,c\\\\");
    }
}
