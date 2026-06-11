use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use std::time::Duration;

use super::Transcriber;
use crate::types::{Transcript, Utterance};

pub struct AssemblyAi {
    api_key: String,
    client: reqwest::Client,
}

impl AssemblyAi {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct UploadResponse {
    upload_url: String,
}

#[derive(Debug, Deserialize)]
struct AaiTranscript {
    id: String,
    status: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    audio_duration: Option<f64>,
    #[serde(default)]
    language_code: Option<String>,
    #[serde(default)]
    utterances: Option<Vec<AaiUtterance>>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AaiUtterance {
    speaker: String,
    text: String,
    start: u64,
    end: u64,
}

#[async_trait]
impl Transcriber for AssemblyAi {
    fn name(&self) -> &'static str {
        "assemblyai"
    }

    async fn transcribe(&self, audio_path: &Path) -> Result<Transcript> {
        let bytes = std::fs::read(audio_path)
            .with_context(|| format!("reading audio file {}", audio_path.display()))?;

        let upload: UploadResponse = self
            .client
            .post("https://api.assemblyai.com/v2/upload")
            .header("authorization", &self.api_key)
            .body(bytes)
            .send()
            .await
            .context("uploading to AssemblyAI")?
            .error_for_status()?
            .json()
            .await
            .context("parsing upload response")?;

        let submit_body = json!({
            "audio_url": upload.upload_url,
            "speech_model": "universal",
            "speaker_labels": true,
            "punctuate": true,
            "format_text": true,
        });

        let submitted: AaiTranscript = self
            .client
            .post("https://api.assemblyai.com/v2/transcript")
            .header("authorization", &self.api_key)
            .json(&submit_body)
            .send()
            .await
            .context("submitting transcript job")?
            .error_for_status()?
            .json()
            .await
            .context("parsing submit response")?;

        let poll_url = format!("https://api.assemblyai.com/v2/transcript/{}", submitted.id);
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let t: AaiTranscript = self
                .client
                .get(&poll_url)
                .header("authorization", &self.api_key)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            match t.status.as_str() {
                "completed" => {
                    let utterances = t
                        .utterances
                        .unwrap_or_default()
                        .into_iter()
                        .map(|u| Utterance {
                            speaker: format!("speaker_{}", u.speaker),
                            text: u.text,
                            start_ms: u.start,
                            end_ms: u.end,
                        })
                        .collect();
                    return Ok(Transcript {
                        provider: "assemblyai".to_string(),
                        duration_seconds: t.audio_duration.unwrap_or(0.0),
                        language: t.language_code,
                        utterances,
                        full_text: t.text.unwrap_or_default(),
                    });
                }
                "error" => anyhow::bail!(
                    "AssemblyAI transcription error: {}",
                    t.error.unwrap_or_else(|| "unknown".into())
                ),
                _ => continue,
            }
        }
    }
}
