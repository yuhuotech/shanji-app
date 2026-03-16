use std::collections::HashMap;

const FILLER_WORDS: &[&str] = &[
    "嗯", "啊", "哦", "呃", "额", "哎", "欸", "诶", "呢", "吧", "嘛", "哈", "然后呢",
];

const CONNECTIVE_MARKERS: &[&str] = &[
    "尤其", "但是", "不过", "所以", "因此", "然后", "而且", "并且", "或者", "至少", "因为",
    "另外", "同时", "例如", "比如", "不过", "其实", "如果", "就是", "接着", "最后", "同时",
    "更建议", "建议", "不过如果", "总之",
];

pub fn normalize_transcript(text: &str) -> String {
    let mut result = text.trim().replace("@@", "");
    result = result.replace(['\n', '\r', '\t'], " ");
    result = collapse_whitespace(&result);
    result = remove_filler_words(&result);
    result = collapse_repeated_phrases(&result);
    result = collapse_repeated_chars(&result);
    result = normalize_ascii_tokens(&result);
    result = normalize_mixed_spacing(&result);
    result = apply_common_corrections(&result);
    result.trim().to_string()
}

pub fn render_segmented_transcript(
    committed_segments: &[&str],
    pending_segment: Option<&str>,
    current_partial: &str,
    punct_style: &str,
    insert_punct: bool,
    is_final: bool,
) -> String {
    let committed = committed_segments
        .iter()
        .map(|text| normalize_transcript(text))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    let pending = pending_segment
        .map(normalize_transcript)
        .filter(|text| !text.is_empty());
    let partial = normalize_transcript(current_partial);

    if !insert_punct {
        let mut plain = String::new();
        for part in committed {
            plain.push_str(&part);
        }
        if let Some(pending) = pending {
            plain.push_str(&pending);
        }
        plain.push_str(&partial);
        return plain;
    }

    match punct_style {
        "en" => render_segmented_english(committed, pending.as_deref(), &partial, is_final),
        _ => render_segmented_chinese(committed, pending.as_deref(), &partial, is_final),
    }
}

pub fn finalize_transcript_text(text: &str, punct_style: &str, insert_punct: bool) -> String {
    let normalized = normalize_transcript(text);
    if normalized.is_empty() || !insert_punct {
        return normalized;
    }

    match punct_style {
        "en" => finalize_english_text(&normalized),
        _ => finalize_chinese_text(&normalized),
    }
}

fn render_segmented_chinese(
    committed: Vec<String>,
    pending: Option<&str>,
    current_partial: &str,
    is_final: bool,
) -> String {
    let mut text = committed.join("，");
    if let Some(pending) = pending {
        if !text.is_empty() && !ends_with_punctuation(&text) {
            text.push('，');
        }
        text.push_str(pending);
    }

    let partial = refine_chinese_clause(current_partial, false);
    if !partial.is_empty() {
        if !text.is_empty() && !ends_with_punctuation(&text) {
            text.push('，');
        }
        text.push_str(&partial);
    }

    if text.is_empty() {
        return text;
    }

    let text = refine_chinese_clause(&text, is_final);
    if is_final {
        finalize_chinese_text(&text)
    } else {
        text
    }
}

fn render_segmented_english(
    committed: Vec<String>,
    pending: Option<&str>,
    current_partial: &str,
    is_final: bool,
) -> String {
    let mut text = committed.join(", ");
    if let Some(pending) = pending {
        if !text.is_empty() && !ends_with_punctuation(&text) {
            text.push_str(", ");
        }
        text.push_str(pending);
    }
    if !current_partial.is_empty() {
        if !text.is_empty() && !ends_with_punctuation(&text) {
            text.push_str(", ");
        }
        text.push_str(current_partial);
    }

    if is_final {
        finalize_english_text(&text)
    } else {
        text
    }
}

