//! Format-agnostic document model for analysis reports.
//!
//! `build_doc` walks an [`AnalysisReport`] once into a [`Doc`]; the `to_markdown`,
//! `to_html`, and `to_docx` serializers each render that single model. This keeps
//! the three output formats in lock-step instead of triplicating the walk logic.

use std::io::Cursor;

use docx_rs::{
    AlignmentType, Docx, Paragraph, Run, RunFonts, Shading, ShdType, Table, TableCell,
    TableLayoutType, TableRow, WidthType,
};

use crate::report::format_ts;
use crate::types::{
    Analysis, AnalysisReport, ClassificationResult, Confidence, Evidence, Section, SectionData,
    SectionResult, SectionSpec, SpeakerMap, Strength, SummaryFieldKind,
};

// ============ Model ============

pub struct Doc {
    pub title: String,
    pub blocks: Vec<Block>,
}

pub enum Block {
    Heading(u8, String),
    Paragraph(Vec<Inline>),
    Bullets(Vec<ListItem>),
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    /// An italicized aside, e.g. "(not applicable for this call)".
    Note(String),
}

pub struct ListItem {
    pub runs: Vec<Inline>,
    pub children: Vec<ListItem>,
}

impl ListItem {
    fn leaf(runs: Vec<Inline>) -> Self {
        ListItem {
            runs,
            children: Vec::new(),
        }
    }
}

pub enum Inline {
    Text(String),
    Bold(String),
    Code(String),
}

// ============ Build ============

pub fn build_doc(report: &AnalysisReport, analysis: &Analysis) -> Doc {
    let mut blocks = Vec::new();

    blocks.push(Block::Paragraph(vec![
        Inline::Bold("Duration:".into()),
        Inline::Text(format!(" {:.1} minutes", report.duration_seconds / 60.0)),
    ]));
    blocks.push(Block::Paragraph(vec![
        Inline::Bold("Analysis:".into()),
        Inline::Text(format!(" {}", analysis.name)),
    ]));

    build_speakers(&mut blocks, &report.speakers);

    for section in &analysis.sections {
        let result = report.sections.iter().find(|r| r.id == section.id);
        build_section(&mut blocks, section, result);
    }

    Doc {
        title: format!("Call Analysis — {}", report.recording_name),
        blocks,
    }
}

fn build_speakers(blocks: &mut Vec<Block>, map: &SpeakerMap) {
    if map.speakers.is_empty() {
        return;
    }
    blocks.push(Block::Heading(2, "Speakers".into()));
    let mut rows = Vec::new();
    for sp in &map.speakers {
        rows.push(vec![
            sp.speaker_id.clone(),
            sp.role.label().to_string(),
            sp.display_name.clone().unwrap_or_else(|| "—".into()),
            sp.sub_role.clone().unwrap_or_else(|| "—".into()),
            sp.rationale.replace('\n', " "),
        ]);
    }
    blocks.push(Block::Table {
        headers: vec![
            "Speaker".into(),
            "Role".into(),
            "Name".into(),
            "Sub-role".into(),
            "Rationale".into(),
        ],
        rows,
    });
}

