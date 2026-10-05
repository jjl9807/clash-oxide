use anyhow::{Context, Result};
use oxide_model::{Diagnostic, Phase, Snapshot};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{
        RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

rust_i18n::i18n!("locales", fallback = "en");

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
}

impl Language {
    pub const ALL: [Self; 3] = [Self::Auto, Self::English, Self::SimplifiedChinese];
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "en" => Some(Self::English),
            "zh-CN" => Some(Self::SimplifiedChinese),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
        }
    }
    pub fn label(self) -> String {
        match self {
            Self::Auto => tr!("language.auto"),
            Self::English => "English".into(),
            Self::SimplifiedChinese => "简体中文".into(),
        }
    }
}

static LANGUAGE: RwLock<Language> = RwLock::new(Language::Auto);
static LAUNCH_OVERRIDE: RwLock<Option<Language>> = RwLock::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Default, Serialize, Deserialize)]
struct Preferences {
    #[serde(default)]
    language: Language,
}

pub fn preference_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("clash-oxide/frontend.json")
}

/// CLI override > application environment override > saved choice > system.
/// Initialization is read-only: CLI queries and the daemon never create preferences.
pub fn initialize(cli: Option<&str>) {
    let environment = std::env::var("CLASH_OXIDE_LANG").ok();
    let override_language = cli.or(environment.as_deref()).and_then(Language::parse);
    *LAUNCH_OVERRIDE.write().unwrap() = override_language;
    let saved = load(&preference_path());
    let language =
        override_language.unwrap_or_else(|| saved.as_ref().map(|p| p.language).unwrap_or_default());
    apply(language);
    if let Err(error) = saved {
        eprintln!("{}", tr!("language.read_failed", detail = error));
    }
}

fn load(path: &Path) -> Result<Preferences> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("Invalid frontend preferences"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Preferences::default()),
        Err(error) => Err(error.into()),
    }
}

/// Save before applying so a failed write leaves the selected language intact.
pub fn set_language(language: Language) -> Result<()> {
    save(&preference_path(), language).context(Diagnostic::new(
        "error.save_language",
        "Could not save the language preference",
    ))?;
    apply(language);
    Ok(())
}

fn save(path: &Path, language: Language) -> Result<()> {
    let parent = path
        .parent()
        .context("Preferences need a parent directory")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, &Preferences { language })?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

fn apply(language: Language) {
    // Initialize the compiled catalogue before selecting the process locale.
    let _ = translate_in("en", "nav.overview", &[]);
    let locale = match language {
        Language::English => "en",
        Language::SimplifiedChinese => "zh-CN",
        Language::Auto => system_locale(),
    };
    *LANGUAGE.write().unwrap() = language;
    rust_i18n::set_locale(locale);
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub fn launch_override() -> Option<Language> {
    *LAUNCH_OVERRIDE.read().unwrap()
}
pub fn set_session_language(language: Language) {
    apply(language);
}

pub fn language() -> Language {
    *LANGUAGE.read().unwrap()
}
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}
pub fn locale() -> String {
    rust_i18n::locale().to_string()
}

fn supported_locale(value: &str) -> Option<&'static str> {
    let value = value
        .split(['.', '@'])
        .next()
        .unwrap_or(value)
        .replace('_', "-")
        .to_ascii_lowercase();
    match value.as_str() {
        "c" | "posix" | "en" => Some("en"),
        "zh" | "zh-cn" | "zh-sg" | "zh-hans" | "zh-hans-cn" | "zh-hans-sg" => Some("zh-CN"),
        value if value.starts_with("en-") => Some("en"),
        _ => None,
    }
}

fn select_system_locale(message_locale: &str, languages: Option<&str>) -> &'static str {
    if matches!(message_locale.split('.').next(), Some("C" | "POSIX")) {
        return "en";
    }
    languages
        .into_iter()
        .flat_map(|s| s.split(':'))
        .chain([message_locale])
        .find_map(supported_locale)
        .unwrap_or("en")
}

fn system_locale() -> &'static str {
    let message_locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "C".into());
    select_system_locale(&message_locale, std::env::var("LANGUAGE").ok().as_deref())
}

pub fn translate(key: &str, args: &[(&str, String)]) -> String {
    translate_in(&locale(), key, args)
}

pub fn translate_in(locale: &str, key: &str, args: &[(&str, String)]) -> String {
    let template = rust_i18n::t!(key, locale = locale);
    let (names, values): (Vec<_>, Vec<_>) = args.iter().cloned().unzip();
    rust_i18n::replace_patterns(&template, &names, &values)
}

#[macro_export]
macro_rules! tr {
    ($key:expr $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::translate($key, &[$((stringify!($name), ($value).to_string())),*])
    };
}

pub fn diagnostic(message: &Diagnostic) -> String {
    let args: Vec<_> = message
        .args
        .iter()
        .map(|(key, value)| (key.as_str(), value.clone()))
        .collect();
    let translated = translate(&message.code, &args);
    if translated == message.code {
        return message.detail.clone();
    }
    let english = translate_in("en", &message.code, &args);
    if message.detail.is_empty() || message.detail == english {
        translated
    } else {
        format!(
            "{translated}\n{}",
            tr!("error.details", detail = message.detail)
        )
    }
}

