use super::*;

#[derive(Debug)]
pub(crate) struct TextChunk {
    pub(crate) text: String,
    pub(crate) separator_after: String,
}

pub(super) fn split_text_by_strategy(
    input: &str,
    max_chars: u64,
    strategy: &str,
    custom_separator: &str,
) -> Vec<TextChunk> {
    if max_chars == 0 || (input.chars().count() as u64) <= max_chars {
        return vec![TextChunk {
            text: input.to_string(),
            separator_after: String::new(),
        }];
    }
    match strategy {
        "paragraph" => split_by_delimiter(
            input,
            max_chars,
            if custom_separator.is_empty() {
                "\n\n"
            } else {
                custom_separator
            },
        ),
        "sentence" => split_by_sentence(input, max_chars),
        "chars" => split_by_chars(input, max_chars),
        _ => vec![TextChunk {
            text: input.to_string(),
            separator_after: String::new(),
        }],
    }
}

fn split_by_delimiter(input: &str, max_chars: u64, delimiter: &str) -> Vec<TextChunk> {
    let parts: Vec<&str> = input.split(delimiter).collect();
    let max = max_chars as usize;
    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut current = String::new();

    for (i, part) in parts.iter().enumerate() {
        let part_len = part.chars().count();
        let current_len = current.chars().count();
        let delimiter_len = if current.is_empty() {
            0
        } else {
            delimiter.chars().count()
        };

        if current_len + delimiter_len + part_len <= max {
            if !current.is_empty() {
                current.push_str(delimiter);
            }
            current.push_str(part);
        } else {
            if !current.is_empty() {
                chunks.push(TextChunk {
                    text: current.clone(),
                    separator_after: delimiter.to_string(),
                });
                current.clear();
            }
            if part_len > max {
                let sub_chunks = split_by_sentence(part, max_chars);
                let sub_len = sub_chunks.len();
                for (j, sub) in sub_chunks.into_iter().enumerate() {
                    chunks.push(TextChunk {
                        text: sub.text,
                        separator_after: if j < sub_len - 1 {
                            sub.separator_after
                        } else if i < parts.len() - 1 {
                            delimiter.to_string()
                        } else {
                            String::new()
                        },
                    });
                }
            } else {
                current = part.to_string();
            }
        }
    }
    if !current.is_empty() {
        chunks.push(TextChunk {
            text: current,
            separator_after: String::new(),
        });
    }
    if chunks.is_empty() {
        chunks.push(TextChunk {
            text: input.to_string(),
            separator_after: String::new(),
        });
    }
    chunks
}

fn split_by_sentence(input: &str, max_chars: u64) -> Vec<TextChunk> {
    let max = max_chars as usize;
    let mut sentences: Vec<String> = Vec::new();
    let mut current_sentence = String::new();

    for ch in input.chars() {
        current_sentence.push(ch);
        if matches!(
            ch,
            '\u{3002}' | '\u{FF01}' | '\u{FF1F}' | '.' | '!' | '?' | '\n'
        ) {
            sentences.push(current_sentence.clone());
            current_sentence.clear();
        }
    }
    if !current_sentence.is_empty() {
        sentences.push(current_sentence);
    }

    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut current = String::new();

    for sentence in &sentences {
        let sentence_len = sentence.chars().count();
        let current_len = current.chars().count();

        if current_len + sentence_len <= max {
            current.push_str(sentence);
        } else {
            if !current.is_empty() {
                chunks.push(TextChunk {
                    text: current.clone(),
                    separator_after: String::new(),
                });
                current.clear();
            }
            if sentence_len > max {
                let char_chunks = split_by_chars(sentence, max_chars);
                chunks.extend(char_chunks);
            } else {
                current = sentence.clone();
            }
        }
    }
    if !current.is_empty() {
        chunks.push(TextChunk {
            text: current,
            separator_after: String::new(),
        });
    }
    if chunks.is_empty() {
        chunks.push(TextChunk {
            text: input.to_string(),
            separator_after: String::new(),
        });
    }
    chunks
}

