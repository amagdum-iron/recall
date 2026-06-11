use serde::{Deserialize, Serialize};

// --- Transcript ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub provider: String,
    pub duration_seconds: f64,
    pub language: Option<String>,
    pub utterances: Vec<Utterance>,
    pub full_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utterance {
    pub speaker: String,
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

// --- Analysis spec (input) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    pub name: String,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub role: Role,
    #[serde(flatten)]
    pub spec: SectionSpec,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Section scored against rep utterances only.
    Rep,
    /// Section scored against prospect utterances only.
    Prospect,
    /// Section that sees the full transcript without role filtering (default).
    Both,
    /// Section produced in a final pass from prior section outputs.
    Synthesis,
}

impl Default for Role {
    fn default() -> Self {
        Role::Both
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SectionSpec {
    Questions {
        questions: Vec<Question>,
    },
    ScoredRubric {
        criteria: Vec<Criterion>,
    },
    SignalCategorization {
        categories: Vec<SignalCategory>,
    },
    Classification {
        #[serde(default)]
        allow_secondary: bool,
        options: Vec<ClassificationOption>,
    },
    Summary {
        fields: Vec<SummaryField>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub guidance: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Criterion {
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub guidance: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalCategory {
    pub id: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationOption {
    pub id: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryField {
    pub id: String,
    pub prompt: String,
    #[serde(flatten)]
    pub kind: SummaryFieldKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SummaryFieldKind {
    Text,
    Score { min: u32, max: u32 },
    Enum { values: Vec<String> },
}

// --- Common output sub-types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub speaker: String,
    pub quote: String,
    pub start_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strength {
    High,
    Medium,
    Low,
    None,
}

// --- Section results (output from Claude) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub recording_name: String,
    pub duration_seconds: f64,
    pub analysis_name: String,
    pub speakers: SpeakerMap,
    pub sections: Vec<SectionResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpeakerMap {
    pub speakers: Vec<SpeakerInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerInfo {
    pub speaker_id: String,
    pub role: SpeakerRole,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub sub_role: Option<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerRole {
    SalesRep,
    Prospect,
    InternalOther,
    Unknown,
}

impl SpeakerRole {
    pub fn label(&self) -> &'static str {
        match self {
            SpeakerRole::SalesRep => "sales rep",
            SpeakerRole::Prospect => "prospect",
            SpeakerRole::InternalOther => "internal other",
            SpeakerRole::Unknown => "unknown",
        }
    }
}

impl SpeakerMap {
    /// Speaker IDs that should be cited as evidence for the given section role.
    /// Returns None when no filter applies (i.e. the model may cite anyone).
    pub fn citation_filter(&self, role: Role) -> Option<Vec<String>> {
        match role {
            Role::Rep => Some(
                self.speakers
                    .iter()
                    .filter(|s| s.role == SpeakerRole::SalesRep)
                    .map(|s| s.speaker_id.clone())
                    .collect(),
            ),
            Role::Prospect => Some(
                self.speakers
                    .iter()
                    .filter(|s| s.role == SpeakerRole::Prospect)
                    .map(|s| s.speaker_id.clone())
                    .collect(),
            ),
            Role::Both | Role::Synthesis => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionResult {
    pub id: String,
    pub title: String,
    pub data: SectionData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SectionData {
    Questions(Vec<Answer>),
    ScoredRubric(Vec<CriterionScore>),
    SignalCategorization(Vec<CategoryFindings>),
    Classification(Option<ClassificationResult>),
    Summary(serde_json::Map<String, serde_json::Value>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Answer {
    pub question_id: String,
    pub answer: String,
    pub evidence: Vec<Evidence>,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionScore {
    pub criterion_id: String,
    pub score: u32,
    pub rationale: String,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryFindings {
    pub category_id: String,
    pub strength: Strength,
    pub signals: Vec<Signal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub description: String,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationResult {
    pub primary: String,
    pub secondary: Option<String>,
    pub notes: String,
    pub evidence: Vec<Evidence>,
}
