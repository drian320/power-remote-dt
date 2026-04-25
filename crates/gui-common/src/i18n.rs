//! Mozilla Fluent based localization. Bundled `.ftl` files for English and
//! Japanese live under `crates/gui-common/locales/`. The active locale is a
//! global selected at startup via `init()` and switchable at runtime via
//! `set_locale()`. Translation lookups are non-panicking — missing IDs
//! return `"missing-string: <id>"` so the UI shows broken strings inline
//! rather than crashing.

use std::collections::HashMap;
use std::sync::RwLock;

pub use fluent_templates::fluent_bundle::FluentValue;
use fluent_templates::Loader;
use unic_langid::{langid, LanguageIdentifier};

fluent_templates::static_loader! {
    static LOCALES = {
        locales: "./locales",
        fallback_language: "en",
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    Ja,
}

impl Locale {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ja => "ja",
        }
    }

    pub fn langid(self) -> LanguageIdentifier {
        match self {
            Self::En => langid!("en"),
            Self::Ja => langid!("ja"),
        }
    }

    /// Parse a Config-locale string. Accepts "en" / "ja"; "" / "auto" /
    /// unknown values yield None (caller falls back to OS detect).
    pub fn from_config_str(s: &str) -> Option<Self> {
        match s {
            "en" => Some(Self::En),
            "ja" => Some(Self::Ja),
            _ => None,
        }
    }
}

/// Detect from the OS locale via sys-locale. `ja*` → Ja, else En.
pub fn detect_locale() -> Locale {
    if let Some(s) = sys_locale::get_locale() {
        let lower = s.to_lowercase();
        if lower.starts_with("ja") {
            return Locale::Ja;
        }
    }
    Locale::En
}

static CURRENT: RwLock<Option<Locale>> = RwLock::new(None);

/// Initialize the active locale. `config_str` is `Config.gui.locale`:
/// "" / "auto" / unknown → OS detect, "en" / "ja" → forced.
pub fn init(config_str: &str) {
    let l = Locale::from_config_str(config_str).unwrap_or_else(detect_locale);
    *CURRENT.write().unwrap() = Some(l);
}

/// Switch the active locale at runtime (Settings UI calls this).
pub fn set_locale(l: Locale) {
    *CURRENT.write().unwrap() = Some(l);
}

/// Read the active locale, defaulting to En if `init()` was never called
/// (defensive — should not happen in production, but unit tests skip init).
pub fn current_locale() -> Locale {
    CURRENT.read().unwrap().unwrap_or(Locale::En)
}

/// Translate `id` in the active locale. Missing → "missing-string: <id>".
pub fn tr(id: &str) -> String {
    let lid = current_locale().langid();
    LOCALES
        .try_lookup(&lid, id)
        .unwrap_or_else(|| format!("missing-string: {id}"))
}

/// Translate `id` with `{ $name }` placeholders substituted from `args`.
pub fn tr_args(id: &str, args: &HashMap<&str, FluentValue>) -> String {
    let lid = current_locale().langid();
    let mut owned: HashMap<String, FluentValue> = HashMap::with_capacity(args.len());
    for (k, v) in args {
        owned.insert((*k).to_string(), v.clone());
    }
    LOCALES
        .try_lookup_with_args(&lid, id, &owned)
        .unwrap_or_else(|| format!("missing-string: {id}"))
}

