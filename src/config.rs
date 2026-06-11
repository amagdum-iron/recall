use anyhow::{Context, Result};
use std::env;
use std::path::Path;

use crate::types::Analysis;

pub struct Keys {
    pub anthropic: String,
    pub elevenlabs: Option<String>,
    pub deepgram: Option<String>,
    pub assemblyai: Option<String>,
}

impl Keys {
    pub fn from_env() -> Result<Self> {
        let _ = dotenvy::dotenv();
        Ok(Self {
            anthropic: env::var("ANTHROPIC_API_KEY")
                .context("ANTHROPIC_API_KEY not set (required for analysis)")?,
            elevenlabs: env::var("ELEVENLABS_API_KEY").ok(),
            deepgram: env::var("DEEPGRAM_API_KEY").ok(),
            assemblyai: env::var("ASSEMBLYAI_API_KEY").ok(),
        })
    }
}

pub fn load_analysis(path: &Path) -> Result<Analysis> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading analysis spec at {}", path.display()))?;
    let a: Analysis = serde_yaml::from_str(&raw).context("parsing analysis YAML")?;
    Ok(a)
}