fn finalize_chinese_text(text: &str) -> String {
    let mut text = refine_chinese_clause(text, true);
    if text.is_empty() {
        return text;
    }

    if !ends_with_punctuation(&text) {
        let terminal = match text.chars().last() {
            Some('吗' | '么' | '呢') => '？',
            _ => '。',
        };
        text.push(terminal);
    }
    text
}

fn finalize_english_text(text: &str) -> String {
    let mut result = text.trim().to_string();
    if result.is_empty() {
        return result;
    }

    if !ends_with_punctuation(&result) {
        result.push('.');
    }
    result
}

fn refine_chinese_clause(text: &str, is_final: bool) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let mut idx = 0usize;
    let mut since_punct = 0usize;

    while idx < chars.len() {
        let suffix = chars[idx..].iter().collect::<String>();
        let starts_marker = CONNECTIVE_MARKERS
            .iter()
            .any(|marker| suffix.starts_with(marker));
        if starts_marker && !out.is_empty() && !ends_with_punctuation(&out) && since_punct >= 6 {
            out.push('，');
            since_punct = 0;
        }

        let ch = chars[idx];
        out.push(ch);
        since_punct += 1;

        if !is_final
            && since_punct >= 22
            && idx + 1 < chars.len()
            && !ends_with_punctuation(&out)
            && matches!(
                ch,
                '是'
                    | '要'
                    | '用'
                    | '做'
                    | '让'
                    | '把'
                    | '在'
                    | '对'
                    | '到'
                    | '后'
                    | '前'
                    | '时'
                    | '并'
                    | '或'
            )
        {
            out.push('，');
            since_punct = 0;
        }

        if ends_with_punctuation(&out) {
            since_punct = 0;
        }

        idx += 1;
    }

    out
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn remove_filler_words(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let mut out = String::new();
    let mut idx = 0usize;

    while idx < chars.len() {
        let suffix = chars[idx..].iter().collect::<String>();
        if let Some(filler) = FILLER_WORDS
            .iter()
            .find(|filler| suffix.starts_with(**filler) && is_filler_boundary(&chars, idx, filler))
        {
            idx += filler.chars().count();
            continue;
        }
        out.push(chars[idx]);
        idx += 1;
    }

    out
}

fn is_filler_boundary(chars: &[char], start: usize, filler: &str) -> bool {
    let len = filler.chars().count();
    let prev_ok = start == 0
        || chars[start.saturating_sub(1)].is_whitespace()
        || is_boundary_punctuation(chars[start - 1]);
    let end = start + len;
    let next_ok = end >= chars.len()
        || chars[end].is_whitespace()
        || is_boundary_punctuation(chars[end]);

    prev_ok && next_ok
}

fn collapse_repeated_chars(text: &str) -> String {
    let mut out = String::new();
    let mut prev = '\0';
    for ch in text.chars() {
        if ch == prev && is_repeatable_cjk(ch) {
            continue;
        }
        out.push(ch);
        prev = ch;
    }
    out
}

fn collapse_repeated_phrases(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let mut idx = 0usize;
    let mut out = String::new();

    while idx < chars.len() {
        let mut collapsed = false;
        for width in (1..=4).rev() {
            if idx + width * 2 > chars.len() {
                continue;
            }
            let left = &chars[idx..idx + width];
            let right = &chars[idx + width..idx + width * 2];
            if left == right {
                out.extend(left.iter());
                idx += width * 2;
                collapsed = true;
                break;
            }
        }

        if !collapsed {
            out.push(chars[idx]);
            idx += 1;
        }
    }

    out
}