fn build_section(blocks: &mut Vec<Block>, section: &Section, result: Option<&SectionResult>) {
    blocks.push(Block::Heading(2, section.title.clone()));

    let Some(result) = result else {
        let msg = if section.optional {
            "(not applicable for this call)"
        } else {
            "(no result produced)"
        };
        blocks.push(Block::Note(msg.into()));
        return;
    };

    match (&section.spec, &result.data) {
        (SectionSpec::Questions { questions }, SectionData::Questions(answers)) => {
            for q in questions {
                blocks.push(Block::Heading(3, q.prompt.clone()));
                if let Some(a) = answers.iter().find(|a| a.question_id == q.id) {
                    blocks.push(Block::Paragraph(vec![
                        Inline::Bold(format!("Answer ({}):", confidence_label(a.confidence))),
                        Inline::Text(format!(" {}", a.answer)),
                    ]));
                    push_evidence(blocks, &a.evidence);
                } else {
                    blocks.push(Block::Note("(no answer produced)".into()));
                }
            }
        }
        (SectionSpec::ScoredRubric { criteria }, SectionData::ScoredRubric(scores)) => {
            let total: u32 = scores.iter().map(|c| c.score).sum();
            let max = (criteria.len() as u32) * 10;
            blocks.push(Block::Paragraph(vec![Inline::Bold(format!(
                "Overall: {}/{}",
                total, max
            ))]));
            let mut rows = Vec::new();
            for c in criteria {
                if let Some(score) = scores.iter().find(|s| s.criterion_id == c.id) {
                    rows.push(vec![
                        humanize(&c.id),
                        format!("{}/10", score.score),
                        score.rationale.replace('\n', " "),
                    ]);
                } else {
                    rows.push(vec![humanize(&c.id), "—".into(), "(no score)".into()]);
                }
            }
            blocks.push(Block::Table {
                headers: vec!["Criterion".into(), "Score".into(), "Rationale".into()],
                rows,
            });
            for c in criteria {
                if let Some(score) = scores.iter().find(|s| s.criterion_id == c.id) {
                    if !score.evidence.is_empty() {
                        blocks.push(Block::Paragraph(vec![Inline::Bold(format!(
                            "{} — evidence:",
                            humanize(&c.id)
                        ))]));
                        push_evidence(blocks, &score.evidence);
                    }
                }
            }
        }
        (
            SectionSpec::SignalCategorization { categories },
            SectionData::SignalCategorization(findings),
        ) => {
            for cat in categories {
                let f = findings.iter().find(|f| f.category_id == cat.id);
                let badge = f.map(|f| strength_label(f.strength)).unwrap_or("—");
                blocks.push(Block::Heading(3, format!("{} ({})", humanize(&cat.id), badge)));
                blocks.push(Block::Paragraph(vec![Inline::Text(
                    cat.description.trim().to_string(),
                )]));
                match f {
                    None => blocks.push(Block::Note("(no result)".into())),
                    Some(f) if f.signals.is_empty() => {
                        blocks.push(Block::Note("No signals detected.".into()))
                    }
                    Some(f) => {
                        let items = f
                            .signals
                            .iter()
                            .map(|sig| ListItem {
                                runs: vec![Inline::Text(sig.description.clone())],
                                children: evidence_items(&sig.evidence),
                            })
                            .collect();
                        blocks.push(Block::Bullets(items));
                    }
                }
            }
        }
        (SectionSpec::Classification { .. }, SectionData::Classification(opt)) => match opt {
            None => blocks.push(Block::Note("(not applicable for this call)".into())),
            Some(c) => build_classification(blocks, c),
        },
        (SectionSpec::Summary { fields }, SectionData::Summary(map)) => {
            let mut items = Vec::new();
            for f in fields {
                let val = map
                    .get(&f.id)
                    .map(|v| match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_else(|| "—".into());
                let suffix = match &f.kind {
                    SummaryFieldKind::Score { max, .. } => format!("/{}", max),
                    _ => String::new(),
                };
                items.push(ListItem::leaf(vec![
                    Inline::Bold(format!("{}:", humanize(&f.id))),
                    Inline::Text(format!(" {}{}", val, suffix)),
                ]));
            }
            blocks.push(Block::Bullets(items));
        }
        _ => blocks.push(Block::Note(
            "(section result type mismatch — check logs)".into(),
        )),
    }
}

fn build_classification(blocks: &mut Vec<Block>, c: &ClassificationResult) {
    let mut items = vec![ListItem::leaf(vec![
        Inline::Bold("Primary:".into()),
        Inline::Text(format!(" {}", humanize(&c.primary))),
    ])];
    if let Some(sec) = &c.secondary {
        items.push(ListItem::leaf(vec![
            Inline::Bold("Secondary:".into()),
            Inline::Text(format!(" {}", humanize(sec))),
        ]));
    }
    items.push(ListItem::leaf(vec![
        Inline::Bold("Notes:".into()),
        Inline::Text(format!(" {}", c.notes)),
    ]));
    blocks.push(Block::Bullets(items));
    if !c.evidence.is_empty() {
        blocks.push(Block::Paragraph(vec![Inline::Bold("Evidence:".into())]));
        push_evidence(blocks, &c.evidence);
    }
}

fn push_evidence(blocks: &mut Vec<Block>, evidence: &[Evidence]) {
    if evidence.is_empty() {
        return;
    }
    blocks.push(Block::Bullets(evidence_items(evidence)));
}

fn evidence_items(evidence: &[Evidence]) -> Vec<ListItem> {
    evidence
        .iter()
        .map(|e| {
            ListItem::leaf(vec![
                Inline::Code(format!("[{} @ {}]", e.speaker, format_ts(e.start_ms))),
                Inline::Text(format!(" \"{}\"", e.quote)),
            ])
        })
        .collect()
}

// ============ Shared label helpers ============

fn confidence_label(c: Confidence) -> &'static str {
    match c {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    }
}

