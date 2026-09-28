//! Font loading.
//!
//! cosmic-text's default scans every installed font, which takes hundreds of
//! milliseconds with thousands of fonts. Instead only the fonts fontconfig
//! picks for sans-serif are loaded: the regular and bold faces plus
//! fontconfig's pruned fallback chain (`fc-match -s`), which keeps coverage
//! for other scripts and emoji in window titles. Without `fc-match`, it falls
//! back to the full scan.

use std::collections::HashSet;
use std::process::Command;
use std::thread;

use cosmic_text::{FontSystem, fontdb};

pub fn font_system() -> FontSystem {
    sans_serif().unwrap_or_else(FontSystem::new)
}

fn sans_serif() -> Option<FontSystem> {
    let (primary, bold, fallback) = thread::scope(|s| {
        let primary = s.spawn(|| fc_match(&["-f", "%{family[0]}\n%{file}\n", "sans-serif"]));
        let bold = s.spawn(|| fc_match(&["-f", "%{file}\n", "sans-serif:bold"]));
        let fallback = s.spawn(|| fc_match(&["-s", "-f", "%{file}\n", "sans-serif"]));
        let join = |h: thread::ScopedJoinHandle<'_, Option<String>>| h.join().ok().flatten();
        (join(primary), join(bold), join(fallback))
    });
    let primary = primary?;
    let mut lines = primary.lines();
    let family = lines.next().filter(|f| !f.is_empty())?;

    let mut db = fontdb::Database::new();
    let mut seen = HashSet::new();
    let files =
        lines.chain(bold.as_deref().unwrap_or("").lines()).chain(fallback.as_deref().unwrap_or("").lines());
    for file in files {
        if !file.is_empty() && seen.insert(file) {
            // An unreadable font only costs its coverage.
            let _ = db.load_font_file(file);
        }
    }
    if db.is_empty() {
        return None;
    }
    db.set_sans_serif_family(family);
    Some(FontSystem::new_with_locale_and_db(locale(|v| std::env::var(v).ok()), db))
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
    fn loads_fontconfig_sans_serif() {
        // Needs fc-match and at least one font, as on any desktop.
        let Some(fs) = sans_serif() else { return };
        assert!(!fs.db().is_empty());
    }
}
