//! Input class and the write target the user asked for.
//!
//! OCR, subtitles, transcripts and extracted document text land in a text
//! slot. They do not replace the source image, video, audio or document
//! bytes unless the field output is an explicit binary replace.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClassDecision {
    pub(crate) class_name: &'static str,
    pub(crate) text_only: bool,
    pub(crate) detail: &'static str,
}

pub(crate) fn decide(content_format: &str, input: Option<&str>, output: Option<&str>) -> ClassDecision {
    let class_name = class_name(content_format, input);
    let text_only = match output.unwrap_or("").trim() {
        "replace" | "attachment" | "binary" => false,
        "text" | "ocr" | "subtitle" | "transcript" | "document_text" => true,
        _ => !is_media_format(content_format),
    };
    let detail = if text_only {
        match class_name {
            "image" => "ocr_text",
            "video" => "subtitle_text",
            "audio" => "transcript_text",
            "document" => "document_text",
            _ => "text_field",
        }
    } else {
        "replace_binary"
    };
    ClassDecision {
        class_name,
        text_only,
        detail,
    }
}

fn class_name(content_format: &str, input: Option<&str>) -> &'static str {
    match input.unwrap_or("").trim() {
        "image" => "image",
        "video" => "video",
        "audio" => "audio",
        "document" => "document",
        "text" => "text",
        _ => match content_format {
            "image" | "image_ref" => "image",
            "video" | "video_ref" => "video",
            "audio" | "audio_ref" => "audio",
            "document" | "document_ref" => "document",
            _ => "text",
        },
    }
}

fn is_media_format(content_format: &str) -> bool {
    matches!(
        content_format,
        "media_ref" | "image" | "image_ref" | "video" | "video_ref" | "audio" | "audio_ref"
            | "document" | "document_ref"
    )
}

/// Text intents consume the mock/provider text product. A file URL is not
/// accepted as OCR, subtitle, transcript, or document text.
pub(crate) fn text_product<'a>(translated_text: &'a str, translated_ref: &str) -> Option<&'a str> {
    let text = translated_text.trim();
    if text.is_empty() || text == translated_ref.trim() {
        None
    } else {
        Some(text)
    }
}

pub(crate) fn field_class_tokens(
    capabilities: &serde_json::Value,
    field_name: &str,
) -> (Option<String>, Option<String>) {
    let Some(value) = capabilities.get(field_name) else {
        return (None, None);
    };
    let Some(object) = value.as_object() else {
        return (None, None);
    };
    let input = object
        .get("input")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let output = object
        .get("output")
        .or_else(|| object.get("intent"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    (input, output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_classes_write_text_unless_the_user_replaces_the_file() {
        let text = decide("plain_text", Some("text"), None);
        assert_eq!(text.class_name, "text");
        assert!(text.text_only);

        let ocr = decide("media_ref", Some("image"), Some("ocr"));
        assert_eq!(ocr.detail, "ocr_text");
        assert!(ocr.text_only);

        let subtitle = decide("media_ref", Some("video"), Some("subtitle"));
        assert_eq!(subtitle.detail, "subtitle_text");
        assert!(subtitle.text_only);

        let transcript = decide("media_ref", Some("audio"), Some("transcript"));
        assert_eq!(transcript.detail, "transcript_text");
        assert!(transcript.text_only);

        let document = decide("media_ref", Some("document"), Some("document_text"));
        assert_eq!(document.detail, "document_text");
        assert!(document.text_only);

        let replace = decide("media_ref", Some("image"), Some("replace"));
        assert!(!replace.text_only);
        assert_eq!(replace.detail, "replace_binary");
    }

    #[test]
    fn text_intent_rejects_the_file_url() {
        let url = "https://cdn.example/a-zh.png";
        assert_eq!(
            text_product("【zh】ocr:a.png【/zh】", url),
            Some("【zh】ocr:a.png【/zh】")
        );
        assert!(text_product(url, url).is_none());
        assert!(text_product("  ", url).is_none());
    }

    #[test]
    fn unspecified_media_ref_stays_on_the_binary_mapping() {
        let legacy = decide("media_ref", None, None);
        assert!(!legacy.text_only);
    }
}
