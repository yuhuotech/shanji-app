use crate::hotwords::Hotword;
use std::collections::HashMap;

const FILLER_WORDS: &[&str] = &[
    "嗯",
    "啊",
    "哦",
    "呃",
    "额",
    "哎",
    "欸",
    "诶",
    "呢",
    "吧",
    "嘛",
    "哈",
    "然后呢",
    "这个",
];

const CONNECTIVE_MARKERS: &[&str] = &[
    "尤其",
    "但是",
    "不过",
    "所以",
    "因此",
    "然后",
    "而且",
    "并且",
    "或者",
    "至少",
    "因为",
    "另外",
    "同时",
    "例如",
    "比如",
    "其实",
    "如果",
    "就是",
    "接着",
    "最后",
    "更建议",
    "建议",
    "总之",
    "并且",
    "不过如果",
];

const PREDICATE_CLAUSE_MARKERS: &[&str] = &[
    "适合",
    "更适合",
    "适用于",
    "用于",
    "支持",
    "可以",
    "能够",
    "需要",
    "值得",
    "便于",
];

const PREDICATE_HEAD_SUFFIXES: &[&str] = &[
    "模型", "方案", "系统", "工具", "服务", "接口", "能力", "引擎", "框架", "架构", "模块", "组件",
    "平台", "设备", "产品", "功能", "方法", "技术",
];

const OPEN_ENDED_SUFFIXES: &[&str] = &[
    "低延迟",
    "高延迟",
    "高并发",
    "高精度",
    "高性能",
    "多语言",
    "多模态",
    "端到端",
    "实时",
    "流式",
];

const TIGHT_BOUNDARY_PHRASES: &[&str] = &[
    "低延迟实时转写",
    "低延迟语音转写",
    "低延迟实时识别",
    "实时语音识别",
    "实时语音转写",
    "流式语音识别",
    "中文流式语音识别",
];

#[derive(Clone, Debug)]
pub struct TextProcessingConfig {
    pub punct_style: String,
    pub insert_punct: bool,
    pub comma_pause_ms: u32,
    pub sentence_pause_ms: u32,
    pub hotwords: Vec<Hotword>,
}

