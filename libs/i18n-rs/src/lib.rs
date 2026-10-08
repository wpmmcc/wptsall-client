//! WPTSALL Internationalization (i18n) Library
//!
//! Centralized translation management for all WPTSALL applications.
//! Provides unified access to translations stored in JSON locale files.

use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use thiserror::Error;

/// Supported language codes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    /// English
    En,
    /// Simplified Chinese
    ZhCn,
}

impl Language {
    /// Get the language code string
    pub fn code(&self) -> &'static str {
        match self {
            Language::En => "en",
            Language::ZhCn => "zh-CN",
        }
    }

    /// Parse from string
    pub fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "en" | "en-US" | "en-GB" => Ok(Language::En),
            "zh-CN" | "zh-Hans" => Ok(Language::ZhCn),
            _ => Err(format!("Unsupported language: {}", s)),
        }
    }
}

/// Translation error types
#[derive(Error, Debug)]
pub enum I18nError {
    #[error("Translation not found: {0}")]
    KeyNotFound(String),

    #[error("Language not supported: {0}")]
    LanguageNotSupported(String),

    #[error("Failed to parse translation file: {0}")]
    ParseError(String),

    #[error("Failed to read translation file: {0}")]
    ReadError(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),
}

type I18nResult<T> = std::result::Result<T, I18nError>;

/// Main translation manager
#[derive(Debug, Clone)]
pub struct I18nManager {
    translations: HashMap<String, JsonValue>,
    default_language: Language,
}

static I18N_INSTANCE: OnceLock<I18nManager> = OnceLock::new();

/// Resolve a dotted key against one language's catalog.
///
/// Two catalog styles are supported (2026-09-12, resolving the flat/nested
/// mismatch documented in the client's main.rs):
///
/// 1. Nested objects (documented example: `"common.save"` →
///    `{"common": {"save": "..."}}`). The nested walk runs first, so a
///    collision between both styles resolves to the nested entry.
/// 2. Flat dotted keys stored as literal object members
///    (`{"app.title": "..."}`). The embedded client catalogs
///    (`client-wpplugin/source/locales/*.json`) use this style; without
///    the fallback every dotted lookup against them failed with
///    KeyNotFound.
fn translate_in(translations: &JsonValue, key: &str) -> I18nResult<String> {
    let keys: Vec<&str> = key.split('.').collect();
    let mut current = translations;

    for (i, k) in keys.iter().enumerate() {
        match current.get(k) {
            Some(value) => {
                if i == keys.len() - 1 {
                    if let Some(text) = value.as_str() {
                        return Ok(text.to_string());
                    }
                    // Last segment resolved to a non-string (or an object
                    // when the flat style was intended) — try the flat
                    // lookup below before giving up.
                    break;
                }
                current = value;
            }
            None => {
                // Segment missing in the nested walk — the catalog may be
                // flat; try the whole dotted key as a literal member.
                break;
            }
        }
    }

    translations
        .get(key)
        .and_then(|value| value.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| I18nError::KeyNotFound(format!("Translation key not found: {}", key)))
}

impl I18nManager {
    /// Initialize the i18n manager with translation files
    ///
    /// # Arguments
    ///
    /// * `locales_path` - Path to the locales directory containing JSON translation files
    /// * `default_lang` - Default language to use
    ///
    /// # Example
    ///
    /// ```ignore
    /// I18nManager::init("./locales", Language::En)?;
    /// ```
    pub fn init(locales_path: &str, default_lang: Language) -> I18nResult<()> {
        let manager = Self::load(locales_path, default_lang)?;
        I18N_INSTANCE
            .set(manager)
            .map_err(|_| I18nError::ReadError("Already initialized".to_string()))?;
        Ok(())
    }

    /// Initialize with both JSON content and default language
    pub fn init_with_content(
        en_content: &str,
        zh_cn_content: &str,
        default_lang: Language,
    ) -> I18nResult<()> {
        let manager = Self::load_from_content(en_content, zh_cn_content, default_lang)?;
        I18N_INSTANCE
            .set(manager)
            .map_err(|_| I18nError::ReadError("Already initialized".to_string()))?;
        Ok(())
    }

