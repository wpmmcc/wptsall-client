//! Vendor-accurate language code tables and validation rules.
//!
//! Enforces T-MF-03: Illegal or non-standard language codes (e.g. `en_US` sent to DeepL
//! or `zh_CN` sent to Baidu) return realistic vendor-specific error responses rather than
//! being permissively translated.

/// Baidu Translate VIP language codes.
/// Baidu uses specific short codes (e.g. `zh` for Simplified Chinese, `cht` for Traditional,
/// `yue` for Cantonese). It strictly rejects compound codes like `zh_CN`, `zh-CN`, `en_US`.
pub fn is_valid_baidu_code(code: &str) -> bool {
    matches!(
        code,
        "auto"
            | "zh"
            | "en"
            | "yue"
            | "wyw"
            | "jp"
            | "kor"
            | "fra"
            | "spa"
            | "th"
            | "ara"
            | "ru"
            | "pt"
            | "de"
            | "it"
            | "el"
            | "nl"
            | "pl"
            | "bul"
            | "est"
            | "dan"
            | "fin"
            | "cs"
            | "rom"
            | "slo"
            | "swe"
            | "hu"
            | "cht"
            | "vie"
    )
}

/// DeepL API target languages (must be UPPERCASE).
/// DeepL strictly distinguishes between `EN-GB`/`EN-US` or `EN`, `PT-PT`/`PT-BR` or `PT`,
/// `ZH` or `ZH-HANS`/`ZH-HANT`. Lowercase or underscore codes like `en_US` or `zh_CN` are rejected.
pub fn is_valid_deepl_target(code: &str) -> bool {
    matches!(
        code,
        "BG" | "CS"
            | "DA"
            | "DE"
            | "EL"
            | "EN-GB"
            | "EN-US"
            | "EN"
            | "ES"
            | "ET"
            | "FI"
            | "FR"
            | "HU"
            | "ID"
            | "IT"
            | "JA"
            | "KO"
            | "LT"
            | "LV"
            | "NB"
            | "NL"
            | "PL"
            | "PT-BR"
            | "PT-PT"
            | "PT"
            | "RO"
            | "RU"
            | "SK"
            | "SL"
            | "SV"
            | "TR"
            | "UK"
            | "ZH"
            | "ZH-HANS"
            | "ZH-HANT"
    )
}

/// DeepL API source languages (must be UPPERCASE).
pub fn is_valid_deepl_source(code: &str) -> bool {
    matches!(
        code,
        "BG" | "CS"
            | "DA"
            | "DE"
            | "EL"
            | "EN"
            | "ES"
            | "ET"
            | "FI"
            | "FR"
            | "HU"
            | "ID"
            | "IT"
            | "JA"
            | "KO"
            | "LT"
            | "LV"
            | "NB"
            | "NL"
            | "PL"
            | "PT"
            | "RO"
            | "RU"
            | "SK"
            | "SL"
            | "SV"
            | "TR"
            | "UK"
            | "ZH"
    )
}

/// Google Cloud Translation v2/v3 language codes (BCP-47).
/// Rejects underscore formats like `zh_CN` or `en_US` (must be hyphenated `zh-CN` or pure `en`).
pub fn is_valid_google_lang(code: &str) -> bool {
    if code.contains('_') || code.is_empty() {
        return false;
    }
    matches!(
        code,
        "af" | "sq"
            | "am"
            | "ar"
            | "hy"
            | "az"
            | "eu"
            | "be"
            | "bn"
            | "bs"
            | "bg"
            | "ca"
            | "ceb"
            | "zh-CN"
            | "zh-TW"
            | "zh"
            | "co"
            | "hr"
            | "cs"
            | "da"
            | "nl"
            | "en"
            | "eo"
            | "et"
            | "fi"
            | "fr"
            | "fy"
            | "gl"
            | "ka"
            | "de"
            | "el"
            | "gu"
            | "ht"
            | "ha"
            | "haw"
            | "he"
            | "hi"
            | "hmn"
            | "hu"
            | "is"
            | "ig"
            | "id"
            | "ga"
            | "it"
            | "ja"
            | "jv"
            | "kn"
            | "kk"
            | "km"
            | "rw"
            | "ko"
            | "ku"
            | "ky"
            | "lo"
            | "la"
            | "lv"
            | "lt"
            | "lb"
            | "mk"
            | "mg"
            | "ms"
            | "ml"
            | "mt"
            | "mi"
            | "mr"
            | "mn"
            | "my"
            | "ne"
            | "no"
            | "ny"
            | "or"
            | "ps"
            | "fa"
            | "pl"
            | "pt"
            | "pa"
            | "ro"
            | "ru"
            | "sm"
            | "gd"
            | "sr"
            | "st"
            | "sn"
            | "sd"
            | "si"
            | "sk"
            | "sl"
            | "so"
            | "es"
            | "su"
            | "sw"
            | "sv"
            | "tl"
            | "tg"
            | "ta"
            | "tt"
            | "te"
            | "th"
            | "tr"
            | "tk"
            | "uk"
            | "ur"
            | "ug"
            | "uz"
            | "vi"
            | "cy"
            | "xh"
            | "yi"
            | "yo"
            | "zu"
    )
}

