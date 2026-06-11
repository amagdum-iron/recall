use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::types::{
    Analysis, AnalysisReport, Role, Section, SectionData, SectionResult, SectionSpec, SpeakerInfo,
    SpeakerMap, SpeakerRole, SummaryFieldKind, Transcript,
};

const MODEL: &str = "claude-opus-4-7";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct ClaudeAnalyzer {
    api_key: String,
    client: reqwest::Client,
}

impl ClaudeAnalyzer {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .expect("reqwest client"),
        }
    }

    pub async fn analyze(
        &self,
        transcript: &Transcript,
        analysis: &Analysis,
        recording_name: &str,
    ) -> Result<AnalysisReport> {
        // Stage 0: speaker identification.
        let speakers = self.identify_speakers(transcript).await?;
        log_speakers(&speakers);

        // Stage 1: per-role analyses, in parallel.
        let rep_sections: Vec<&Section> = analysis
            .sections
            .iter()
            .filter(|s| s.role == Role::Rep)
            .collect();
        let prospect_sections: Vec<&Section> = analysis
            .sections
            .iter()
            .filter(|s| s.role == Role::Prospect)
            .collect();
        let both_sections: Vec<&Section> = analysis
            .sections
            .iter()
            .filter(|s| s.role == Role::Both)
            .collect();
        let synthesis_sections: Vec<&Section> = analysis
            .sections
            .iter()
            .filter(|s| s.role == Role::Synthesis)
            .collect();

        let (rep_results, prospect_results, both_results) = tokio::try_join!(
            self.analyze_group(Role::Rep, &rep_sections, transcript, &speakers),
            self.analyze_group(Role::Prospect, &prospect_sections, transcript, &speakers),
            self.analyze_group(Role::Both, &both_sections, transcript, &speakers),
        )?;

        // Stage 2: synthesis.
        let prior: Vec<&SectionResult> = rep_results
            .iter()
            .chain(prospect_results.iter())
            .chain(both_results.iter())
            .collect();
        let synthesis_results = self
            .synthesize(&synthesis_sections, &speakers, &prior)
            .await?;

        // Recombine in YAML order.
        let mut by_id: std::collections::HashMap<String, SectionResult> =
            std::collections::HashMap::new();
        for r in rep_results
            .into_iter()
            .chain(prospect_results)
            .chain(both_results)
            .chain(synthesis_results)
        {
            by_id.insert(r.id.clone(), r);
        }
        let ordered: Vec<SectionResult> = analysis
            .sections
            .iter()
            .filter_map(|s| by_id.remove(&s.id))
            .collect();

        Ok(AnalysisReport {
            recording_name: recording_name.to_string(),
            duration_seconds: transcript.duration_seconds,
            analysis_name: analysis.name.clone(),
            speakers,
            sections: ordered,
        })
    }

    // ---------- Stage 0: speaker identification ----------

    async fn identify_speakers(&self, transcript: &Transcript) -> Result<SpeakerMap> {
        let observed = observed_speaker_ids(transcript);
        let schema = speaker_map_schema(&observed);
        let user_prompt = format!(
            "Identify the role of each speaker in the transcript above.\n\n\
            Observed speaker IDs: {}\n\n\
            Return a `speakers` object whose keys are exactly the speaker IDs listed above \
            (one entry per ID, no duplicates, no missing IDs). For each, decide:\n\
            - role: one of `sales_rep` (anyone on the vendor side: AE, BDR, SDR, CSM, \
            solutions engineer, support agent, etc.), `prospect` (customer / buyer side), \
            `internal_other` (vendor-side but not selling, e.g. note-taker, observer), \
            or `unknown` (cannot tell).\n\
            - display_name: the person's first name if they are addressed by name, otherwise null.\n\
            - sub_role: a short free-form description of their specific function \
            (\"solutions engineer\", \"account executive\", \"support agent\", \"customer\", etc.) \
            or null if unclear.\n\
            - rationale: one sentence grounding your call in specific transcript signals \
            (who pitches vs evaluates, who says \"our\" vs \"your\" product, who introduces whom, etc.).\n\n\
            Prefer `unknown` over a guess.",
            observed.join(", ")
        );

        let body = json!({
            "model": MODEL,
            "max_tokens": 4000,
            "thinking": { "type": "adaptive" },
            "output_config": {
                "effort": "medium",
                "format": { "type": "json_schema", "schema": schema }
            },
            "system": shared_system_blocks(transcript, None),
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": user_prompt }
                ]}
            ]
        });

        let text = self.call_claude(body, "stage 0 speaker identification").await?;
        let parsed: Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing speaker-id JSON: {}", text))?;
        decode_speaker_map(&parsed, &observed)
    }

    // ---------- Stage 1: per-role analysis ----------

    async fn analyze_group(
        &self,
        role: Role,
        sections: &[&Section],
        transcript: &Transcript,
        speakers: &SpeakerMap,
    ) -> Result<Vec<SectionResult>> {
        if sections.is_empty() {
            return Ok(Vec::new());
        }

        let owned: Vec<Section> = sections.iter().map(|s| (*s).clone()).collect();
        let spec_block = build_spec_block(&owned);
        let schema = build_sections_schema(&owned);
        let user_prompt = build_role_user_prompt(role, &owned, speakers);

        let body = json!({
            "model": MODEL,
            "max_tokens": 16000,
            "thinking": { "type": "adaptive" },
            "output_config": {
                "effort": "high",
                "format": { "type": "json_schema", "schema": schema }
            },
            "system": shared_system_blocks(transcript, Some(&spec_block)),
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": user_prompt }
                ]}
            ]
        });

        let text = self
            .call_claude(body, &format!("stage 1 ({:?})", role))
            .await?;
        let parsed: Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing stage-1 JSON: {}", text))?;
        decode_sections(&owned, &parsed, Some(transcript))
    }

    // ---------- Stage 2: synthesis ----------

    async fn synthesize(
        &self,
        sections: &[&Section],
        speakers: &SpeakerMap,
        prior: &[&SectionResult],
    ) -> Result<Vec<SectionResult>> {
        if sections.is_empty() {
            return Ok(Vec::new());
        }

        let owned: Vec<Section> = sections.iter().map(|s| (*s).clone()).collect();
        let spec_block = build_spec_block(&owned);
        let schema = build_sections_schema(&owned);
        let prior_json = serde_json::to_string_pretty(prior)
            .context("serializing prior outputs for synthesis")?;
        let speaker_block = render_speaker_block(speakers);
        let user_prompt = format!(
            "You are producing the synthesis sections of an analysis. Prior stages already \
            produced grounded findings; your job is to summarize and judge.\n\n\
            {speaker_block}\n\
            Prior analysis outputs (JSON):\n\n```json\n{prior_json}\n```\n\n\
            Produce the synthesis sections defined in the spec.\n\
            - You may reuse evidence quotes from the prior outputs.\n\
            - For optional classifications (e.g. a lost-deal reason on a call that did not end \
            in a lost deal), return null for the whole section.\n\
            - Be specific and decisive in summary fields; do not hedge with empty strings."
        );

        let body = json!({
            "model": MODEL,
            "max_tokens": 8000,
            "thinking": { "type": "adaptive" },
            "output_config": {
                "effort": "medium",
                "format": { "type": "json_schema", "schema": schema }
            },
            "system": [
                { "type": "text", "text": system_preamble() },
                { "type": "text", "text": spec_block }
            ],
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": user_prompt }
                ]}
            ]
        });

        let text = self.call_claude(body, "stage 2 synthesis").await?;
        let parsed: Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing stage-2 JSON: {}", text))?;
        decode_sections(&owned, &parsed, None)
    }

    // ---------- Shared HTTP call ----------

    async fn call_claude(&self, body: Value, label: &str) -> Result<String> {
        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("calling Anthropic API ({label})"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic API error {} ({label}): {}", status, body);
        }

        let raw = resp
            .text()
            .await
            .with_context(|| format!("reading response body ({label})"))?;
        tracing::debug!(stage = label, "raw response: {}", raw);

        let parsed: ClaudeResponse = serde_json::from_str(&raw)
            .with_context(|| format!("parsing Anthropic response ({label})"))?;

        tracing::info!(
            stage = label,
            stop_reason = parsed.stop_reason.as_deref().unwrap_or("?"),
            "Claude stop_reason"
        );
        if let Some(usage) = &parsed.usage {
            tracing::info!(
                stage = label,
                input_tokens = usage.input_tokens,
                output_tokens = usage.output_tokens,
                cache_creation = usage.cache_creation_input_tokens.unwrap_or(0),
                cache_read = usage.cache_read_input_tokens.unwrap_or(0),
                "Claude usage"
            );
        }
        if parsed.stop_reason.as_deref() == Some("refusal") {
            anyhow::bail!("Claude refused ({label}). stop_details: {:?}", parsed.stop_details);
        }
        if parsed.stop_reason.as_deref() == Some("max_tokens") {
            tracing::warn!(stage = label, "response truncated by max_tokens");
        }

        let text = parsed
            .content
            .iter()
            .find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .with_context(|| format!("no text block in response ({label}): {}", raw))?;
        Ok(text)
    }
}

