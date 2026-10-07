//! Structure and interpolation tokens are not translation content.

use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;

pub(crate) fn is_technical_key(key: &str) -> bool {
    let key = key.rsplit('\0').next().unwrap_or(key);
    let lower = key.to_ascii_lowercase();
    let normalized: String = lower
        .chars()
        .filter(|ch| !matches!(ch, '_' | '-'))
        .collect();
    matches!(
        normalized.as_str(),
        "id" | "uuid"
            | "guid"
            | "key"
            | "slug"
            | "sku"
            | "ref"
            | "$ref"
            | "@id"
            | "reference"
            | "references"
            | "schema"
            | "$schema"
            | "@context"
            | "version"
            | "type"
            | "@type"
            | "eltype"
            | "widgettype"
            | "blockname"
            | "mime"
            | "mimetype"
            | "contenttype"
            | "url"
            | "uri"
            | "href"
            | "src"
            | "path"
            | "filename"
            | "classname"
            | "class"
            | "cssclass"
            | "cssclasses"
            | "style"
            | "styles"
            | "selector"
            | "htmltag"
            | "tagname"
            | "tag"
            | "icon"
            | "lang"
            | "language"
            | "locale"
    ) || lower.ends_with("_id")
        || lower.ends_with("_ids")
        || key.ends_with("Id")
        || key.ends_with("ID")
        || lower.ends_with("_url")
        || key.ends_with("Url")
        || key.ends_with("URL")
}

pub(crate) fn is_technical_string(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return true;
    }
    if let Ok(url) = url::Url::parse(trimmed) {
        if matches!(
            url.scheme(),
            "http"
                | "https"
                | "ftp"
                | "file"
                | "s3"
                | "gs"
                | "oss"
                | "mailto"
                | "tel"
                | "data"
                | "urn"
        ) {
            return true;
        }
    }
    (trimmed.starts_with('#') && !trimmed.chars().any(char::is_whitespace))
        || trimmed.starts_with("//")
        || trimmed.parse::<f64>().is_ok()
}

pub(crate) struct PhpString<'a> {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) value: &'a str,
    pub(crate) ordinal: usize,
    value_id: usize,
}

struct PhpValue {
    object: bool,
    preserve: bool,
    children: Vec<usize>,
}

struct PhpParser<'a> {
    source: &'a str,
    pos: usize,
    ordinal: usize,
    values: Vec<PhpValue>,
    strings: Vec<PhpString<'a>>,
}

pub(crate) fn php_translatable_strings(source: &str) -> Result<Vec<PhpString<'_>>> {
    if !["a:", "O:", "s:", "N;", "b:", "i:", "d:", "C:", "R:", "r:"]
        .iter()
        .any(|prefix| source.starts_with(prefix))
    {
        bail!("does not look like PHP serialized data");
    }
    let mut parser = PhpParser {
        source,
        pos: 0,
        ordinal: 0,
        values: Vec::new(),
        strings: Vec::new(),
    };
    parser.value(0, false)?;
    if parser.pos != source.len() {
        bail!(
            "invalid serialized_php: trailing data at byte {}",
            parser.pos
        );
    }
    // References can point backwards into otherwise readable content. Protect
    // the referenced graph too: changing "title" must not change an aliased id.
    let mut preserved = vec![false; parser.values.len()];
    let mut pending: Vec<_> = parser
        .values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| value.preserve.then_some(index + 1))
        .collect();
    while let Some(id) = pending.pop() {
        if !preserved[id - 1] {
            preserved[id - 1] = true;
            pending.extend(&parser.values[id - 1].children);
        }
    }
    parser
        .strings
        .retain(|string| !preserved[string.value_id - 1]);
    Ok(parser.strings)
}

impl<'a> PhpParser<'a> {
    fn take(&mut self, expected: &[u8]) -> Result<()> {
        if !self.source.as_bytes()[self.pos..].starts_with(expected) {
            bail!("invalid serialized_php token at byte {}", self.pos);
        }
        self.pos += expected.len();
        Ok(())
    }

    fn size(&mut self) -> Result<usize> {
        let start = self.pos;
        while self
            .source
            .as_bytes()
            .get(self.pos)
            .is_some_and(u8::is_ascii_digit)
        {
            self.pos += 1;
        }
        self.source[start..self.pos]
            .parse()
            .map_err(|_| anyhow!("invalid serialized_php length/count at byte {start}"))
    }

