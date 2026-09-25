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
    Type,
    Constructor,
    Implementation,
    AnonymousFunction,
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

/// Prospective #22 evidence types. These do not reinterpret the historical browser Graph.
pub mod v1 {
    use super::{Graph, Position, ViewQuery};
    use serde::{Deserialize, Deserializer, Serialize};
    use std::collections::BTreeMap;

    pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
    #[serde(transparent)]
    pub struct UInt(u64);
    impl UInt {
        pub fn new(value: u64) -> Option<Self> {
            (value <= MAX_SAFE_INTEGER).then_some(Self(value))
        }
        pub fn get(self) -> u64 {
            self.0
        }
    }
    impl<'de> Deserialize<'de> for UInt {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let value = u64::deserialize(deserializer)?;
            Self::new(value)
                .ok_or_else(|| serde::de::Error::custom("UInt exceeds the safe integer range"))
        }
    }

    macro_rules! checked_text {
        ($name:ident, $valid:expr, $message:literal) => {
            #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
            #[serde(transparent)]
            pub struct $name(String);
            impl $name {
                pub fn new(value: impl Into<String>) -> Option<Self> {
                    let value = value.into();
                    ($valid)(&value).then_some(Self(value))
                }
                pub fn as_str(&self) -> &str {
                    &self.0
                }
            }
            impl<'de> Deserialize<'de> for $name {
                fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                    let value = String::deserialize(deserializer)?;
                    Self::new(value).ok_or_else(|| serde::de::Error::custom($message))
                }
            }
        };
    }
    checked_text!(Text, |s: &str| !s.is_empty(), "expected nonempty text");
    checked_text!(
        Hash,
        |s: &str| s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "expected lowercase SHA-256 hex"
    );
    checked_text!(
        SyntaxId,
        |s: &str| s
            .strip_prefix("sid:v1:")
            .is_some_and(|tail| tail.len() == 32
                && tail
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
        "expected sid:v1: followed by 32 lowercase hex digits"
    );
    checked_text!(
        OccurrenceId,
        |s: &str| s
            .strip_prefix("occ:v1:")
            .is_some_and(|tail| tail.len() == 32
                && tail
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
        "expected occ:v1: followed by 32 lowercase hex digits"
    );
    checked_text!(
        Path,
        |s: &str| !s.is_empty()
            && !s.contains(['\\', '\0'])
            && s.split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "expected source-root-relative POSIX path"
    );

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum Language {
        Java,
        Rust,
        Python,
        Javascript,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum Kind {
        Module,
        Namespace,
        Type,
        Implementation,
        Function,
        Method,
        Constructor,
        Field,
        Variable,
        Parameter,
        TypeParameter,
        Alias,
        AnonymousFunction,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum Role {
        Definition,
        Read,
        Write,
        Call,
        Type,
        Import,
        Alias,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum Resolution {
        Resolved,
        External,
        Ambiguous,
        Unresolved,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum ProducerKind {
        Native,
        Semantic,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum PositionEncoding {
        Utf8,
        Utf16,
        UnicodeScalar,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum CoverageState {
        NotRequested,
        Omitted,
        Unsupported,
        Failed,
        Partial,
        Complete,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum EvidenceKind {
        MeasuredSyntax,
        DeclarationBinding,
        SemanticReference,
        TypeRelationship,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum Freshness {
        Fresh,
        PossiblyStale,
        Stale,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum SymbolScope {
        Global,
        Document,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum SymbolScheme {
        Scip,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum RelationshipKind {
        Extends,
        Implements,
        Overrides,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum AnchorKind {
        DeclarationName,
        Callee,
        Invocation,
        Reference,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum JoinStatus {
        Exact,
        Ambiguous,
        Unmatched,
        Unsupported,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum ReferenceSite {
        Declaration,
        Use,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum Dispatch {
        Direct,
        Constructor,
        Virtual,
        Interface,
        Dynamic,
        Unknown,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum ContinuityState {
        Unchanged,
        Changed,
        Unknown,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum AnchorStatus {
        Attached,
        Orphaned,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum AnchorReason {
        None,
        Missing,
        HeaderMismatch,
        GroupChanged,
        UnprovenContinuity,
    }

    fn required_nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        Option::<T>::deserialize(deserializer)
    }

    macro_rules! record {
        ($name:ident { $( $(#[$attr:meta])* $field:ident: $ty:ty),* $(,)? }) => {
            #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
            #[serde(rename_all="camelCase", deny_unknown_fields)]
            pub struct $name { $( $(#[$attr])* pub $field: $ty),* }
        };
    }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", try_from = "UncheckedRange")]
    pub struct Range {
        pub start: UInt,
        pub end: UInt,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct UncheckedRange {
        start: UInt,
        end: UInt,
    }
    impl TryFrom<UncheckedRange> for Range {
        type Error = &'static str;
        fn try_from(value: UncheckedRange) -> Result<Self, Self::Error> {
            if value.start > value.end {
                return Err("range start exceeds end");
            }
            Ok(Self {
                start: value.start,
                end: value.end,
            })
        }
    }
    record!(Producer { id: Text, version: Text, executable_hash: Hash, kind: ProducerKind, languages: Vec<Language>, position_encoding: PositionEncoding });
    record!(SourceSet { id: Text, root_id: Text, languages: Vec<Language>, dependencies: Vec<Text> });
    record!(DocumentKey {
        source_set_id: Text,
        language: Language,
        path: Path
    });
    record!(Document {
        key: DocumentKey,
        revision_id: Text,
        content_hash: Hash,
        byte_length: UInt
    });
    record!(Revision { id: Text, source_set_id: Text, documents: Vec<Document>, toolchain_hash: Hash, config_hash: Hash, dependency_hash: Hash });
    record!(Coverage { producer_id: Text, language: Language, source_set_id: Text, document_path: Path, revision_id: Text, requested: bool, selected: bool, state: CoverageState, supported_roles: Vec<Role>, observed_roles: Vec<Role>, #[serde(deserialize_with="required_nullable")] diagnostic: Option<Text> });
    record!(SemanticBasis { producer_id: Text, producer_version: Text, producer_hash: Hash, artifact_hash: Hash, language: Language, source_set_id: Text, revision_id: Text, source_manifest_hash: Hash, toolchain_hash: Hash, config_hash: Hash, dependency_hash: Hash, lookup_dependencies: Vec<Text> });
    record!(Provenance { id: Text, producer_id: Text, document: DocumentKey, revision_id: Text, content_hash: Hash, evidence_kind: EvidenceKind, #[serde(deserialize_with="required_nullable")] basis: Option<SemanticBasis>, freshness: Freshness });
    record!(Signature { parameter_types: Vec<Text>, type_parameter_count: UInt, variadic: bool });
    record!(Key { kind: Kind, #[serde(deserialize_with="required_nullable")] name: Option<Text>, #[serde(deserialize_with="required_nullable")] signature: Option<Signature>, ordinal: UInt });
    record!(Parameter { #[serde(deserialize_with="required_nullable")] name: Option<Text>, #[serde(deserialize_with="required_nullable")] r#type: Option<Text>, variadic: bool });
    record!(Header { kind: Kind, #[serde(deserialize_with="required_nullable")] name: Option<Text>, modifiers: Vec<Text>, type_parameters: Vec<Text>, parameters: Vec<Parameter>, #[serde(deserialize_with="required_nullable")] result_type: Option<Text>, bases: Vec<Text> });
    record!(Declaration { syntax_id: SyntaxId, document: DocumentKey, revision_id: Text, kind: Kind, #[serde(deserialize_with="required_nullable")] name: Option<Text>, #[serde(deserialize_with="required_nullable")] lookup_key: Option<Text>, ancestors: Vec<Key>, key: Key, range: Range, #[serde(deserialize_with="required_nullable")] name_range: Option<Range>, header: Header, provenance_id: Text });
    record!(SymbolKey { scheme: SymbolScheme, symbol: Text, scope: SymbolScope, #[serde(deserialize_with="required_nullable")] document: Option<DocumentKey> });
    record!(Symbol { key: SymbolKey, #[serde(deserialize_with="required_nullable")] display_name: Option<Text>, declarations: Vec<Target>, provenance_id: Text });
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(
        tag = "kind",
        rename_all = "camelCase",
        rename_all_fields = "camelCase",
        deny_unknown_fields
    )]
    pub enum Target {
        Internal {
            syntax_id: SyntaxId,
            document: DocumentKey,
            revision_id: Text,
        },
        External {
            symbol: SymbolKey,
        },
    }
    record!(DeclarationBinding { #[serde(deserialize_with="required_nullable")] syntax_id: Option<SyntaxId>, symbols: Vec<SymbolKey>, join: Join, provenance_id: Text });
    record!(TypeRelationship {
        kind: RelationshipKind,
        source: Target,
        target: Target,
        provenance_id: Text
    });
    record!(MeasuredAnchor {
        document: DocumentKey,
        revision_id: Text,
        content_hash: Hash,
        range: Range,
        kind: AnchorKind
    });
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum CandidateId {
        Syntax(SyntaxId),
        Occurrence(OccurrenceId),
    }
    record!(Join { anchor: MeasuredAnchor, status: JoinStatus, candidate_ids: Vec<CandidateId>, #[serde(deserialize_with="required_nullable")] diagnostic: Option<Text> });
    record!(Call { id: OccurrenceId, owner_syntax_id: SyntaxId, ordinal: UInt, document: DocumentKey, revision_id: Text, range: Range, #[serde(deserialize_with="required_nullable")] callee_range: Option<Range>, #[serde(deserialize_with="required_nullable")] spelling: Option<Text>, region_ids: Vec<OccurrenceId>, provenance_id: Text });
    record!(ControlRegion { id: OccurrenceId, owner_syntax_id: SyntaxId, ordinal: UInt, document: DocumentKey, revision_id: Text, kind: Text, range: Range, #[serde(deserialize_with="required_nullable")] parent_id: Option<OccurrenceId>, #[serde(deserialize_with="required_nullable")] arm: Option<Text>, provenance_id: Text });
    record!(Reference { id: OccurrenceId, owner_syntax_id: SyntaxId, ordinal: UInt, document: DocumentKey, revision_id: Text, range: Range, spelling: Text, lookup_key: Text, site: ReferenceSite, roles: Vec<Role>, resolution: Resolution, #[serde(deserialize_with="required_nullable")] declared_target: Option<Target>, candidates: Vec<Target>, provenance_id: Text });
    record!(CallBinding { #[serde(deserialize_with="required_nullable")] call_id: Option<OccurrenceId>, join: Join, resolution: Resolution, #[serde(deserialize_with="required_nullable")] declared_target: Option<Target>, candidates: Vec<Target>, dispatch: Dispatch, possible_dispatch: Vec<Target>, possible_dispatch_complete: bool, #[serde(deserialize_with="required_nullable")] stale_target: Option<bool>, provenance_id: Text });
    record!(DurableAnchor {
        syntax_id: SyntaxId,
        document: DocumentKey,
        captured_revision_id: Text,
        header_hash: Hash,
        sibling_group_hash: Hash,
        sibling_count: UInt,
        identical_header_count: UInt
    });
    record!(GroupContinuity { from_revision_id: Text, to_revision_id: Text, state: ContinuityState, #[serde(deserialize_with="required_nullable")] evidence: Option<Text> });
    record!(AnchorResult { status: AnchorStatus, #[serde(deserialize_with="required_nullable")] target_id: Option<SyntaxId>, reason: AnchorReason });

    record!(NativeRevisionContext {
        source_set: SourceSet,
        revision: Revision,
        producer: Producer
    });
    record!(NativeFileEvidence { document: Document, coverage: Coverage, provenance: Provenance, declarations: Vec<Declaration>, calls: Vec<Call>, control_regions: Vec<ControlRegion>, diagnostics: Vec<Text> });
    record!(Evidence { context: NativeRevisionContext, native_files: Vec<NativeFileEvidence>, producers: Vec<Producer>, coverage: Vec<Coverage>, provenance: Vec<Provenance>, symbols: Vec<Symbol>, declaration_bindings: Vec<DeclarationBinding>, type_relationships: Vec<TypeRelationship>, references: Vec<Reference>, call_bindings: Vec<CallBinding> });
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct IndexSnapshot {
        pub graph: Graph,
        pub evidence: Evidence,
    }

    /// Requests cannot set the captured server-managed anchors.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct SavedViewWrite {
        pub id: String,
        pub title: String,
        pub query: ViewQuery,
        pub pins: BTreeMap<String, Position>,
        pub hidden: Vec<String>,
    }
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct AnnotationWrite {
        pub id: String,
        pub node_id: String,
        pub body: String,
    }
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct SavedViewDurable {
        pub id: String,
        pub title: String,
        pub query: ViewQuery,
        pub pins: BTreeMap<String, Position>,
        pub hidden: Vec<String>,
        pub anchors: BTreeMap<SyntaxId, DurableAnchor>,
    }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct AnnotationDurable {
        pub id: String,
        pub node_id: String,
        pub body: String,
        #[serde(deserialize_with = "required_nullable")]
        pub anchor: Option<DurableAnchor>,
    }
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct SavedViewResponse {
        pub view: SavedViewDurable,
        pub attachments: BTreeMap<String, Attachment>,
        pub orphaned_ids: Vec<String>,
    }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct AnnotationResponse {
        pub annotation: AnnotationDurable,
        pub attachment: Attachment,
        pub orphaned: bool,
    }
    /// Compatibility dispositions cannot masquerade as #22 AnchorResult reasons.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum CompatibilityDisposition {
        MissingAnchor,
        CaptureUnavailable,
    }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct CompatibilityAttachment {
        pub status: CompatibilityDisposition,
    }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum Attachment {
        Evaluated(AnchorResult),
        Compatibility(CompatibilityAttachment),
    }
}