// ---------- Response wire types ----------

#[derive(Debug, Deserialize)]
struct ClaudeResponse {
    content: Vec<ContentBlock>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_details: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
}

// ---------- Prompt building ----------

fn system_preamble() -> String {
    "You are an expert call analyst. You are given a diarized transcript of a \
    recorded conversation and an analysis spec describing sections to produce. \
    Each section has a `type` that determines the expected output shape. Produce \
    structured output that conforms exactly to the JSON schema described in the spec.\n\n\
    Core rules:\n\
    - Ground every claim in the transcript. Evidence quotes must be verbatim, with the \
    speaker label exactly as it appears (e.g. \"speaker_0\") and the `start_ms` from \
    that utterance.\n\
    - Never invent facts. If something is not discussed, say so honestly using the \
    section type's mechanism (low confidence, score reflecting absence, strength=\"none\", etc.).\n\
    - Never omit a required item from the schema. If a criterion / question / category \
    cannot be assessed, produce it with a value that reflects the absence \
    (score 1 + \"not discussed\" rationale; confidence=low; strength=\"none\"; empty signals array).\n\
    - For optional sections that do not apply (e.g. lost-deal analysis on a won or in-progress call), \
    return null.\n\
    - Be concise and specific. No preamble, no meta-commentary."
        .to_string()
}