impl Default for TextProcessingConfig {
    fn default() -> Self {
        Self {
            punct_style: "zh".to_string(),
            insert_punct: true,
            comma_pause_ms: 1_200,
            sentence_pause_ms: 2_800,
            hotwords: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TranscriptChunk<'a> {
    pub text: &'a str,
    pub leading_pause_ms: u32,
}

pub fn normalize_transcript(text: &str) -> String {
    normalize_transcript_with_hotwords(text, &[])
}

pub fn normalize_transcript_with_hotwords(text: &str, hotwords: &[Hotword]) -> String {
    let mut result = text.trim().replace("@@", "");
    result = result.replace(['\n', '\r', '\t'], " ");
    result = collapse_whitespace(&result);
    result = remove_filler_words(&result);
    result = collapse_repeated_phrases(&result);
    result = collapse_repeated_chars(&result);
    result = normalize_ascii_tokens(&result, hotwords);
    result = normalize_mixed_spacing(&result);
    result = apply_itn(&result);
    result = apply_common_corrections(&result);
    result = apply_hotword_overrides(&result, hotwords);
    collapse_whitespace(&result)
}

pub fn render_segmented_transcript(
    committed_segments: &[TranscriptChunk<'_>],
    pending_segment: Option<TranscriptChunk<'_>>,
    current_partial: Option<TranscriptChunk<'_>>,
    config: &TextProcessingConfig,
    is_final: bool,
) -> String {
    let committed = committed_segments
        .iter()
        .map(|chunk| RenderedChunk {
            text: normalize_transcript_with_hotwords(chunk.text, &config.hotwords),
            leading_pause_ms: chunk.leading_pause_ms,
        })
        .filter(|chunk| !chunk.text.is_empty())
        .collect::<Vec<_>>();
    let pending = pending_segment
        .map(|chunk| RenderedChunk {
            text: normalize_transcript_with_hotwords(chunk.text, &config.hotwords),
            leading_pause_ms: chunk.leading_pause_ms,
        })
        .filter(|chunk| !chunk.text.is_empty());
    let partial = current_partial
        .map(|chunk| RenderedChunk {
            text: normalize_transcript_with_hotwords(chunk.text, &config.hotwords),
            leading_pause_ms: chunk.leading_pause_ms,
        })
        .filter(|chunk| !chunk.text.is_empty());

    if !config.insert_punct {
        return join_without_punctuation(
            &committed,
            pending.as_ref(),
            partial.as_ref(),
            &config.punct_style,
        );
    }

    match config.punct_style.as_str() {
        "en" => render_segmented_english(
            &committed,
            pending.as_ref(),
            partial.as_ref(),
            config,
            is_final,
        ),
        _ => render_segmented_chinese(
            &committed,
            pending.as_ref(),
            partial.as_ref(),
            config,
            is_final,
        ),
    }
}

pub fn finalize_transcript_text(text: &str, config: &TextProcessingConfig) -> String {
    let normalized = normalize_transcript_with_hotwords(text, &config.hotwords);
    if normalized.is_empty() || !config.insert_punct {
        return normalized;
    }

    match config.punct_style.as_str() {
        "en" => finalize_english_text(&normalized),
        _ => finalize_chinese_text(&normalized),
    }
}

#[derive(Clone, Debug)]
struct RenderedChunk {
    text: String,
    leading_pause_ms: u32,
}

fn join_without_punctuation(
    committed: &[RenderedChunk],
    pending: Option<&RenderedChunk>,
    partial: Option<&RenderedChunk>,
    punct_style: &str,
) -> String {
    let mut parts = committed
        .iter()
        .map(|chunk| chunk.text.as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    if let Some(pending) = pending {
        parts.push(pending.text.as_str());
    }
    if let Some(partial) = partial {
        parts.push(partial.text.as_str());
    }

    match punct_style {
        "en" => parts.join(" "),
        _ => parts.join(""),
    }
}

fn render_segmented_chinese(
    committed: &[RenderedChunk],
    pending: Option<&RenderedChunk>,
    current_partial: Option<&RenderedChunk>,
    config: &TextProcessingConfig,
    is_final: bool,
) -> String {
    let mut text = String::new();
    for chunk in committed {
        append_chunk_with_pause(&mut text, chunk, config);
    }

    if let Some(pending) = pending {
        append_chunk_with_pause(&mut text, pending, config);
    }

    if let Some(current_partial) = current_partial {
        if !text.is_empty() && !ends_with_punctuation(&text) {
            text.push_str(chinese_pause_punctuation(
                &text,
                &current_partial.text,
                current_partial.leading_pause_ms,
                config,
            ));
        }
        text.push_str(&current_partial.text);
    }

    let text = refine_chinese_clause(&text, is_final);
    if is_final {
        finalize_chinese_text(&text)
    } else {
        text
    }
}

fn render_segmented_english(
    committed: &[RenderedChunk],
    pending: Option<&RenderedChunk>,
    current_partial: Option<&RenderedChunk>,
    config: &TextProcessingConfig,
    is_final: bool,
) -> String {
    let mut text = String::new();
    for chunk in committed {
        append_english_chunk_with_pause(&mut text, chunk, config);
    }

    if let Some(pending) = pending {
        append_english_chunk_with_pause(&mut text, pending, config);
    }

    if let Some(current_partial) = current_partial {
        if !text.is_empty() && !ends_with_punctuation(&text) {
            text.push_str(english_pause_punctuation(
                current_partial.leading_pause_ms,
                config,
            ));
        }
        text.push_str(&current_partial.text);
    }

    if is_final {
        finalize_english_text(&text)
    } else {
        text
    }
}

fn append_chunk_with_pause(out: &mut String, chunk: &RenderedChunk, config: &TextProcessingConfig) {
    if chunk.text.is_empty() {
        return;
    }

    if !out.is_empty() && !ends_with_punctuation(out) {
        out.push_str(chinese_pause_punctuation(
            out,
            &chunk.text,
            chunk.leading_pause_ms,
            config,
        ));
    }

    out.push_str(&chunk.text);
}

fn append_english_chunk_with_pause(
    out: &mut String,
    chunk: &RenderedChunk,
    config: &TextProcessingConfig,
) {
    if chunk.text.is_empty() {
        return;
    }

    if !out.is_empty() && !ends_with_punctuation(out) {
        out.push_str(english_pause_punctuation(chunk.leading_pause_ms, config));
    }

    out.push_str(&chunk.text);
}

fn chinese_pause_punctuation(
    previous_text: &str,
    next_text: &str,
    leading_pause_ms: u32,
    config: &TextProcessingConfig,
) -> &'static str {
    if leading_pause_ms < config.comma_pause_ms {
        ""
    } else {
        choose_chinese_boundary_punctuation(previous_text, next_text, leading_pause_ms, config)
    }
}

fn english_pause_punctuation(leading_pause_ms: u32, config: &TextProcessingConfig) -> &'static str {
    if leading_pause_ms < config.comma_pause_ms {
        " "
    } else if leading_pause_ms < config.sentence_pause_ms {
        ", "
    } else {
        ". "
    }
}

fn choose_chinese_boundary_punctuation(
    previous_text: &str,
    next_text: &str,
    leading_pause_ms: u32,
    config: &TextProcessingConfig,
) -> &'static str {
    let previous_text = trim_boundary_edges(previous_text);
    let next_text = trim_boundary_edges(next_text);
    if previous_text.is_empty() || next_text.is_empty() {
        return "";
    }

    if forms_tight_boundary_phrase(previous_text, next_text)
        || ends_with_any(previous_text, OPEN_ENDED_SUFFIXES)
    {
        return "";
    }

    if leading_pause_ms >= config.sentence_pause_ms {
        return "。";
    }

    if starts_connective_marker(next_text) || starts_predicate_clause(previous_text, next_text) {
        return "，";
    }

    if is_likely_complete_clause(previous_text) && starts_new_sentence_candidate(next_text) {
        return "。";
    }

    "，"
}

fn starts_connective_marker(text: &str) -> bool {
    CONNECTIVE_MARKERS
        .iter()
        .any(|marker| text.starts_with(marker))
}

fn starts_predicate_clause(previous_text: &str, next_text: &str) -> bool {
    PREDICATE_CLAUSE_MARKERS
        .iter()
        .any(|marker| next_text.starts_with(marker))
        && ends_with_any(previous_text, PREDICATE_HEAD_SUFFIXES)
}

fn starts_new_sentence_candidate(text: &str) -> bool {
    text.chars().count() >= 2 && !starts_connective_marker(text)
}

fn is_likely_complete_clause(text: &str) -> bool {
    text.chars().count() >= 6 && !ends_with_any(text, OPEN_ENDED_SUFFIXES)
}

fn forms_tight_boundary_phrase(previous_text: &str, next_text: &str) -> bool {
    TIGHT_BOUNDARY_PHRASES
        .iter()
        .any(|phrase| boundary_matches_phrase(previous_text, next_text, phrase))
}

fn trim_boundary_edges(text: &str) -> &str {
    text.trim_matches(|ch: char| ch.is_whitespace() || is_boundary_punctuation(ch))
}

fn ends_with_any(text: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suffix| text.ends_with(suffix))
}