fn split_by_chars(input: &str, max_chars: u64) -> Vec<TextChunk> {
    let max = max_chars as usize;
    if max == 0 {
        return vec![TextChunk {
            text: input.to_string(),
            separator_after: String::new(),
        }];
    }
    let chars: Vec<char> = input.chars().collect();
    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut start = 0;

    while start < chars.len() {
        let end = (start + max).min(chars.len());
        if end >= chars.len() {
            chunks.push(TextChunk {
                text: chars[start..].iter().collect(),
                separator_after: String::new(),
            });
            break;
        }
        let mut split_at = end;
        for i in (start + max / 2..end).rev() {
            if chars[i].is_whitespace() {
                split_at = i + 1;
                break;
            }
        }
        chunks.push(TextChunk {
            text: chars[start..split_at].iter().collect(),
            separator_after: String::new(),
        });
        start = split_at;
    }
    if chunks.is_empty() {
        chunks.push(TextChunk {
            text: input.to_string(),
            separator_after: String::new(),
        });
    }
    chunks
}

fn merge_translated_chunks(translated_chunks: &[(TextChunk, String)]) -> String {
    let mut result = String::new();
    for (i, (_original, translated)) in translated_chunks.iter().enumerate() {
        result.push_str(translated);
        if i < translated_chunks.len() - 1 {
            let sep = &translated_chunks[i].0.separator_after;
            if !sep.is_empty() {
                result.push_str(sep);
            }
        }
    }
    result
}

pub(super) fn is_gutenberg_content(html: &str) -> bool {
    html.contains("<!-- wp:")
}

pub(super) fn split_rich_html_by_blocks(html: &str, max_chars: u64) -> Vec<TextChunk> {
    if html.is_empty() {
        return vec![TextChunk {
            text: String::new(),
            separator_after: String::new(),
        }];
    }
    let raw_blocks = if is_gutenberg_content(html) {
        split_gutenberg_top_level_blocks(html)
    } else {
        split_classic_html_blocks(html)
    };
    if raw_blocks.is_empty() {
        return vec![TextChunk {
            text: html.to_string(),
            separator_after: String::new(),
        }];
    }
    if max_chars == 0 {
        return raw_blocks;
    }
    batch_html_chunks(raw_blocks, max_chars)
}

fn split_gutenberg_top_level_blocks(html: &str) -> Vec<TextChunk> {
    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut depth: usize = 0;
    let mut block_start: Option<usize> = None;
    let mut last_end: usize = 0;

    let bytes = html.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if i + 4 <= len && &bytes[i..i + 4] == b"<!--" {
            if let Some(rel_end) = html[i + 4..].find("-->") {
                let comment_end = i + 4 + rel_end + 3;
                let comment_body = html[i + 4..i + 4 + rel_end].trim();

                if let Some(rest) = comment_body.strip_prefix("wp:") {
                    if rest.ends_with('/') {
                        if depth == 0 {
                            if i > last_end {
                                let between = &html[last_end..i];
                                if !between.trim().is_empty() {
                                    chunks.push(TextChunk {
                                        text: between.to_string(),
                                        separator_after: String::new(),
                                    });
                                }
                            }
                            chunks.push(TextChunk {
                                text: html[i..comment_end].to_string(),
                                separator_after: String::new(),
                            });
                            last_end = comment_end;
                        }
                    } else {
                        if depth == 0 {
                            if i > last_end {
                                let between = &html[last_end..i];
                                if !between.trim().is_empty() {
                                    chunks.push(TextChunk {
                                        text: between.to_string(),
                                        separator_after: String::new(),
                                    });
                                }
                            }
                            block_start = Some(i);
                        }
                        depth += 1;
                    }
                } else if let Some(rest) = comment_body.strip_prefix("/wp:") {
                    let _ = rest;
                    if depth > 0 {
                        depth -= 1;
                        if depth == 0 {
                            if let Some(start) = block_start.take() {
                                chunks.push(TextChunk {
                                    text: html[start..comment_end].to_string(),
                                    separator_after: String::new(),
                                });
                                last_end = comment_end;
                            }
                        }
                    }
                }
                i = comment_end;
                continue;
            }
        }
        i += 1;
    }

    if last_end < len {
        let trailing = &html[last_end..];
        if !trailing.trim().is_empty() {
            chunks.push(TextChunk {
                text: trailing.to_string(),
                separator_after: String::new(),
            });
        }
    }

    chunks
}

