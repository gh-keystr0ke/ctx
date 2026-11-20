use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub schema: u32,
    pub guide_version: u32,
    #[serde(rename = "project")]
    pub name: String,
    pub language: Language,
    pub role: Role,
    pub repository: String,
    pub release: String,
    pub commit: String,
    pub evidence_sources: Vec<EvidenceSource>,
    pub reference_patterns: Vec<ReferencePattern>,
    pub modules: Vec<String>,
    pub excluded_paths: Vec<String>,
    pub sample: Sample,
    pub points: Vec<Point>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    Python,
    Rust,
    Go,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Tuning,
    Gate,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSource {
    pub kind: EvidenceKind,
    pub repository: String,
    pub commit: String,
    pub paths: Vec<String>,
    pub url_template: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferencePattern {
    pub regex: String,
    pub url: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub seed: u64,
    pub size: usize,
    pub ranking_version: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub id: String,
    pub anchor: String,
    pub location: String,
    pub draft: Option<Draft>,
    pub drafted_by: Option<AgentRun>,
    pub label: Option<Label>,
    pub category: Option<Category>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    pub verified_by: Option<String>,
    pub second_label: Option<Label>,
    pub second_by: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub label: Label,
    pub category: Option<Category>,
    pub evidence: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRun {
    pub agent: String,
    pub model: Option<String>,
    pub prompt_fingerprint: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Label {
    Significant,
    Trivial,
    Unclear,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    Limit,
    StateTransition,
    Permission,
    Default,
    Retry,
    Validation,
    ErrorHandling,
    Observability,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceKind {
    Docs,
    Spec,
    Issue,
    Test,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    #[serde(rename = "type")]
    pub kind: EvidenceKind,
    pub url: String,
    pub quote: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub schema: u32,
    #[serde(default)]
    pub fill_order: Vec<String>,
    #[serde(default)]
    pub fill_points: BTreeMap<String, FillPoint>,
    pub points: BTreeMap<String, SelectedPoint>,
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SelectedPoint {
    pub selected_by: SelectedBy,
    pub rank: usize,
    pub score: i64,
    pub contributions: BTreeMap<String, i64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectedBy {
    Top,
    Random,
    Fill,
    Pool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FillPoint {
    pub location: String,
    pub rank: usize,
    pub score: i64,
    pub contributions: BTreeMap<String, i64>,
}