    fn raw_string(&mut self) -> Result<&'a str> {
        let size = self.size()?;
        self.take(b":\"")?;
        let end = self
            .pos
            .checked_add(size)
            .filter(|end| *end <= self.source.len())
            .ok_or_else(|| anyhow!("invalid serialized_php string length at byte {}", self.pos))?;
        let value = self.source.get(self.pos..end).ok_or_else(|| {
            anyhow!(
                "invalid serialized_php UTF-8 byte length at byte {}",
                self.pos
            )
        })?;
        self.pos = end;
        self.take(b"\"")?;
        Ok(value)
    }

    fn scalar(&mut self, kind: u8) -> Result<()> {
        self.pos += 1;
        self.take(b":")?;
        let start = self.pos;
        let end = self.source[start..]
            .find(';')
            .map(|offset| start + offset)
            .ok_or_else(|| anyhow!("invalid serialized_php scalar at byte {start}"))?;
        let raw = &self.source[start..end];
        let valid = match kind {
            b'b' => matches!(raw, "0" | "1"),
            b'i' => raw.parse::<i64>().is_ok(),
            b'd' => {
                matches!(raw, "NAN" | "INF" | "-INF")
                    || (raw.parse::<f64>().is_ok()
                        && raw
                            .bytes()
                            .all(|ch| ch.is_ascii_digit() || b"+-.eE".contains(&ch)))
            }
            _ => false,
        };
        if !valid {
            bail!("invalid serialized_php scalar at byte {start}");
        }
        self.pos = end + 1;
        Ok(())
    }

    fn reference(&mut self, kind: u8) -> Result<usize> {
        self.pos += 1;
        self.take(b":")?;
        let start = self.pos;
        let id = self.size()?;
        let limit = self.values.len().saturating_sub(usize::from(kind == b'r'));
        if id == 0 || id > limit || (kind == b'r' && !self.values[id - 1].object) {
            bail!("invalid serialized_php reference at byte {start}");
        }
        self.take(b";")?;
        Ok(id)
    }

    fn key(&mut self, object: bool) -> Result<Option<&'a str>> {
        match self.source.as_bytes().get(self.pos) {
            Some(b's') => {
                self.take(b"s:")?;
                let key = self.raw_string()?;
                self.take(b";")?;
                self.ordinal += 1;
                Ok(Some(key))
            }
            Some(b'i') if !object => {
                self.scalar(b'i')?;
                Ok(None)
            }
            _ => bail!("invalid serialized_php key at byte {}", self.pos),
        }
    }

    fn value(&mut self, depth: usize, preserve: bool) -> Result<usize> {
        if depth > 128 {
            bail!("serialized_php nesting too deep");
        }
        let kind = self.source.as_bytes().get(self.pos).copied();
        if kind == Some(b'R') {
            // PHP's hard reference does not allocate another value index.
            let id = self.reference(b'R')?;
            self.values[id - 1].preserve |= preserve;
            return Ok(id);
        }
        self.values.push(PhpValue {
            object: matches!(kind, Some(b'O' | b'C' | b'r')),
            preserve,
            children: Vec::new(),
        });
        let id = self.values.len();
        match kind {
            Some(b'N') => self.take(b"N;"),
            Some(kind @ (b'b' | b'i' | b'd')) => self.scalar(kind),
            Some(b'r') => {
                let target = self.reference(b'r')?;
                self.values[id - 1].children.push(target);
                Ok(())
            }
            Some(b's') => {
                let start = self.pos;
                self.take(b"s:")?;
                let value = self.raw_string()?;
                self.take(b";")?;
                // Count keys too, retaining the existing php_s:N resume identity.
                self.ordinal += 1;
                if !preserve && !is_technical_string(value) {
                    self.strings.push(PhpString {
                        start,
                        end: self.pos,
                        value,
                        ordinal: self.ordinal,
                        value_id: id,
                    });
                }
                Ok(())
            }
            Some(kind @ (b'a' | b'O')) => {
                self.pos += 1;
                self.take(b":")?;
                if kind == b'O' {
                    self.raw_string()?;
                    self.take(b":")?;
                }
                let count = self.size()?;
                self.take(b":{")?;
                if count > self.source.len().saturating_sub(self.pos) {
                    bail!("invalid serialized_php container count");
                }
                for _ in 0..count {
                    let key = self.key(kind == b'O')?;
                    let child =
                        self.value(depth + 1, preserve || key.is_some_and(is_technical_key))?;
                    self.values[id - 1].children.push(child);
                }
                self.take(b"}")
            }
            Some(b'C') => {
                // A custom serializer's private payload is opaque, not string tokens.
                self.take(b"C:")?;
                self.raw_string()?;
                self.take(b":")?;
                let size = self.size()?;
                self.take(b":{")?;
                self.pos = self
                    .pos
                    .checked_add(size)
                    .filter(|end| *end <= self.source.len() && self.source.is_char_boundary(*end))
                    .ok_or_else(|| anyhow!("invalid serialized_php custom payload length"))?;
                self.take(b"}")
            }
            _ => bail!("invalid serialized_php value at byte {}", self.pos),
        }?;
        Ok(id)
    }
}

