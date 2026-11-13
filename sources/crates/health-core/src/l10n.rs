//! Inline localization. Every user-visible string is written as a Russian/English pair at the
//! call site, so there is no central string table to keep in sync:
//!
//! ```ignore
//! use health_core::{t, tf};
//! let title = t!("Модели", "Models");
//! let msg = tf!("Найдено {} моделей", "Found {} models", n);
//! ```

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Ru,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Ru => "ru",
            Lang::En => "en",
        }
    }
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::Ru => "Русский",
            Lang::En => "English",
        }
    }
    pub fn parse(s: &str) -> Option<Lang> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ru" | "rus" | "russian" | "русский" | "рус" => Some(Lang::Ru),
            "en" | "eng" | "english" | "английский" | "англ" => Some(Lang::En),
            _ => None,
        }
    }
    pub fn all() -> &'static [Lang] {
        &[Lang::Ru, Lang::En]
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

/// Set the global UI language.
pub fn set_lang(l: Lang) {
    LANG.store(
        match l {
            Lang::Ru => 0,
            Lang::En => 1,
        },
        Ordering::Relaxed,
    );
}

/// Current global UI language.
pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::Ru,
    }
}

/// Pick the string for the current language.
#[inline]
pub fn tr(ru: &'static str, en: &'static str) -> &'static str {
    match lang() {
        Lang::Ru => ru,
        Lang::En => en,
    }
}

/// Pick between two owned strings for the current language.
pub fn trs(ru: impl Into<String>, en: impl Into<String>) -> String {
    match lang() {
        Lang::Ru => ru.into(),
        Lang::En => en.into(),
    }
}

/// Russian plural form: `plural_ru(5, "модель", "модели", "моделей") == "моделей"`.
pub fn plural_ru<'a>(n: u64, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    let n10 = n % 10;
    let n100 = n % 100;
    if n10 == 1 && n100 != 11 {
        one
    } else if (2..=4).contains(&n10) && !(12..=14).contains(&n100) {
        few
    } else {
        many
    }
}

/// Localized count with noun: `count_noun(3, ("модель","модели","моделей"), ("model","models"))`.
pub fn count_noun(n: u64, ru: (&str, &str, &str), en: (&str, &str)) -> String {
    match lang() {
        Lang::Ru => format!("{n} {}", plural_ru(n, ru.0, ru.1, ru.2)),
        Lang::En => format!("{n} {}", if n == 1 { en.0 } else { en.1 }),
    }
}

/// Best-effort language detection: Russian when the Windows UI language is Russian-family OR a
/// Russian/Ukrainian/Belarusian/Kazakh keyboard layout is installed (common for people who run an
/// English Windows but write in Russian); English otherwise.
pub fn detect_system_lang() -> Lang {
    #[cfg(windows)]
    {
        // LANGID primary language ids: 0x19 Russian, 0x22 Ukrainian, 0x23 Belarusian, 0x3F Kazakh
        fn russian_family(langid: u16) -> bool {
            matches!(langid & 0x3ff, 0x19 | 0x22 | 0x23 | 0x3F)
        }
        let ui = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
        if russian_family(ui) {
            return Lang::Ru;
        }
        unsafe {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayoutList;
            let n = GetKeyboardLayoutList(0, std::ptr::null_mut());
            if n > 0 {
                let mut layouts = vec![std::ptr::null_mut(); n as usize];
                let got = GetKeyboardLayoutList(n, layouts.as_mut_ptr());
                for hkl in layouts.iter().take(got.max(0) as usize) {
                    // low word of an HKL is the input language id
                    if russian_family((*hkl as usize & 0xffff) as u16) {
                        return Lang::Ru;
                    }
                }
            }
        }
        Lang::En
    }
    #[cfg(not(windows))]
    {
        // POSIX precedence: LC_ALL > LC_MESSAGES > LANG
        let v = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find(|v| !v.is_empty())
            .unwrap_or_default()
            .to_lowercase();
        if ["ru", "uk", "be", "kk"].iter().any(|p| v.starts_with(p)) {
            Lang::Ru
        } else {
            Lang::En
        }
    }
}

/// `t!("Русский", "English")` → `&'static str` for the current language.
#[macro_export]
macro_rules! t {
    ($ru:expr, $en:expr $(,)?) => {
        $crate::l10n::tr($ru, $en)
    };
}

/// `tf!("Найдено {} моделей", "Found {} models", n)` → `String` for the current language.
/// Both format strings receive the same arguments.
#[macro_export]
macro_rules! tf {
    ($ru:expr, $en:expr, $($arg:tt)*) => {
        match $crate::l10n::lang() {
            $crate::l10n::Lang::Ru => format!($ru, $($arg)*),
            $crate::l10n::Lang::En => format!($en, $($arg)*),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plural() {
        assert_eq!(plural_ru(1, "a", "b", "c"), "a");
        assert_eq!(plural_ru(3, "a", "b", "c"), "b");
        assert_eq!(plural_ru(11, "a", "b", "c"), "c");
        assert_eq!(plural_ru(22, "a", "b", "c"), "b");
        assert_eq!(plural_ru(25, "a", "b", "c"), "c");
    }
}