/// Youdao Translate v3 language codes.
/// Official docs specify `zh-CHS`, but real-world commercial testing proves Youdao
/// accepts `zh_CN`, `zh-CN`, `en_US`, and `en-US` aliases seamlessly.
pub fn is_valid_youdao_code(code: &str) -> bool {
    matches!(
        code,
        "auto"
            | "zh-CHS"
            | "zh-CHT"
            | "zh"
            | "zh_CN"
            | "zh-CN"
            | "en"
            | "en_US"
            | "en-US"
            | "ja"
            | "ko"
            | "fr"
            | "es"
            | "pt"
            | "it"
            | "ru"
            | "vi"
            | "de"
            | "ar"
            | "id"
            | "af"
            | "bs"
            | "bg"
            | "ca"
            | "hr"
            | "cs"
            | "da"
            | "nl"
            | "et"
            | "fj"
            | "fi"
            | "el"
            | "ht"
            | "he"
            | "hi"
            | "mww"
            | "hu"
            | "sw"
            | "tlh"
            | "lv"
            | "lt"
            | "ms"
            | "mt"
            | "no"
            | "fa"
            | "pl"
            | "ro"
            | "sk"
            | "sl"
            | "sv"
            | "ty"
            | "th"
            | "to"
            | "tr"
            | "uk"
            | "ur"
            | "cy"
            | "yua"
    )
}

/// Tencent Cloud TMT language codes.
/// Rejects `zh_CN`, `en_US`.
pub fn is_valid_tencent_code(code: &str) -> bool {
    matches!(
        code,
        "auto"
            | "zh"
            | "zh-TW"
            | "en"
            | "ja"
            | "ko"
            | "fr"
            | "es"
            | "it"
            | "de"
            | "tr"
            | "ru"
            | "pt"
            | "vi"
            | "id"
            | "th"
            | "ms"
            | "ar"
            | "hi"
    )
}

/// AWS Translate language codes.
/// Rejects `zh_CN`, `en_US`.
pub fn is_valid_aws_code(code: &str) -> bool {
    matches!(
        code,
        "auto"
            | "af"
            | "sq"
            | "am"
            | "ar"
            | "hy"
            | "az"
            | "bn"
            | "bs"
            | "bg"
            | "ca"
            | "zh"
            | "zh-TW"
            | "hr"
            | "cs"
            | "da"
            | "fa-AF"
            | "nl"
            | "en"
            | "et"
            | "fa"
            | "tl"
            | "fi"
            | "fr"
            | "fr-CA"
            | "ka"
            | "de"
            | "el"
            | "gu"
            | "ht"
            | "ha"
            | "he"
            | "hi"
            | "hu"
            | "is"
            | "id"
            | "ga"
            | "it"
            | "ja"
            | "kn"
            | "kk"
            | "ko"
            | "lv"
            | "lt"
            | "mk"
            | "ms"
            | "ml"
            | "mt"
            | "mr"
            | "mn"
            | "no"
            | "ps"
            | "pl"
            | "pt"
            | "pt-PT"
            | "pa"
            | "ro"
            | "ru"
            | "sr"
            | "si"
            | "sk"
            | "sl"
            | "so"
            | "es"
            | "es-MX"
            | "sw"
            | "sv"
            | "ta"
            | "te"
            | "th"
            | "tr"
            | "uk"
            | "ur"
            | "uz"
            | "vi"
            | "cy"
    )
}

