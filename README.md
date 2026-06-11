# recall

Analyze call recordings against a configurable analysis spec.

`recall` takes a directory of audio recordings (sales calls, support calls, …), transcribes them with a speech-to-text provider, then runs a multi-stage Claude analysis driven by a YAML spec — answering questions, scoring rubrics, categorizing buying signals, classifying outcomes — and writes evidence-grounded reports as Markdown, HTML, or DOCX.

## How it works

For each recording:

1. **Transcribe** — the audio is sent to the chosen provider (ElevenLabs, Deepgram, or AssemblyAI) and the diarized transcript is cached as JSON (plus a readable `.md` rendering) in `transcripts/`. Cached transcripts are reused on subsequent runs unless `--force` is passed.
2. **Stage 0: speaker identification** — Claude maps each diarized speaker ID to a role (`sales_rep`, `prospect`, `internal_other`, `unknown`), with a display name, sub-role, and rationale.
3. **Stage 1: per-role analysis** — sections from the YAML spec are grouped by their `role` (`rep`, `prospect`, `both`) and run as parallel Claude calls. Role-scoped sections may only cite evidence from the matching speakers. The transcript is shared across calls via prompt caching.
4. **Stage 2: synthesis** — sections with `role: synthesis` (e.g. an executive summary or lost-deal classification) are produced from the prior sections' outputs.
5. **Report** — results are rendered to `reports/` in the requested format(s).

Every claim in the output is backed by verbatim transcript quotes with speaker and timestamp. Structured output is enforced with JSON schemas generated from the spec, so required questions, criteria, and categories can't be silently skipped.

## Requirements

- Rust (2021 edition) and Cargo
- An Anthropic API key
- An API key for at least one transcription provider (ElevenLabs, Deepgram, or AssemblyAI)

## Setup

```sh
cp .env.example .env
# then fill in your keys:
#   ANTHROPIC_API_KEY=sk-ant-...
#   ELEVENLABS_API_KEY=...   (and/or DEEPGRAM_API_KEY / ASSEMBLYAI_API_KEY)
```

## Usage

```sh
# Analyze every recording in a directory with the Iron Software sales spec
cargo run --release -- \
  --input recordings \
  --analysis analyses/iron-software-sales.yaml

# A single file, DOCX + HTML output, using Deepgram
cargo run --release -- \
  -i recordings/new_license_01.mp4 \
  -a analyses/iron-software-sales.yaml \
  -f docx -f html \
  -p deepgram
```

### Options

| Flag | Default | Description |
|---|---|---|
| `-i, --input` | (required) | Directory of audio files, or a single file. Supported extensions: `mp3`, `wav`, `m4a`, `flac`, `ogg`, `webm`, `mp4`. |
| `-a, --analysis` | (required) | Path to the analysis YAML spec. |
| `-o, --out` | `reports` | Output directory for reports. |
| `-f, --format` | `md` | Report format: `md`, `html`, `docx`. Repeatable. |
| `-p, --provider` | `eleven-labs` | Transcription provider: `eleven-labs`, `deepgram`, `assembly-ai`. |
| `--transcript-cache` | `transcripts` | Directory for cached transcripts. |
| `--force` | off | Re-transcribe even if a cached transcript exists. |

Logging is controlled with `RUST_LOG` (defaults to `recall=info`).

## Analysis specs

An analysis spec is a YAML file with a `name` and a list of `sections`. Each section has an `id`, a `title`, a `type`, and optionally a `role` and `optional: true`. See `analyses/iron-software-sales.yaml` for a complete example: a full sales analysis with rubric scoring, buying-intent signals, lost-deal classification, and an executive summary.

### Section types

| Type | Output |
|---|---|
| `questions` | An answer per question, with evidence quotes and a confidence level. |
| `scored_rubric` | A 1–10 score per criterion, with rationale and evidence. |
| `signal_categorization` | Per category: a strength rating (`high`/`medium`/`low`/`none`) and the supporting signals with quotes. |
| `classification` | One primary option (optionally a secondary), with notes and evidence. Returns null when not applicable. |
| `summary` | A key-value object of free-text, score, or enum fields. |

### Section roles

| Role | Behavior |
|---|---|
| `rep` | Evidence may only be cited from speakers identified as the sales rep / vendor side. |
| `prospect` | Evidence may only be cited from prospect-side speakers. |
| `both` (default) | No speaker filter. |
| `synthesis` | Produced in a final pass from the other sections' outputs (e.g. executive summary, lost-deal reason). |

### Example

```yaml
name: Sales Discovery Call — v1

sections:
  - id: discovery_questions
    type: questions
    title: Discovery Call Review
    questions:
      - id: budget_discussed
        prompt: Was budget discussed, and if so, what range?
        guidance: Look for explicit dollar amounts or ranges. "We have budget" is partial.
      - id: next_steps
        prompt: Was a concrete next step booked (demo, follow-up call, proposal)?
```

Validate your specs without spending any API calls:

```sh
cargo run --example validate_analyses
```

## Project layout

```
src/
  main.rs        CLI entry point and per-recording pipeline
  cli.rs         clap argument definitions
  config.rs      .env keys and analysis-spec loading
  transcribe/    Transcriber trait + ElevenLabs, Deepgram, AssemblyAI backends
  analyze.rs     Multi-stage Claude analysis (speaker ID → per-role → synthesis)
  types.rs       Transcript, analysis-spec, and report types
  doc.rs         Report document model + Markdown/HTML/DOCX rendering
  report.rs      Transcript Markdown rendering
analyses/        Example analysis specs
recordings/      Input audio (gitignored in practice)
transcripts/     Cached transcripts (JSON + Markdown)
reports/         Generated reports
```