/// System blocks used by stages 0 and 1: preamble + (cached) transcript + optional spec.
/// The transcript block carries the cache breakpoint so stages re-use it.
fn shared_system_blocks(transcript: &Transcript, spec_block: Option<&str>) -> Vec<Value> {
    let mut blocks = vec![
        json!({ "type": "text", "text": system_preamble() }),
        json!({
            "type": "text",
            "text": build_transcript_block(transcript),
            "cache_control": { "type": "ephemeral" }
        }),
    ];
    if let Some(spec) = spec_block {
        blocks.push(json!({ "type": "text", "text": spec }));
    }
    blocks
}

fn build_spec_block(sections: &[Section]) -> String {
    let mut s = String::from("Sections to produce (in the order shown):\n\n");
    for sec in sections {
        s.push_str(&format!(
            "### Section `{}` — \"{}\"{}\n",
            sec.id,
            sec.title,
            if sec.optional { " (optional)" } else { "" }
        ));
        match &sec.spec {
            SectionSpec::Questions { questions } => {
                s.push_str("Type: questions — answer each with evidence and confidence.\n");
                for q in questions {
                    s.push_str(&format!("- {}: {}\n", q.id, q.prompt));
                    if let Some(g) = &q.guidance {
                        s.push_str(&format!("  guidance: {}\n", g));
                    }
                }
            }
            SectionSpec::ScoredRubric { criteria } => {
                let n = criteria.len();
                s.push_str(&format!(
                    "Type: scored_rubric — output a `scores` array with EXACTLY {n} entries: one per \
                    criterion_id, no duplicates, no omissions. For each criterion: score 1-10 \
                    (1-3 = poor/absent, 4-6 = adequate, 7-8 = good, 9-10 = exceptional), a brief \
                    rationale, and evidence. If a criterion was not discussed at all, use score=1 and \
                    say so in the rationale; evidence may be empty in that case.\n"
                ));
                for c in criteria {
                    s.push_str(&format!("- {}: {}\n", c.id, c.prompt));
                    if let Some(g) = &c.guidance {
                        s.push_str(&format!("  guidance: {}\n", g));
                    }
                }
            }
            SectionSpec::SignalCategorization { categories } => {
                s.push_str(
                    "Type: signal_categorization — output a `findings` object whose keys are \
                    EXACTLY the category_ids below (every key is required). For each category, \
                    produce { strength: high|medium|low|none, signals: [...] }. Each signal has \
                    { description, quotes: [verbatim quote strings] }. Rules: if strength is \
                    high|medium|low, signals MUST contain at least one entry with at least one \
                    quote; if strength=\"none\", signals MUST be empty (and vice versa). Quotes \
                    must be verbatim substrings of an utterance in the transcript so they can be \
                    matched back to speaker + timestamp.\n",
                );
                for c in categories {
                    s.push_str(&format!("- {}: {}\n", c.id, c.description));
                }
            }
            SectionSpec::Classification {
                allow_secondary,
                options,
            } => {
                s.push_str(&format!(
                    "Type: classification — pick exactly one `primary` reason from the \
                    options below{}. Include `notes` and `evidence`. Return null for the \
                    whole section if it does not apply (e.g. lost-deal section on a won/open call).\n",
                    if *allow_secondary {
                        ", and optionally one `secondary`"
                    } else {
                        ""
                    }
                ));
                for o in options {
                    s.push_str(&format!("- {}: {}\n", o.id, o.description));
                }
            }
            SectionSpec::Summary { fields } => {
                s.push_str("Type: summary — produce a key-value object with the fields below.\n");
                for f in fields {
                    let descr = match &f.kind {
                        SummaryFieldKind::Text => "free text".to_string(),
                        SummaryFieldKind::Score { min, max } => {
                            format!("integer score {}-{}", min, max)
                        }
                        SummaryFieldKind::Enum { values } => {
                            format!("one of: {}", values.join(", "))
                        }
                    };
                    s.push_str(&format!("- {} ({}): {}\n", f.id, descr, f.prompt));
                }
            }
        }
        s.push('\n');
    }
    s
}

