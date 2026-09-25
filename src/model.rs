use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, atomic::AtomicBool};

pub const SCHEMA_VERSION: u32 = 1;
pub type CancelFlag = Arc<AtomicBool>;

/// Identity of one published disposable index snapshot. Numeric revisions are local to a generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    deny_unknown_fields,
    try_from = "UncheckedIndexPin"
)]
pub struct IndexPin {
    #[serde(with = "uuid_text")]
    pub index_generation: uuid::Uuid,
    pub index_revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UncheckedIndexPin {
    index_generation: String,
    index_revision: u64,
}
impl TryFrom<UncheckedIndexPin> for IndexPin {
    type Error = String;
    fn try_from(value: UncheckedIndexPin) -> Result<Self, Self::Error> {
        let generation =
            uuid::Uuid::parse_str(&value.index_generation).map_err(|e| e.to_string())?;
        if generation.is_nil()
            || generation.get_version_num() != 4
            || generation.to_string() != value.index_generation
        {
            return Err("indexGeneration must be a canonical non-nil UUIDv4".into());
        }
        if value.index_revision > 9_007_199_254_740_991 {
            return Err("indexRevision exceeds the safe integer range".into());
        }
        Ok(Self {
            index_generation: generation,
            index_revision: value.index_revision,
        })
    }
}
mod uuid_text {
    use serde::Serializer;
    pub fn serialize<S: Serializer>(value: &uuid::Uuid, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceFile {
    pub path: String,
    pub hash: String,
    pub language: String,
    pub text: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SymbolKind {
    Module,
    Function,
    Method,
    Class,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SemanticState {
    Fresh,
    Stale,
    #[default]
    Unavailable,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub source: String,
    pub semantic: SemanticState,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Symbol {
    pub id: String,
    pub name: String,
    pub kind: SymbolKind,
    pub path: String,
    pub range: SourceRange,
    pub parent: Option<String>,
    pub accessor: bool,
    pub provenance: Provenance,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Resolution {
    Internal,
    External,
    Unresolved,
    Ambiguous,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CallSite {
    pub id: String,
    pub caller: String,
    pub callee_text: String,
    pub path: String,
    pub range: SourceRange,
    pub target: Option<String>,
    pub candidate_symbols: Vec<String>,
    pub resolution: Resolution,
    pub ordinal: usize,
    pub regions: Vec<String>,
    pub callback_arguments: Vec<String>,
    pub provenance: Provenance,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ControlRegion {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub parent: Option<String>,
    pub owner: String,
    pub path: String,
    pub range: SourceRange,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub path: Option<String>,
    pub code: String,
    pub message: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IndexStats {
    pub files: usize,
    pub symbols: usize,
    pub calls: usize,
    pub regions: usize,
    pub internal: usize,
    pub external: usize,
    pub unresolved: usize,
    pub ambiguous: usize,
    pub parse_error_files: usize,
    pub semantic_state: SemanticState,
    pub changed_files: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Graph {
    pub schema_version: u32,
    pub files: Vec<SourceFile>,
    pub nodes: Vec<Symbol>,
    pub calls: Vec<CallSite>,
    pub regions: Vec<ControlRegion>,
    pub diagnostics: Vec<Diagnostic>,
    pub stats: IndexStats,
}
impl Default for Graph {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            files: vec![],
            nodes: vec![],
            calls: vec![],
            regions: vec![],
            diagnostics: vec![],
            stats: IndexStats::default(),
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub phase: String,
    pub completed: usize,
    pub total: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub workspace_root: String,
    pub revision: IndexPin,
    pub indexed_at: Option<String>,
    pub stats: IndexStats,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewQuery {
    pub seed: String,
    #[serde(default = "default_depth")]
    pub depth: usize,
    #[serde(default = "default_nodes")]
    pub max_nodes: usize,
    #[serde(default = "default_calls")]
    pub max_calls: usize,
    #[serde(default)]
    pub include_callbacks: bool,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
}
fn default_depth() -> usize {
    1
}
fn default_nodes() -> usize {
    40
}
fn default_calls() -> usize {
    200
}
impl ViewQuery {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.seed.is_empty() && self.seed.len() <= 8192,
            "seed must contain 1..8192 bytes"
        );
        anyhow::ensure!(self.depth <= 5, "depth must be at most 5");
        anyhow::ensure!(
            (1..=150).contains(&self.max_nodes),
            "maxNodes must be 1..150"
        );
        anyhow::ensure!(
            (1..=500).contains(&self.max_calls),
            "maxCalls must be 1..500"
        );
        anyhow::ensure!(
            self.exclude_paths.len() <= 100 && self.exclude_paths.iter().all(|x| x.len() <= 1024),
            "too many or oversized excluded path prefixes"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ViewResult {
    pub revision: IndexPin,
    pub query: ViewQuery,
    pub nodes: Vec<Symbol>,
    pub calls: Vec<CallSite>,
    pub regions: Vec<ControlRegion>,
    pub truncated: bool,
    pub omitted_nodes: usize,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedView {
    pub id: String,
    pub title: String,
    pub query: ViewQuery,
    #[serde(default)]
    pub pins: BTreeMap<String, Position>,
    #[serde(default)]
    pub hidden: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SavedViewState {
    pub view: SavedView,
    pub orphaned_ids: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Annotation {
    pub id: String,
    pub node_id: String,
    pub body: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationState {
    pub annotation: Annotation,
    pub orphaned: bool,
}

impl SavedView {
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_record_id(&self.id)?;
        anyhow::ensure!(
            !self.title.trim().is_empty() && self.title.len() <= 256,
            "title must contain 1..256 bytes"
        );
        self.query.validate()?;
        anyhow::ensure!(
            self.pins.len() <= 150 && self.hidden.len() <= 150,
            "at most 150 pins/hidden symbols"
        );
        for (id, p) in &self.pins {
            anyhow::ensure!(
                !id.is_empty()
                    && id.len() <= 8192
                    && p.x.is_finite()
                    && p.y.is_finite()
                    && p.x.abs() <= 1e7
                    && p.y.abs() <= 1e7,
                "invalid pin"
            );
        }
        anyhow::ensure!(
            self.hidden
                .iter()
                .all(|id| !id.is_empty() && id.len() <= 8192),
            "invalid hidden symbol"
        );
        Ok(())
    }
}
impl Annotation {
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_record_id(&self.id)?;
        anyhow::ensure!(
            !self.node_id.is_empty() && self.node_id.len() <= 8192,
            "invalid nodeId"
        );
        anyhow::ensure!(
            !self.body.trim().is_empty() && self.body.len() <= 65536,
            "annotation body must contain 1..65536 bytes"
        );
        Ok(())
    }
}
pub fn validate_record_id(id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "id must contain 1..128 ASCII letters, digits, '.', '_' or '-'"
    );
    Ok(())
}