fn printf_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut pos = start + 1;
    if bytes.get(pos) == Some(&b'%') {
        return Some(pos + 1);
    }
    let digits_start = pos;
    while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
        pos += 1;
    }
    if pos > digits_start && bytes.get(pos) == Some(&b'$') {
        pos += 1;
    } else {
        pos = digits_start;
    }
    while bytes.get(pos).is_some_and(|ch| b"-+0#'".contains(ch)) {
        pos += 1;
    }
    while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
        pos += 1;
    }
    if bytes.get(pos) == Some(&b'*') {
        pos += 1;
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        if bytes.get(pos) == Some(&b'$') {
            pos += 1;
        }
    }
    if bytes.get(pos) == Some(&b'.') {
        pos += 1;
        if bytes.get(pos) == Some(&b'*') {
            pos += 1;
        }
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        if bytes.get(pos) == Some(&b'$') {
            pos += 1;
        }
    }
    while bytes.get(pos).is_some_and(|ch| b"hlLjzt".contains(ch)) {
        pos += 1;
    }
    bytes
        .get(pos)
        .filter(|ch| b"bcdeEfFgGiosuxXaApn".contains(ch))
        .map(|_| pos + 1)
}

fn interpolation_tokens(text: &str) -> (BTreeMap<&str, usize>, Vec<&str>) {
    let mut tokens = BTreeMap::new();
    let mut implicit_printf = Vec::new();
    let mut pos = 0;
    let bytes = text.as_bytes();
    while pos < bytes.len() {
        let end = match bytes[pos] {
            b'%' => printf_end(bytes, pos),
            b'{' => {
                let double = bytes.get(pos + 1) == Some(&b'{');
                let offset = if double { 2 } else { 1 };
                let close = if double { "}}" } else { "}" };
                text[pos + offset..].find(close).and_then(|length| {
                    let name = text[pos + offset..pos + offset + length].trim();
                    (!name.is_empty()
                        && name
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-')))
                    .then_some(pos + offset + length + close.len())
                })
            }
            _ => None,
        };
        if let Some(end) = end {
            let token = &text[pos..end];
            *tokens.entry(token).or_insert(0) += 1;
            if token.starts_with('%') && token != "%%" && !token.contains('$') {
                implicit_printf.push(token);
            }
            pos = end;
        } else {
            // Byte scanning only slices at ASCII delimiters, never inside UTF-8.
            pos += 1;
        }
    }
    (tokens, implicit_printf)
}

pub(crate) fn validate_interpolation_tokens(source: &str, translated: &str) -> Result<()> {
    if interpolation_tokens(source) != interpolation_tokens(translated) {
        bail!("translation changed interpolation placeholders");
    }
    Ok(())
}

fn html_comments(html: &str) -> Result<Vec<&str>> {
    let mut comments = Vec::new();
    let mut stack = Vec::new();
    let mut pos = 0;
    while let Some(offset) = html[pos..].find("<!--") {
        let start = pos + offset;
        let end = html[start + 4..]
            .find("-->")
            .map(|offset| start + 4 + offset)
            .ok_or_else(|| anyhow!("rich_html has an unterminated comment at byte {start}"))?;
        let body = html[start + 4..end].trim();
        comments.push(&html[start..end + 3]);
        if let Some(rest) = body.strip_prefix("/wp:") {
            let name = rest.trim();
            if stack.pop() != Some(name) {
                bail!("rich_html has mismatched Gutenberg block nesting");
            }
        } else if let Some(rest) = body.strip_prefix("wp:") {
            let self_closing = rest.ends_with('/');
            let rest = if self_closing {
                rest[..rest.len() - 1].trim_end()
            } else {
                rest
            };
            let (name, attrs) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if name.is_empty()
                || !name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/'))
            {
                bail!("rich_html has an invalid Gutenberg block name");
            }
            if !attrs.trim().is_empty() {
                let attrs: serde_json::Value = serde_json::from_str(attrs.trim())
                    .map_err(|_| anyhow!("rich_html has invalid Gutenberg block attributes"))?;
                if !attrs.is_object() {
                    bail!("rich_html Gutenberg block attributes must be an object");
                }
            }
            if !self_closing {
                stack.push(name);
            }
        }
        pos = end + 3;
    }
    if !stack.is_empty() {
        bail!("rich_html has unclosed Gutenberg blocks");
    }
    Ok(comments)
}

pub(crate) fn validate_rich_html_source(html: &str) -> Result<()> {
    html_comments(html).map(|_| ())
}

pub(crate) fn validate_rich_html_translation(source: &str, translated: &str) -> Result<()> {
    if html_comments(source)? != html_comments(translated)? {
        bail!("translation changed HTML comments or Gutenberg block attributes");
    }
    validate_interpolation_tokens(source, translated)
}
