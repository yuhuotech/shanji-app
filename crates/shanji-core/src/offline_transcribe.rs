use crate::asr::{AsrConfig, AsrEngine};
use crate::error::Result;
use crate::fsmn_vad::{FsmnVadSegment, FsmnVadSegmenter};
use crate::model;
use crate::paths::AppPaths;
use crate::punc::CtPuncModel;
use crate::text_processing::{
    finalize_existing_punctuation_text, finalize_transcript_text,
    normalize_transcript_with_hotwords, TextProcessingConfig,
};

#[derive(Debug, Clone)]
pub struct OfflineTranscriptionResult {
    pub raw_text: String,
    pub punctuated_text: Option<String>,
    pub final_text: String,
    pub segment_count: usize,
    pub used_vad: bool,
    pub used_punc: bool,
}

pub struct OfflineTranscriber {
    asr_engine: AsrEngine,
    vad: Option<FsmnVadSegmenter>,
    punc: Option<CtPuncModel>,
    text_config: TextProcessingConfig,
}

impl OfflineTranscriber {
    pub fn new(
        paths: &AppPaths,
        model_id: &str,
        text_config: TextProcessingConfig,
    ) -> Result<Self> {
        let mut asr_engine = AsrEngine::new(AsrConfig {
            insert_punct: text_config.insert_punct,
            punct_style: text_config.punct_style.clone(),
            hotwords: text_config.hotwords.clone(),
            ..AsrConfig::default()
        })?;
        let layout = model::resolve_model_layout_with_paths(paths, model_id)?;
        asr_engine.load_model(&layout)?;

        let vad = if model::is_model_downloaded_with_paths(paths, "fsmn-vad") {
            let model_dir = model::get_model_dir_with_paths(paths, "fsmn-vad");
            match FsmnVadSegmenter::new(&model_dir) {
                Ok(segmenter) => Some(segmenter),
                Err(err) => {
                    log::warn!(
                        "Failed to initialize FSMN VAD, falling back to full audio: {}",
                        err
                    );
                    None
                }
            }
        } else {
            None
        };

        let punc = if text_config.insert_punct
            && model::is_model_downloaded_with_paths(paths, "ct-punc")
        {
            let model_dir = model::get_model_dir_with_paths(paths, "ct-punc");
            match CtPuncModel::new(&model_dir) {
                Ok(model) => Some(model),
                Err(err) => {
                    log::warn!(
                        "Failed to initialize ct-punc, falling back to heuristic punctuation: {}",
                        err
                    );
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            asr_engine,
            vad,
            punc,
            text_config,
        })
    }

    pub fn update_text_config(&mut self, text_config: TextProcessingConfig) {
        self.asr_engine.reconfigure(AsrConfig {
            insert_punct: text_config.insert_punct,
            punct_style: text_config.punct_style.clone(),
            hotwords: text_config.hotwords.clone(),
            ..AsrConfig::default()
        });
        self.text_config = text_config;
    }

    pub fn transcribe(
        &mut self,
        samples: &[f32],
        is_final: bool,
    ) -> Result<OfflineTranscriptionResult> {
        if samples.is_empty() {
            return Ok(OfflineTranscriptionResult {
                raw_text: String::new(),
                punctuated_text: None,
                final_text: String::new(),
                segment_count: 0,
                used_vad: self.vad.is_some(),
                used_punc: self.punc.is_some(),
            });
        }

        let spans = self.segment_audio(samples);
        let mut raw_segments = Vec::new();
        for span in &spans {
            let text = self.transcribe_span(samples, span)?;
            if !text.trim().is_empty() {
                raw_segments.push(text);
            }
        }

        if raw_segments.is_empty() {
            let fallback = self.transcribe_span(
                samples,
                &FsmnVadSegment {
                    start_ms: 0,
                    end_ms: (samples.len() as u32 * 1000 / 16_000u32),
                    start_sample: 0,
                    end_sample: samples.len(),
                },
            )?;
            if !fallback.trim().is_empty() {
                raw_segments.push(fallback);
            }
        }

        let raw_text = join_transcript_fragments(&raw_segments);
        let punctuated_text = if raw_text.is_empty() {
            None
        } else if let Some(punc) = self.punc.as_mut() {
            match punc.restore(&raw_text, is_final) {
                Ok(text) if !text.trim().is_empty() => Some(text),
                Ok(_) => None,
                Err(err) => {
                    log::warn!(
                        "ct-punc failed, using heuristic punctuation fallback: {}",
                        err
                    );
                    None
                }
            }
        } else {
            None
        };

        let final_text = if let Some(text) = punctuated_text.as_deref() {
            if is_final {
                finalize_existing_punctuation_text(text, &self.text_config)
            } else {
                normalize_transcript_with_hotwords(text, &self.text_config.hotwords)
            }
        } else {
            let normalized =
                normalize_transcript_with_hotwords(&raw_text, &self.text_config.hotwords);
            if is_final {
                finalize_transcript_text(&normalized, &self.text_config)
            } else {
                normalized
            }
        };
        let used_punc = punctuated_text.is_some();

        Ok(OfflineTranscriptionResult {
            raw_text,
            punctuated_text,
            final_text,
            segment_count: raw_segments.len(),
            used_vad: self.vad.is_some(),
            used_punc,
        })
    }

    fn segment_audio(&mut self, samples: &[f32]) -> Vec<FsmnVadSegment> {
        let Some(vad) = self.vad.as_mut() else {
            return vec![FsmnVadSegment {
                start_ms: 0,
                end_ms: samples.len() as u32 * 1000 / 16_000u32,
                start_sample: 0,
                end_sample: samples.len(),
            }];
        };

        match vad.segment(samples) {
            Ok(segments) if !segments.is_empty() => segments,
            Ok(_) => vec![FsmnVadSegment {
                start_ms: 0,
                end_ms: samples.len() as u32 * 1000 / 16_000u32,
                start_sample: 0,
                end_sample: samples.len(),
            }],
            Err(err) => {
                log::warn!(
                    "FSMN VAD failed during offline transcription, falling back to full audio: {}",
                    err
                );
                vec![FsmnVadSegment {
                    start_ms: 0,
                    end_ms: samples.len() as u32 * 1000 / 16_000u32,
                    start_sample: 0,
                    end_sample: samples.len(),
                }]
            }
        }
    }

    fn transcribe_span(&mut self, samples: &[f32], span: &FsmnVadSegment) -> Result<String> {
        if span.end_sample <= span.start_sample || span.end_sample > samples.len() {
            return Ok(String::new());
        }

        self.asr_engine.reset();
        let chunk = &samples[span.start_sample..span.end_sample];
        let _ = self.asr_engine.process_chunk(chunk)?;
        self.asr_engine.finalize()
    }
}

fn join_transcript_fragments(fragments: &[String]) -> String {
    let mut out = String::new();

    for fragment in fragments
        .iter()
        .map(|fragment| fragment.trim())
        .filter(|fragment| !fragment.is_empty())
    {
        if !out.is_empty() && needs_ascii_separator(&out, fragment) {
            out.push(' ');
        }
        out.push_str(fragment);
    }

    out
}

fn needs_ascii_separator(previous: &str, next: &str) -> bool {
    let prev = previous.chars().last();
    let next = next.chars().next();
    matches!(
        (prev, next),
        (Some(left), Some(right))
            if left.is_ascii_alphanumeric() && right.is_ascii_alphanumeric()
    )
}

#[cfg(test)]
mod tests {
    use super::{join_transcript_fragments, needs_ascii_separator};

    #[test]
    fn join_transcript_fragments_inserts_ascii_space() {
        let joined = join_transcript_fragments(&["system".to_string(), "two".to_string()]);
        assert_eq!(joined, "system two");
    }

    #[test]
    fn join_transcript_fragments_keeps_chinese_compact() {
        let joined = join_transcript_fragments(&["现在".to_string(), "开始".to_string()]);
        assert_eq!(joined, "现在开始");
    }

    #[test]
    fn ascii_separator_requires_word_boundaries() {
        assert!(needs_ascii_separator("hello", "world"));
        assert!(!needs_ascii_separator("你好", "world"));
    }
}