const BLOCK_LEVEL_TAGS: &[&str] = &[
    "p",
    "div",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "figure",
    "figcaption",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "ul",
    "ol",
    "li",
    "blockquote",
    "pre",
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "aside",
    "main",
    "details",
    "summary",
    "hr",
    "br",
    "dl",
    "dt",
    "dd",
];

fn split_classic_html_blocks(html: &str) -> Vec<TextChunk> {
    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut depth: usize = 0;
    let mut block_start: Option<usize> = None;
    let mut last_end: usize = 0;
    let mut current_block_tag: Option<String> = None;

    let len = html.len();
    let mut i = 0;

    while i < len {
        if html.as_bytes()[i] == b'<' {
            if let Some(tag_info) = parse_html_tag_at(html, i) {
                let tag_lower = tag_info.name.to_lowercase();
                let is_block = BLOCK_LEVEL_TAGS.contains(&tag_lower.as_str());

                if is_block {
                    if tag_info.is_self_closing || tag_lower == "hr" || tag_lower == "br" {
                        if depth == 0 {
                            if i > last_end {
                                let between = &html[last_end..i];
                                if !between.trim().is_empty() {
                                    chunks.push(TextChunk {
                                        text: between.to_string(),
                                        separator_after: String::new(),
                                    });
                                }
                            }
                            chunks.push(TextChunk {
                                text: html[i..tag_info.end].to_string(),
                                separator_after: String::new(),
                            });
                            last_end = tag_info.end;
                        }
                    } else if tag_info.is_closing {
                        if depth > 0 && current_block_tag.as_deref() == Some(&tag_lower) {
                            depth -= 1;
                            if depth == 0 {
                                if let Some(start) = block_start.take() {
                                    chunks.push(TextChunk {
                                        text: html[start..tag_info.end].to_string(),
                                        separator_after: String::new(),
                                    });
                                    last_end = tag_info.end;
                                }
                                current_block_tag = None;
                            }
                        }
                    } else {
                        if depth == 0 {
                            if i > last_end {
                                let between = &html[last_end..i];
                                if !between.trim().is_empty() {
                                    chunks.push(TextChunk {
                                        text: between.to_string(),
                                        separator_after: String::new(),
                                    });
                                }
                            }
                            block_start = Some(i);
                            current_block_tag = Some(tag_lower.clone());
                        }
                        depth += 1;
                    }
                }
                i = tag_info.end;
                continue;
            }
        }
        i += 1;
    }

    if last_end < len {
        let trailing = &html[last_end..];
        if !trailing.trim().is_empty() {
            chunks.push(TextChunk {
                text: trailing.to_string(),
                separator_after: String::new(),
            });
        }
    }
    if chunks.is_empty() {
        chunks.push(TextChunk {
            text: html.to_string(),
            separator_after: String::new(),
        });
    }
    chunks
}

struct HtmlTagInfo {
    name: String,
    is_closing: bool,
    is_self_closing: bool,
    end: usize,
}