fn build_transcript_block(t: &Transcript) -> String {
    let mut s = format!("Transcript (duration: {:.1}s):\n\n", t.duration_seconds);
    for u in &t.utterances {
        s.push_str(&format!("[{} @ {}ms] {}\n", u.speaker, u.start_ms, u.text));
    }
    s
}

fn build_role_user_prompt(role: Role, sections: &[Section], speakers: &SpeakerMap) -> String {
    let speaker_block = render_speaker_block(speakers);
    let subjects = speakers.citation_filter(role);
    let role_label = match role {
        Role::Rep => "Sales Rep",
        Role::Prospect => "Prospect",
        Role::Both => "Both sides (no role filter)",
        Role::Synthesis => "Synthesis",
    };

    let scope = match (&role, &subjects) {
        (Role::Both, _) | (_, None) => {
            "No speaker filter applies for this section group. Cite evidence from any speaker \
            as relevant."
                .to_string()
        }
        (_, Some(ids)) if ids.is_empty() => format!(
            "No speakers were identified as `{}`. For every required item, produce a value that \
            reflects the absence (score 1; confidence=low; strength=\"none\"; empty signals) \
            and explain in the rationale that no such speaker was identified.",
            match role {
                Role::Rep => "sales_rep",
                Role::Prospect => "prospect",
                _ => "subject",
            }
        ),
        (_, Some(ids)) => format!(
            "Subject speakers for this analysis: {}.\n\n\
            Quote evidence ONLY from these speakers' utterances. You may read the other \
            speakers' lines for context (e.g. to understand what question was being answered) \
            but do not cite them as evidence for this section group.",
            ids.join(", ")
        ),
    };

    let mut checklist = String::from("Coverage checklist (you must produce every entry below — do not skip any):\n");
    for sec in sections {
        match &sec.spec {
            SectionSpec::ScoredRubric { criteria } => {
                checklist.push_str(&format!("\n- Section `{}` — rate every criterion:\n", sec.id));
                for c in criteria {
                    checklist.push_str(&format!("    [ ] {}\n", c.id));
                }
            }
            SectionSpec::SignalCategorization { categories } => {
                checklist.push_str(&format!(
                    "\n- Section `{}` — produce one finding per category (rate each as \
                    high|medium|low|none and emit a signals array — signals must be non-empty \
                    when strength is high|medium|low):\n",
                    sec.id
                ));
                for c in categories {
                    checklist.push_str(&format!("    [ ] {}\n", c.id));
                }
            }
            SectionSpec::Questions { questions } => {
                checklist.push_str(&format!("\n- Section `{}` — answer every question:\n", sec.id));
                for q in questions {
                    checklist.push_str(&format!("    [ ] {}\n", q.id));
                }
            }
            SectionSpec::Classification { .. } => {
                checklist.push_str(&format!(
                    "\n- Section `{}` — produce a classification (or null if not applicable).\n",
                    sec.id
                ));
            }
            SectionSpec::Summary { fields } => {
                checklist.push_str(&format!("\n- Section `{}` — fill every field:\n", sec.id));
                for f in fields {
                    checklist.push_str(&format!("    [ ] {}\n", f.id));
                }
            }
        }
    }

    format!(
        "Role under analysis: {role_label}.\n\n\
        {speaker_block}\n\
        {scope}\n\n\
        {checklist}\n\
        Work through the checklist item by item. Never omit a required item: if it cannot be \
        assessed from the subject's utterances, produce a value that reflects absence \
        (score 1 + rationale; confidence=low; strength=none + empty signals; etc.) rather than skipping it."
    )
}