fn strength_label(s: Strength) -> &'static str {
    match s {
        Strength::High => "high",
        Strength::Medium => "medium",
        Strength::Low => "low",
        Strength::None => "none",
    }
}

fn humanize(id: &str) -> String {
    id.split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ============ Markdown ============

pub fn to_markdown(doc: &Doc) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    writeln!(s, "# {}\n", doc.title).unwrap();
    for block in &doc.blocks {
        match block {
            Block::Heading(level, text) => {
                let hashes = "#".repeat(*level as usize);
                writeln!(s, "{} {}\n", hashes, text).unwrap();
            }
            Block::Paragraph(runs) => {
                writeln!(s, "{}\n", md_inline(runs)).unwrap();
            }
            Block::Note(text) => {
                writeln!(s, "_{}_\n", text).unwrap();
            }
            Block::Bullets(items) => {
                md_bullets(&mut s, items, 0);
                writeln!(s).unwrap();
            }
            Block::Table { headers, rows } => {
                writeln!(s, "| {} |", headers.join(" | ")).unwrap();
                writeln!(
                    s,
                    "|{}|",
                    headers.iter().map(|_| "---").collect::<Vec<_>>().join("|")
                )
                .unwrap();
                for row in rows {
                    let cells: Vec<String> = row.iter().map(|c| md_cell(c)).collect();
                    writeln!(s, "| {} |", cells.join(" | ")).unwrap();
                }
                writeln!(s).unwrap();
            }
        }
    }
    s
}

fn md_bullets(s: &mut String, items: &[ListItem], depth: usize) {
    use std::fmt::Write;
    let indent = "  ".repeat(depth);
    for item in items {
        writeln!(s, "{}- {}", indent, md_inline(&item.runs)).unwrap();
        md_bullets(s, &item.children, depth + 1);
    }
}

fn md_inline(runs: &[Inline]) -> String {
    runs.iter()
        .map(|r| match r {
            Inline::Text(t) => t.clone(),
            Inline::Bold(t) => format!("**{}**", t),
            Inline::Code(t) => format!("`{}`", t),
        })
        .collect()
}

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

// ============ HTML ============

pub fn to_html(doc: &Doc) -> String {
    use std::fmt::Write;
    let mut body = String::new();
    writeln!(body, "<h1>{}</h1>", esc(&doc.title)).unwrap();
    for block in &doc.blocks {
        match block {
            Block::Heading(level, text) => {
                let l = (*level).clamp(1, 6);
                writeln!(body, "<h{l}>{}</h{l}>", esc(text)).unwrap();
            }
            Block::Paragraph(runs) => {
                writeln!(body, "<p>{}</p>", html_inline(runs)).unwrap();
            }
            Block::Note(text) => {
                writeln!(body, "<p class=\"note\">{}</p>", esc(text)).unwrap();
            }
            Block::Bullets(items) => html_bullets(&mut body, items),
            Block::Table { headers, rows } => {
                writeln!(body, "<table>").unwrap();
                writeln!(body, "<thead><tr>").unwrap();
                for h in headers {
                    writeln!(body, "<th>{}</th>", esc(h)).unwrap();
                }
                writeln!(body, "</tr></thead>").unwrap();
                writeln!(body, "<tbody>").unwrap();
                for row in rows {
                    writeln!(body, "<tr>").unwrap();
                    for cell in row {
                        writeln!(body, "<td>{}</td>", esc(cell)).unwrap();
                    }
                    writeln!(body, "</tr>").unwrap();
                }
                writeln!(body, "</tbody></table>").unwrap();
            }
        }
    }
    wrap_html(&doc.title, &body)
}

fn html_bullets(body: &mut String, items: &[ListItem]) {
    use std::fmt::Write;
    writeln!(body, "<ul>").unwrap();
    for item in items {
        write!(body, "<li>{}", html_inline(&item.runs)).unwrap();
        if !item.children.is_empty() {
            writeln!(body).unwrap();
            html_bullets(body, &item.children);
        }
        writeln!(body, "</li>").unwrap();
    }
    writeln!(body, "</ul>").unwrap();
}