fn parse_html_tag_at(html: &str, pos: usize) -> Option<HtmlTagInfo> {
    let rest = &html[pos..];
    let gt = rest.find('>')?;
    let tag_content = &rest[1..gt];
    if tag_content.is_empty() {
        return None;
    }
    let is_closing = tag_content.starts_with('/');
    let name_part = if is_closing {
        &tag_content[1..]
    } else {
        tag_content
    };
    let name_end = name_part
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .unwrap_or(name_part.len());
    let name = name_part[..name_end].to_string();
    if name.is_empty() || !name.chars().next()?.is_ascii_alphabetic() {
        return None;
    }
    let is_self_closing = tag_content.ends_with('/');
    Some(HtmlTagInfo {
        name,
        is_closing,
        is_self_closing,
        end: pos + gt + 1,
    })
}

fn batch_html_chunks(raw_chunks: Vec<TextChunk>, max_chars: u64) -> Vec<TextChunk> {
    let max = max_chars as usize;
    let mut result: Vec<TextChunk> = Vec::new();
    let mut current = String::new();

    for chunk in raw_chunks {
        let chunk_len = chunk.text.chars().count();
        let current_len = current.chars().count();

        if current.is_empty() {
            if chunk_len > max {
                let sub = sub_split_oversized_html_block(&chunk.text, max_chars);
                result.extend(sub);
            } else {
                current = chunk.text;
            }
        } else if current_len + chunk_len <= max {
            current.push_str(&chunk.text);
        } else {
            result.push(TextChunk {
                text: current,
                separator_after: String::new(),
            });
            current = String::new();
            if chunk_len > max {
                let sub = sub_split_oversized_html_block(&chunk.text, max_chars);
                result.extend(sub);
            } else {
                current = chunk.text;
            }
        }
    }
    if !current.is_empty() {
        result.push(TextChunk {
            text: current,
            separator_after: String::new(),
        });
    }
    if result.is_empty() {
        result.push(TextChunk {
            text: String::new(),
            separator_after: String::new(),
        });
    }
    result
}

fn sub_split_oversized_html_block(html: &str, max_chars: u64) -> Vec<TextChunk> {
    let inner = split_classic_html_blocks(html);
    if inner.len() > 1 {
        return batch_html_chunks(inner, max_chars);
    }
    vec![TextChunk {
        text: html.to_string(),
        separator_after: String::new(),
    }]
}

pub(crate) async fn translate_rich_html_blocks(
    client: &Client,
    runtime: &ComponentRuntime,
    html: &str,
    source_lang: &str,
    target_lang: &str,
    constraints: &crate::types::EffectiveConstraints,
) -> anyhow::Result<String> {
    let chunks = split_rich_html_by_blocks(html, constraints.max_input_chars);

    if chunks.len() <= 1 {
        return translate_text_via_component(client, runtime, html, source_lang, target_lang).await;
    }

    let mut results: Vec<(TextChunk, String)> = Vec::new();
    for chunk in chunks {
        let translated =
            translate_text_via_component(client, runtime, &chunk.text, source_lang, target_lang)
                .await?;
        results.push((chunk, translated));
    }

    Ok(merge_translated_chunks(&results))
}

pub(crate) async fn translate_text_with_constraints(
    client: &Client,
    runtime: &ComponentRuntime,
    input_text: &str,
    source_lang: &str,
    target_lang: &str,
    constraints: &crate::types::EffectiveConstraints,
) -> anyhow::Result<String> {
    let chunks = split_text_by_strategy(
        input_text,
        constraints.max_input_chars,
        &constraints.split_strategy,
        &constraints.split_separator,
    );

    if chunks.len() <= 1 {
        return translate_text_via_component(client, runtime, input_text, source_lang, target_lang)
            .await;
    }

    let mut results: Vec<(TextChunk, String)> = Vec::new();
    for chunk in chunks {
        let translated =
            translate_text_via_component(client, runtime, &chunk.text, source_lang, target_lang)
                .await?;
        results.push((chunk, translated));
    }

    Ok(merge_translated_chunks(&results))
}