    /// Get the global i18n instance
    pub fn get() -> I18nResult<&'static I18nManager> {
        I18N_INSTANCE
            .get()
            .ok_or_else(|| I18nError::ReadError("I18n not initialized".to_string()))
    }

    /// Load translations from files
    fn load(locales_path: &str, default_lang: Language) -> I18nResult<Self> {
        let path = Path::new(locales_path);
        let mut translations = HashMap::new();

        // Load English translations
        let en_path = path.join("en.json");
        let en_content = std::fs::read_to_string(&en_path)
            .map_err(|e| I18nError::ReadError(format!("Failed to read en.json: {}", e)))?;
        let en_json: JsonValue = serde_json::from_str(&en_content)
            .map_err(|e| I18nError::ParseError(format!("Failed to parse en.json: {}", e)))?;
        translations.insert(Language::En.code().to_string(), en_json);

        // Load Chinese translations
        let zh_cn_path = path.join("zh-CN.json");
        let zh_cn_content = std::fs::read_to_string(&zh_cn_path)
            .map_err(|e| I18nError::ReadError(format!("Failed to read zh-CN.json: {}", e)))?;
        let zh_cn_json: JsonValue = serde_json::from_str(&zh_cn_content)
            .map_err(|e| I18nError::ParseError(format!("Failed to parse zh-CN.json: {}", e)))?;
        translations.insert(Language::ZhCn.code().to_string(), zh_cn_json);

        Ok(I18nManager {
            translations,
            default_language: default_lang,
        })
    }

    /// Load translations from JSON content strings
    fn load_from_content(
        en_content: &str,
        zh_cn_content: &str,
        default_lang: Language,
    ) -> I18nResult<Self> {
        let mut translations = HashMap::new();

        let en_json: JsonValue = serde_json::from_str(en_content)
            .map_err(|e| I18nError::ParseError(format!("Failed to parse en.json: {}", e)))?;
        translations.insert(Language::En.code().to_string(), en_json);

        let zh_cn_json: JsonValue = serde_json::from_str(zh_cn_content)
            .map_err(|e| I18nError::ParseError(format!("Failed to parse zh-CN.json: {}", e)))?;
        translations.insert(Language::ZhCn.code().to_string(), zh_cn_json);

        Ok(I18nManager {
            translations,
            default_language: default_lang,
        })
    }

    /// Translate a key in the specified language
    ///
    /// # Arguments
    ///
    /// * `key` - Dot-separated translation key (e.g., "common.save")
    /// * `lang` - Language to translate to
    ///
    /// # Example
    ///
    /// ```ignore
    /// let text = I18nManager::get()?.translate("common.save", Language::En)?;
    /// ```
    pub fn translate(&self, key: &str, lang: Language) -> I18nResult<String> {
        let lang_code = lang.code();
        let translations = self
            .translations
            .get(lang_code)
            .ok_or_else(|| I18nError::LanguageNotSupported(lang_code.to_string()))?;

        translate_in(translations, key)
    }

    /// Translate with fallback to default language if key not found
    pub fn translate_with_fallback(&self, key: &str, lang: Language) -> I18nResult<String> {
        match self.translate(key, lang) {
            Ok(text) => Ok(text),
            Err(I18nError::KeyNotFound(_)) if lang != self.default_language => {
                self.translate(key, self.default_language)
            }
            Err(e) => Err(e),
        }
    }

    /// Get the default language
    pub fn default_language(&self) -> Language {
        self.default_language
    }

    /// Get all available language codes
    pub fn available_languages(&self) -> Vec<&'static str> {
        self.translations
            .keys()
            .map(|k| {
                if k == "en" {
                    Language::En.code()
                } else {
                    Language::ZhCn.code()
                }
            })
            .collect()
    }

    /// Get a complete translation section as JSON
    ///
    /// # Example
    ///
    /// ```ignore
    /// let common_section = I18nManager::get()?.get_section("common", Language::En)?;
    /// ```
    pub fn get_section(&self, section: &str, lang: Language) -> I18nResult<JsonValue> {
        let lang_code = lang.code();
        let translations = self
            .translations
            .get(lang_code)
            .ok_or_else(|| I18nError::LanguageNotSupported(lang_code.to_string()))?;

        translations
            .get(section)
            .cloned()
            .ok_or_else(|| I18nError::KeyNotFound(format!("Section not found: {}", section)))
    }

    /// Get all translations for a language
    pub fn get_all(&self, lang: Language) -> I18nResult<JsonValue> {
        let lang_code = lang.code();
        self.translations
            .get(lang_code)
            .cloned()
            .ok_or_else(|| I18nError::LanguageNotSupported(lang_code.to_string()))
    }
}

/// Global translation helper function
///
/// # Example
///
/// ```ignore
/// let text = translate("common.save", Language::En)?;
/// ```
pub fn translate(key: &str, lang: Language) -> I18nResult<String> {
    I18nManager::get()?.translate(key, lang)
}

/// Global translation helper with fallback
pub fn translate_with_fallback(key: &str, lang: Language) -> I18nResult<String> {
    I18nManager::get()?.translate_with_fallback(key, lang)
}
