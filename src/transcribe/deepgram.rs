use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

use super::Transcriber;
use crate::types::{Transcript, Utterance};

pub struct Deepgram {
    api_key: String,
    client: reqwest::Client,
}

impl Deepgram {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct DgResponse {
    results: DgResults,
}

#[derive(Debug, Deserialize)]
struct DgResults {
    channels: Vec<DgChannel>,
}

#[derive(Debug, Deserialize)]
struct DgChannel {
    alternatives: Vec<DgAlt>,
}

#[derive(Debug, Deserialize)]
struct DgAlt {
    transcript: String,
    #[serde(default)]
    paragraphs: Option<DgParagraphs>,
    #[serde(default)]
    words: Vec<DgWord>,
}

#[derive(Debug, Deserialize)]
struct DgParagraphs {
    paragraphs: Vec<DgParagraph>,
}

#[derive(Debug, Deserialize)]
struct DgParagraph {
    sentences: Vec<DgSentence>,
    #[serde(default)]
    speaker: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct DgSentence {
    text: String,
    start: f64,
    end: f64,
}

#[derive(Debug, Deserialize)]
struct DgWord {
    end: f64,
}

#[async_trait]
impl Transcriber for Deepgram {
    fn name(&self) -> &'static str {
        "deepgram"
    }

    async fn transcribe(&self, audio_path: &Path) -> Result<Transcript> {
        let bytes = std::fs::read(audio_path)
            .with_context(|| format!("reading audio file {}", audio_path.display()))?;
        let mime = guess_mime(audio_path);

        let url = "https://api.deepgram.com/v1/listen\
            ?model=nova-3&smart_format=true&diarize=true&punctuate=true&paragraphs=true";

        let resp = self
            .client
            .post(url)
            .header("Authorization", format!("Token {}", self.api_key))
            .header("Content-Type", mime)
            .body(bytes)
            .send()
            .await
            .context("sending request to Deepgram")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Deepgram error {}: {}", status, body);
        }

        let parsed: DgResponse = resp.json().await.context("parsing Deepgram response")?;
        let alt = parsed
            .results
            .channels
            .into_iter()
            .next()
            .and_then(|c| c.alternatives.into_iter().next())
            .context("no alternatives in Deepgram response")?;

        let duration_seconds = alt.words.last().map(|w| w.end).unwrap_or(0.0);
        let mut utterances = Vec::new();
        if let Some(paragraphs) = alt.paragraphs {
            for p in paragraphs.paragraphs {
                let speaker = p
                    .speaker
                    .map(|s| format!("speaker_{}", s))
                    .unwrap_or_else(|| "unknown".to_string());
                let text = p
                    .sentences
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                let start_ms = p.sentences.first().map(|s| (s.start * 1000.0) as u64).unwrap_or(0);
                let end_ms = p.sentences.last().map(|s| (s.end * 1000.0) as u64).unwrap_or(0);
                utterances.push(Utterance { speaker, text, start_ms, end_ms });
            }
        }

        Ok(Transcript {
            provider: "deepgram".to_string(),
            duration_seconds,
            language: None,
            utterances,
            full_text: alt.transcript,
        })
    }
}

fn guess_mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|s| s.to_lowercase()).as_deref() {
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("m4a") => "audio/mp4",
        Some("flac") => "audio/flac",
        Some("ogg") => "audio/ogg",
        Some("webm") => "audio/webm",
        _ => "application/octet-stream",
    }
}