fn render_speaker_block(speakers: &SpeakerMap) -> String {
    let mut s = String::from("Speaker map (from stage 0):\n");
    for sp in &speakers.speakers {
        let name = sp.display_name.as_deref().unwrap_or("?");
        let sub = sp.sub_role.as_deref().unwrap_or("");
        s.push_str(&format!(
            "- {} ({}) — {}{}: {}\n",
            sp.speaker_id,
            name,
            sp.role.label(),
            if sub.is_empty() {
                String::new()
            } else {
                format!(", {}", sub)
            },
            sp.rationale
        ));
    }
    s.push('\n');
    s
}

fn observed_speaker_ids(t: &Transcript) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    for u in &t.utterances {
        seen.insert(u.speaker.clone());
    }
    seen.into_iter().collect()
}

fn log_speakers(map: &SpeakerMap) {
    for sp in &map.speakers {
        tracing::info!(
            speaker_id = %sp.speaker_id,
            role = sp.role.label(),
            display_name = sp.display_name.as_deref().unwrap_or(""),
            sub_role = sp.sub_role.as_deref().unwrap_or(""),
            "identified speaker"
        );
    }
    let _ = SpeakerRole::Unknown; // touch enum so unused-variant warnings stay quiet
}

// ---------- Schema building ----------

fn evidence_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["speaker", "quote", "start_ms"],
            "properties": {
                "speaker": { "type": "string" },
                "quote": { "type": "string" },
                "start_ms": { "type": "integer" }
            }
        }
    })
}

fn speaker_map_schema(observed: &[String]) -> Value {
    // Key the response by speaker_id so the schema structurally guarantees one
    // entry per observed speaker — model can't duplicate or skip.
    let mut props = serde_json::Map::new();
    for id in observed {
        props.insert(
            id.clone(),
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["role", "display_name", "sub_role", "rationale"],
                "properties": {
                    "role": {
                        "type": "string",
                        "enum": ["sales_rep", "prospect", "internal_other", "unknown"]
                    },
                    "display_name": { "anyOf": [ { "type": "string" }, { "type": "null" } ] },
                    "sub_role":     { "anyOf": [ { "type": "string" }, { "type": "null" } ] },
                    "rationale":    { "type": "string" }
                }
            }),
        );
    }
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["speakers"],
        "properties": {
            "speakers": {
                "type": "object",
                "additionalProperties": false,
                "required": observed,
                "properties": props
            }
        }
    })
}

fn build_sections_schema(sections: &[Section]) -> Value {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for sec in sections {
        props.insert(sec.id.clone(), section_schema(sec));
        if !sec.optional {
            required.push(sec.id.clone());
        }
    }
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["sections"],
        "properties": {
            "sections": {
                "type": "object",
                "additionalProperties": false,
                "required": required,
                "properties": props
            }
        }
    })
}