fn boundary_matches_phrase(previous_text: &str, next_text: &str, phrase: &str) -> bool {
    let phrase_chars = phrase.chars().collect::<Vec<_>>();
    for split in 1..phrase_chars.len() {
        let left = phrase_chars[..split].iter().collect::<String>();
        let right = phrase_chars[split..].iter().collect::<String>();
        if previous_text.ends_with(&left) && next_text.starts_with(&right) {
            return true;
        }
    }
    false
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
        let starts_connective = starts_connective_marker(&suffix);
        let starts_predicate = starts_predicate_clause(&out, &suffix);
        if starts_connective && !out.is_empty() && !ends_with_punctuation(&out) && since_punct >= 5
        {
            out.push('，');
            since_punct = 0;
        } else if starts_predicate
            && !out.is_empty()
            && !ends_with_punctuation(&out)
            && since_punct >= 4
        {
            out.push('，');
            since_punct = 0;
        }

        let ch = chars[idx];
        out.push(ch);
        since_punct += 1;

        if !is_final
            && since_punct >= 28
            && idx + 1 < chars.len()
            && !ends_with_punctuation(&out)
            && matches!(
                ch,
                '是' | '要'
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
    let next_ok =
        end >= chars.len() || chars[end].is_whitespace() || is_boundary_punctuation(chars[end]);

    prev_ok && next_ok
}

fn collapse_repeated_chars(text: &str) -> String {
    let mut out = String::new();
    let mut prev = '\0';
    let mut repeat_count = 0usize;
    for ch in text.chars() {
        if ch == prev {
            repeat_count += 1;
            if is_repeatable_cjk(ch) || (ch.is_ascii_alphabetic() && repeat_count >= 2) {
                continue;
            }
        } else {
            repeat_count = 0;
        }
        out.push(ch);
        prev = ch;
    }
    out
}

fn collapse_repeated_phrases(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if is_standalone_short_phrase_repetition(&chars) {
        return text.to_string();
    }
    let mut idx = 0usize;
    let mut out = String::new();

    while idx < chars.len() {
        let mut collapsed = false;
        for width in (1..=6).rev() {
            if idx + width * 2 > chars.len() {
                continue;
            }
            let left = &chars[idx..idx + width];
            let right = &chars[idx + width..idx + width * 2];
            if left == right && is_phrase_duplicate_candidate(left) {
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

fn is_standalone_short_phrase_repetition(chars: &[char]) -> bool {
    if chars.len() < 4 {
        return false;
    }

    for width in 2..=4 {
        if chars.len() < width * 2 || chars.len() % width != 0 {
            continue;
        }

        let phrase = &chars[..width];
        if !is_phrase_duplicate_candidate(phrase)
            || phrase
                .iter()
                .any(|ch| ch.is_whitespace() || is_boundary_punctuation(*ch))
        {
            continue;
        }

        if chars.chunks_exact(width).all(|chunk| chunk == phrase) {
            return true;
        }
    }

    false
}

fn normalize_ascii_tokens(text: &str, hotwords: &[Hotword]) -> String {
    let lexicon = build_ascii_lexicon(hotwords);
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

fn build_ascii_lexicon(hotwords: &[Hotword]) -> HashMap<String, String> {
    let mut lexicon = HashMap::new();
    for (key, value) in [
        ("xcode", "Xcode"),
        ("ios", "iOS"),
        ("iphone", "iPhone"),
        ("ipad", "iPad"),
        ("macbook", "MacBook"),
        ("mac", "Mac"),
        ("intel", "Intel"),
        ("apple", "Apple"),
        ("github", "GitHub"),
        ("huggingface", "Hugging Face"),
        ("modelscope", "ModelScope"),
        ("onnx", "ONNX"),
        ("paraformer", "Paraformer"),
        ("funasr", "FunASR"),
        ("api", "API"),
        ("sdk", "SDK"),
        ("llm", "LLM"),
        ("cpu", "CPU"),
        ("gpu", "GPU"),
        ("openai", "OpenAI"),
        ("rust", "Rust"),
    ] {
        lexicon.insert(key.to_string(), value.to_string());
    }

    for hotword in hotwords {
        if hotword.word.is_ascii() {
            lexicon.insert(hotword.word.to_ascii_lowercase(), hotword.word.clone());
        }
    }
    lexicon
}

fn normalize_ascii_token(token: &str, lexicon: &HashMap<String, String>) -> String {
    if token.contains('/') {
        return token
            .split('/')
            .map(|part| normalize_ascii_token(part, lexicon))
            .collect::<Vec<_>>()
            .join("/");
    }

    let lower = token.to_ascii_lowercase();
    if let Some(mapped) = lexicon.get(lower.as_str()) {
        return mapped.clone();
    }

    if let Some(segmented) = segment_ascii_token(&lower, lexicon) {
        return segmented;
    }

    token.to_string()
}

fn segment_ascii_token(token: &str, lexicon: &HashMap<String, String>) -> Option<String> {
    let mut idx = 0usize;
    let mut parts = Vec::new();
    while idx < token.len() {
        let mut found = None;
        for end in (idx + 1..=token.len()).rev() {
            let candidate = &token[idx..end];
            if let Some(mapped) = lexicon.get(candidate) {
                found = Some((end, mapped.clone()));
                break;
            }
        }
        let Some((end, mapped)) = found else {
            return None;
        };
        parts.push(mapped);
        idx = end;
    }

    if parts.len() >= 2 {
        Some(parts.join(" "))
    } else {
        None
    }
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

fn apply_itn(text: &str) -> String {
    let mut text = replace_spoken_years(text);
    text = replace_percentage(&text);
    text = replace_suffixed_number(&text, "元");
    text = replace_suffixed_number(&text, "块");
    text = replace_suffixed_number(&text, "MB");
    text = replace_suffixed_number(&text, "GB");
    text
}

fn replace_percentage(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let prefix_chars = "百分之".chars().collect::<Vec<_>>();
    let mut out = String::new();
    let mut idx = 0usize;

    while idx < chars.len() {
        if chars[idx..].starts_with(&prefix_chars) {
            let start = idx + prefix_chars.len();
            let max_end = (start + 5).min(chars.len());
            let mut replaced = false;

            for end in (start + 1..=max_end).rev() {
                let number_text = chars[start..end].iter().collect::<String>();
                let Some(value) = parse_chinese_number(&number_text) else {
                    continue;
                };
                if value > 100 {
                    continue;
                }

                let next = chars.get(end).copied();
                if matches!(next, Some(ch) if is_percentage_continuation(ch)) {
                    continue;
                }

                out.push_str(&value.to_string());
                out.push('%');
                idx = end;
                replaced = true;
                break;
            }

            if replaced {
                continue;
            }
        }

        out.push(chars[idx]);
        idx += 1;
    }

    out
}

fn replace_spoken_years(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let mut out = String::new();
    let mut idx = 0usize;
    while idx < chars.len() {
        if idx + 4 < chars.len() && chars[idx + 4] == '年' {
            let maybe_year = chars[idx..idx + 4]
                .iter()
                .map(|ch| chinese_digit_char(*ch))
                .collect::<Option<String>>();
            if let Some(year) = maybe_year {
                out.push_str(&year);
                out.push('年');
                idx += 5;
                continue;
            }
        }
        out.push(chars[idx]);
        idx += 1;
    }
    out
}

fn replace_suffixed_number(text: &str, suffix: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let suffix_chars = suffix.chars().collect::<Vec<_>>();
    let mut out = String::new();
    let mut idx = 0usize;

    while idx < chars.len() {
        let start = idx;
        while idx < chars.len() && is_chinese_number_char(chars[idx]) {
            idx += 1;
        }
        if idx > start && chars[idx..].starts_with(&suffix_chars) {
            let number_text = chars[start..idx].iter().collect::<String>();
            if let Some(value) = parse_chinese_number(&number_text) {
                out.push_str(&value.to_string());
                out.push_str(suffix);
                idx += suffix_chars.len();
                continue;
            }
        }

        if start != idx {
            out.push_str(&chars[start..idx].iter().collect::<String>());
            continue;
        }

        out.push(chars[idx]);
        idx += 1;
    }

    out
}

fn parse_chinese_number(text: &str) -> Option<u64> {
    if text.is_empty() {
        return None;
    }

    if text.chars().all(|ch| chinese_digit_char(ch).is_some()) {
        return text
            .chars()
            .map(chinese_digit_value)
            .collect::<Option<Vec<_>>>()
            .map(|digits| {
                digits
                    .into_iter()
                    .fold(0u64, |acc, digit| acc * 10 + digit as u64)
            });
    }

    let mut result = 0u64;
    let mut section = 0u64;
    let mut number = 0u64;

    for ch in text.chars() {
        match ch {
            '十' => {
                number = if number == 0 { 1 } else { number };
                section += number * 10;
                number = 0;
            }
            '百' => {
                number = if number == 0 { 1 } else { number };
                section += number * 100;
                number = 0;
            }
            '千' => {
                number = if number == 0 { 1 } else { number };
                section += number * 1000;
                number = 0;
            }
            '万' => {
                section += number;
                result += section * 10_000;
                section = 0;
                number = 0;
            }
            '亿' => {
                section += number;
                result += section * 100_000_000;
                section = 0;
                number = 0;
            }
            _ => {
                number = chinese_digit_value(ch)? as u64;
            }
        }
    }

    Some(result + section + number)
}

fn chinese_digit_char(ch: char) -> Option<String> {
    Some(
        match ch {
            '零' | '〇' => "0",
            '一' => "1",
            '二' | '两' => "2",
            '三' => "3",
            '四' => "4",
            '五' => "5",
            '六' => "6",
            '七' => "7",
            '八' => "8",
            '九' => "9",
            _ => return None,
        }
        .to_string(),
    )
}

fn chinese_digit_value(ch: char) -> Option<u32> {
    match ch {
        '零' | '〇' => Some(0),
        '一' => Some(1),
        '二' | '两' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        _ => None,
    }
}

fn is_chinese_number_char(ch: char) -> bool {
    matches!(
        ch,
        '零' | '〇'
            | '一'
            | '二'
            | '两'
            | '三'
            | '四'
            | '五'
            | '六'
            | '七'
            | '八'
            | '九'
            | '十'
            | '百'
            | '千'
            | '万'
            | '亿'
    )
}

fn is_percentage_continuation(ch: char) -> bool {
    matches!(ch, '十' | '百' | '千' | '万' | '亿')
}

fn apply_hotword_overrides(text: &str, hotwords: &[Hotword]) -> String {
    hotwords
        .iter()
        .filter(|hotword| !hotword.word.is_empty() && !hotword.word.is_ascii())
        .fold(text.to_string(), |acc, hotword| {
            acc.replace(&hotword.word, &hotword.word)
        })
}

fn apply_common_corrections(text: &str) -> String {
    [
        ("测是", "测试"),
        ("那试", "测试"),
        ("侧是", "测试"),
        ("册是", "测试"),
        ("中文流逝语音识别", "中文流式语音识别"),
        ("流逝语音识别", "流式语音识别"),
        ("流失语音识别", "流式语音识别"),
        ("英特尔mi", "Intel Mac"),
        ("英特尔mac", "Intel Mac"),
        ("英特尔 ma", "Intel Mac"),
        ("apple硬件", "Apple 硬件"),
        ("xcodeios", "Xcode iOS"),
        ("xcodeiios", "Xcode iOS"),
        ("构构建", "构建"),
        ("显显著", "显著"),
    ]
    .into_iter()
    .fold(text.to_string(), |acc, (wrong, correct)| {
        acc.replace(wrong, correct)
    })
}

fn is_repeatable_cjk(ch: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&ch)
}

fn is_phrase_duplicate_candidate(phrase: &[char]) -> bool {
    phrase
        .iter()
        .any(|ch| is_repeatable_cjk(*ch) || ch.is_whitespace())
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

    fn zh_config() -> TextProcessingConfig {
        TextProcessingConfig {
            punct_style: "zh".to_string(),
            insert_punct: true,
            comma_pause_ms: 1_200,
            sentence_pause_ms: 2_800,
            hotwords: vec![Hotword {
                word: "Xcode".to_string(),
                weight: 90,
            }],
        }
    }

    #[test]
    fn normalize_removes_fillers_and_duplicates() {
        let text = "嗯 我我我觉得这个这个方案还还可以";
        let result = normalize_transcript(text);
        assert_eq!(result, "我觉得这个方案还可以");
    }

    #[test]
    fn normalize_keeps_standalone_short_phrase_repetitions() {
        assert_eq!(normalize_transcript("测试测试"), "测试测试");
        assert_eq!(normalize_transcript("测试测试测试"), "测试测试测试");
    }

    #[test]
    fn normalize_still_collapses_embedded_phrase_repetitions() {
        assert_eq!(normalize_transcript("这个这个方案还可以"), "这个方案还可以");
    }

    #[test]
    fn normalize_ascii_tokens_keeps_tech_brands() {
        let text = "依赖 xcode ios github onnx";
        let result = normalize_transcript_with_hotwords(text, &zh_config().hotwords);
        assert!(result.contains("Xcode"));
        assert!(result.contains("iOS"));
        assert!(result.contains("GitHub"));
        assert!(result.contains("ONNX"));
    }

    #[test]
    fn normalize_itn_handles_year_percent_and_money() {
        let text = "二零二五年百分之九十五一千八百元";
        let result = normalize_transcript(text);
        assert!(result.contains("2025年"));
        assert!(result.contains("95%"));
        assert!(result.contains("1800元"));
    }

    #[test]
    fn normalize_corrects_streaming_phrase_confusions() {
        let text = "中文流逝语音识别模型适合低延迟实时转写";
        let result = normalize_transcript(text);
        assert!(result.contains("中文流式语音识别模型"));
    }

    #[test]
    fn segmented_chinese_prefers_pause_boundaries() {
        let result = render_segmented_transcript(
            &[
                TranscriptChunk {
                    text: "如果你的目标是长期稳定生产使用",
                    leading_pause_ms: 0,
                },
                TranscriptChunk {
                    text: "尤其要升级系统",
                    leading_pause_ms: 1_500,
                },
            ],
            Some(TranscriptChunk {
                text: "更建议直接用 Apple 硬件",
                leading_pause_ms: 3_200,
            }),
            None,
            &zh_config(),
            false,
        );
        assert!(result.contains('，'));
        assert!(result.contains('。'));
        assert!(!result.ends_with('。'));

        let final_result = finalize_transcript_text(&result, &zh_config());
        assert!(final_result.ends_with('。'));
    }

    #[test]
    fn segmented_chinese_partial_uses_pause_boundary() {
        let result = render_segmented_transcript(
            &[TranscriptChunk {
                text: "中文流逝语音识别模型",
                leading_pause_ms: 0,
            }],
            None,
            Some(TranscriptChunk {
                text: "适合低延迟实时转写",
                leading_pause_ms: 1_532,
            }),
            &zh_config(),
            false,
        );
        assert!(result.contains("中文流式语音识别模型，适合低延迟实时转写"));
    }

    #[test]
    fn segmented_chinese_sentence_pause_is_configurable() {
        let mut config = zh_config();
        config.sentence_pause_ms = 1_400;

        let result = render_segmented_transcript(
            &[TranscriptChunk {
                text: "中文流逝语音识别模型",
                leading_pause_ms: 0,
            }],
            None,
            Some(TranscriptChunk {
                text: "适合低延迟实时转写",
                leading_pause_ms: 1_532,
            }),
            &config,
            false,
        );
        assert!(result.contains("模型。适合"));
    }

    #[test]
    fn segmented_chinese_suppresses_pause_punctuation_for_tight_phrase() {
        let result = render_segmented_transcript(
            &[TranscriptChunk {
                text: "中文流式语音识别模型适合低延迟",
                leading_pause_ms: 0,
            }],
            None,
            Some(TranscriptChunk {
                text: "实时转写",
                leading_pause_ms: 1_596,
            }),
            &zh_config(),
            false,
        );
        assert!(result.contains("低延迟实时转写"));
        assert!(!result.contains("低延迟，实时转写"));
    }

    #[test]
    fn segmented_chinese_medium_pause_can_start_new_sentence() {
        let result = render_segmented_transcript(
            &[
                TranscriptChunk {
                    text: "中文流逝语音识别模型适合低延迟实时转写",
                    leading_pause_ms: 0,
                },
                TranscriptChunk {
                    text: "中文流逝语音识别模型适合低延迟实时转写",
                    leading_pause_ms: 1_596,
                },
            ],
            None,
            None,
            &zh_config(),
            true,
        );
        assert_eq!(
            result,
            "中文流式语音识别模型，适合低延迟实时转写。中文流式语音识别模型，适合低延迟实时转写。"
        );
    }
}
