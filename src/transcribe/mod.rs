pub mod assemblyai;
pub mod deepgram;
pub mod elevenlabs;

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

use crate::types::Transcript;

#[async_trait]
pub trait Transcriber: Send + Sync {
    fn name(&self) -> &'static str;
    async fn transcribe(&self, audio_path: &Path) -> Result<Transcript>;
}
