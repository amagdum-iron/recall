use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "recall", about = "Analyze call recordings against an analysis spec")]
pub struct Cli {
    /// Directory containing audio files (or a single file)
    #[arg(short, long)]
    pub input: PathBuf,

    /// Path to the analysis YAML
    #[arg(short, long)]
    pub analysis: PathBuf,

    /// Output directory for reports
    #[arg(short, long, default_value = "reports")]
    pub out: PathBuf,

    /// Report format(s) to write; repeatable (e.g. `-f html -f docx`)
    #[arg(short = 'f', long = "format", value_enum, default_values_t = [Format::Md])]
    pub formats: Vec<Format>,

    /// Directory to cache transcripts (skips re-transcription)
    #[arg(long, default_value = "transcripts")]
    pub transcript_cache: PathBuf,

    /// Transcription provider
    #[arg(short = 'p', long, value_enum, default_value_t = Provider::ElevenLabs)]
    pub provider: Provider,

    /// Force re-transcription even if cache exists
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Provider {
    ElevenLabs,
    Deepgram,
    AssemblyAi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Md,
    Html,
    Docx,
}

impl Format {
    pub fn ext(self) -> &'static str {
        match self {
            Format::Md => "md",
            Format::Html => "html",
            Format::Docx => "docx",
        }
    }
}
