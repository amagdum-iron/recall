use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::multipart;
use serde::Deserialize;
use std::path::Path;

use super::Transcriber;
use crate::types::{Transcript, Utterance};

pub struct ElevenLabs {
    api_key: String,
    client: reqwest::Client,
}

impl ElevenLabs {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ScribeResponse {
    text: String,
    #[serde(default)]
    language_code: Option<String>,
    #[serde(default)]
    words: Vec<Word>,
}

#[derive(Debug, Deserialize)]
struct Word {
    text: String,
    start: f64,
    end: f64,
    #[serde(default)]
    speaker_id: Option<String>,
    #[serde(rename = "type", default)]
    word_type: Option<String>,
}

#[async_trait]
impl Transcriber for ElevenLabs {
    fn name(&self) -> &'static str {
        "elevenlabs"
    }

    async fn transcribe(&self, audio_path: &Path) -> Result<Transcript> {
        let bytes = std::fs::read(audio_path)
            .with_context(|| format!("reading audio file {}", audio_path.display()))?;
        let filename = audio_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio")
            .to_string();

        let part = multipart::Part::bytes(bytes).file_name(filename);
        let form = multipart::Form::new()
            .text("model_id", "scribe_v1")
            .text("diarize", "true")
            .text("timestamps_granularity", "word")
            .part("file", part);

        let resp = self
            .client
            .post("https://api.elevenlabs.io/v1/speech-to-text")
            .header("xi-api-key", &self.api_key)
            .multipart(form)
            .send()
            .await
            .context("sending request to ElevenLabs")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("ElevenLabs error {}: {}", status, body);
        }

        let parsed: ScribeResponse = resp.json().await.context("parsing ElevenLabs response")?;

        let utterances = group_words_into_utterances(&parsed.words);
        let duration_seconds = parsed.words.last().map(|w| w.end).unwrap_or(0.0);

        Ok(Transcript {
            provider: "elevenlabs".to_string(),
            duration_seconds,
            language: parsed.language_code,
            utterances,
            full_text: parsed.text,
        })
    }
}

fn group_words_into_utterances(words: &[Word]) -> Vec<Utterance> {
    let mut utterances = Vec::new();
    let mut current: Option<Utterance> = None;

    for w in words {
        if w.word_type.as_deref() == Some("spacing") {
            if let Some(u) = current.as_mut() {
                u.text.push_str(&w.text);
            }
            continue;
        }
        let speaker = w.speaker_id.clone().unwrap_or_else(|| "unknown".to_string());
        let start_ms = (w.start * 1000.0) as u64;
        let end_ms = (w.end * 1000.0) as u64;

        match current.as_mut() {
            Some(u) if u.speaker == speaker => {
                u.text.push_str(&w.text);
                u.end_ms = end_ms;
            }
            _ => {
                if let Some(u) = current.take() {
                    utterances.push(u);
                }
                current = Some(Utterance {
                    speaker,
                    text: w.text.clone(),
                    start_ms,
                    end_ms,
                });
            }
        }
    }
    if let Some(u) = current {
        utterances.push(u);
    }
    for u in &mut utterances {
        u.text = u.text.trim().to_string();
    }
    utterances
}
