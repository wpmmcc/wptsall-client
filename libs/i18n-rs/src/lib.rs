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

        let keys: Vec<&str> = key.split('.').collect();
        let mut current = translations;

        for (i, k) in keys.iter().enumerate() {
            match current.get(k) {
                Some(value) => {
                    if i == keys.len() - 1 {
                        // Last key - should be a string
                        return value
                            .as_str()
                            .ok_or_else(|| {
                                I18nError::KeyNotFound(format!(
                                    "Translation value is not a string: {}",
                                    key
                                ))
                            })
                            .map(|s| s.to_string());
                    } else {
                        // Intermediate key - should be an object
                        current = value;
                    }
                }
                None => {
                    return Err(I18nError::KeyNotFound(format!(
                        "Translation key not found: {} (missing: {})",
                        key, k
                    )));
                }
            }
        }

        Err(I18nError::KeyNotFound(key.to_string()))
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

#[cfg(test)]
mod tests {
    use super::*;

    const EN_JSON: &str = r#"{
      "common": {
        "save": "Save",
        "cancel": "Cancel",
        "delete": "Delete"
      },
      "login": {
        "prompt": "Login"
      }
    }"#;

    const ZH_CN_JSON: &str = r#"{
      "common": {
        "save": "保存",
        "cancel": "取消",
        "delete": "删除"
      },
      "login": {
        "prompt": "登录"
      }
    }"#;

    #[test]
    fn test_language_from_str() {
        assert_eq!(Language::from_str("en").unwrap(), Language::En);
        assert_eq!(Language::from_str("en-US").unwrap(), Language::En);
        assert_eq!(Language::from_str("zh-CN").unwrap(), Language::ZhCn);
        assert_eq!(Language::from_str("zh-Hans").unwrap(), Language::ZhCn);
        assert!(Language::from_str("fr").is_err());
    }

    #[test]
    fn test_i18n_initialization() {
        let _manager = I18nManager::load_from_content(EN_JSON, ZH_CN_JSON, Language::En)
            .expect("Failed to load translations");
    }

    #[test]
    fn test_translate_english() {
        let manager = I18nManager::load_from_content(EN_JSON, ZH_CN_JSON, Language::En)
            .expect("Failed to load translations");

        assert_eq!(
            manager.translate("common.save", Language::En).unwrap(),
            "Save"
        );
        assert_eq!(
            manager.translate("common.cancel", Language::En).unwrap(),
            "Cancel"
        );
    }

    #[test]
    fn test_translate_chinese() {
        let manager = I18nManager::load_from_content(EN_JSON, ZH_CN_JSON, Language::ZhCn)
            .expect("Failed to load translations");

        assert_eq!(
            manager.translate("common.save", Language::ZhCn).unwrap(),
            "保存"
        );
        assert_eq!(
            manager.translate("common.cancel", Language::ZhCn).unwrap(),
            "取消"
        );
    }

    #[test]
    fn test_missing_key() {
        let manager = I18nManager::load_from_content(EN_JSON, ZH_CN_JSON, Language::En)
            .expect("Failed to load translations");

        match manager.translate("nonexistent.key", Language::En) {
            Err(I18nError::KeyNotFound(_)) => {}
            _ => panic!("Expected KeyNotFound error"),
        }
    }

    #[test]
    fn test_fallback() {
        let manager = I18nManager::load_from_content(EN_JSON, ZH_CN_JSON, Language::En)
            .expect("Failed to load translations");

        // Key exists in both, should return Chinese
        assert_eq!(
            manager
                .translate_with_fallback("common.save", Language::ZhCn)
                .unwrap(),
            "保存"
        );
    }

    #[test]
    fn test_get_section() {
        let manager = I18nManager::load_from_content(EN_JSON, ZH_CN_JSON, Language::En)
            .expect("Failed to load translations");

        let common_section = manager
            .get_section("common", Language::En)
            .expect("Failed to get section");
        assert!(common_section.is_object());
        assert_eq!(
            common_section.get("save").unwrap().as_str().unwrap(),
            "Save"
        );
    }
}
