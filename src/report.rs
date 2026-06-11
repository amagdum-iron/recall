use std::fmt::Write;

use crate::types::Transcript;

// ============ Transcript ============

pub fn render_transcript_markdown(transcript: &Transcript, recording_name: &str) -> String {
    let mut s = String::new();
    writeln!(s, "# Transcript — {}", recording_name).unwrap();
    writeln!(s).unwrap();
    writeln!(s, "- **Provider:** {}", transcript.provider).unwrap();
    writeln!(
        s,
        "- **Duration:** {:.1} minutes ({:.1}s)",
        transcript.duration_seconds / 60.0,
        transcript.duration_seconds
    )
    .unwrap();
    if let Some(lang) = &transcript.language {
        writeln!(s, "- **Language:** {}", lang).unwrap();
    }
    writeln!(s, "- **Speakers:** {}", count_speakers(transcript)).unwrap();
    writeln!(s).unwrap();
    writeln!(s, "---").unwrap();
    writeln!(s).unwrap();

    for u in &transcript.utterances {
        let ts = format_ts(u.start_ms);
        writeln!(s, "**[{} @ {}]** {}", u.speaker, ts, u.text).unwrap();
        writeln!(s).unwrap();
    }
    s
}

fn count_speakers(t: &Transcript) -> usize {
    let mut set = std::collections::HashSet::new();
    for u in &t.utterances {
        set.insert(u.speaker.as_str());
    }
    set.len()
}

/// Format milliseconds as `MM:SS`. Shared with the analysis-report serializers in [`crate::doc`].
pub fn format_ts(ms: u64) -> String {
    let total_s = ms / 1000;
    let m = total_s / 60;
    let s = total_s % 60;
    format!("{:02}:{:02}", m, s)
}