pub fn snapshot_error(snapshot: &Snapshot) -> Option<String> {
    snapshot
        .last_diagnostic
        .as_ref()
        .map(diagnostic)
        .or_else(|| snapshot.last_error.clone())
}

pub fn phase(phase: &Phase) -> String {
    tr!(match phase {
        Phase::Stopped => "phase.stopped",
        Phase::Starting => "phase.starting",
        Phase::Running => "phase.running",
        Phase::Reloading => "phase.reloading",
        Phase::Stopping => "phase.stopping",
        Phase::Failed => "phase.failed",
    })
}

pub fn mode(mode: &str) -> String {
    match mode {
        "rule" => tr!("mode.rule"),
        "global" => tr!("mode.global"),
        "direct" => tr!("mode.direct"),
        _ => mode.into(),
    }
}

pub fn active(active: bool) -> String {
    tr!(if active {
        "common.active"
    } else {
        "common.off"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn placeholders(text: &str) -> BTreeSet<&str> {
        text.split("%{")
            .skip(1)
            .map(|part| part.split_once('}').expect("unclosed placeholder").0)
            .collect()
    }

    #[test]
    fn catalogues_have_matching_keys_and_placeholders() {
        let en: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(include_str!("../locales/en.json")).unwrap();
        let zh: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(include_str!("../locales/zh-CN.json")).unwrap();
        assert_eq!(en.keys().collect::<Vec<_>>(), zh.keys().collect::<Vec<_>>());
        for (key, value) in &en {
            if key == "_version" {
                continue;
            }
            let english = value.as_str().unwrap();
            let chinese = zh[key].as_str().unwrap();
            assert!(!english.is_empty() && !chinese.is_empty(), "{key}");
            assert_eq!(placeholders(english), placeholders(chinese), "{key}");
            // Test the compiled backend too, not just source resource files.
            assert_eq!(translate_in("en", key, &[]), english, "{key}");
            assert_eq!(translate_in("zh-CN", key, &[]), chinese, "{key}");
        }
    }

    #[test]
    fn linux_locale_priorities_and_fallbacks() {
        for (message_locale, languages, expected) in [
            ("zh_CN.UTF-8", None, "zh-CN"),
            ("zh_SG.utf8", None, "zh-CN"),
            ("zh-Hans-CN", None, "zh-CN"),
            ("en_GB.UTF-8", None, "en"),
            ("de_DE.UTF-8", Some("fr:zh_CN:en"), "zh-CN"),
            ("zh_CN.UTF-8", Some("en:zh_CN"), "en"),
            ("zh_TW.UTF-8", None, "en"),
            ("C.UTF-8", Some("zh_CN"), "en"),
            ("POSIX", Some("zh_CN"), "en"),
            ("fr_FR", Some("de:ja_JP"), "en"),
        ] {
            assert_eq!(select_system_locale(message_locale, languages), expected);
        }
    }

    #[test]
    fn embedded_fallback_and_interpolation_keep_user_values_opaque() {
        assert_eq!(translate_in("unsupported", "nav.overview", &[]), "Overview");
        assert_eq!(translate_in("en", "not.a.key", &[]), "not.a.key");
        let output = translate_in(
            "zh-CN",
            "tui.rates",
            &[
                ("upload", "节点%{download}".into()),
                ("download", "42".into()),
            ],
        );
        assert!(output.contains("节点%{download}"), "{output}");
        assert!(output.contains("42"));
    }

    #[test]
    fn preferences_roundtrip_and_bad_files_are_not_silently_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/frontend.json");
        assert_eq!(load(&path).unwrap().language, Language::Auto);
        save(&path, Language::SimplifiedChinese).unwrap();
        assert_eq!(load(&path).unwrap().language, Language::SimplifiedChinese);
        save(&path, Language::English).unwrap();
        assert_eq!(load(&path).unwrap().language, Language::English);
        std::fs::write(&path, "broken preferences").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "broken preferences"
        );
        assert!(save(&path.join("child"), Language::Auto).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "broken preferences"
        );
    }

    // Only this test changes the process locale; all others use explicit locales.
    #[test]
    fn live_switch_and_diagnostics_preserve_details() {
        let generation_before = generation();
        apply(Language::SimplifiedChinese);
        assert_eq!(language(), Language::SimplifiedChinese);
        assert_eq!(locale(), "zh-CN");
        assert!(generation() > generation_before);
        let plain = Diagnostic::new("error.no_profile", "Import and select a profile first");
        assert_eq!(diagnostic(&plain), tr!("error.no_profile"));
        let error = anyhow::anyhow!("original OS error").context(plain);
        let message = diagnostic(&Diagnostic::from_error(&error));
        assert!(message.contains(&tr!("error.no_profile")));
        assert!(message.contains("original OS error"));
        let unknown = Diagnostic::new("future.error", "keep this raw detail");
        assert_eq!(diagnostic(&unknown), "keep this raw detail");
        apply(Language::English);
        assert_eq!(tr!("nav.settings"), "Settings");
    }
}