fn section_schema(sec: &Section) -> Value {
    let body = match &sec.spec {
        SectionSpec::Questions { questions } => {
            let ids: Vec<&str> = questions.iter().map(|q| q.id.as_str()).collect();
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["answers"],
                "properties": {
                    "answers": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["question_id", "answer", "evidence", "confidence"],
                            "properties": {
                                "question_id": { "type": "string", "enum": ids },
                                "answer": { "type": "string" },
                                "evidence": evidence_schema(),
                                "confidence": { "type": "string", "enum": ["high", "medium", "low"] }
                            }
                        }
                    }
                }
            })
        }
        SectionSpec::ScoredRubric { criteria } => {
            let ids: Vec<&str> = criteria.iter().map(|c| c.id.as_str()).collect();
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["scores"],
                "properties": {
                    "scores": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["criterion_id", "score", "rationale", "evidence"],
                            "properties": {
                                "criterion_id": { "type": "string", "enum": ids },
                                "score": { "type": "integer" },
                                "rationale": { "type": "string" },
                                "evidence": evidence_schema()
                            }
                        }
                    }
                }
            })
        }
        SectionSpec::SignalCategorization { categories } => {
            // Fixed-key object so every category is structurally required. Evidence is a flat
            // array of verbatim quote strings here (rather than the full evidence_schema used by
            // other section types) — repeating the full nested schema 6 times blows out
            // Anthropic's strict-grammar size limit. We recover speaker/start_ms post-decode by
            // matching the quote back to a transcript utterance.
            let mut props = serde_json::Map::new();
            let mut required: Vec<String> = Vec::new();
            for c in categories {
                props.insert(
                    c.id.clone(),
                    json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["strength", "signals"],
                        "properties": {
                            "strength": { "type": "string", "enum": ["high", "medium", "low", "none"] },
                            "signals": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "required": ["description", "quotes"],
                                    "properties": {
                                        "description": { "type": "string" },
                                        "quotes": {
                                            "type": "array",
                                            "items": { "type": "string" }
                                        }
                                    }
                                }
                            }
                        }
                    }),
                );
                required.push(c.id.clone());
            }
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["findings"],
                "properties": {
                    "findings": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": required,
                        "properties": props
                    }
                }
            })
        }
        SectionSpec::Classification {
            allow_secondary,
            options,
        } => {
            let ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
            let mut props = serde_json::Map::new();
            props.insert("primary".into(), json!({ "type": "string", "enum": ids.clone() }));
            if *allow_secondary {
                props.insert(
                    "secondary".into(),
                    json!({ "anyOf": [ { "type": "string", "enum": ids }, { "type": "null" } ] }),
                );
            }
            props.insert("notes".into(), json!({ "type": "string" }));
            props.insert("evidence".into(), evidence_schema());
            let mut required = vec!["primary".to_string(), "notes".to_string(), "evidence".to_string()];
            if *allow_secondary {
                required.push("secondary".into());
            }
            let object_schema = json!({
                "type": "object",
                "additionalProperties": false,
                "required": required,
                "properties": props
            });
            json!({ "anyOf": [ object_schema, { "type": "null" } ] })
        }
        SectionSpec::Summary { fields } => {
            let mut props = serde_json::Map::new();
            let mut required = Vec::new();
            for f in fields {
                let field_schema = match &f.kind {
                    SummaryFieldKind::Text => json!({ "type": "string" }),
                    SummaryFieldKind::Score { min, max } => {
                        let values: Vec<u32> = (*min..=*max).collect();
                        json!({ "type": "integer", "enum": values })
                    }
                    SummaryFieldKind::Enum { values } => {
                        json!({ "type": "string", "enum": values })
                    }
                };
                props.insert(f.id.clone(), field_schema);
                required.push(f.id.clone());
            }
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": required,
                "properties": props
            })
        }
    };

    if sec.optional {
        if matches!(sec.spec, SectionSpec::Classification { .. }) {
            body
        } else {
            json!({ "anyOf": [ body, { "type": "null" } ] })
        }
    } else {
        body
    }
}

// ---------- Response decoding ----------

