use anyhow::{Context, Result};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use recall::analyze::ClaudeAnalyzer;
use recall::cli::{Cli, Format, Provider};
use recall::config::{load_analysis, Keys};
use recall::doc::{build_doc, to_docx, to_html, to_markdown};
use recall::report::render_transcript_markdown;
use recall::transcribe::{
    assemblyai::AssemblyAi, deepgram::Deepgram, elevenlabs::ElevenLabs, Transcriber,
};
use recall::types::{Analysis, Transcript};

const AUDIO_EXTS: &[&str] = &["mp3", "wav", "m4a", "flac", "ogg", "webm", "mp4"];

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "recall=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let keys = Keys::from_env()?;

    let transcriber = build_transcriber(cli.provider, &keys)?;
    let analyzer = ClaudeAnalyzer::new(keys.anthropic.clone());
    let analysis = load_analysis(&cli.analysis)?;

    std::fs::create_dir_all(&cli.out)?;
    std::fs::create_dir_all(&cli.transcript_cache)?;

    let audio_files = collect_audio_files(&cli.input)?;
    if audio_files.is_empty() {
        anyhow::bail!("no audio files found in {}", cli.input.display());
    }

    tracing::info!("found {} recording(s) to process", audio_files.len());

    for audio in audio_files {
        if let Err(e) = process_one(
            &audio,
            transcriber.as_ref(),
            &analyzer,
            &analysis,
            &cli.transcript_cache,
            &cli.out,
            &cli.formats,
            cli.force,
        )
        .await
        {
            tracing::error!("failed on {}: {:#}", audio.display(), e);
        }
    }

    Ok(())
}

fn build_transcriber(provider: Provider, keys: &Keys) -> Result<Arc<dyn Transcriber>> {
    Ok(match provider {
        Provider::ElevenLabs => {
            let key = keys.elevenlabs.clone().context("ELEVENLABS_API_KEY not set")?;
            Arc::new(ElevenLabs::new(key))
        }
        Provider::Deepgram => {
            let key = keys.deepgram.clone().context("DEEPGRAM_API_KEY not set")?;
            Arc::new(Deepgram::new(key))
        }
        Provider::AssemblyAi => {
            let key = keys.assemblyai.clone().context("ASSEMBLYAI_API_KEY not set")?;
            Arc::new(AssemblyAi::new(key))
        }
    })
}

async fn process_one(
    audio: &Path,
    transcriber: &dyn Transcriber,
    analyzer: &ClaudeAnalyzer,
    analysis: &Analysis,
    transcript_cache: &Path,
    out_dir: &Path,
    formats: &[Format],
    force: bool,
) -> Result<()> {
    let stem = audio
        .file_stem()
        .and_then(|s| s.to_str())
        .context("invalid audio file name")?;
    let cache_path = transcript_cache.join(format!("{}.{}.json", stem, transcriber.name()));

    let transcript = if !force && cache_path.exists() {
        tracing::info!("using cached transcript {}", cache_path.display());
        let raw = std::fs::read_to_string(&cache_path)?;
        serde_json::from_str::<Transcript>(&raw)?
    } else {
        tracing::info!("transcribing {} via {}", audio.display(), transcriber.name());
        let t = transcriber.transcribe(audio).await?;
        std::fs::write(&cache_path, serde_json::to_string_pretty(&t)?)?;
        t
    };

    let transcript_md_path = transcript_cache.join(format!("{}.{}.md", stem, transcriber.name()));
    std::fs::write(
        &transcript_md_path,
        render_transcript_markdown(&transcript, stem),
    )?;
    tracing::info!("wrote {}", transcript_md_path.display());

    tracing::info!("analyzing transcript ({} utterances)", transcript.utterances.len());
    let report = analyzer.analyze(&transcript, analysis, stem).await?;
    let doc = build_doc(&report, analysis);
    for &fmt in formats {
        let report_path = out_dir.join(format!("{}.{}", stem, fmt.ext()));
        let bytes = match fmt {
            Format::Md => to_markdown(&doc).into_bytes(),
            Format::Html => to_html(&doc).into_bytes(),
            Format::Docx => to_docx(&doc)?,
        };
        std::fs::write(&report_path, bytes)?;
        tracing::info!("wrote {}", report_path.display());
    }
    Ok(())
}

fn collect_audio_files(input: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if input.is_file() {
        out.push(input.to_path_buf());
        return Ok(out);
    }
    for entry in std::fs::read_dir(input)? {
        let entry = entry?;
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).map(|s| s.to_lowercase());
        if let Some(ext) = ext {
            if AUDIO_EXTS.contains(&ext.as_str()) {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}