/// Convenience macro. `t!("id")` or `t!("id", name => &val, count => 5)`.
#[macro_export]
macro_rules! t {
    ($id:literal) => {{
        $crate::tr($id)
    }};
    ($id:literal, $($k:ident => $v:expr),+ $(,)?) => {{
        let mut args: ::std::collections::HashMap<&str, $crate::FluentValue> =
            ::std::collections::HashMap::new();
        $(args.insert(stringify!($k), $crate::FluentValue::from($v));)+
        $crate::tr_args($id, &args)
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_from_config_str_recognizes_known() {
        assert_eq!(Locale::from_config_str("en"), Some(Locale::En));
        assert_eq!(Locale::from_config_str("ja"), Some(Locale::Ja));
        assert_eq!(Locale::from_config_str(""), None);
        assert_eq!(Locale::from_config_str("auto"), None);
        assert_eq!(Locale::from_config_str("zh"), None);
    }

    #[test]
    fn detect_locale_returns_one_of_en_or_ja() {
        let l = detect_locale();
        assert!(matches!(l, Locale::En | Locale::Ja));
    }

    #[test]
    fn tr_returns_translated_in_each_locale() {
        set_locale(Locale::En);
        assert_eq!(tr("common-button-cancel"), "Cancel");
        set_locale(Locale::Ja);
        assert_eq!(tr("common-button-cancel"), "キャンセル");
    }

    #[test]
    fn missing_id_returns_marker() {
        let s = tr("definitely-not-a-real-id");
        assert!(
            s.starts_with("missing-string:"),
            "expected marker prefix, got {s:?}"
        );
    }

    #[test]
    fn init_overrides_detected_locale() {
        init("ja");
        assert_eq!(current_locale(), Locale::Ja);
        init("en");
        assert_eq!(current_locale(), Locale::En);
        init("");
        let l = current_locale();
        assert!(matches!(l, Locale::En | Locale::Ja));
    }

    fn parse_ftl_messages(src: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut cur_id: Option<String> = None;
        let mut cur_val = String::new();
        for line in src.lines() {
            let trimmed_left = line.trim_start();
            if trimmed_left.is_empty() || trimmed_left.starts_with('#') {
                if let Some(id) = cur_id.take() {
                    out.push((id, std::mem::take(&mut cur_val)));
                }
                continue;
            }
            if line.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                if let Some(eq) = line.find('=') {
                    if let Some(id) = cur_id.take() {
                        out.push((id, std::mem::take(&mut cur_val)));
                    }
                    let id = line[..eq].trim().to_string();
                    let value = line[eq + 1..].trim_start().to_string();
                    cur_id = Some(id);
                    cur_val = value;
                    continue;
                }
            }
            if !cur_val.is_empty() {
                cur_val.push('\n');
            }
            cur_val.push_str(trimmed_left);
        }
        if let Some(id) = cur_id.take() {
            out.push((id, cur_val));
        }
        out
    }

    fn extract_placeholders(value: &str) -> std::collections::BTreeSet<String> {
        let mut set = std::collections::BTreeSet::new();
        let bytes = value.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == b'{' {
                if let Some(end) = value[i + 1..].find('}') {
                    let inside = value[i + 1..i + 1 + end].trim();
                    if let Some(name) = inside.strip_prefix('$') {
                        set.insert(name.trim().to_string());
                    }
                    i += 1 + end + 1;
                    continue;
                }
            }
            i += 1;
        }
        set
    }

    const EN_FTL: &str = include_str!("../locales/en/main.ftl");
    const JA_FTL: &str = include_str!("../locales/ja/main.ftl");

    #[test]
    fn locale_files_have_same_ids() {
        let en = parse_ftl_messages(EN_FTL);
        let ja = parse_ftl_messages(JA_FTL);
        let en_ids: std::collections::HashSet<_> = en.iter().map(|(id, _)| id.clone()).collect();
        let ja_ids: std::collections::HashSet<_> = ja.iter().map(|(id, _)| id.clone()).collect();
        let only_en: Vec<_> = en_ids.difference(&ja_ids).collect();
        let only_ja: Vec<_> = ja_ids.difference(&en_ids).collect();
        assert!(
            only_en.is_empty() && only_ja.is_empty(),
            "Locale ID mismatch.\n  Only in en: {only_en:?}\n  Only in ja: {only_ja:?}",
        );
    }

    #[test]
    fn placeholders_match_across_locales() {
        let en = parse_ftl_messages(EN_FTL);
        let ja = parse_ftl_messages(JA_FTL);
        let ja_map: std::collections::HashMap<_, _> = ja.into_iter().collect();
        for (id, val_en) in &en {
            let val_ja = ja_map
                .get(id)
                .unwrap_or_else(|| panic!("ja.ftl missing id {id}"));
            let p_en = extract_placeholders(val_en);
            let p_ja = extract_placeholders(val_ja);
            assert_eq!(p_en, p_ja, "Placeholder mismatch for {id}");
        }
    }

    #[test]
    fn host_status_listening_substitutes_bind() {
        set_locale(Locale::En);
        let s = t!("host-status-listening", bind => "0.0.0.0:9000");
        assert!(s.contains("0.0.0.0:9000"), "got {s:?}");
        set_locale(Locale::Ja);
        let s = t!("host-status-listening", bind => "0.0.0.0:9000");
        assert!(s.contains("0.0.0.0:9000"), "got {s:?}");
    }
}