fn normalize_ascii_tokens(text: &str) -> String {
    let mut lexicon = HashMap::new();
    lexicon.insert("xcode", "Xcode");
    lexicon.insert("ios", "iOS");
    lexicon.insert("iphone", "iPhone");
    lexicon.insert("ipad", "iPad");
    lexicon.insert("macbook", "MacBook");
    lexicon.insert("mac", "Mac");
    lexicon.insert("intel", "Intel");
    lexicon.insert("apple", "Apple");
    lexicon.insert("github", "GitHub");
    lexicon.insert("onnx", "ONNX");
    lexicon.insert("paraformer", "Paraformer");
    lexicon.insert("funasr", "FunASR");
    lexicon.insert("api", "API");
    lexicon.insert("llm", "LLM");

    let mut out = String::new();
    let mut token = String::new();

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.') {
            token.push(ch);
            continue;
        }

        if !token.is_empty() {
            out.push_str(&normalize_ascii_token(&token, &lexicon));
            token.clear();
        }
        out.push(ch);
    }

    if !token.is_empty() {
        out.push_str(&normalize_ascii_token(&token, &lexicon));
    }

    out
}

fn normalize_ascii_token(token: &str, lexicon: &HashMap<&str, &str>) -> String {
    if token.contains('/') {
        return token
            .split('/')
            .map(|part| normalize_ascii_token(part, lexicon))
            .collect::<Vec<_>>()
            .join("/");
    }

    let lower = token.to_ascii_lowercase();
    if let Some(mapped) = lexicon.get(lower.as_str()) {
        return (*mapped).to_string();
    }

    token.to_string()
}

fn normalize_mixed_spacing(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let mut out = String::new();
    for (idx, &ch) in chars.iter().enumerate() {
        if ch == ' ' {
            let prev = idx.checked_sub(1).and_then(|i| chars.get(i)).copied();
            let next = chars.get(idx + 1).copied();
            if matches!((prev, next), (Some(a), Some(b)) if !needs_space_between(a, b)) {
                continue;
            }
        }
        out.push(ch);
    }
    out
}

fn needs_space_between(left: char, right: char) -> bool {
    (left.is_ascii_alphanumeric() && right.is_ascii_alphanumeric())
        || (left.is_ascii_alphanumeric() && matches!(right, '(' | '['))
        || (matches!(left, ')' | ']') && right.is_ascii_alphanumeric())
}

fn apply_common_corrections(text: &str) -> String {
    [
        ("测是", "测试"),
        ("那试", "测试"),
        ("侧是", "测试"),
        ("册是", "测试"),
        ("英特尔mi", "Intel Mac"),
        ("英特尔mac", "Intel Mac"),
        ("apple硬件", "Apple 硬件"),
    ]
    .into_iter()
    .fold(text.to_string(), |acc, (wrong, correct)| acc.replace(wrong, correct))
}

fn is_repeatable_cjk(ch: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&ch)
}

fn is_boundary_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '，' | '。' | '！' | '？' | '；' | '：' | ',' | '.' | '!' | '?' | ';' | ':' | '、'
    )
}

fn ends_with_punctuation(text: &str) -> bool {
    text.chars()
        .last()
        .map(is_boundary_punctuation)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_removes_fillers_and_duplicates() {
        let text = "嗯 我我我觉得这个这个方案还还可以";
        let result = normalize_transcript(text);
        assert_eq!(result, "我觉得这个方案还可以");
    }

    #[test]
    fn normalize_ascii_tokens_keeps_tech_brands() {
        let text = "依赖 xcode ios github onnx";
        let result = normalize_transcript(text);
        assert!(result.contains("Xcode"));
        assert!(result.contains("iOS"));
        assert!(result.contains("GitHub"));
    }

    #[test]
    fn segmented_chinese_prefers_commas_and_final_period() {
        let result = render_segmented_transcript(
            &["如果你的目标是长期稳定生产使用", "尤其要升级系统"],
            Some("更建议直接用 Apple 硬件"),
            "",
            "zh",
            true,
            false,
        );
        assert!(result.contains('，'));
        assert!(!result.ends_with('。'));

        let final_result = finalize_transcript_text(&result, "zh", true);
        assert!(final_result.ends_with('。'));
    }
}