fn html_inline(runs: &[Inline]) -> String {
    runs.iter()
        .map(|r| match r {
            Inline::Text(t) => esc(t),
            Inline::Bold(t) => format!("<strong>{}</strong>", esc(t)),
            Inline::Code(t) => format!("<code>{}</code>", esc(t)),
        })
        .collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn wrap_html(title: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
  :root {{ --fg: #1a1a1a; --muted: #6b7280; --border: #e5e7eb; --accent: #2563eb; --code-bg: #f3f4f6; }}
  * {{ box-sizing: border-box; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
         color: var(--fg); line-height: 1.55; max-width: 820px; margin: 2.5rem auto; padding: 0 1.25rem; }}
  h1 {{ font-size: 1.9rem; border-bottom: 3px solid var(--accent); padding-bottom: .4rem; margin-bottom: 1.2rem; }}
  h2 {{ font-size: 1.35rem; margin-top: 2rem; border-bottom: 1px solid var(--border); padding-bottom: .25rem; }}
  h3 {{ font-size: 1.08rem; margin-top: 1.4rem; color: #111827; }}
  p {{ margin: .55rem 0; }}
  p.note {{ color: var(--muted); font-style: italic; }}
  ul {{ margin: .4rem 0 .8rem; padding-left: 1.4rem; }}
  li {{ margin: .25rem 0; }}
  code {{ background: var(--code-bg); padding: .1rem .35rem; border-radius: 4px;
          font-family: "SF Mono", Menlo, Consolas, monospace; font-size: .85em; color: #374151; }}
  table {{ border-collapse: collapse; width: 100%; margin: .8rem 0 1.4rem; font-size: .92rem; }}
  th, td {{ border: 1px solid var(--border); padding: .5rem .7rem; text-align: left; vertical-align: top; }}
  th {{ background: #f9fafb; font-weight: 600; }}
  tr:nth-child(even) td {{ background: #fcfcfd; }}
  @media print {{ body {{ margin: 0; max-width: none; }} h2 {{ page-break-after: avoid; }} }}
</style>
</head>
<body>
{body}</body>
</html>
"#
    )
}

// ============ DOCX ============

const HEADER_FILL: &str = "F3F4F6";
const TABLE_WIDTH: usize = 9000;

pub fn to_docx(doc: &Doc) -> anyhow::Result<Vec<u8>> {
    let mut d = Docx::new().add_paragraph(heading_para(&doc.title, 1));

    for block in &doc.blocks {
        match block {
            Block::Heading(level, text) => d = d.add_paragraph(heading_para(text, *level)),
            Block::Paragraph(runs) => d = d.add_paragraph(inline_para(runs)),
            Block::Note(text) => {
                d = d.add_paragraph(
                    Paragraph::new().add_run(Run::new().add_text(text).italic().color("6B7280")),
                )
            }
            Block::Bullets(items) => d = add_bullets(d, items, 0),
            Block::Table { headers, rows } => d = d.add_table(build_table(headers, rows)),
        }
    }

    let mut buf = Cursor::new(Vec::new());
    d.build().pack(&mut buf)?;
    Ok(buf.into_inner())
}

fn heading_para(text: &str, level: u8) -> Paragraph {
    // sizes in half-points: h1=32pt, h2=26pt, h3=22pt
    let size = match level {
        1 => 32,
        2 => 26,
        _ => 22,
    };
    Paragraph::new().add_run(Run::new().add_text(text).bold().size(size))
}

fn inline_para(runs: &[Inline]) -> Paragraph {
    add_inline(Paragraph::new(), runs)
}

fn add_inline(mut p: Paragraph, runs: &[Inline]) -> Paragraph {
    for r in runs {
        let run = match r {
            Inline::Text(t) => Run::new().add_text(t),
            Inline::Bold(t) => Run::new().add_text(t).bold(),
            Inline::Code(t) => Run::new()
                .add_text(t)
                .fonts(RunFonts::new().ascii("Consolas"))
                .color("374151"),
        };
        p = p.add_run(run);
    }
    p
}

fn add_bullets(mut d: Docx, items: &[ListItem], depth: usize) -> Docx {
    let glyph = if depth == 0 { "• " } else { "◦ " };
    for item in items {
        let mut runs = vec![Inline::Text(glyph.to_string())];
        runs.extend(item.runs.iter().map(clone_inline));
        let indent = 360 + (depth as i32) * 360;
        let p = add_inline(Paragraph::new(), &runs).indent(Some(indent), None, None, None);
        d = d.add_paragraph(p);
        d = add_bullets(d, &item.children, depth + 1);
    }
    d
}

fn clone_inline(i: &Inline) -> Inline {
    match i {
        Inline::Text(t) => Inline::Text(t.clone()),
        Inline::Bold(t) => Inline::Bold(t.clone()),
        Inline::Code(t) => Inline::Code(t.clone()),
    }
}

fn build_table(headers: &[String], rows: &[Vec<String>]) -> Table {
    let widths = column_widths(headers);
    let mut trows = Vec::new();

    let header_cells = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            TableCell::new()
                .width(widths[i], WidthType::Dxa)
                .shading(Shading::new().shd_type(ShdType::Clear).fill(HEADER_FILL))
                .add_paragraph(
                    Paragraph::new()
                        .add_run(Run::new().add_text(h).bold())
                        .align(AlignmentType::Left),
                )
        })
        .collect();
    trows.push(TableRow::new(header_cells));

    for row in rows {
        let cells = row
            .iter()
            .enumerate()
            .map(|(i, c)| {
                TableCell::new()
                    .width(*widths.get(i).unwrap_or(&0), WidthType::Dxa)
                    .add_paragraph(Paragraph::new().add_run(Run::new().add_text(c)))
            })
            .collect();
        trows.push(TableRow::new(cells));
    }

    Table::new(trows)
        .set_grid(widths.clone())
        .layout(TableLayoutType::Fixed)
        .width(TABLE_WIDTH, WidthType::Dxa)
}

/// Distribute the table width across columns, giving free-text columns
/// (e.g. "Rationale") more room than short, fixed-value columns.
fn column_widths(headers: &[String]) -> Vec<usize> {
    let weights: Vec<usize> = headers
        .iter()
        .map(|h| if h.eq_ignore_ascii_case("rationale") { 3 } else { 1 })
        .collect();
    let total: usize = weights.iter().sum::<usize>().max(1);
    weights
        .iter()
        .map(|w| w * TABLE_WIDTH / total)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use serde_json::json;

    fn ev(speaker: &str, quote: &str, ms: u64) -> Evidence {
        Evidence {
            speaker: speaker.into(),
            quote: quote.into(),
            start_ms: ms,
        }
    }

    fn sample() -> (AnalysisReport, Analysis) {
        let analysis = Analysis {
            name: "Test Analysis — v1".into(),
            sections: vec![
                Section {
                    id: "q".into(),
                    title: "Questions".into(),
                    optional: false,
                    role: Role::Both,
                    spec: SectionSpec::Questions {
                        questions: vec![Question {
                            id: "q1".into(),
                            prompt: "What did they ask?".into(),
                            guidance: None,
                        }],
                    },
                },
                Section {
                    id: "rubric".into(),
                    title: "Rubric".into(),
                    optional: false,
                    role: Role::Rep,
                    spec: SectionSpec::ScoredRubric {
                        criteria: vec![Criterion {
                            id: "discovery_depth".into(),
                            prompt: "Depth".into(),
                            guidance: None,
                        }],
                    },
                },
                Section {
                    id: "signals".into(),
                    title: "Signals".into(),
                    optional: false,
                    role: Role::Both,
                    spec: SectionSpec::SignalCategorization {
                        categories: vec![SignalCategory {
                            id: "buying_signal".into(),
                            description: "Interest cues".into(),
                        }],
                    },
                },
                Section {
                    id: "class".into(),
                    title: "Outcome".into(),
                    optional: true,
                    role: Role::Synthesis,
                    spec: SectionSpec::Classification {
                        allow_secondary: true,
                        options: vec![],
                    },
                },
                Section {
                    id: "summary".into(),
                    title: "Summary".into(),
                    optional: false,
                    role: Role::Synthesis,
                    spec: SectionSpec::Summary {
                        fields: vec![SummaryField {
                            id: "overall_score".into(),
                            prompt: "Score".into(),
                            kind: SummaryFieldKind::Score { min: 0, max: 100 },
                        }],
                    },
                },
            ],
        };

        let report = AnalysisReport {
            recording_name: "demo".into(),
            duration_seconds: 132.0,
            analysis_name: analysis.name.clone(),
            speakers: SpeakerMap {
                speakers: vec![SpeakerInfo {
                    speaker_id: "speaker_0".into(),
                    role: SpeakerRole::SalesRep,
                    display_name: Some("Pat".into()),
                    sub_role: None,
                    rationale: "Leads | drives the call".into(),
                }],
            },
            sections: vec![
                SectionResult {
                    id: "q".into(),
                    title: "Questions".into(),
                    data: SectionData::Questions(vec![Answer {
                        question_id: "q1".into(),
                        answer: "About <pricing> & terms".into(),
                        evidence: vec![ev("speaker_0", "how much?", 3000)],
                        confidence: Confidence::High,
                    }]),
                },
                SectionResult {
                    id: "rubric".into(),
                    title: "Rubric".into(),
                    data: SectionData::ScoredRubric(vec![CriterionScore {
                        criterion_id: "discovery_depth".into(),
                        score: 7,
                        rationale: "Solid | probing".into(),
                        evidence: vec![ev("speaker_0", "tell me more", 9000)],
                    }]),
                },
                SectionResult {
                    id: "signals".into(),
                    title: "Signals".into(),
                    data: SectionData::SignalCategorization(vec![CategoryFindings {
                        category_id: "buying_signal".into(),
                        strength: Strength::Medium,
                        signals: vec![Signal {
                            description: "Asked about rollout".into(),
                            evidence: vec![ev("speaker_0", "when can we start?", 60000)],
                        }],
                    }]),
                },
                SectionResult {
                    id: "class".into(),
                    title: "Outcome".into(),
                    data: SectionData::Classification(Some(ClassificationResult {
                        primary: "qualified_lead".into(),
                        secondary: None,
                        notes: "Strong fit".into(),
                        evidence: vec![],
                    })),
                },
                SectionResult {
                    id: "summary".into(),
                    title: "Summary".into(),
                    data: SectionData::Summary(
                        json!({ "overall_score": 82 }).as_object().unwrap().clone(),
                    ),
                },
            ],
        };
        (report, analysis)
    }

    #[test]
    fn markdown_covers_all_sections() {
        let (r, a) = sample();
        let md = to_markdown(&build_doc(&r, &a));
        assert!(md.contains("# Call Analysis — demo"));
        assert!(md.contains("**Answer (high):** About <pricing> & terms"));
        assert!(md.contains("**Overall: 7/10**"));
        assert!(md.contains("### Buying Signal (medium)"));
        assert!(md.contains("**Primary:** Qualified Lead"));
        assert!(md.contains("**Overall Score:** 82/100"));
        // table cell pipes are escaped, not column separators
        assert!(md.contains("Leads \\| drives the call"));
        // nested evidence under a signal is indented
        assert!(md.contains("  - `[speaker_0 @ 01:00]`"));
    }

    #[test]
    fn html_is_escaped_and_structured() {
        let (r, a) = sample();
        let html = to_html(&build_doc(&r, &a));
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<title>Call Analysis — demo</title>"));
        // user content with angle brackets / ampersands must be escaped
        assert!(html.contains("About &lt;pricing&gt; &amp; terms"));
        assert!(!html.contains("About <pricing>"));
        assert!(html.contains("<table>") && html.contains("<th>Criterion</th>"));
        assert!(html.contains("<code>[speaker_0 @ 01:00]</code>"));
    }

    #[test]
    #[ignore = "writes sample artifacts to target/ for manual inspection"]
    fn dump_samples() {
        let (r, a) = sample();
        let doc = build_doc(&r, &a);
        std::fs::write("target/sample.md", to_markdown(&doc)).unwrap();
        std::fs::write("target/sample.html", to_html(&doc)).unwrap();
        std::fs::write("target/sample.docx", to_docx(&doc).unwrap()).unwrap();
    }

    #[test]
    fn docx_packs_to_valid_zip() {
        let (r, a) = sample();
        let bytes = to_docx(&build_doc(&r, &a)).expect("docx packs");
        assert!(bytes.len() > 1000, "docx unexpectedly small");
        // OOXML files are zip archives; zip local-file header magic is "PK\x03\x04"
        assert_eq!(&bytes[0..4], b"PK\x03\x04");
    }
}
