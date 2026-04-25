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
}