/// Azure Translator target language codes.
/// Rejects `zh_CN` (must be `zh-Hans` or `zh-Hant`) and `en_US` (must be `en`).
pub fn is_valid_azure_target(code: &str) -> bool {
    if code.contains('_') {
        return false;
    }
    matches!(
        code,
        "af" | "sq"
            | "am"
            | "ar"
            | "hy"
            | "as"
            | "az"
            | "bn"
            | "ba"
            | "eu"
            | "bho"
            | "brx"
            | "bs"
            | "bg"
            | "yue"
            | "ca"
            | "lzh"
            | "zh-Hans"
            | "zh-Hant"
            | "zh"
            | "sn"
            | "hr"
            | "cs"
            | "da"
            | "prs"
            | "dv"
            | "doi"
            | "nl"
            | "en"
            | "et"
            | "fo"
            | "fj"
            | "fil"
            | "fi"
            | "fr"
            | "fr-CA"
            | "gl"
            | "ka"
            | "de"
            | "el"
            | "gu"
            | "ht"
            | "ha"
            | "he"
            | "hi"
            | "mww"
            | "hu"
            | "is"
            | "ig"
            | "id"
            | "ikt"
            | "iu"
            | "iu-Latn"
            | "ga"
            | "it"
            | "ja"
            | "kn"
            | "ks"
            | "kk"
            | "km"
            | "rw"
            | "tlh-Latn"
            | "tlh-Piqd"
            | "gom"
            | "ko"
            | "ku"
            | "kmr"
            | "ky"
            | "lo"
            | "lv"
            | "lt"
            | "lug"
            | "mk"
            | "mai"
            | "mg"
            | "ms"
            | "ml"
            | "mt"
            | "mi"
            | "mr"
            | "mn-Cyrl"
            | "mn-Mong"
            | "my"
            | "ne"
            | "nb"
            | "nya"
            | "or"
            | "ps"
            | "fa"
            | "pl"
            | "pt"
            | "pt-PT"
            | "pa"
            | "otq"
            | "ro"
            | "run"
            | "ru"
            | "sm"
            | "sa"
            | "sr-Cyrl"
            | "sr-Latn"
            | "st"
            | "nso"
            | "tn"
            | "sd"
            | "si"
            | "sk"
            | "sl"
            | "so"
            | "es"
            | "sw"
            | "sv"
            | "ty"
            | "ta"
            | "tt"
            | "te"
            | "th"
            | "bo"
            | "ti"
            | "to"
            | "tr"
            | "tk"
            | "uk"
            | "ur"
            | "ug"
            | "uz"
            | "vi"
            | "cy"
            | "xh"
            | "yo"
            | "yua"
            | "zu"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_baidu_language_validation() {
        assert!(is_valid_baidu_code("zh"));
        assert!(is_valid_baidu_code("en"));
        assert!(is_valid_baidu_code("auto"));
        assert!(!is_valid_baidu_code("zh_CN"));
        assert!(!is_valid_baidu_code("en_US"));
    }

    #[test]
    fn test_deepl_language_validation() {
        assert!(is_valid_deepl_target("ZH"));
        assert!(is_valid_deepl_target("EN-US"));
        assert!(!is_valid_deepl_target("en_US"));
        assert!(!is_valid_deepl_target("zh_CN"));
        assert!(!is_valid_deepl_target("zh")); // lowercase rejected
    }

    #[test]
    fn test_youdao_language_validation() {
        assert!(is_valid_youdao_code("zh-CHS"));
        assert!(is_valid_youdao_code("en"));
        assert!(is_valid_youdao_code("zh_CN")); // Real-world verified tolerance
        assert!(is_valid_youdao_code("en_US"));
        assert!(!is_valid_youdao_code("invalid_youdao_lang"));
    }

    #[test]
    fn test_google_language_validation() {
        assert!(is_valid_google_lang("zh-CN"));
        assert!(is_valid_google_lang("en"));
        assert!(!is_valid_google_lang("zh_CN")); // underscore rejected
        assert!(!is_valid_google_lang("en_US"));
    }

    #[test]
    fn test_tencent_and_aws_validation() {
        assert!(is_valid_tencent_code("zh"));
        assert!(!is_valid_tencent_code("zh_CN"));
        assert!(is_valid_aws_code("zh"));
        assert!(!is_valid_aws_code("zh_CN"));
        assert!(is_valid_azure_target("zh-Hans"));
        assert!(!is_valid_azure_target("zh_CN"));
    }
}