fn decode_sections(
    sections: &[Section],
    response: &Value,
    transcript: Option<&Transcript>,
) -> Result<Vec<SectionResult>> {
    let sections_obj = response
        .get("sections")
        .and_then(|v| v.as_object())
        .context("response missing `sections` object")?;

    let mut out = Vec::new();
    for sec in sections {
        let raw = sections_obj.get(&sec.id);
        let data = match (&sec.spec, raw) {
            (_, None) | (_, Some(Value::Null)) if sec.optional => continue,
            (_, None) => anyhow::bail!("required section `{}` missing from response", sec.id),
            (SectionSpec::Questions { .. }, Some(v)) => {
                let answers = v
                    .get("answers")
                    .context("questions section missing `answers`")?
                    .clone();
                SectionData::Questions(serde_json::from_value(answers)?)
            }
            (SectionSpec::ScoredRubric { criteria }, Some(v)) => {
                let scores_arr = v
                    .get("scores")
                    .context("scored_rubric section missing `scores`")?
                    .clone();
                let raw: Vec<crate::types::CriterionScore> = serde_json::from_value(scores_arr)?;
                let by_id: std::collections::HashMap<&str, &crate::types::CriterionScore> =
                    raw.iter().map(|s| (s.criterion_id.as_str(), s)).collect();
                let mut out = Vec::with_capacity(criteria.len());
                for c in criteria {
                    match by_id.get(c.id.as_str()) {
                        Some(s) => out.push((*s).clone()),
                        None => {
                            tracing::warn!(
                                section = %sec.id,
                                criterion = %c.id,
                                "rubric missing criterion in response; filling with score=1 placeholder"
                            );
                            out.push(crate::types::CriterionScore {
                                criterion_id: c.id.clone(),
                                score: 1,
                                rationale: "Not scored by the model (missing from response).".into(),
                                evidence: vec![],
                            });
                        }
                    }
                }
                SectionData::ScoredRubric(out)
            }
            (SectionSpec::SignalCategorization { categories }, Some(v)) => {
                let findings = v
                    .get("findings")
                    .and_then(|s| s.as_object())
                    .context("signal_categorization missing `findings` object")?;
                let mut out = Vec::with_capacity(categories.len());
                for c in categories {
                    let entry = match findings.get(&c.id) {
                        Some(inner) => {
                            let strength: crate::types::Strength = serde_json::from_value(
                                inner.get("strength").cloned().with_context(|| {
                                    format!("category `{}` missing strength", c.id)
                                })?,
                            )?;
                            let raw_signals = inner
                                .get("signals")
                                .and_then(|s| s.as_array())
                                .cloned()
                                .unwrap_or_default();
                            let mut signals = Vec::with_capacity(raw_signals.len());
                            for sig in raw_signals {
                                let description = sig
                                    .get("description")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let quotes = sig
                                    .get("quotes")
                                    .and_then(|q| q.as_array())
                                    .cloned()
                                    .unwrap_or_default();
                                let evidence = quotes
                                    .iter()
                                    .filter_map(|q| q.as_str())
                                    .map(|q| resolve_quote(q, transcript))
                                    .collect();
                                signals.push(crate::types::Signal { description, evidence });
                            }
                            let strength = if signals.is_empty()
                                && !matches!(strength, crate::types::Strength::None)
                            {
                                tracing::warn!(
                                    section = %sec.id,
                                    category = %c.id,
                                    "category has signals=[] but strength!=none; demoting to none"
                                );
                                crate::types::Strength::None
                            } else {
                                strength
                            };
                            crate::types::CategoryFindings {
                                category_id: c.id.clone(),
                                strength,
                                signals,
                            }
                        }
                        None => {
                            tracing::warn!(
                                section = %sec.id,
                                category = %c.id,
                                "signals missing category in response; filling with strength=none"
                            );
                            crate::types::CategoryFindings {
                                category_id: c.id.clone(),
                                strength: crate::types::Strength::None,
                                signals: vec![],
                            }
                        }
                    };
                    out.push(entry);
                }
                SectionData::SignalCategorization(out)
            }
            (SectionSpec::Classification { .. }, Some(v)) => {
                SectionData::Classification(serde_json::from_value(v.clone())?)
            }
            (SectionSpec::Summary { .. }, Some(v)) => {
                let map = v
                    .as_object()
                    .context("summary section is not an object")?
                    .clone();
                SectionData::Summary(map)
            }
        };
        out.push(SectionResult {
            id: sec.id.clone(),
            title: sec.title.clone(),
            data,
        });
    }
    Ok(out)
}

