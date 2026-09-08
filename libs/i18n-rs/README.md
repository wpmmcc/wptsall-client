# wptsall-i18n

Centralized internationalization (i18n) library for WPTSALL applications written in Rust.

## Features

- **Unified Translation Management**: Single source of truth for all application translations
- **Multiple Language Support**: English (en) and Simplified Chinese (zh-CN)
- **Easy Integration**: Simple API for backend applications
- **Fallback Support**: Graceful fallback to default language when translation not found
- **File-based Configuration**: JSON locale files for easy maintenance
- **Thread-safe**: Uses `OnceLock` for global instance management

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
wptsall-i18n = { path = "../libs/i18n-rs" }
```

## Quick Start

### 1. Initialize with Files

```rust
use wptsall_i18n::{I18nManager, Language};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize from locales directory
    I18nManager::init("../locales", Language::En)?;
    
    // Use translations
    let manager = I18nManager::get()?;
    let text = manager.translate("common.save", Language::En)?;
    println!("{}", text); // Output: "Save"
    
    Ok(())
}
```

### 2. Initialize with Content (Testing)

```rust
use wptsall_i18n::{I18nManager, Language};

const EN_JSON: &str = r#"{
  "common": {
    "save": "Save",
    "cancel": "Cancel"
  }
}"#;

const ZH_CN_JSON: &str = r#"{
  "common": {
    "save": "保存",
    "cancel": "取消"
  }
}"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    I18nManager::init_with_content(EN_JSON, ZH_CN_JSON, Language::En)?;
    
    let manager = I18nManager::get()?;
    let text = manager.translate("common.save", Language::En)?;
    println!("{}", text); // Output: "Save"
    
    Ok(())
}
```

## API Reference

### Language Enum

```rust
pub enum Language {
    En,      // English
    ZhCn,    // Simplified Chinese
}

impl Language {
    pub fn code(&self) -> &'static str { /* ... */ }
    pub fn from_str(s: &str) -> Result<Self, String> { /* ... */ }
}
```

### I18nManager

#### Initialize

```rust
// From file system
I18nManager::init(locales_path: &str, default_lang: Language) -> Result<()>

// From content strings
I18nManager::init_with_content(
    en_content: &str,
    zh_cn_content: &str,
    default_lang: Language
) -> Result<()>
```

#### Get Instance

```rust
I18nManager::get() -> Result<&'static I18nManager>
```

#### Translate

```rust
// Translate to specific language
manager.translate(key: &str, lang: Language) -> Result<String>

// Translate with fallback to default language
manager.translate_with_fallback(key: &str, lang: Language) -> Result<String>

// Get translation section
manager.get_section(section: &str, lang: Language) -> Result<JsonValue>

// Get all translations for language
manager.get_all(lang: Language) -> Result<JsonValue>
```

#### Metadata

```rust
manager.default_language() -> Language
manager.available_languages() -> Vec<&'static str>
```

### Global Helper Functions

```rust
// Shorthand for I18nManager::get()?.translate()
translate(key: &str, lang: Language) -> Result<String>

// Shorthand with fallback
translate_with_fallback(key: &str, lang: Language) -> Result<String>
```

## Translation Key Format

Keys use dot notation to access nested values:

```json
{
  "common": {
    "save": "Save"
  },
  "errors": {
    "validation": {
      "email": "Invalid email"
    }
  }
}
```

```rust
// Access translations
manager.translate("common.save", Language::En)?;
manager.translate("errors.validation.email", Language::En)?;
```

## Error Handling

```rust
use wptsall_i18n::I18nError;

match manager.translate("key", Language::En) {
    Ok(text) => println!("{}", text),
    Err(I18nError::KeyNotFound(msg)) => eprintln!("Translation not found: {}", msg),
    Err(I18nError::LanguageNotSupported(lang)) => eprintln!("Language not supported: {}", lang),
    Err(e) => eprintln!("Error: {}", e),
}
```

## Integration Examples

### Axum Web Framework

```rust
use axum::{
    extract::{Query},
    Json,
};
use serde::Deserialize;
use wptsall_i18n::{I18nManager, Language};

#[derive(Deserialize)]
pub struct TranslateQuery {
    key: String,
    lang: Option<String>,
}

pub async fn get_translation(
    Query(query): Query<TranslateQuery>,
) -> Json<serde_json::Value> {
    let manager = I18nManager::get().expect("I18n not initialized");
    let lang = Language::from_str(&query.lang.unwrap_or_else(|| "en".to_string()))
        .unwrap_or(Language::En);
    
    match manager.translate(&query.key, lang) {
        Ok(text) => Json(serde_json::json!({ "text": text })),
        Err(e) => Json(serde_json::json!({ "error": e.to_string() })),
    }
}
```

### API Response with i18n

```rust
use wptsall_i18n::{I18nManager, Language};

pub struct ApiResponse {
    pub status: String,
    pub message: String,
}

impl ApiResponse {
    pub fn error(key: &str, lang: Language) -> Self {
        let manager = I18nManager::get().expect("I18n not initialized");
        let message = manager
            .translate_with_fallback(key, lang)
            .unwrap_or_else(|_| "Unknown error".to_string());
        
        ApiResponse {
            status: "error".to_string(),
            message,
        }
    }
}
```

## Testing

The library includes comprehensive tests:

```bash
cd libs/i18n-rs
cargo test
```

Tests cover:
- Language parsing
- Translation initialization
- English/Chinese translations
- Missing key handling
- Fallback behavior
- Section retrieval

## Adding New Languages

1. Create new JSON file in `locales/`:
   ```
   locales/
   ├── en.json
   ├── zh-CN.json
   └── ja.json          # New language
   ```

2. Update `Language` enum:
   ```rust
   pub enum Language {
       En,
       ZhCn,
       Ja,  // New language
   }
   ```

3. Implement language handling:
   ```rust
   impl Language {
       pub fn code(&self) -> &'static str {
           match self {
               Language::En => "en",
               Language::ZhCn => "zh-CN",
               Language::Ja => "ja",  // New language code
           }
       }
       
       pub fn from_str(s: &str) -> Result<Self, String> {
           match s {
               "en" | "en-US" => Ok(Language::En),
               "zh-CN" | "zh-Hans" => Ok(Language::ZhCn),
               "ja" | "ja-JP" => Ok(Language::Ja),  // New language parsing
               _ => Err(format!("Unsupported language: {}", s)),
           }
       }
   }
   ```

4. Update `load_from_content()` and `load()` methods to load the new language file.

## File Structure

```
libs/i18n-rs/
├── Cargo.toml              # Package manifest
├── README.md               # This file
└── src/
    └── lib.rs              # Main library implementation
```

## Dependencies

- `serde` - Serialization framework
- `serde_json` - JSON support
- `thiserror` - Error handling
- `once_cell` - Single-initialization utilities
- `lazy_static` - Lazy static variables

## Performance Considerations

- Translations are loaded once at initialization and cached in memory
- All language codes are `&'static str` references (zero-cost)
- Thread-safe access via `OnceLock` with no runtime locking overhead
- JSON parsing happens only during initialization

## Contributing

When adding new translations:

1. Add keys to `locales/en.json` and `locales/zh-CN.json`
2. Maintain consistent structure (keys in both files)
3. Use kebab-case for multi-word keys
4. Group related translations under sections
5. Update tests if API changes

## License

Same as WPTSALL project

## Related

- Frontend i18n: `/web/source/app/i18n.ts`
- Svelte i18n: `/client-*/src/i18n/`
- Translation files: `/locales/`
