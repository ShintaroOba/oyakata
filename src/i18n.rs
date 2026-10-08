//! Language of the terminal output and of server messages shown in the browser. The browser
//! UI translates itself (web/i18n.js); this picks the same language for everything that is
//! printed by Rust. Resolution: `OYAKATA_LANG`, then `lang` in `~/.oyakata/config.json`, then
//! the operating system's UI language, then English.

use std::path::Path;
use std::sync::OnceLock;

static LANG: OnceLock<String> = OnceLock::new();

/// Decide the language once, given the config file location.
pub fn init(oyakata_dir: &Path) {
    let _ = LANG.set(resolve(oyakata_dir));
}

fn resolve(oyakata_dir: &Path) -> String {
    if let Some(l) = std::env::var("OYAKATA_LANG").ok().and_then(|v| normalize(&v)) {
        return l;
    }
    if let Ok(text) = std::fs::read_to_string(oyakata_dir.join("config.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(l) = v.get("lang").and_then(serde_json::Value::as_str).and_then(normalize) {
                return l;
            }
        }
    }
    os_language().unwrap_or_else(|| "en".to_string())
}

/// `ja`, `ja-JP`, `ja_JP.UTF-8`, `Japanese_Japan.932` → `ja`; anything English-ish → `en`.
pub fn normalize(v: &str) -> Option<String> {
    let v = v.trim().to_ascii_lowercase();
    if v.starts_with("ja") {
        Some("ja".into())
    } else if v.starts_with("en") || v == "c" || v == "posix" {
        Some("en".into())
    } else {
        None
    }
}

#[cfg(windows)]
fn os_language() -> Option<String> {
    // Windows UI language; 0x11 is the primary language id of Japanese.
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // SAFETY: plain Win32 call without arguments.
    let id = unsafe { GetUserDefaultUILanguage() };
    Some(if id & 0x3ff == 0x11 { "ja".into() } else { "en".into() })
}

#[cfg(not(windows))]
fn os_language() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|k| std::env::var(k).ok().and_then(|v| normalize(&v)))
}

pub fn lang() -> &'static str {
    LANG.get().map(String::as_str).unwrap_or("en")
}

pub fn is_ja() -> bool {
    lang() == "ja"
}

/// Pick the Japanese or English text for the current language.
pub fn tr<'a>(ja: &'a str, en: &'a str) -> &'a str {
    if is_ja() {
        ja
    } else {
        en
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_locale_strings() {
        assert_eq!(normalize("ja_JP.UTF-8").as_deref(), Some("ja"));
        assert_eq!(normalize("Japanese_Japan.932").as_deref(), Some("ja"));
        assert_eq!(normalize("en-US").as_deref(), Some("en"));
        assert_eq!(normalize("C").as_deref(), Some("en"));
        assert_eq!(normalize("de_DE"), None);
    }
}