/// Map a model-emitted quote string back to a transcript utterance so we can attach the
/// real speaker_id and start_ms. Falls back to placeholder values if no match is found
/// (the quote is preserved verbatim either way).
fn resolve_quote(quote: &str, transcript: Option<&Transcript>) -> crate::types::Evidence {
    let trimmed = quote.trim();
    let normalize = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .collect::<String>()
            .to_lowercase()
    };
    if trimmed.is_empty() {
        return crate::types::Evidence {
            speaker: String::new(),
            quote: trimmed.to_string(),
            start_ms: 0,
        };
    }
    let Some(t) = transcript else {
        return crate::types::Evidence {
            speaker: String::new(),
            quote: trimmed.to_string(),
            start_ms: 0,
        };
    };

    // Try the whole quote first; if it doesn't match (e.g., the model joined fragments across
    // utterances with " ... "), fall back to the longest fragment.
    let candidates: Vec<String> = std::iter::once(trimmed.to_string())
        .chain(
            trimmed
                .split("...")
                .map(|s| s.trim().to_string())
                .filter(|s| s.len() >= 8),
        )
        .collect();

    for needle_src in &candidates {
        let needle = normalize(needle_src);
        if needle.is_empty() {
            continue;
        }
        let mut best: Option<&crate::types::Utterance> = None;
        for u in &t.utterances {
            if normalize(&u.text).contains(&needle) {
                match best {
                    None => best = Some(u),
                    Some(b) if u.text.len() > b.text.len() => best = Some(u),
                    _ => {}
                }
            }
        }
        if let Some(u) = best {
            return crate::types::Evidence {
                speaker: u.speaker.clone(),
                quote: trimmed.to_string(),
                start_ms: u.start_ms,
            };
        }
    }
    tracing::warn!(quote = %trimmed, "could not match quote to any utterance");
    crate::types::Evidence {
        speaker: String::new(),
        quote: trimmed.to_string(),
        start_ms: 0,
    }
}

/// Trim and bound a short label field (display_name, sub_role) so a model glitch
/// that emits overlapping/repeated attempts doesn't produce a runaway string.
fn sanitize_short_text(s: String) -> String {
    let trimmed = s.trim();
    // Cut at the first sentence terminator so multiple concatenated attempts
    // collapse to the first try.
    let end = trimmed
        .find(|c: char| matches!(c, '.' | ';' | '\n'))
        .unwrap_or(trimmed.len());
    let first = trimmed[..end].trim();
    let candidate = if first.is_empty() { trimmed } else { first };
    // Detect immediate word repetition ("client client client…") and clip at first repeat.
    let candidate = clip_at_word_repeat(candidate);
    const MAX_LEN: usize = 80;
    if candidate.chars().count() <= MAX_LEN {
        candidate.to_string()
    } else {
        let truncated: String = candidate.chars().take(MAX_LEN).collect();
        format!("{}…", truncated.trim_end())
    }
}

fn clip_at_word_repeat(s: &str) -> &str {
    let mut last_word: Option<&str> = None;
    let mut byte_cursor = 0usize;
    for word in s.split_inclusive(char::is_whitespace) {
        let trimmed_word = word.trim();
        if !trimmed_word.is_empty() {
            if Some(trimmed_word) == last_word {
                return s[..byte_cursor].trim_end();
            }
            last_word = Some(trimmed_word);
        }
        byte_cursor += word.len();
    }
    s
}

fn decode_speaker_map(response: &Value, observed: &[String]) -> Result<SpeakerMap> {
    #[derive(Deserialize)]
    struct Inner {
        role: SpeakerRole,
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        sub_role: Option<String>,
        rationale: String,
    }
    let map = response
        .get("speakers")
        .and_then(|v| v.as_object())
        .context("speaker response missing `speakers` object")?;

    // Iterate in the observed order so report rendering is stable.
    let mut out = Vec::with_capacity(observed.len());
    for id in observed {
        let raw = map
            .get(id)
            .with_context(|| format!("speaker `{}` missing from response", id))?;
        let inner: Inner = serde_json::from_value(raw.clone())
            .with_context(|| format!("decoding speaker `{}`", id))?;
        out.push(SpeakerInfo {
            speaker_id: id.clone(),
            role: inner.role,
            display_name: inner.display_name.filter(|s| !s.is_empty()).map(sanitize_short_text),
            sub_role: inner.sub_role.filter(|s| !s.is_empty()).map(sanitize_short_text),
            rationale: inner.rationale,
        });
    }
    Ok(SpeakerMap { speakers: out })
}
