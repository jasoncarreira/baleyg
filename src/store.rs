//! SQLite snapshots and durable user data. Connections are never shared between threads.
pub mod anchors;
pub mod requests;
pub mod topology;
use crate::model::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::{Deref, DerefMut},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestOneShotHook(Mutex<Option<Box<dyn FnOnce() + Send>>>);
#[cfg(test)]
impl std::fmt::Debug for TestOneShotHook {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TestOneShotHook")
    }
}
#[cfg(test)]
impl TestOneShotHook {
    pub(crate) fn set(&self, hook: impl FnOnce() + Send + 'static) {
        *self.0.lock().unwrap() = Some(Box::new(hook));
    }
    pub(crate) fn run(&self) {
        let hook = self.0.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    roots: topology::TopologyRoots,
    identity: Arc<topology::WorkspaceIdentity>,
    workspace_root: String,
    recovery_required: Arc<AtomicBool>,
    recovery_disposition: Arc<AtomicU8>,
    obsolete_format_marker: Arc<Mutex<Option<IndexFormatMarker>>>,
    pending_request_completion: Arc<Mutex<Option<requests::PendingCompletion>>>,
    request_file_witness: Arc<Mutex<Option<(u64, u64)>>>,
    writer_counters: Arc<Mutex<Option<WriterCounters>>>,
    #[cfg(test)]
    test_queue_before_shared_hook: Arc<TestOneShotHook>,
    #[cfg(test)]
    test_queue_select_hook: Arc<TestOneShotHook>,
    #[cfg(test)]
    test_exclusive_recovery_hook: Arc<TestOneShotHook>,
    #[cfg(test)]
    test_queue_finish_failures: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    test_queue_post_commit_failures: Arc<std::sync::atomic::AtomicUsize>,
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryDisposition {
    Ready = 0,
    Rebuild = 1,
    RecreatePending = 2,
    RootReplaced = 3,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MetadataColumn {
    SchemaVersion,
    ExtractorVersion,
    IndexGeneration,
    IndexRevision,
}
impl MetadataColumn {
    fn sql(self) -> (&'static str, &'static str, &'static str, &'static [u8]) {
        match self {
            Self::SchemaVersion => (
                "SELECT typeof(schema_version) FROM index_metadata WHERE singleton=1",
                "SELECT schema_version FROM index_metadata WHERE singleton=1",
                "schema_version",
                b"schema_version",
            ),
            Self::ExtractorVersion => (
                "SELECT typeof(extractor_version) FROM index_metadata WHERE singleton=1",
                "SELECT extractor_version FROM index_metadata WHERE singleton=1",
                "extractor_version",
                b"extractor_version",
            ),
            Self::IndexGeneration => (
                "SELECT typeof(index_generation) FROM index_metadata WHERE singleton=1",
                "SELECT index_generation FROM index_metadata WHERE singleton=1",
                "index_generation",
                b"index_generation",
            ),
            Self::IndexRevision => (
                "SELECT typeof(index_revision) FROM index_metadata WHERE singleton=1",
                "SELECT index_revision FROM index_metadata WHERE singleton=1",
                "index_revision",
                b"index_revision",
            ),
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
enum MetadataAtom {
    Null,
    Integer(i64),
    Real(u64),
    Streamed {
        column: MetadataColumn,
        text: bool,
        byte_length: usize,
        sha256: [u8; 32],
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct RecoveryWitness {
    pragma_schema: u32,
    schema_version: MetadataAtom,
    extractor_version: MetadataAtom,
    index_generation: MetadataAtom,
    index_revision: MetadataAtom,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecoveryBaseline {
    witness: RecoveryWitness,
    pin: Option<IndexPin>,
    compatible: bool,
}
impl RecoveryBaseline {
    pub(crate) fn pin(&self) -> Option<IndexPin> {
        self.pin
    }
}
#[derive(Clone, Debug)]
enum ExpectedPublication {
    Pin(IndexPin),
    Recovery(Box<RecoveryBaseline>),
}
#[derive(Clone, Copy)]
enum PublicationTarget<'a> {
    Live,
    Stage(&'a StagedIndex),
}
struct PublicationPlan<'a> {
    expected: ExpectedPublication,
    target: PublicationTarget<'a>,
}
#[derive(Debug)]
struct ExceptionalIndexFormat;
impl std::fmt::Display for ExceptionalIndexFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("exceptional index format recovery deferred")
    }
}
impl std::error::Error for ExceptionalIndexFormat {}
#[derive(Clone, Debug, Eq, PartialEq)]
struct IndexFormatMarker {
    schema_version: i64,
    extractor_version: String,
}
impl IndexFormatMarker {
    fn is_obsolete(&self) -> bool {
        self.schema_version != i64::from(DATABASE_SCHEMA_VERSION)
            || self.extractor_version != EXTRACTOR_VERSION
    }
}
#[derive(Debug)]
struct ObsoleteIndexFormat(IndexFormatMarker);
impl std::fmt::Display for ObsoleteIndexFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("recovery_required: obsolete index format")
    }
}
impl std::error::Error for ObsoleteIndexFormat {}
#[derive(Debug)]
struct SelectedIntegrity(String);
impl std::fmt::Display for SelectedIntegrity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for SelectedIntegrity {}
#[derive(Debug)]
struct ControlIntegrity(String);
impl std::fmt::Display for ControlIntegrity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for ControlIntegrity {}
macro_rules! control_ensure {
    ($condition:expr, $message:expr $(,)?) => {
        if !$condition {
            return Err(ControlIntegrity($message.into()).into());
        }
    };
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublishStage {
    BeforeTransaction,
    AfterFile,
    BeforeCommit,
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum ActivationStage {
    BeforeRename,
    AfterJournalBackupBeforeDirFsync,
    AfterRenameBeforeDirFsync,
}
// Serde's compact JSON serializer writes exactly the bytes persisted by `json()`.
// Count and abort while streaming; never build an over-limit encoded source.
struct EncodedSourceBudget {
    bytes: usize,
    max_bytes: usize,
}
impl std::io::Write for EncodedSourceBudget {
    fn write(&mut self, chunk: &[u8]) -> std::io::Result<usize> {
        let next = self
            .bytes
            .checked_add(chunk.len())
            .filter(|size| *size <= self.max_bytes)
            .ok_or_else(|| std::io::Error::other("selected source JSON budget exceeded"))?;
        self.bytes = next;
        Ok(chunk.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
const SELECTED_ANCILLARY_BYTE_SQL: &str = r#"SELECT COALESCE(max(row_bytes),0),COALESCE(sum(row_bytes),0) FROM (
SELECT COALESCE(length(CAST(x.version_id AS BLOB)),0)+COALESCE(length(CAST(x.role_kind AS BLOB)),0)+COALESCE(length(CAST(x.role AS BLOB)),0)+COALESCE(length(CAST(x.ordinal AS BLOB)),0) AS row_bytes FROM native_version_coverage_roles x WHERE x.version_id=?1
UNION ALL
SELECT COALESCE(length(CAST(x.version_id AS BLOB)),0)+COALESCE(length(CAST(x.syntax_id AS BLOB)),0)+COALESCE(length(CAST(x.owner_syntax_id AS BLOB)),0)+COALESCE(length(CAST(x.kind AS BLOB)),0)+COALESCE(length(CAST(x.name AS BLOB)),0)+COALESCE(length(CAST(x.lookup_key AS BLOB)),0)+COALESCE(length(CAST(x.key_signature_present AS BLOB)),0)+COALESCE(length(CAST(x.key_type_parameter_count AS BLOB)),0)+COALESCE(length(CAST(x.key_variadic AS BLOB)),0)+COALESCE(length(CAST(x.key_ordinal AS BLOB)),0)+COALESCE(length(CAST(x.start_byte AS BLOB)),0)+COALESCE(length(CAST(x.end_byte AS BLOB)),0)+COALESCE(length(CAST(x.name_start AS BLOB)),0)+COALESCE(length(CAST(x.name_end AS BLOB)),0) AS row_bytes FROM native_version_declarations x WHERE x.version_id=?1
UNION ALL
SELECT COALESCE(length(CAST(x.version_id AS BLOB)),0)+COALESCE(length(CAST(x.syntax_id AS BLOB)),0)+COALESCE(length(CAST(x.ordinal AS BLOB)),0)+COALESCE(length(CAST(x.kind AS BLOB)),0)+COALESCE(length(CAST(x.name AS BLOB)),0)+COALESCE(length(CAST(x.sibling_ordinal AS BLOB)),0)+COALESCE(length(CAST(x.signature_present AS BLOB)),0)+COALESCE(length(CAST(x.type_parameter_count AS BLOB)),0)+COALESCE(length(CAST(x.variadic AS BLOB)),0) AS row_bytes FROM native_version_declaration_ancestors x WHERE x.version_id=?1
UNION ALL
SELECT COALESCE(length(CAST(x.version_id AS BLOB)),0)+COALESCE(length(CAST(x.syntax_id AS BLOB)),0)+COALESCE(length(CAST(x.ordinal AS BLOB)),0)+COALESCE(length(CAST(x.type_name AS BLOB)),0) AS row_bytes FROM native_version_own_signature_types x WHERE x.version_id=?1
UNION ALL
SELECT COALESCE(length(CAST(x.version_id AS BLOB)),0)+COALESCE(length(CAST(x.syntax_id AS BLOB)),0)+COALESCE(length(CAST(x.ancestor_ordinal AS BLOB)),0)+COALESCE(length(CAST(x.ordinal AS BLOB)),0)+COALESCE(length(CAST(x.type_name AS BLOB)),0) AS row_bytes FROM native_version_ancestor_signature_types x WHERE x.version_id=?1
UNION ALL
SELECT COALESCE(length(CAST(x.version_id AS BLOB)),0)+COALESCE(length(CAST(x.syntax_id AS BLOB)),0)+COALESCE(length(CAST(x.kind AS BLOB)),0)+COALESCE(length(CAST(x.name AS BLOB)),0)+COALESCE(length(CAST(x.result_type AS BLOB)),0) AS row_bytes FROM native_version_headers x WHERE x.version_id=?1
UNION ALL
SELECT COALESCE(length(CAST(x.version_id AS BLOB)),0)+COALESCE(length(CAST(x.syntax_id AS BLOB)),0)+COALESCE(length(CAST(x.item_kind AS BLOB)),0)+COALESCE(length(CAST(x.ordinal AS BLOB)),0)+COALESCE(length(CAST(x.value AS BLOB)),0) AS row_bytes FROM native_version_header_items x WHERE x.version_id=?1
UNION ALL
SELECT COALESCE(length(CAST(x.version_id AS BLOB)),0)+COALESCE(length(CAST(x.syntax_id AS BLOB)),0)+COALESCE(length(CAST(x.ordinal AS BLOB)),0)+COALESCE(length(CAST(x.name AS BLOB)),0)+COALESCE(length(CAST(x.type_name AS BLOB)),0)+COALESCE(length(CAST(x.variadic AS BLOB)),0) AS row_bytes FROM native_version_parameters x WHERE x.version_id=?1
UNION ALL
SELECT COALESCE(length(CAST(x.version_id AS BLOB)),0)+COALESCE(length(CAST(x.id AS BLOB)),0)+COALESCE(length(CAST(x.owner_syntax_id AS BLOB)),0)+COALESCE(length(CAST(x.ordinal AS BLOB)),0)+COALESCE(length(CAST(x.start_byte AS BLOB)),0)+COALESCE(length(CAST(x.end_byte AS BLOB)),0)+COALESCE(length(CAST(x.callee_start AS BLOB)),0)+COALESCE(length(CAST(x.callee_end AS BLOB)),0)+COALESCE(length(CAST(x.spelling AS BLOB)),0) AS row_bytes FROM native_version_calls x WHERE x.version_id=?1
UNION ALL
SELECT COALESCE(length(CAST(x.version_id AS BLOB)),0)+COALESCE(length(CAST(x.id AS BLOB)),0)+COALESCE(length(CAST(x.owner_syntax_id AS BLOB)),0)+COALESCE(length(CAST(x.ordinal AS BLOB)),0)+COALESCE(length(CAST(x.kind AS BLOB)),0)+COALESCE(length(CAST(x.start_byte AS BLOB)),0)+COALESCE(length(CAST(x.end_byte AS BLOB)),0)+COALESCE(length(CAST(x.parent_id AS BLOB)),0)+COALESCE(length(CAST(x.arm AS BLOB)),0) AS row_bytes FROM native_version_control_regions x WHERE x.version_id=?1
UNION ALL
SELECT COALESCE(length(CAST(x.version_id AS BLOB)),0)+COALESCE(length(CAST(x.call_id AS BLOB)),0)+COALESCE(length(CAST(x.region_id AS BLOB)),0)+COALESCE(length(CAST(x.ordinal AS BLOB)),0) AS row_bytes FROM native_version_call_regions x WHERE x.version_id=?1
UNION ALL
SELECT COALESCE(length(CAST(x.projection_id AS BLOB)),0)+COALESCE(length(CAST(x.graph_projection_id AS BLOB)),0)+COALESCE(length(CAST(x.id AS BLOB)),0)+COALESCE(length(CAST(x.name AS BLOB)),0)+COALESCE(length(CAST(x.qualified_name AS BLOB)),0)+COALESCE(length(CAST(x.path AS BLOB)),0)+COALESCE(length(CAST(x.payload AS BLOB)),0) AS row_bytes FROM classes x WHERE x.projection_id=?2
UNION ALL
SELECT COALESCE(length(CAST(x.projection_id AS BLOB)),0)+COALESCE(length(CAST(x.id AS BLOB)),0)+COALESCE(length(CAST(x.owner AS BLOB)),0)+COALESCE(length(CAST(x.target AS BLOB)),0)+COALESCE(length(CAST(x.payload AS BLOB)),0) AS row_bytes FROM class_relations x WHERE x.projection_id=?2
)"#;
const DATABASE_SCHEMA_VERSION: u32 = 8;
const EXTRACTOR_VERSION: &str = "native-v4-delta-v1";
const EVIDENCE_FORMAT: &str = "terminal-native-graph-v1";
const CACHE_SCHEMA_V8: &str = r#"
CREATE TABLE index_metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1), schema_version INTEGER NOT NULL CHECK(schema_version=8), extractor_version TEXT NOT NULL CHECK(extractor_version='native-v4-delta-v1'), root_spelling TEXT NOT NULL, root_device TEXT NOT NULL, root_inode TEXT NOT NULL, index_generation TEXT NOT NULL, index_revision INTEGER NOT NULL CHECK(index_revision BETWEEN 0 AND 9007199254740991), last_opened_at INTEGER NOT NULL CHECK(last_opened_at BETWEEN 0 AND 9007199254740991), indexed_at TEXT NOT NULL, stats TEXT NOT NULL, diagnostics TEXT NOT NULL, reconciled_incarnation TEXT, reconcile_options TEXT);
CREATE TABLE native_producers(id TEXT NOT NULL,version TEXT NOT NULL,executable_hash TEXT NOT NULL CHECK(length(executable_hash)=64),kind TEXT NOT NULL CHECK(kind='native'),position_encoding TEXT NOT NULL CHECK(position_encoding='utf8'),PRIMARY KEY(id,version));
CREATE TABLE native_producer_languages(producer_id TEXT NOT NULL,producer_version TEXT NOT NULL,language TEXT NOT NULL,inventory_authenticated INTEGER NOT NULL CHECK(inventory_authenticated IN (0,1)),ordinal INTEGER NOT NULL CHECK(ordinal>=0),PRIMARY KEY(producer_id,producer_version,language),UNIQUE(producer_id,producer_version,ordinal),FOREIGN KEY(producer_id,producer_version) REFERENCES native_producers(id,version) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE native_producer_inputs(producer_id TEXT NOT NULL,producer_version TEXT NOT NULL,language TEXT NOT NULL,component_name TEXT NOT NULL,ordinal INTEGER NOT NULL CHECK(ordinal>=0),PRIMARY KEY(producer_id,producer_version,language,component_name),UNIQUE(producer_id,producer_version,language,ordinal),FOREIGN KEY(producer_id,producer_version,language) REFERENCES native_producer_languages(producer_id,producer_version,language) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE native_source_sets(id TEXT PRIMARY KEY,root_id TEXT NOT NULL);
CREATE TABLE native_source_set_languages(source_set_id TEXT NOT NULL REFERENCES native_source_sets(id) DEFERRABLE INITIALLY DEFERRED,language TEXT NOT NULL,ordinal INTEGER NOT NULL CHECK(ordinal>=0),PRIMARY KEY(source_set_id,language),UNIQUE(source_set_id,ordinal));
CREATE TABLE native_source_set_dependencies(source_set_id TEXT NOT NULL REFERENCES native_source_sets(id) DEFERRABLE INITIALLY DEFERRED,dependency_id TEXT NOT NULL REFERENCES native_source_sets(id) DEFERRABLE INITIALLY DEFERRED,ordinal INTEGER NOT NULL CHECK(ordinal>=0),PRIMARY KEY(source_set_id,dependency_id),UNIQUE(source_set_id,ordinal));
CREATE TABLE native_revisions(id TEXT PRIMARY KEY,source_set_id TEXT NOT NULL REFERENCES native_source_sets(id) DEFERRABLE INITIALLY DEFERRED,toolchain_hash TEXT NOT NULL CHECK(length(toolchain_hash)=64),config_hash TEXT NOT NULL CHECK(length(config_hash)=64),dependency_hash TEXT NOT NULL CHECK(length(dependency_hash)=64),native_revision_id TEXT NOT NULL,source_inventory TEXT NOT NULL,dependency_observations TEXT NOT NULL,reconciled_incarnation TEXT,reconcile_options TEXT,class_warnings TEXT NOT NULL,class_truncated INTEGER NOT NULL CHECK(class_truncated IN (0,1)),graph_stats TEXT NOT NULL CHECK(json_valid(graph_stats) AND json_type(graph_stats)='object'),graph_diagnostics TEXT NOT NULL CHECK(json_valid(graph_diagnostics) AND json_type(graph_diagnostics)='array'),published_index_revision INTEGER NOT NULL CHECK(published_index_revision BETWEEN 0 AND 9007199254740991));
CREATE UNIQUE INDEX native_revisions_pin ON native_revisions(published_index_revision);
CREATE TABLE revision_capture_inputs(revision_id TEXT NOT NULL REFERENCES native_revisions(id) DEFERRABLE INITIALLY DEFERRED,input_key TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(revision_id,input_key));
CREATE TABLE document_versions(id TEXT PRIMARY KEY,source_set_id TEXT NOT NULL REFERENCES native_source_sets(id) DEFERRABLE INITIALLY DEFERRED,language TEXT NOT NULL,path TEXT NOT NULL,content_hash TEXT NOT NULL CHECK(length(content_hash)=64),extraction_context TEXT NOT NULL CHECK(length(extraction_context)=64),producer_id TEXT NOT NULL,producer_version TEXT NOT NULL,byte_length INTEGER NOT NULL CHECK(byte_length>=0 AND byte_length=length(source_bytes)),source_bytes BLOB NOT NULL,native_witness TEXT NOT NULL CHECK(length(native_witness)=64),UNIQUE(source_set_id,language,path,content_hash,extraction_context,producer_id,producer_version),UNIQUE(id,source_set_id,language,path),UNIQUE(id,language),FOREIGN KEY(producer_id,producer_version,language) REFERENCES native_producer_languages(producer_id,producer_version,language) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX document_versions_path ON document_versions(source_set_id,language,path,id);
CREATE TABLE graph_projections(id TEXT PRIMARY KEY,document_version_id TEXT NOT NULL,language TEXT NOT NULL,graph_hash TEXT NOT NULL CHECK(length(graph_hash)=64),state TEXT NOT NULL CHECK(state IN ('staged','ready','unavailable')),class_extraction_state TEXT NOT NULL CHECK(class_extraction_state IN ('notApplicable','ready')),class_extraction_payload TEXT,CHECK((class_extraction_state='ready')=(class_extraction_payload IS NOT NULL)),CHECK((language IN ('java','python'))=(class_extraction_state IN ('ready'))),UNIQUE(document_version_id,graph_hash),UNIQUE(id,document_version_id),FOREIGN KEY(document_version_id,language) REFERENCES document_versions(id,language) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX graph_projections_version ON graph_projections(document_version_id,id);
CREATE TABLE graph_nodes(projection_id TEXT NOT NULL REFERENCES graph_projections(id) DEFERRABLE INITIALLY DEFERRED,id TEXT NOT NULL,name TEXT NOT NULL,path TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(projection_id,id));
CREATE INDEX graph_nodes_name ON graph_nodes(projection_id,name,id);
CREATE TABLE graph_calls(projection_id TEXT NOT NULL,id TEXT NOT NULL,caller TEXT NOT NULL,target TEXT,path TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(projection_id,id),FOREIGN KEY(projection_id,caller) REFERENCES graph_nodes(projection_id,id) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX graph_calls_caller ON graph_calls(projection_id,caller,id);
CREATE TABLE graph_regions(projection_id TEXT NOT NULL,id TEXT NOT NULL,owner TEXT NOT NULL,path TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(projection_id,id),FOREIGN KEY(projection_id,owner) REFERENCES graph_nodes(projection_id,id) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX graph_regions_owner ON graph_regions(projection_id,owner,id);
CREATE TABLE class_projections(id TEXT PRIMARY KEY,graph_projection_id TEXT NOT NULL REFERENCES graph_projections(id) DEFERRABLE INITIALLY DEFERRED,content_hash TEXT NOT NULL CHECK(length(content_hash)=64),state TEXT NOT NULL CHECK(state IN ('ready','unavailable')),UNIQUE(graph_projection_id,content_hash),UNIQUE(id,graph_projection_id));
CREATE TABLE classes(projection_id TEXT NOT NULL,graph_projection_id TEXT NOT NULL,id TEXT NOT NULL,name TEXT NOT NULL,qualified_name TEXT NOT NULL,path TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(projection_id,id),FOREIGN KEY(projection_id,graph_projection_id) REFERENCES class_projections(id,graph_projection_id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(graph_projection_id,id) REFERENCES graph_nodes(projection_id,id) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX classes_path ON classes(projection_id,path,id);
CREATE TABLE class_relations(projection_id TEXT NOT NULL,id TEXT NOT NULL,owner TEXT NOT NULL,target TEXT,payload TEXT NOT NULL,PRIMARY KEY(projection_id,id),FOREIGN KEY(projection_id,owner) REFERENCES classes(projection_id,id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(projection_id,target) REFERENCES classes(projection_id,id) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX class_relations_owner ON class_relations(projection_id,owner,id);
CREATE INDEX class_relations_target ON class_relations(projection_id,target,id);
CREATE TABLE revision_documents(revision_id TEXT NOT NULL REFERENCES native_revisions(id) DEFERRABLE INITIALLY DEFERRED,source_set_id TEXT NOT NULL,language TEXT NOT NULL,path TEXT NOT NULL,document_version_id TEXT NOT NULL,graph_projection_id TEXT NOT NULL,class_projection_id TEXT,capture_stat TEXT,coverage_requested INTEGER NOT NULL CHECK(coverage_requested IN (0,1)),coverage_selected INTEGER NOT NULL CHECK(coverage_selected IN (0,1)),coverage_state TEXT NOT NULL CHECK(coverage_state IN ('notRequested','omitted','unsupported','failed','partial','complete')),coverage_diagnostic TEXT,ordinal INTEGER NOT NULL CHECK(ordinal>=0),CHECK((coverage_state='complete')=(coverage_diagnostic IS NULL)),PRIMARY KEY(revision_id,source_set_id,language,path),UNIQUE(revision_id,ordinal),FOREIGN KEY(document_version_id,source_set_id,language,path) REFERENCES document_versions(id,source_set_id,language,path) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(graph_projection_id,document_version_id) REFERENCES graph_projections(id,document_version_id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(class_projection_id,graph_projection_id) REFERENCES class_projections(id,graph_projection_id) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX revision_documents_version ON revision_documents(document_version_id,revision_id);
CREATE INDEX revision_documents_graph ON revision_documents(graph_projection_id,revision_id);
CREATE INDEX revision_documents_class ON revision_documents(class_projection_id,revision_id);
CREATE TABLE native_version_coverage_roles(version_id TEXT NOT NULL REFERENCES document_versions(id) DEFERRABLE INITIALLY DEFERRED,role_kind TEXT NOT NULL CHECK(role_kind IN ('supported','observed')),role TEXT NOT NULL CHECK(role IN ('definition','call')),ordinal INTEGER NOT NULL CHECK(ordinal>=0),PRIMARY KEY(version_id,role_kind,ordinal),UNIQUE(version_id,role_kind,role));
CREATE TABLE native_version_declarations(version_id TEXT NOT NULL REFERENCES document_versions(id) DEFERRABLE INITIALLY DEFERRED,syntax_id TEXT NOT NULL,owner_syntax_id TEXT,kind TEXT NOT NULL,name TEXT,lookup_key TEXT,key_signature_present INTEGER NOT NULL CHECK(key_signature_present IN (0,1)),key_type_parameter_count INTEGER,key_variadic INTEGER,key_ordinal INTEGER NOT NULL CHECK(key_ordinal>=0),start_byte INTEGER NOT NULL,end_byte INTEGER NOT NULL,name_start INTEGER,name_end INTEGER,CHECK(start_byte>=0 AND end_byte>=start_byte),CHECK((name IS NULL)=(lookup_key IS NULL) AND (name IS NULL)=(name_start IS NULL) AND (name_start IS NULL)=(name_end IS NULL)),CHECK(name_start IS NULL OR (name_start>=start_byte AND name_end<=end_byte AND name_end>name_start)),CHECK((key_signature_present=0 AND key_type_parameter_count IS NULL AND key_variadic IS NULL) OR (key_signature_present=1 AND key_type_parameter_count>=0 AND key_variadic IN (0,1))),PRIMARY KEY(version_id,syntax_id),FOREIGN KEY(version_id,owner_syntax_id) REFERENCES native_version_declarations(version_id,syntax_id) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX native_version_declarations_lookup ON native_version_declarations(lookup_key,version_id,syntax_id);
CREATE TABLE native_version_declaration_ancestors(version_id TEXT NOT NULL,syntax_id TEXT NOT NULL,ordinal INTEGER NOT NULL CHECK(ordinal>=0),kind TEXT NOT NULL,name TEXT,sibling_ordinal INTEGER NOT NULL CHECK(sibling_ordinal>=0),signature_present INTEGER NOT NULL CHECK(signature_present IN (0,1)),type_parameter_count INTEGER,variadic INTEGER,CHECK((signature_present=0 AND type_parameter_count IS NULL AND variadic IS NULL) OR (signature_present=1 AND type_parameter_count>=0 AND variadic IN (0,1))),PRIMARY KEY(version_id,syntax_id,ordinal),FOREIGN KEY(version_id,syntax_id) REFERENCES native_version_declarations(version_id,syntax_id) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE native_version_own_signature_types(version_id TEXT NOT NULL,syntax_id TEXT NOT NULL,ordinal INTEGER NOT NULL CHECK(ordinal>=0),type_name TEXT NOT NULL,PRIMARY KEY(version_id,syntax_id,ordinal),FOREIGN KEY(version_id,syntax_id) REFERENCES native_version_declarations(version_id,syntax_id) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE native_version_ancestor_signature_types(version_id TEXT NOT NULL,syntax_id TEXT NOT NULL,ancestor_ordinal INTEGER NOT NULL CHECK(ancestor_ordinal>=0),ordinal INTEGER NOT NULL CHECK(ordinal>=0),type_name TEXT NOT NULL,PRIMARY KEY(version_id,syntax_id,ancestor_ordinal,ordinal),FOREIGN KEY(version_id,syntax_id,ancestor_ordinal) REFERENCES native_version_declaration_ancestors(version_id,syntax_id,ordinal) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE native_version_headers(version_id TEXT NOT NULL,syntax_id TEXT NOT NULL,kind TEXT NOT NULL,name TEXT,result_type TEXT,PRIMARY KEY(version_id,syntax_id),FOREIGN KEY(version_id,syntax_id) REFERENCES native_version_declarations(version_id,syntax_id) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE native_version_header_items(version_id TEXT NOT NULL,syntax_id TEXT NOT NULL,item_kind TEXT NOT NULL CHECK(item_kind IN ('modifier','typeParameter','base')),ordinal INTEGER NOT NULL CHECK(ordinal>=0),value TEXT NOT NULL,PRIMARY KEY(version_id,syntax_id,item_kind,ordinal),FOREIGN KEY(version_id,syntax_id) REFERENCES native_version_headers(version_id,syntax_id) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE native_version_parameters(version_id TEXT NOT NULL,syntax_id TEXT NOT NULL,ordinal INTEGER NOT NULL CHECK(ordinal>=0),name TEXT,type_name TEXT,variadic INTEGER NOT NULL CHECK(variadic IN (0,1)),PRIMARY KEY(version_id,syntax_id,ordinal),FOREIGN KEY(version_id,syntax_id) REFERENCES native_version_headers(version_id,syntax_id) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE native_version_calls(version_id TEXT NOT NULL,id TEXT NOT NULL,owner_syntax_id TEXT NOT NULL,ordinal INTEGER NOT NULL CHECK(ordinal>=0),start_byte INTEGER NOT NULL,end_byte INTEGER NOT NULL,callee_start INTEGER,callee_end INTEGER,spelling TEXT,CHECK(start_byte>=0 AND end_byte>start_byte),CHECK((callee_start IS NULL)=(callee_end IS NULL)),CHECK(callee_start IS NULL OR (callee_start>=start_byte AND callee_end<=end_byte AND callee_end>callee_start)),PRIMARY KEY(version_id,id),UNIQUE(version_id,owner_syntax_id,ordinal),FOREIGN KEY(version_id,owner_syntax_id) REFERENCES native_version_declarations(version_id,syntax_id) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX native_version_calls_owner ON native_version_calls(version_id,owner_syntax_id,ordinal);
CREATE TABLE native_version_control_regions(version_id TEXT NOT NULL,id TEXT NOT NULL,owner_syntax_id TEXT NOT NULL,ordinal INTEGER NOT NULL CHECK(ordinal>=0),kind TEXT NOT NULL,start_byte INTEGER NOT NULL,end_byte INTEGER NOT NULL,parent_id TEXT,arm TEXT,CHECK(start_byte>=0 AND end_byte>start_byte),PRIMARY KEY(version_id,id),UNIQUE(version_id,owner_syntax_id,ordinal),UNIQUE(version_id,id,owner_syntax_id),FOREIGN KEY(version_id,owner_syntax_id) REFERENCES native_version_declarations(version_id,syntax_id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(version_id,parent_id,owner_syntax_id) REFERENCES native_version_control_regions(version_id,id,owner_syntax_id) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX native_version_regions_owner ON native_version_control_regions(version_id,owner_syntax_id,ordinal);
CREATE TABLE native_version_call_regions(version_id TEXT NOT NULL,call_id TEXT NOT NULL,region_id TEXT NOT NULL,ordinal INTEGER NOT NULL CHECK(ordinal>=0),PRIMARY KEY(version_id,call_id,ordinal),UNIQUE(version_id,call_id,region_id),FOREIGN KEY(version_id,call_id) REFERENCES native_version_calls(version_id,id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(version_id,region_id) REFERENCES native_version_control_regions(version_id,id) DEFERRABLE INITIALLY DEFERRED);
"#;
/// A normal connection keeps the verified index use lock until SQLite closes.
struct IndexConnection {
    db: Connection,
    _use_guard: topology::UseGuard,
}
impl Deref for IndexConnection {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.db
    }
}
impl DerefMut for IndexConnection {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.db
    }
}
// Only the live variant owns a new shared-use guard. The stage variant keeps
// the caller's verified exclusive leader guard alive across its transaction.
enum PublicationConnection {
    Live(IndexConnection),
    Stage(Connection),
}
impl Deref for PublicationConnection {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self {
            Self::Live(db) => &db.db,
            Self::Stage(db) => db,
        }
    }
}
impl DerefMut for PublicationConnection {
    fn deref_mut(&mut self) -> &mut Connection {
        match self {
            Self::Live(db) => &mut db.db,
            Self::Stage(db) => db,
        }
    }
}

/// Immutable revision chosen inside an admitted read transaction. This is not
/// a surrogate for control metadata: status, root and schema remain live-only.
struct ReadRevision {
    pin: IndexPin,
    key: String,
}
impl ReadRevision {
    fn current(db: &Connection) -> Result<Self> {
        let (generation, revision): (String, i64) = db.query_row(
            "SELECT index_generation,index_revision FROM index_metadata WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let pin: IndexPin = serde_json::from_value(
            serde_json::json!({"indexGeneration": generation, "indexRevision": revision}),
        )?;
        Ok(Self {
            pin,
            key: format!("pin:v1:{}:{}", pin.index_generation, pin.index_revision),
        })
    }
}
/// One coherent native read snapshot retained until its owned result passes T03.
pub struct EvidenceResponse {
    store: Store,
    db: IndexConnection,
    follower: topology::FollowerGuard,
    marker: uuid::Uuid,
}
impl EvidenceResponse {
    pub fn finish<T>(&self, value: T) -> Result<T> {
        self.store.identity.verify()?;
        self.follower.verify(self.marker)?;
        Ok(value)
    }
    pub fn status(&self) -> Result<IndexStatus> {
        self.store.read_status(&self.db)
    }
    pub fn source_at(
        &self,
        path: &str,
        expected: Option<IndexPin>,
    ) -> Result<Option<(IndexPin, SourceFile)>> {
        let selected = self.store.read_revision(&self.db, expected)?;
        let source = self
            .store
            .selected_source_row_for(&self.db, path, &selected)?;
        if source.is_some() {
            self.store
                .attest_selected_document_for(&self.db, path, &selected)?;
        }
        Ok(source.map(|source| (selected.pin, source)))
    }
    pub fn validate_selected_view(&self, view: &ViewResult, sources: &[SourceFile]) -> Result<()> {
        self.store
            .validate_selected_view_in(&self.db, view, sources)
    }
    pub fn query_view(&self, query: &ViewQuery) -> Result<Option<ViewResult>> {
        query.validate()?;
        self.store.query_view_in(&self.db, query, None)
    }
    /// Release the SQLite snapshot before slow response assembly or provider work.
    /// The follower guard retains protected use and the snapshot's incarnation.
    pub fn into_fence(self, policy: EvidenceFencePolicy) -> EvidenceFence {
        let Self {
            store,
            db,
            follower,
            marker,
        } = self;
        drop(db);
        EvidenceFence {
            store,
            follower,
            marker,
            policy,
        }
    }
}
/// Unpinned responses need T03 only; cached packets also require their exact pin.
#[derive(Clone, Copy)]
pub enum EvidenceFencePolicy {
    T03,
    ExactPin(IndexPin),
}
pub struct EvidenceFence {
    store: Store,
    follower: topology::FollowerGuard,
    marker: uuid::Uuid,
    policy: EvidenceFencePolicy,
}
impl EvidenceFence {
    pub fn finish<T>(&self, value: T) -> Result<T> {
        self.store.identity.verify()?;
        self.follower.verify(self.marker)?;
        if let EvidenceFencePolicy::ExactPin(pin) = self.policy {
            self.store.with_evidence(|db| {
                self.store.read_revision(db, Some(pin))?;
                Ok(())
            })?;
            self.store.identity.verify()?;
            self.follower.verify(self.marker)?;
        }
        Ok(value)
    }
}

fn reject_sidecars(path: &Path, writable: bool) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        if suffix == "-journal" && !writable {
            continue;
        }
        let sidecar = path.with_file_name(format!(
            "{}{suffix}",
            path.file_name()
                .context("index filename missing")?
                .to_string_lossy()
        ));
        match std::fs::symlink_metadata(&sidecar) {
            Ok(_) => anyhow::bail!("recovery_required: {}", sidecar.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn verify_index_file(path: &Path) -> Result<()> {
    use std::io::Read;
    use std::os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    };
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .context("incompatible_index: missing or unreadable database")?;
    let meta = file.metadata()?;
    let named = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o777 == 0o600
            && meta.nlink() == 1
            && meta.dev() == named.dev()
            && meta.ino() == named.ino(),
        "unsafe_index: {}",
        path.display()
    );
    let mut header = [0u8; 20];
    match file.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(ExceptionalIndexFormat.into());
        }
        Err(error) => return Err(error.into()),
    }
    if &header[..16] != b"SQLite format 3\0" {
        return Err(ExceptionalIndexFormat.into());
    }
    // WAL (2,2) and other valid-header journaling modes are operational or
    // incompatible state, not permission to replace this inode.
    ensure!(
        header[18] == 1 && header[19] == 1,
        "incompatible_index: unsupported SQLite journaling mode"
    );
    let _ = file.as_raw_fd();
    Ok(())
}
// Treat dangling symlinks as existing, so a first open never replaces an unsafe path.
fn index_path_present(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn restore_index_journal(journal: &IndexFileWitness, backup: &Path, dir: &Path) -> Result<()> {
    journal.verify_at(backup)?;
    ensure!(
        !index_path_present(&journal.path)?,
        "unsafe_index: journal pathname reappeared"
    );
    std::fs::rename(backup, &journal.path)?;
    std::fs::File::open(dir)?.sync_all()?;
    journal.verify()
}

// A corrupt index can be witnessed without trusting its SQLite header or payload.
struct IndexFileWitness {
    path: std::path::PathBuf,
    file: std::fs::File,
}
impl IndexFileWitness {
    fn open(path: &Path) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let witness = Self {
            path: path.to_owned(),
            file,
        };
        witness.verify()?;
        Ok(witness)
    }
    fn verify(&self) -> Result<()> {
        self.verify_at(&self.path)
    }
    fn verify_at(&self, path: &Path) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let held = self.file.metadata()?;
        let named = std::fs::symlink_metadata(path)?;
        ensure!(
            held.is_file()
                && held.uid() == unsafe { libc::geteuid() }
                && held.mode() & 0o777 == 0o600
                && held.nlink() == 1
                && named.is_file()
                && !named.file_type().is_symlink()
                && held.dev() == named.dev()
                && held.ino() == named.ino(),
            "unsafe_index: protected index pathname changed"
        );
        Ok(())
    }
}
// The staged file is private to this attempt. On failure, only unlink our own inode.
struct StagedIndex {
    path: std::path::PathBuf,
    file: std::fs::File,
    published: bool,
}
impl StagedIndex {
    fn verify_path(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let held = self.file.metadata()?;
        let named = std::fs::symlink_metadata(&self.path)?;
        ensure!(
            held.is_file()
                && held.uid() == unsafe { libc::geteuid() }
                && held.mode() & 0o777 == 0o600
                && held.nlink() == 1
                && named.is_file()
                && !named.file_type().is_symlink()
                && held.dev() == named.dev()
                && held.ino() == named.ino(),
            "unsafe_index: staged pathname changed"
        );
        Ok(())
    }
}
impl Drop for StagedIndex {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if self.published {
            return;
        }
        if let (Ok(owned), Ok(named)) =
            (self.file.metadata(), std::fs::symlink_metadata(&self.path))
            && owned.dev() == named.dev()
            && owned.ino() == named.ino()
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
/// Match every cache schema object, including type, name, owning table, SQL,
/// and SQLite autoindexes. Unknown views/triggers must never execute on rebaseline.
fn validate_cache_shape(db: &Connection) -> Result<()> {
    type Object = (String, String, String, Option<String>);
    fn objects(db: &Connection) -> Result<Vec<Object>> {
        Ok(db
            .prepare("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name")?
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    let expected = Connection::open_in_memory()?;
    let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    control_ensure!(
        version == DATABASE_SCHEMA_VERSION,
        "incompatible_index: unknown schema version"
    );
    expected.execute_batch(CACHE_SCHEMA_V8)?;
    control_ensure!(
        objects(db)? == objects(&expected)?,
        "incompatible_index: unknown cache object type, name or shape"
    );
    Ok(())
}
fn open_index_marker_probe(path: &Path, writable: bool) -> Result<Connection> {
    use rusqlite::OpenFlags;
    reject_sidecars(path, writable)?;
    verify_index_file(path)?;
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let db = storage_result(Connection::open_with_flags(path, flags))?;
    storage_result(db.busy_timeout(Duration::ZERO))?;
    storage_result(db.pragma_update(None, "temp_store", "MEMORY"))?;
    storage_result(db.pragma_update(None, "foreign_keys", "ON"))?;
    if writable {
        storage_result(db.pragma_update(None, "synchronous", "FULL"))?;
    } else {
        storage_result(db.pragma_update(None, "query_only", "ON"))?;
    }
    let mode: String = storage_result(db.pragma_query_value(None, "journal_mode", |r| r.get(0)))?;
    ensure!(mode == "delete", "incompatible_index: journal mode");
    Ok(db)
}

/// Inspect only the bounded control marker, never historical evidence or schema objects.
fn read_index_format_marker(db: &Connection) -> Result<IndexFormatMarker> {
    let mut statement = storage_result(db.prepare(
        "SELECT typeof(schema_version),schema_version,typeof(extractor_version),
                length(CAST(extractor_version AS BLOB)),
                CASE WHEN length(CAST(extractor_version AS BLOB))<=256
                     THEN extractor_version END
         FROM index_metadata LIMIT 2",
    ))?;
    let mut rows = storage_result(statement.query([]))?;
    let row = storage_result(rows.next())?
        .context("incompatible_index: missing index metadata marker")?;
    let schema_type: String = row.get(0)?;
    let schema: i64 = row.get(1)?;
    let extractor_type: String = row.get(2)?;
    let length: i64 = row.get(3)?;
    let extractor: Option<String> = row.get(4)?;
    control_ensure!(
        schema_type == "integer"
            && extractor_type == "text"
            && (0..=256).contains(&length)
            && extractor
                .as_ref()
                .is_some_and(|s| s.len() == length as usize)
            && storage_result(rows.next())?.is_none(),
        "incompatible_index: malformed index metadata marker"
    );
    Ok(IndexFormatMarker {
        schema_version: schema,
        extractor_version: extractor.expect("typed extractor checked"),
    })
}

fn open_index(path: &Path, writable: bool) -> Result<Connection> {
    let db = open_index_marker_probe(path, writable)?;
    let marker = read_index_format_marker(&db)?;
    if marker.is_obsolete() {
        return Err(ObsoleteIndexFormat(marker).into());
    }
    let version: u32 = storage_result(db.pragma_query_value(None, "user_version", |r| r.get(0)))?;
    ensure!(
        version == DATABASE_SCHEMA_VERSION,
        "incompatible_index: schema version {version}"
    );
    storage_result(db.prepare(
        "SELECT index_generation,index_revision,indexed_at,stats,diagnostics FROM index_metadata",
    ))?;
    storage_result(db.prepare("SELECT revision_id,source_set_id,language,path,document_version_id,graph_projection_id,class_projection_id FROM revision_documents"))?;
    storage_result(db.prepare(
        "SELECT id,source_set_id,language,path,content_hash,source_bytes FROM document_versions",
    ))?;
    storage_result(db.prepare(
        "SELECT id,document_version_id,language,class_extraction_state FROM graph_projections",
    ))?;
    storage_result(db.prepare("SELECT projection_id,id,payload FROM graph_nodes"))?;
    storage_result(db.prepare("SELECT projection_id,id,payload FROM graph_calls"))?;
    storage_result(db.prepare("SELECT projection_id,id,payload FROM graph_regions"))?;
    storage_result(db.prepare("SELECT id,graph_projection_id FROM class_projections"))?;
    storage_result(db.prepare("SELECT projection_id,id,payload FROM classes"))?;
    storage_result(db.prepare("SELECT projection_id,id,payload FROM class_relations"))?;
    validate_cache_shape(&db)?;
    Ok(db)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryClass {
    Rebuild,
    RecreatePending,
    Hard,
}
fn recovery_class(error: &anyhow::Error) -> RecoveryClass {
    if error.downcast_ref::<ExceptionalIndexFormat>().is_some()
        || error.downcast_ref::<ObsoleteIndexFormat>().is_some()
    {
        return RecoveryClass::RecreatePending;
    }
    if error.downcast_ref::<SelectedIntegrity>().is_some()
        || error.downcast_ref::<ControlIntegrity>().is_some()
        || error.downcast_ref::<serde_json::Error>().is_some()
        || error.downcast_ref::<std::string::FromUtf8Error>().is_some()
    {
        return RecoveryClass::Rebuild;
    }
    if let Some(sqlite) = error.downcast_ref::<rusqlite::Error>() {
        return match sqlite {
            rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::Utf8Error(..)
            | rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::QueryReturnedNoRows
            | rusqlite::Error::QueryReturnedMoreThanOneRow => RecoveryClass::Rebuild,
            rusqlite::Error::SqliteFailure(info, _)
                if matches!(
                    info.code,
                    rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
                ) =>
            {
                RecoveryClass::RecreatePending
            }
            _ => RecoveryClass::Hard,
        };
    }
    RecoveryClass::Hard
}
fn selected_integrity(error: anyhow::Error) -> anyhow::Error {
    if recovery_class(&error) != RecoveryClass::Hard
        || error.downcast_ref::<rusqlite::Error>().is_some()
        || error.downcast_ref::<std::io::Error>().is_some()
    {
        error
    } else {
        SelectedIntegrity(format!("{error:#}")).into()
    }
}

fn storage_result<T>(result: rusqlite::Result<T>) -> Result<T> {
    match result {
        Err(rusqlite::Error::SqliteFailure(info, _))
            if info.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK =>
        {
            anyhow::bail!("recovery_required: hot index journal")
        }
        Err(rusqlite::Error::SqliteFailure(info, _))
            if matches!(
                info.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            anyhow::bail!("storage_busy: SQLite lock contention")
        }
        other => Ok(other?),
    }
}
fn json<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}
fn rows_at<T: DeserializeOwned>(db: &Connection, sql: &str, revision_id: &str) -> Result<Vec<T>> {
    let mut stmt = storage_result(db.prepare(sql))?;
    let values = storage_result(stmt.query_map([revision_id], |r| r.get::<_, String>(0)))?;
    values
        .map(|v| Ok(serde_json::from_str(&storage_result(v)?)?))
        .collect()
}
fn one_at<T: DeserializeOwned>(
    db: &Connection,
    sql: &str,
    id: &str,
    revision_id: &str,
) -> Result<Option<T>> {
    let value: Option<String> = storage_result(
        db.query_row(sql, params![id, revision_id], |r| r.get(0))
            .optional(),
    )?;
    value.map(|v| Ok(serde_json::from_str(&v)?)).transpose()
}
fn check_cancel(cancel: &CancelFlag) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "index publication cancelled"
    );
    Ok(())
}

fn validate_graph(graph: &Graph, cancel: &CancelFlag) -> Result<IndexStats> {
    let mut files = BTreeMap::new();
    for file in &graph.files {
        check_cancel(cancel)?;
        ensure!(
            !file.path.is_empty()
                && !Path::new(&file.path).is_absolute()
                && !file.path.split('/').any(|p| p == ".." || p.is_empty()),
            "invalid source path"
        );
        let mut lines = vec![0usize];
        lines.extend(
            file.text
                .bytes()
                .enumerate()
                .filter_map(|(i, b)| (b == b'\n').then_some(i + 1)),
        );
        ensure!(
            files.insert(file.path.as_str(), (file, lines)).is_none(),
            "duplicate file path"
        );
    }
    let span = |path: &str, range: &SourceRange| -> Result<()> {
        let (file, lines) = files
            .get(path)
            .context("graph references missing source file")?;
        ensure!(
            range.start_byte <= range.end_byte
                && range.end_byte <= file.text.len()
                && file.text.is_char_boundary(range.start_byte)
                && file.text.is_char_boundary(range.end_byte),
            "invalid source byte span"
        );
        for (byte, line, column) in [
            (range.start_byte, range.start_line, range.start_column),
            (range.end_byte, range.end_line, range.end_column),
        ] {
            let index = lines.partition_point(|start| *start <= byte) - 1;
            ensure!(
                line == index + 1 && column == byte - lines[index] + 1,
                "invalid source line/column span"
            );
        }
        Ok(())
    };
    let id = |id: &str| -> Result<()> {
        ensure!(
            !id.is_empty() && id.len() <= 8192 && !id.contains('\0'),
            "invalid graph id"
        );
        Ok(())
    };
    let mut nodes = BTreeMap::new();
    for node in &graph.nodes {
        check_cancel(cancel)?;
        id(&node.id)?;
        span(&node.path, &node.range)?;
        ensure!(
            nodes.insert(node.id.as_str(), node).is_none(),
            "duplicate node id"
        );
    }
    for node in &graph.nodes {
        if let Some(parent) = &node.parent {
            ensure!(nodes.contains_key(parent.as_str()), "dangling node parent");
        }
    }
    let mut regions = BTreeMap::new();
    for region in &graph.regions {
        check_cancel(cancel)?;
        id(&region.id)?;
        span(&region.path, &region.range)?;
        ensure!(
            nodes.contains_key(region.owner.as_str()),
            "dangling region owner"
        );
        ensure!(
            regions.insert(region.id.as_str(), region).is_none(),
            "duplicate region id"
        );
    }
    for region in &graph.regions {
        if let Some(parent) = &region.parent {
            ensure!(
                regions
                    .get(parent.as_str())
                    .is_some_and(|r| r.owner == region.owner),
                "dangling or foreign region parent"
            );
        }
    }
    let mut calls = BTreeSet::new();
    let mut stats = graph.stats.clone();
    stats.files = graph.files.len();
    stats.symbols = graph.nodes.len();
    stats.calls = graph.calls.len();
    stats.regions = graph.regions.len();
    stats.internal = 0;
    stats.external = 0;
    stats.unresolved = 0;
    stats.ambiguous = 0;
    ensure!(
        stats.parse_error_files <= stats.files,
        "parse error count exceeds source files"
    );
    for call in &graph.calls {
        check_cancel(cancel)?;
        id(&call.id)?;
        span(&call.path, &call.range)?;
        ensure!(calls.insert(call.id.as_str()), "duplicate call id");
        ensure!(
            nodes.contains_key(call.caller.as_str()),
            "dangling call caller"
        );
        // A call is a source-witnessed terminal occurrence, never a graph edge.
        // Production indexers project validated native IDs; shape-only Store
        // fixtures exercise graph integrity without replaying the extractor.
        stats.unresolved += 1;
        for region in &call.regions {
            ensure!(
                regions
                    .get(region.as_str())
                    .is_some_and(|r| r.owner == call.caller),
                "dangling or foreign call region"
            );
        }
    }
    Ok(stats)
}

fn class_metadata(db: &Connection, selected: &ReadRevision) -> Result<(Vec<String>, bool)> {
    let length: i64 = db.query_row(
        "SELECT length(CAST(class_warnings AS BLOB)) FROM native_revisions WHERE id=?1",
        [&selected.key],
        |r| r.get(0),
    )?;
    ensure!(
        (0..=256 * 1024).contains(&length),
        "incompatible_index: class catalog byte budget exceeded"
    );
    let (warnings, mut truncated): (String, bool) = db.query_row(
        "SELECT class_warnings,class_truncated FROM native_revisions WHERE id=?1",
        [&selected.key],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let warnings: Vec<String> = serde_json::from_str(&warnings)?;
    let mut bytes = 0;
    let mut visible = Vec::new();
    for warning in warnings {
        bytes += serde_json::to_vec(&warning)?.len() + 1;
        if bytes > 256 * 1024 {
            truncated = true;
            visible.push("Further catalog warnings omitted by the presentation byte limit.".into());
            break;
        }
        visible.push(warning);
    }
    Ok((visible, truncated))
}
/// Read at most 64 KiB of class JSON into Rust. Large member arrays are
/// clipped in SQLite, without materializing their full strings in the API.
/// IDs, class metadata, and source ranges remain exact; only member arrays shrink.
fn presentation_class(
    store: &Store,
    db: &Connection,
    id: &str,
    selected: &ReadRevision,
) -> Result<Option<(crate::classes::ClassDefinition, usize, bool)>> {
    use crate::class_diagram::CLASS_BYTES;
    let size: Option<i64> = db
        .query_row(
            "SELECT length(CAST(payload AS BLOB)) FROM classes JOIN revision_documents m ON m.revision_id=?2 AND m.class_projection_id=classes.projection_id WHERE classes.id=?1",
            params![id, selected.key],
            |r| r.get(0),
        )
        .optional()
        .map_err(|error| store.report_selected_failure(error.into()))?;
    let Some(size) = size else {
        return Ok(None);
    };
    let valid: bool = db
        .query_row(
            "SELECT json_valid(payload) FROM classes JOIN revision_documents m ON m.revision_id=?2 AND m.class_projection_id=classes.projection_id WHERE classes.id=?1",
            params![id, selected.key],
            |r| r.get(0),
        )
        .map_err(|error| store.report_selected_failure(error.into()))?;
    if !valid {
        return Err(store.report_selected_failure(
            SelectedIntegrity("incompatible_index: selected class JSON invalid".into()).into(),
        ));
    }
    if size <= CLASS_BYTES as i64 {
        let payload: String = db
            .query_row("SELECT payload FROM classes JOIN revision_documents m ON m.revision_id=?2 AND m.class_projection_id=classes.projection_id WHERE classes.id=?1", params![id, selected.key], |r| {
                r.get(0)
            })
            .map_err(|error| store.report_selected_failure(error.into()))?;
        let class = serde_json::from_str(&payload)
            .map_err(|error| store.report_selected_failure(error.into()))?;
        return Ok(Some((class, payload.len(), false)));
    }
    let clip_shape: bool = db
        .query_row(
            "SELECT json_type(payload,'$.fields')='array'
                AND json_type(payload,'$.methods')='array'
                AND NOT EXISTS(SELECT 1 FROM json_each(payload,'$.fields') WHERE type!='object')
                AND NOT EXISTS(SELECT 1 FROM json_each(payload,'$.methods') WHERE type!='object')
             FROM classes JOIN revision_documents m ON m.revision_id=?2 AND m.class_projection_id=classes.projection_id WHERE classes.id=?1",
            params![id, selected.key],
            |r| r.get(0),
        )
        .map_err(|error| store.report_selected_failure(error.into()))?;
    if !clip_shape {
        return Err(store.report_selected_failure(
            SelectedIntegrity("incompatible_index: selected class member JSON invalid".into())
                .into(),
        ));
    }
    for count in [32, 16, 8, 4, 2, 1, 0] {
        let payload: Option<String> = db.query_row("WITH clipped AS (
            SELECT json_set(payload,
                '$.fields',(SELECT json_group_array(json(value)) FROM json_each(classes.payload,'$.fields') WHERE key < ?2),
                '$.methods',(SELECT json_group_array(json(value)) FROM json_each(classes.payload,'$.methods') WHERE key < ?2),
                '$.truncated',json('true')) AS payload FROM classes JOIN revision_documents m ON m.revision_id=?4 AND m.class_projection_id=classes.projection_id WHERE classes.id=?1)
            SELECT CASE WHEN length(CAST(payload AS BLOB)) <= ?3 THEN payload ELSE NULL END FROM clipped",
            params![id, count, CLASS_BYTES as i64, selected.key], |r| r.get(0))
            .map_err(|error| store.report_selected_failure(error.into()))?;
        if let Some(payload) = payload {
            let class = serde_json::from_str(&payload)
                .map_err(|error| store.report_selected_failure(error.into()))?;
            return Ok(Some((class, payload.len(), true)));
        }
    }
    // Pathological non-member metadata cannot fit without changing identity.
    Ok(None)
}
fn resolve_class_id(
    store: &Store,
    db: &Connection,
    id: &str,
    selected: &ReadRevision,
) -> Result<String> {
    use crate::class_diagram::InvalidRequest;
    let mut current = id.to_owned();
    let mut seen = BTreeSet::new();
    for depth in 0..64 {
        ensure!(
            seen.insert(current.clone()),
            InvalidRequest("The selected symbol has a cyclic class ancestry.")
        );
        if db.query_row(
            "SELECT EXISTS(SELECT 1 FROM classes c JOIN revision_documents m ON m.revision_id=?2 AND m.class_projection_id=c.projection_id WHERE c.id=?1)",
            params![current, selected.key],
            |r| r.get::<_, bool>(0),
        )? {
            return Ok(current);
        }
        let symbol = one_at::<Symbol>(db, "SELECT n.payload FROM graph_nodes n JOIN revision_documents m ON m.revision_id=?2 AND m.graph_projection_id=n.projection_id WHERE n.id=?1", &current, &selected.key)
            .map_err(|error| store.report_selected_failure(error))?
            .ok_or(InvalidRequest(
                "Choose an indexed Java or Python class or method.",
            ))?;
        ensure!(
            depth > 0 || matches!(symbol.kind, SymbolKind::Method | SymbolKind::Function),
            InvalidRequest("Choose an indexed Java or Python class or method.")
        );
        current = symbol.parent.ok_or(InvalidRequest(
            "The selected method has no supported enclosing class.",
        ))?;
    }
    Err(InvalidRequest("The selected symbol exceeds the class ancestry limit.").into())
}

fn resolve_class(
    store: &Store,
    db: &Connection,
    id: &str,
    selected: &ReadRevision,
) -> Result<(crate::classes::ClassDefinition, bool)> {
    let id = resolve_class_id(store, db, id, selected)?;
    let (class, _, clipped) = presentation_class(store, db, &id, selected)?.ok_or(
        crate::class_diagram::InvalidRequest("Class metadata exceeds the presentation byte limit."),
    )?;
    Ok((class, clipped))
}

fn v8_id(prefix: &str, domain: &[u8], value: serde_json::Value) -> String {
    format!(
        "{prefix}:{}",
        crate::native_ids::digest(domain, &crate::native_ids::canonical(&value))
    )
}

struct V8DocumentProjection {
    version_id: String,
    graph_id: String,
    class_id: String,
    graph_hash: String,
    class_hash: String,
}

// Group immutable publication facts once, outside the IMMEDIATE writer transaction.
// The original slices retain source order; projection hashing applies its own ID order.
#[derive(Default)]
struct PublicationRows<'a> {
    nodes: Vec<&'a Symbol>,
    calls: Vec<&'a CallSite>,
    regions: Vec<&'a ControlRegion>,
    classes: Vec<&'a crate::classes::ClassDefinition>,
    relations: Vec<&'a crate::classes::ClassRelation>,
    native_declarations: Vec<&'a crate::native_evidence::Declaration>,
    native_calls: Vec<&'a crate::native_evidence::Call>,
    native_regions: Vec<&'a crate::native_evidence::ControlRegion>,
}
fn publication_rows<'a>(
    graph: &'a Graph,
    native: &'a crate::native_evidence::Artifact,
    classes: &'a crate::classes::Catalog,
) -> Result<BTreeMap<&'a str, PublicationRows<'a>>> {
    let mut rows = BTreeMap::<&str, PublicationRows>::new();
    for file in &graph.files {
        ensure!(
            rows.insert(&file.path, PublicationRows::default())
                .is_none(),
            "duplicate graph path"
        );
    }
    for node in &graph.nodes {
        rows.get_mut(node.path.as_str())
            .context("graph node path missing")?
            .nodes
            .push(node);
    }
    for call in &graph.calls {
        rows.get_mut(call.path.as_str())
            .context("graph call path missing")?
            .calls
            .push(call);
    }
    for region in &graph.regions {
        rows.get_mut(region.path.as_str())
            .context("graph region path missing")?
            .regions
            .push(region);
    }
    let mut owners = BTreeMap::new();
    for class in &classes.classes {
        rows.get_mut(class.symbol.path.as_str())
            .context("class path missing")?
            .classes
            .push(class);
        ensure!(
            owners
                .insert(class.symbol.id.as_str(), class.symbol.path.as_str())
                .is_none(),
            "duplicate class owner"
        );
    }
    for relation in &classes.relations {
        let path = owners
            .get(relation.owner.as_str())
            .context("class relation owner missing")?;
        rows.get_mut(path)
            .context("class relation path missing")?
            .relations
            .push(relation);
    }
    for decl in &native.declarations {
        rows.get_mut(decl.document.path.as_str())
            .context("native declaration path missing")?
            .native_declarations
            .push(decl);
    }
    for call in &native.calls {
        rows.get_mut(call.document.path.as_str())
            .context("native call path missing")?
            .native_calls
            .push(call);
    }
    for region in &native.control_regions {
        rows.get_mut(region.document.path.as_str())
            .context("native control path missing")?
            .native_regions
            .push(region);
    }
    Ok(rows)
}

fn v8_document_projection(
    file: &SourceFile,
    native: &crate::native_evidence::Artifact,
    grouped: &PublicationRows<'_>,
) -> Result<V8DocumentProjection> {
    let context = crate::native_ids::extraction_context(&file.language, &[])?;
    let version_id = v8_id(
        "document:v1",
        b"baleyg.document-version.v1\0",
        serde_json::json!({
            "sourceSetId": native.source_set.id, "language": file.language, "path": file.path,
            "contentHash": file.hash, "extractionContext": context,
            "producerId": native.producer.id, "producerVersion": native.producer.version,
        }),
    );
    let mut nodes = grouped.nodes.clone();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut calls = grouped.calls.clone();
    calls.sort_by(|a, b| a.id.cmp(&b.id));
    let mut regions = grouped.regions.clone();
    regions.sort_by(|a, b| a.id.cmp(&b.id));
    let graph_hash = v8_id(
        "",
        b"baleyg.graph-projection.v1\0",
        serde_json::json!({
            "documentVersionId": version_id,
            "nodes": nodes, "calls": calls, "regions": regions,
        }),
    );
    let graph_hash = graph_hash.trim_start_matches(':').to_owned();
    let graph_id = format!("graph:v1:{graph_hash}");
    let mut selected_classes = grouped.classes.clone();
    selected_classes.sort_by(|a, b| a.symbol.id.cmp(&b.symbol.id));
    let mut selected_relations = grouped.relations.clone();
    selected_relations.sort_by(|a, b| a.id.cmp(&b.id));
    let class_hash = v8_id(
        "",
        b"baleyg.class-projection.v1\0",
        serde_json::json!({
            "graphProjectionId": graph_id,
            "classes": selected_classes, "relations": selected_relations,
        }),
    );
    let class_hash = class_hash.trim_start_matches(':').to_owned();
    let class_id = format!("class-projection:v1:{class_hash}");
    Ok(V8DocumentProjection {
        version_id,
        graph_id,
        class_id,
        graph_hash,
        class_hash,
    })
}

// Reuse decisions are made against one authenticated head before BEGIN IMMEDIATE.
// The writer checks the witnessed pin again under its CAS before using these IDs.
#[derive(Clone, Copy, Debug, Default)]
struct ReusedFamilies {
    native: bool,
    graph: bool,
    class: bool,
}

#[derive(Default)]
struct PreflightReuse {
    pin: Option<IndexPin>,
    families: BTreeMap<String, ReusedFamilies>,
}
impl PreflightReuse {
    fn for_path(&self, path: &str) -> ReusedFamilies {
        self.families.get(path).copied().unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowByteCount {
    pub rows: u64,
    pub bytes: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WriterCounters {
    pub manifest: RowByteCount,
    pub native: RowByteCount,
    pub graph: RowByteCount,
    pub class: RowByteCount,
    pub total: RowByteCount,
    pub reused_occurrence_reads: u64,
}
impl WriterCounters {
    fn record(&mut self, family: &str, values: &[&dyn rusqlite::ToSql]) -> Result<()> {
        use rusqlite::types::{ToSqlOutput, Value, ValueRef};
        let mut bytes = 0u64;
        for value in values {
            bytes += match value.to_sql()? {
                ToSqlOutput::Borrowed(ValueRef::Null) | ToSqlOutput::Owned(Value::Null) => 0,
                ToSqlOutput::Borrowed(ValueRef::Integer(_) | ValueRef::Real(_))
                | ToSqlOutput::Owned(Value::Integer(_) | Value::Real(_)) => 8,
                ToSqlOutput::Borrowed(ValueRef::Text(v) | ValueRef::Blob(v)) => v.len() as u64,
                ToSqlOutput::Owned(Value::Text(v)) => v.len() as u64,
                ToSqlOutput::Owned(Value::Blob(v)) => v.len() as u64,
                _ => anyhow::bail!("unsupported SQLite writer counter value"),
            };
        }
        let group = match family {
            "manifest" => &mut self.manifest,
            "native" => &mut self.native,
            "graph" => &mut self.graph,
            "class" => &mut self.class,
            _ => anyhow::bail!("unknown writer counter family"),
        };
        group.rows += 1;
        group.bytes += bytes;
        self.total.rows += 1;
        self.total.bytes += bytes;
        Ok(())
    }
}

// New and historical version/projection IDs are canonical. A changed document
// can collide with retained history; those non-preflight collisions are compared
// before reuse rather than silently accepting corrupt or incomplete evidence.
#[derive(Default)]
struct ImmutableAppend {
    schema: BTreeMap<String, (Vec<String>, Vec<usize>)>,
    reused: BTreeMap<String, BTreeSet<String>>,
    expected: BTreeMap<(String, String), i64>,
    counters: WriterCounters,
}
impl ImmutableAppend {
    fn insert_manifest(
        &mut self,
        db: &Connection,
        sql: &str,
        values: &[&dyn rusqlite::ToSql],
    ) -> Result<()> {
        ensure!(
            db.execute(sql, values)? == 1,
            "incompatible_index: missing manifest row"
        );
        self.counters.record("manifest", values)
    }
    fn table(sql: &str) -> Result<&str> {
        let table = sql
            .split_whitespace()
            .nth(2)
            .context("missing immutable table")?;
        ensure!(
            matches!(
                table,
                "native_producers"
                    | "native_producer_languages"
                    | "native_source_sets"
                    | "native_source_set_languages"
                    | "native_source_set_dependencies"
                    | "document_versions"
                    | "graph_projections"
                    | "class_projections"
                    | "graph_nodes"
                    | "graph_calls"
                    | "graph_regions"
                    | "classes"
                    | "class_relations"
                    | "native_version_coverage_roles"
                    | "native_version_declarations"
                    | "native_version_declaration_ancestors"
                    | "native_version_own_signature_types"
                    | "native_version_ancestor_signature_types"
                    | "native_version_headers"
                    | "native_version_header_items"
                    | "native_version_parameters"
                    | "native_version_calls"
                    | "native_version_control_regions"
                    | "native_version_call_regions"
            ),
            "incompatible_index: unrecognized immutable table"
        );
        Ok(table)
    }
    fn key(db: &Connection, values: &[&dyn rusqlite::ToSql], i: usize) -> Result<String> {
        Ok(db.query_row("SELECT CAST(?1 AS TEXT)", [values[i]], |row| row.get(0))?)
    }
    fn group(
        table: &str,
        db: &Connection,
        values: &[&dyn rusqlite::ToSql],
    ) -> Result<Option<String>> {
        let key = match table {
            "native_producer_languages" => Some(format!(
                "{}\u{1f}{}",
                Self::key(db, values, 0)?,
                Self::key(db, values, 1)?
            )),
            "native_source_set_languages"
            | "native_source_set_dependencies"
            | "graph_nodes"
            | "graph_calls"
            | "graph_regions"
            | "classes"
            | "class_relations"
            | "native_version_coverage_roles"
            | "native_version_declarations"
            | "native_version_declaration_ancestors"
            | "native_version_own_signature_types"
            | "native_version_ancestor_signature_types"
            | "native_version_headers"
            | "native_version_header_items"
            | "native_version_parameters"
            | "native_version_calls"
            | "native_version_control_regions"
            | "native_version_call_regions" => Some(Self::key(db, values, 0)?),
            _ => None,
        };
        Ok(key)
    }
    fn insert(
        &mut self,
        db: &Connection,
        sql: &str,
        values: &[&dyn rusqlite::ToSql],
    ) -> Result<usize> {
        let table = Self::table(sql)?;
        if !self.schema.contains_key(table) {
            let mut stmt = db.prepare(&format!("PRAGMA table_info({table})"))?;
            let mut columns = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let keys = columns
                .iter()
                .enumerate()
                .filter(|(_, (_, order))| *order > 0)
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            ensure!(
                !keys.is_empty(),
                "incompatible_index: immutable table has no key"
            );
            let names = columns.drain(..).map(|(name, _)| name).collect::<Vec<_>>();
            self.schema.insert(table.to_owned(), (names, keys));
        }
        let (columns, keys) = &self.schema[table];
        ensure!(
            columns.len() == values.len(),
            "incompatible_index: immutable column count"
        );
        let predicate = |indices: &[usize]| {
            indices
                .iter()
                .map(|i| format!("{} IS ?{}", columns[*i], i + 1))
                .collect::<Vec<_>>()
                .join(" AND ")
        };
        let key_predicate = keys
            .iter()
            .enumerate()
            .map(|(parameter, i)| format!("{} IS ?{}", columns[*i], parameter + 1))
            .collect::<Vec<_>>()
            .join(" AND ");
        let key_values = keys.iter().map(|i| values[*i]).collect::<Vec<_>>();
        let exists: i64 = db.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {key_predicate})"),
            key_values.as_slice(),
            |row| row.get(0),
        )?;
        let inserted = if exists != 0 {
            if !matches!(
                table,
                "native_producers"
                    | "native_producer_languages"
                    | "native_source_sets"
                    | "native_source_set_languages"
                    | "native_source_set_dependencies"
            ) {
                self.counters.reused_occurrence_reads += 1;
            }
            let all = (0..columns.len()).collect::<Vec<_>>();
            let exact: i64 = db.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {table} WHERE {})",
                    predicate(&all)
                ),
                values,
                |row| row.get(0),
            )?;
            control_ensure!(
                exact == 1,
                format!("incompatible_index: immutable {table} differs from captured evidence")
            );
            0
        } else {
            db.execute(sql, values)?
        };
        if inserted != 0 {
            let family = if table.starts_with("native_") || table == "document_versions" {
                "native"
            } else if table.starts_with("graph_") {
                "graph"
            } else {
                "class"
            };
            self.counters.record(family, values)?;
        }
        let parent = match table {
            "native_producers" => Some(format!(
                "{}\u{1f}{}",
                Self::key(db, values, 0)?,
                Self::key(db, values, 1)?
            )),
            "native_source_sets" | "document_versions" | "graph_projections"
            | "class_projections" => Some(Self::key(db, values, 0)?),
            _ => None,
        };
        if exists != 0
            && let Some(parent) = parent
        {
            self.reused
                .entry(table.to_owned())
                .or_default()
                .insert(parent);
        }
        if let Some(parent) = Self::group(table, db, values)? {
            *self.expected.entry((table.to_owned(), parent)).or_default() += 1;
        }
        Ok(inserted)
    }
    fn finish(&self, db: &Connection) -> Result<()> {
        const FAMILIES: &[(&str, &[(&str, &str)])] = &[
            (
                "native_producers",
                &[
                    ("native_producer_languages", "producer"),
                    ("native_producer_inputs", "producer"),
                ],
            ),
            (
                "native_source_sets",
                &[
                    ("native_source_set_languages", "source_set_id"),
                    ("native_source_set_dependencies", "source_set_id"),
                ],
            ),
            (
                "document_versions",
                &[
                    ("native_version_coverage_roles", "version_id"),
                    ("native_version_declarations", "version_id"),
                    ("native_version_declaration_ancestors", "version_id"),
                    ("native_version_own_signature_types", "version_id"),
                    ("native_version_ancestor_signature_types", "version_id"),
                    ("native_version_headers", "version_id"),
                    ("native_version_header_items", "version_id"),
                    ("native_version_parameters", "version_id"),
                    ("native_version_calls", "version_id"),
                    ("native_version_control_regions", "version_id"),
                    ("native_version_call_regions", "version_id"),
                ],
            ),
            (
                "graph_projections",
                &[
                    ("graph_nodes", "projection_id"),
                    ("graph_calls", "projection_id"),
                    ("graph_regions", "projection_id"),
                ],
            ),
            (
                "class_projections",
                &[
                    ("classes", "projection_id"),
                    ("class_relations", "projection_id"),
                ],
            ),
        ];
        for (parent_table, children) in FAMILIES {
            if let Some(reused) = self.reused.get(*parent_table) {
                for parent in reused {
                    for (child, column) in *children {
                        let actual: i64 = if *column == "producer" {
                            let (id, version) = parent
                                .split_once('\u{1f}')
                                .context("invalid producer key")?;
                            db.query_row(
                                &format!("SELECT count(*) FROM {child} WHERE producer_id=?1 AND producer_version=?2"),
                                params![id, version], |row| row.get(0),
                            )?
                        } else {
                            db.query_row(
                                &format!("SELECT count(*) FROM {child} WHERE {column}=?1"),
                                [parent],
                                |row| row.get(0),
                            )?
                        };
                        let expected = self
                            .expected
                            .get(&(child.to_string(), parent.clone()))
                            .copied()
                            .unwrap_or(0);
                        control_ensure!(
                            actual == expected,
                            format!(
                                "incompatible_index: incomplete immutable {child} for {parent_table}"
                            )
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)] // The transaction seam keeps the immutable bundle and reuse witness separate.
fn write_native(
    db: &Connection,
    bundle: (
        &crate::native_evidence::Artifact,
        &crate::capture::Capture,
        &Graph,
        &crate::classes::Catalog,
        &BTreeMap<String, crate::classes::FileExtraction>,
    ),
    revision: IndexPin,
    stats: &IndexStats,
    leader: &topology::LeaderGuard,
    cancel: &CancelFlag,
    immutable: &mut ImmutableAppend,
    reuse: &PreflightReuse,
    rows: &BTreeMap<&str, PublicationRows<'_>>,
) -> Result<BTreeMap<String, V8DocumentProjection>> {
    let (artifact, capture, graph, classes, extractions) = bundle;
    let a = artifact;
    let revision_key = format!(
        "pin:v1:{}:{}",
        revision.index_generation, revision.index_revision
    );
    immutable.insert(
        db,
        "INSERT INTO native_producers VALUES(?1,?2,?3,?4,?5)",
        params![
            a.producer.id,
            a.producer.version,
            a.producer.executable_hash,
            a.producer.kind,
            a.producer.position_encoding
        ],
    )?;
    for (ordinal, language) in a.producer.languages.iter().enumerate() {
        immutable.insert(
            db,
            "INSERT INTO native_producer_languages VALUES(?1,?2,?3,?4,?5)",
            params![
                a.producer.id,
                a.producer.version,
                language,
                1,
                ordinal as i64
            ],
        )?;
    }
    immutable.insert(
        db,
        "INSERT INTO native_source_sets VALUES(?1,?2)",
        params![a.source_set.id, a.source_set.root_id],
    )?;
    for (ordinal, language) in a.source_set.languages.iter().enumerate() {
        immutable.insert(
            db,
            "INSERT INTO native_source_set_languages VALUES(?1,?2,?3)",
            params![a.source_set.id, language, ordinal as i64],
        )?;
    }
    for (ordinal, dependency) in a.source_set.dependencies.iter().enumerate() {
        immutable.insert(
            db,
            "INSERT INTO native_source_set_dependencies VALUES(?1,?2,?3)",
            params![a.source_set.id, dependency, ordinal as i64],
        )?;
    }
    immutable.insert_manifest(
        db,
        "INSERT INTO native_revisions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            revision_key,
            a.revision.source_set_id,
            a.revision.toolchain_hash,
            a.revision.config_hash,
            a.revision.dependency_hash,
            a.revision.id,
            json(&a.revision.documents)?,
            json(&a.source_set.dependencies)?,
            leader.incarnation.to_string(),
            json(capture.reconcile_options())?,
            json(&classes.warnings)?,
            classes.truncated,
            json(stats)?,
            json(&graph.diagnostics)?,
            revision.index_revision as i64,
        ],
    )?;
    for (key, observation) in capture.persisted_inputs()? {
        check_cancel(cancel)?;
        ensure!(
            key != "__released:v1",
            "native_evidence_required: reserved release marker"
        );
        immutable.insert_manifest(
            db,
            "INSERT INTO revision_capture_inputs VALUES(?1,?2,?3)",
            params![revision_key, key, json(&observation)?],
        )?;
    }
    let documents: BTreeMap<_, _> = a
        .revision
        .documents
        .iter()
        .map(|doc| (doc.key.path.as_str(), doc))
        .collect();
    let coverages: BTreeMap<_, _> = a
        .coverage
        .iter()
        .map(|coverage| (coverage.document_path.as_str(), coverage))
        .collect();
    let mut projections = BTreeMap::new();
    for (ordinal, file) in graph.files.iter().enumerate() {
        check_cancel(cancel)?;
        let doc = documents
            .get(file.path.as_str())
            .context("native document missing")?;
        ensure!(
            doc.key.language == file.language
                && doc.content_hash == file.hash
                && doc.byte_length == file.text.len(),
            "native document bytes differ from captured graph"
        );
        let source = capture
            .files
            .iter()
            .find(|source| source == &file)
            .context("native source missing immutable capture")?;
        ensure!(
            source.hash == file.hash,
            "native captured source hash differs"
        );
        let grouped = rows
            .get(file.path.as_str())
            .context("publication rows missing")?;
        let ids = v8_document_projection(file, a, grouped)?;
        let context = crate::native_ids::extraction_context(&file.language, &[])?;
        if !reuse.for_path(&file.path).native {
            immutable.insert(
                db,
                "INSERT INTO document_versions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    ids.version_id,
                    a.source_set.id,
                    file.language,
                    file.path,
                    file.hash,
                    context,
                    a.producer.id,
                    a.producer.version,
                    file.text.len() as i64,
                    file.text.as_bytes(),
                    crate::native_evidence::document_witness(
                        doc,
                        &a.producer,
                        coverages
                            .get(file.path.as_str())
                            .context("missing native coverage")?,
                        &grouped
                            .native_declarations
                            .iter()
                            .map(|d| (*d).clone())
                            .collect::<Vec<_>>(),
                        &grouped
                            .native_calls
                            .iter()
                            .map(|c| (*c).clone())
                            .collect::<Vec<_>>(),
                        &grouped
                            .native_regions
                            .iter()
                            .map(|r| (*r).clone())
                            .collect::<Vec<_>>(),
                    )?,
                ],
            )?;
        }
        if !reuse.for_path(&file.path).graph {
            immutable.insert(
                db,
                "INSERT INTO graph_projections VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    ids.graph_id,
                    ids.version_id,
                    file.language,
                    ids.graph_hash,
                    "ready",
                    if matches!(file.language.as_str(), "java" | "python") {
                        "ready"
                    } else {
                        "notApplicable"
                    },
                    extractions.get(&file.path).map(json).transpose()?,
                ],
            )?;
        }
        if !reuse.for_path(&file.path).class {
            immutable.insert(
                db,
                "INSERT INTO class_projections VALUES(?1,?2,?3,?4)",
                params![ids.class_id, ids.graph_id, ids.class_hash, "ready"],
            )?;
        }
        let coverage = coverages
            .get(file.path.as_str())
            .context("missing native coverage")?;
        ensure!(
            coverage.source_set_id == a.source_set.id
                && coverage.language == file.language
                && coverage.revision_id == a.revision.id,
            "native coverage document mismatch"
        );
        immutable.insert_manifest(
            db,
            "INSERT INTO revision_documents VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                revision_key,
                a.source_set.id,
                file.language,
                file.path,
                ids.version_id,
                ids.graph_id,
                ids.class_id,
                json(&capture.source_stat(&file.path)?)?,
                coverage.requested,
                coverage.selected,
                coverage.state,
                coverage.diagnostic,
                ordinal as i64,
            ],
        )?;
        for (kind, roles) in [
            ("supported", &coverage.supported_roles),
            ("observed", &coverage.observed_roles),
        ] {
            for (ordinal, role) in roles.iter().enumerate() {
                if reuse.for_path(&file.path).native {
                    continue;
                }
                immutable.insert(
                    db,
                    "INSERT INTO native_version_coverage_roles VALUES(?1,?2,?3,?4)",
                    params![ids.version_id, kind, role, ordinal as i64],
                )?;
            }
        }
        ensure!(
            projections.insert(file.path.clone(), ids).is_none(),
            "duplicate projected path"
        );
    }
    let mut owners = BTreeMap::new();
    for d in &a.declarations {
        let key = (d.document.path.clone(), json(&d.ancestors)?, json(&d.key)?);
        ensure!(
            owners.insert(key, d.syntax_id.clone()).is_none(),
            "duplicate native owner key"
        );
    }
    for d in &a.declarations {
        check_cancel(cancel)?;
        let version = &projections
            .get(&d.document.path)
            .context("native declaration missing document")?
            .version_id;
        if reuse.for_path(&d.document.path).native {
            continue;
        }
        let owner = d
            .ancestors
            .last()
            .map(|last| -> Result<String> {
                let prefix = json(&d.ancestors[..d.ancestors.len() - 1])?;
                let key = json(last)?;
                owners
                    .get(&(d.document.path.clone(), prefix.clone(), key.clone()))
                    .cloned()
                    .with_context(||format!("native declaration parent missing: path={} syntax={} ancestor={} key={}",d.document.path,d.syntax_id,prefix,key))
            })
            .transpose()?;
        let sig = d.key.signature.as_ref();
        immutable.insert(db,"INSERT INTO native_version_declarations VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",params![
            version,d.syntax_id,owner,d.kind,d.name,d.lookup_key,sig.is_some(),sig.map(|s|s.type_parameter_count as i64),
            sig.map(|s|s.variadic),d.key.ordinal as i64,d.range.start as i64,d.range.end as i64,
            d.name_range.as_ref().map(|r|r.start as i64),d.name_range.as_ref().map(|r|r.end as i64),
        ])?;
        if let Some(sig) = sig {
            for (ordinal, parameter) in sig.parameter_types.iter().enumerate() {
                immutable.insert(
                    db,
                    "INSERT INTO native_version_own_signature_types VALUES(?1,?2,?3,?4)",
                    params![version, d.syntax_id, ordinal as i64, parameter],
                )?;
            }
        }
        for (ordinal, key) in d.ancestors.iter().enumerate() {
            let sig = key.signature.as_ref();
            immutable.insert(db,"INSERT INTO native_version_declaration_ancestors VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![
                version,d.syntax_id,ordinal as i64,key.kind,key.name,key.ordinal as i64,sig.is_some(),
                sig.map(|s|s.type_parameter_count as i64),sig.map(|s|s.variadic),
            ])?;
            if let Some(sig) = sig {
                for (parameter_ordinal, parameter) in sig.parameter_types.iter().enumerate() {
                    immutable.insert(db,"INSERT INTO native_version_ancestor_signature_types VALUES(?1,?2,?3,?4,?5)",params![
                        version,d.syntax_id,ordinal as i64,parameter_ordinal as i64,parameter,
                    ])?;
                }
            }
        }
        immutable.insert(
            db,
            "INSERT INTO native_version_headers VALUES(?1,?2,?3,?4,?5)",
            params![
                version,
                d.syntax_id,
                d.header.kind,
                d.header.name,
                d.header.result_type,
            ],
        )?;
        for (kind, items) in [
            ("modifier", &d.header.modifiers),
            ("typeParameter", &d.header.type_parameters),
            ("base", &d.header.bases),
        ] {
            for (ordinal, item) in items.iter().enumerate() {
                immutable.insert(
                    db,
                    "INSERT INTO native_version_header_items VALUES(?1,?2,?3,?4,?5)",
                    params![version, d.syntax_id, kind, ordinal as i64, item],
                )?;
            }
        }
        for (ordinal, parameter) in d.header.parameters.iter().enumerate() {
            immutable.insert(
                db,
                "INSERT INTO native_version_parameters VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    version,
                    d.syntax_id,
                    ordinal as i64,
                    parameter.name,
                    parameter.type_name,
                    parameter.variadic,
                ],
            )?;
        }
    }
    for r in &a.control_regions {
        check_cancel(cancel)?;
        if reuse.for_path(&r.document.path).native {
            continue;
        }
        let version = &projections
            .get(&r.document.path)
            .context("native region missing document")?
            .version_id;
        immutable.insert(
            db,
            "INSERT INTO native_version_control_regions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                version,
                r.id,
                r.owner_syntax_id,
                r.ordinal as i64,
                r.kind,
                r.range.start as i64,
                r.range.end as i64,
                r.parent_id,
                r.arm,
            ],
        )?;
    }
    for c in &a.calls {
        check_cancel(cancel)?;
        if reuse.for_path(&c.document.path).native {
            continue;
        }
        let version = &projections
            .get(&c.document.path)
            .context("native call missing document")?
            .version_id;
        immutable.insert(
            db,
            "INSERT INTO native_version_calls VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                version,
                c.id,
                c.owner_syntax_id,
                c.ordinal as i64,
                c.range.start as i64,
                c.range.end as i64,
                c.callee_range.as_ref().map(|r| r.start as i64),
                c.callee_range.as_ref().map(|r| r.end as i64),
                c.spelling,
            ],
        )?;
        for (ordinal, region) in c.region_ids.iter().enumerate() {
            immutable.insert(
                db,
                "INSERT INTO native_version_call_regions VALUES(?1,?2,?3,?4)",
                params![version, c.id, region, ordinal as i64],
            )?;
        }
    }
    // Deferred FKs are enforced by COMMIT. A full foreign_key_check here would
    // read every unchanged occurrence while holding BEGIN IMMEDIATE.
    Ok(projections)
}

/// Bounded readiness check for a pair installed by the verified writer. This does not
/// attest every stored BLOB after out-of-band SQLite mutation. Ordinary Store open stays
/// bounded; pinned source reads verify the selected BLOB inside their read snapshot.
/// Schema-8 revision zero is a deliberately empty bootstrap, not a published
/// native pair. Refuse any staged evidence that appears before the first commit.
fn validate_v8_bootstrap(db: &Connection) -> Result<()> {
    let (indexed_at, incarnation, options): (String, Option<String>, Option<String>) = db
        .query_row(
            "SELECT indexed_at,reconciled_incarnation,reconcile_options FROM index_metadata WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    control_ensure!(
        indexed_at.is_empty() && incarnation.is_none() && options.is_none(),
        "incompatible_index: invalid v8 bootstrap metadata"
    );
    // Fixed identifiers from CACHE_SCHEMA_V8 only. Do not read table names from
    // sqlite_master or treat a partial publication as a valid empty bootstrap.
    for name in [
        "class_projections",
        "class_relations",
        "classes",
        "document_versions",
        "graph_calls",
        "graph_nodes",
        "graph_projections",
        "graph_regions",
        "native_producer_inputs",
        "native_producer_languages",
        "native_producers",
        "native_revisions",
        "native_source_set_dependencies",
        "native_source_set_languages",
        "native_source_sets",
        "native_version_ancestor_signature_types",
        "native_version_call_regions",
        "native_version_calls",
        "native_version_control_regions",
        "native_version_coverage_roles",
        "native_version_declaration_ancestors",
        "native_version_declarations",
        "native_version_header_items",
        "native_version_headers",
        "native_version_own_signature_types",
        "native_version_parameters",
        "revision_capture_inputs",
        "revision_documents",
    ] {
        let present: i64 =
            db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {name})"), [], |row| {
                row.get(0)
            })?;
        control_ensure!(present == 0, "incompatible_index: partial v8 bootstrap");
    }
    Ok(())
}

const GRAPH_FIELD_MAX_BYTES: i64 = 4 * 1024 * 1024;
const GRAPH_PAIR_MAX_BYTES: i64 = 6 * 1024 * 1024;

fn bounded_graph_pair(db: &Connection, sql: &str) -> Result<()> {
    let (stats_type, stats_len, diagnostics_type, diagnostics_len): (String, i64, String, i64) = db
        .query_row(sql, [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
    control_ensure!(
        stats_type == "text"
            && diagnostics_type == "text"
            && (0..=GRAPH_FIELD_MAX_BYTES).contains(&stats_len)
            && (0..=GRAPH_FIELD_MAX_BYTES).contains(&diagnostics_len)
            && stats_len + diagnostics_len <= GRAPH_PAIR_MAX_BYTES,
        "incompatible_index: graph metadata field or pair budget exceeded"
    );
    Ok(())
}

fn validate_paired_metadata(db: &Connection, root_id: &str) -> Result<()> {
    fn one_row(db: &Connection, sql: &str) -> Result<Option<(String, String)>> {
        let mut rows = db
            .prepare(sql)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        control_ensure!(
            rows.len() <= 1,
            "incompatible_index: duplicate native pair metadata"
        );
        Ok(rows.pop())
    }
    let producer = one_row(db, "SELECT id,kind FROM native_producers LIMIT 2")?;
    // Occurrence IDs bind the native producer version (Decision 0003). An index persisted by
    // another native producer version holds IDs this binary cannot reproduce, so it is never
    // served; the integrity failure forces an in-place rebaseline from fresh measurement.
    let producer_version = one_row(db, "SELECT id,version FROM native_producers LIMIT 2")?;
    control_ensure!(
        producer_version
            .as_ref()
            .is_none_or(|(_, version)| version == crate::native_evidence::NATIVE_VERSION),
        "incompatible_index: native producer version mismatch"
    );
    let source = one_row(db, "SELECT id,root_id FROM native_source_sets LIMIT 2")?;
    let revision = one_row(
        db,
        "SELECT r.native_revision_id,r.source_set_id FROM native_revisions r JOIN index_metadata m ON m.singleton=1 AND r.published_index_revision=m.index_revision AND r.id='pin:v1:'||m.index_generation||':'||m.index_revision LIMIT 2",
    )?;
    let expected_source = format!("source-set:v1:{root_id}");
    control_ensure!(
        producer
            .as_ref()
            .is_some_and(|(id, kind)| id == "baleyg.native.syntax" && kind == "native")
            && source
                .as_ref()
                .is_some_and(|(id, root)| id == &expected_source && root == root_id)
            && revision.as_ref().is_some_and(
                |(id, source)| id.starts_with("revision:v1:") && source == &expected_source
            ),
        "incompatible_index: missing native pair metadata"
    );
    let (count, first, last, mismatched, current): (i64, Option<i64>, Option<i64>, i64, i64) =
        db.query_row(
            "SELECT count(r.id),min(r.published_index_revision),max(r.published_index_revision),
                coalesce(sum(CASE WHEN r.id IS NULL THEN 0 WHEN r.id='pin:v1:'||m.index_generation||':'||r.published_index_revision
                  AND r.source_set_id=?1 AND r.published_index_revision BETWEEN 1 AND m.index_revision
                  THEN 0 ELSE 1 END),0),m.index_revision
             FROM index_metadata m LEFT JOIN native_revisions r ON true WHERE m.singleton=1",
            [&expected_source],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )?;
    // Released revisions retain their headers and an FK-bound release marker.
    // A missing header remains corruption, never permission to skip a pin.
    control_ensure!(
        count == current && first == Some(1) && last == Some(current) && mismatched == 0,
        "incompatible_index: native header pin mismatch"
    );
    // A release leaves its header as a durable tombstone. Missing manifests
    // without that exact marker (including an empty legitimate revision) are
    // never accepted as released. The marker owns no native capture input.
    let malformed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_revisions r WHERE
            (EXISTS(SELECT 1 FROM revision_capture_inputs i
                WHERE i.revision_id=r.id AND i.input_key='__released:v1') AND
              ((SELECT count(*) FROM revision_capture_inputs i WHERE i.revision_id=r.id)!=1 OR
               NOT EXISTS(SELECT 1 FROM revision_capture_inputs i
                 WHERE i.revision_id=r.id AND i.input_key='__released:v1' AND i.payload='released:v1') OR
               EXISTS(SELECT 1 FROM revision_documents d WHERE d.revision_id=r.id)))
            OR (NOT EXISTS(SELECT 1 FROM revision_capture_inputs i
                WHERE i.revision_id=r.id AND i.input_key='__released:v1') AND
               (SELECT count(*) FROM revision_documents d WHERE d.revision_id=r.id)
                   != json_array_length(r.source_inventory)))",
        [], |row| row.get(0),
    )?;
    control_ensure!(
        !malformed,
        "incompatible_index: retained manifest/release mismatch"
    );
    bounded_graph_pair(
        db,
        "SELECT typeof(r.graph_stats),length(CAST(r.graph_stats AS BLOB)),
        typeof(r.graph_diagnostics),length(CAST(r.graph_diagnostics AS BLOB))
        FROM index_metadata m JOIN native_revisions r
          ON r.id='pin:v1:'||m.index_generation||':'||m.index_revision
             AND r.published_index_revision=m.index_revision WHERE m.singleton=1",
    )?;
    bounded_graph_pair(
        db,
        "SELECT typeof(stats),length(CAST(stats AS BLOB)),
        typeof(diagnostics),length(CAST(diagnostics AS BLOB))
        FROM index_metadata WHERE singleton=1",
    )?;
    let (header_stats, header_diagnostics, head_stats, head_diagnostics): (
        String,
        String,
        String,
        String,
    ) = db.query_row(
        "SELECT r.graph_stats,r.graph_diagnostics,m.stats,m.diagnostics
         FROM index_metadata m JOIN native_revisions r
           ON r.id='pin:v1:'||m.index_generation||':'||m.index_revision
              AND r.published_index_revision=m.index_revision WHERE m.singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    control_ensure!(
        header_stats == head_stats && header_diagnostics == head_diagnostics,
        "incompatible_index: native header/control graph metadata mismatch"
    );
    let _: IndexStats = serde_json::from_str(&header_stats)?;
    let _: Vec<Diagnostic> = serde_json::from_str(&header_diagnostics)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScanComparison {
    pub(crate) examined: usize,
    pub(crate) added: usize,
    pub(crate) deleted: usize,
    pub(crate) stat_changed: usize,
    pub(crate) hash_changed: usize,
    pub(crate) ctime_changed: usize,
    pub(crate) unchanged_stat_input_hash_changed: usize,
    pub(crate) uncertain_timestamp_hashed: usize,
}
impl ScanComparison {
    fn changed(self) -> bool {
        self.added + self.deleted + self.stat_changed + self.hash_changed > 0
    }
}

pub(crate) type SourceObservations = BTreeMap<String, (String, crate::capture::CaptureStat)>;
pub(crate) type InputObservations = BTreeMap<String, crate::capture::CaptureInputObservation>;

pub(crate) fn compare_capture_observations(
    previous_sources: &SourceObservations,
    current_sources: &SourceObservations,
    previous_inputs: &InputObservations,
    current_inputs: &InputObservations,
) -> ScanComparison {
    let uncertain = |stat: &crate::capture::CaptureStat| {
        stat.mtime_seconds.is_none()
            || stat.mtime_nanoseconds.is_none()
            || stat.ctime_seconds.is_none()
            || stat.ctime_nanoseconds.is_none()
    };
    let mut comparison = ScanComparison::default();
    let source_keys: BTreeSet<_> = previous_sources
        .keys()
        .chain(current_sources.keys())
        .cloned()
        .collect();
    for key in &source_keys {
        comparison.examined += 1;
        match (previous_sources.get(key), current_sources.get(key)) {
            (None, Some(_)) => comparison.added += 1,
            (Some(_), None) => comparison.deleted += 1,
            (Some((old_hash, old_stat)), Some((new_hash, new_stat))) => {
                comparison.stat_changed += usize::from(old_stat != new_stat);
                comparison.hash_changed += usize::from(old_hash != new_hash);
                comparison.ctime_changed += usize::from(
                    old_stat.size == new_stat.size
                        && old_stat.device == new_stat.device
                        && old_stat.inode == new_stat.inode
                        && old_stat.mtime_seconds == new_stat.mtime_seconds
                        && old_stat.mtime_nanoseconds == new_stat.mtime_nanoseconds
                        && (old_stat.ctime_seconds != new_stat.ctime_seconds
                            || old_stat.ctime_nanoseconds != new_stat.ctime_nanoseconds),
                );
                comparison.uncertain_timestamp_hashed +=
                    usize::from(uncertain(old_stat) || uncertain(new_stat));
            }
            (None, None) => unreachable!(),
        }
    }
    let input_keys: BTreeSet<_> = previous_inputs
        .keys()
        .chain(current_inputs.keys())
        .cloned()
        .collect();
    for key in &input_keys {
        comparison.examined += 1;
        match (previous_inputs.get(key), current_inputs.get(key)) {
            (None, Some(_)) => comparison.added += 1,
            (Some(_), None) => comparison.deleted += 1,
            (Some(old), Some(new)) => match (old, new) {
                (
                    crate::capture::CaptureInputObservation::Present {
                        stat: old_stat,
                        hash: old_hash,
                    },
                    crate::capture::CaptureInputObservation::Present {
                        stat: new_stat,
                        hash: new_hash,
                    },
                ) => {
                    comparison.stat_changed += usize::from(old_stat != new_stat);
                    comparison.hash_changed += usize::from(old_hash != new_hash);
                    comparison.unchanged_stat_input_hash_changed +=
                        usize::from(old_stat == new_stat && old_hash != new_hash);
                    comparison.uncertain_timestamp_hashed +=
                        usize::from(uncertain(old_stat) || uncertain(new_stat));
                }
                (
                    crate::capture::CaptureInputObservation::Directory { stat: old },
                    crate::capture::CaptureInputObservation::Directory { stat: new },
                )
                | (
                    crate::capture::CaptureInputObservation::Root { stat: old },
                    crate::capture::CaptureInputObservation::Root { stat: new },
                ) => comparison.stat_changed += usize::from(old != new),
                (
                    crate::capture::CaptureInputObservation::Absent,
                    crate::capture::CaptureInputObservation::Absent,
                ) => {}
                _ => comparison.stat_changed += 1,
            },
            (None, None) => unreachable!(),
        }
    }
    comparison
}

fn compare_capture_snapshot(
    db: &Connection,
    capture: &crate::capture::Capture,
) -> Result<ScanComparison> {
    let mut previous_sources = BTreeMap::new();
    let mut statement = db.prepare("SELECT d.path,v.content_hash,d.capture_stat FROM revision_documents d JOIN document_versions v ON v.id=d.document_version_id JOIN native_revisions r ON r.id=d.revision_id JOIN index_metadata m ON m.index_revision=r.published_index_revision AND r.id='pin:v1:'||m.index_generation||':'||m.index_revision ORDER BY d.path")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (path, hash, stat) = row?;
        let stat: crate::capture::CaptureStat = serde_json::from_str(&stat)?;
        ensure!(
            previous_sources.insert(path, (hash, stat)).is_none(),
            "incompatible_index: duplicate captured source observation"
        );
    }
    let mut current_sources = BTreeMap::new();
    for source in &capture.files {
        current_sources.insert(
            source.path.clone(),
            (source.hash.clone(), capture.source_stat(&source.path)?),
        );
    }

    let mut previous_inputs = BTreeMap::new();
    let mut statement =
        db.prepare("SELECT i.input_key,i.payload FROM revision_capture_inputs i JOIN native_revisions r ON r.id=i.revision_id JOIN index_metadata m ON m.index_revision=r.published_index_revision AND r.id='pin:v1:'||m.index_generation||':'||m.index_revision ORDER BY i.input_key")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (key, payload) = row?;
        let observation: crate::capture::CaptureInputObservation = serde_json::from_str(&payload)?;
        ensure!(
            previous_inputs.insert(key, observation).is_none(),
            "incompatible_index: duplicate captured input observation"
        );
    }
    let current_inputs = capture.persisted_inputs()?;
    Ok(compare_capture_observations(
        &previous_sources,
        &current_sources,
        &previous_inputs,
        &current_inputs,
    ))
}

fn validate_capture_stat(stat: &crate::capture::CaptureStat, expected_kind: &str) -> Result<()> {
    let nanos = |value: Option<i64>| value.is_some_and(|n| (0..1_000_000_000).contains(&n));
    control_ensure!(
        stat.version == 1
            && stat.kind == expected_kind
            && stat.device.is_some() == stat.inode.is_some()
            && stat.mtime_seconds.is_some() == stat.mtime_nanoseconds.is_some()
            && stat.ctime_seconds.is_some() == stat.ctime_nanoseconds.is_some()
            && stat.mtime_seconds.is_some() == stat.ctime_seconds.is_some(),
        "incompatible_index: invalid capture stat"
    );
    #[cfg(unix)]
    control_ensure!(
        stat.device.is_some_and(|n| n > 0)
            && stat.inode.is_some_and(|n| n > 0)
            && nanos(stat.mtime_nanoseconds)
            && nanos(stat.ctime_nanoseconds),
        "incompatible_index: incomplete Unix capture stat"
    );
    #[cfg(not(unix))]
    control_ensure!(
        stat.device.is_none()
            && stat.inode.is_none()
            && stat.mtime_seconds.is_none()
            && stat.ctime_seconds.is_none(),
        "incompatible_index: invalid portable capture stat"
    );
    Ok(())
}

fn validate_reconcile_inventory(db: &Connection) -> Result<()> {
    let (incarnation, options): (Option<String>, Option<String>) = db.query_row(
        "SELECT reconciled_incarnation,reconcile_options FROM index_metadata WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let incarnation = incarnation.ok_or_else(|| {
        ControlIntegrity("incompatible_index: missing reconciled incarnation".into())
    })?;
    uuid::Uuid::parse_str(&incarnation).map_err(|error| {
        ControlIntegrity(format!(
            "incompatible_index: invalid reconciled incarnation: {error}"
        ))
    })?;
    let options = options
        .ok_or_else(|| ControlIntegrity("incompatible_index: missing reconcile options".into()))?;
    let decoded_options: crate::indexer::ReconcileOptions =
        serde_json::from_str(&options).context("incompatible_index: invalid reconcile options")?;
    control_ensure!(
        json(&decoded_options)? == options
            && decoded_options.version == 1
            && decoded_options.max_file_bytes > 0
            && decoded_options.max_file_bytes <= 256 * 1024 * 1024,
        "incompatible_index: unsupported reconcile options"
    );

    let mut source_paths = BTreeSet::new();
    let mut file_statement = db.prepare("SELECT d.path,d.capture_stat FROM revision_documents d JOIN native_revisions r ON r.id=d.revision_id JOIN index_metadata m ON m.index_revision=r.published_index_revision AND r.id='pin:v1:'||m.index_generation||':'||m.index_revision ORDER BY d.path")?;
    let mut files = file_statement.query([])?;
    while let Some(row) = files.next()? {
        let path: String = row.get(0)?;
        control_ensure!(
            !path.is_empty()
                && !Path::new(&path).is_absolute()
                && !path.split('/').any(|part| part.is_empty() || part == ".."),
            "incompatible_index: invalid captured source path"
        );
        control_ensure!(
            source_paths.insert(path.clone()),
            "incompatible_index: duplicate captured source path"
        );
        let payload: String = row.get(1)?;
        let decoded: crate::capture::CaptureStat =
            serde_json::from_str(&payload).context("incompatible_index: invalid capture stat")?;
        validate_capture_stat(&decoded, "file")?;
        control_ensure!(
            json(&decoded)? == payload,
            "incompatible_index: invalid capture stat"
        );
    }

    let mut input_statement =
        db.prepare("SELECT i.input_key,i.payload FROM revision_capture_inputs i JOIN native_revisions r ON r.id=i.revision_id JOIN index_metadata m ON m.index_revision=r.published_index_revision AND r.id='pin:v1:'||m.index_generation||':'||m.index_revision ORDER BY i.input_key")?;
    let mut inputs = input_statement.query([])?;
    let mut actual = BTreeSet::new();
    let mut directories = BTreeSet::new();
    let mut executable = None;
    while let Some(row) = inputs.next()? {
        let key: String = row.get(0)?;
        let payload: String = row.get(1)?;
        control_ensure!(
            !key.is_empty() && key.len() <= 8192 && actual.insert(key.clone()),
            "incompatible_index: invalid capture input key"
        );
        let observation: crate::capture::CaptureInputObservation =
            serde_json::from_str(&payload).context("incompatible_index: invalid capture input")?;
        control_ensure!(
            json(&observation)? == payload,
            "incompatible_index: noncanonical capture input"
        );
        match (&key[..], &observation) {
            ("root:.", crate::capture::CaptureInputObservation::Root { stat }) => {
                validate_capture_stat(stat, "directory")?;
            }
            (key, crate::capture::CaptureInputObservation::Directory { stat })
                if key.starts_with("directory:") =>
            {
                validate_capture_stat(stat, "directory")?;
                let relative = key.trim_start_matches("directory:");
                control_ensure!(
                    relative.is_empty()
                        || (!Path::new(relative).is_absolute()
                            && !relative
                                .split('/')
                                .any(|part| part.is_empty() || part == "..")),
                    "incompatible_index: invalid captured directory"
                );
                directories.insert(relative.to_owned());
            }
            (key, crate::capture::CaptureInputObservation::Present { stat, hash })
                if key.starts_with("config:")
                    || key.starts_with("toolchain:")
                    || key.starts_with("ignore:")
                    || key.starts_with("presentation-scip:")
                    || key.starts_with("presentation-manifest:")
                    || key.starts_with("executable:") =>
            {
                validate_capture_stat(stat, "file")?;
                control_ensure!(
                    hash.len() == 64
                        && hash
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                    "incompatible_index: invalid capture input hash"
                );
                if key.starts_with("executable:") {
                    control_ensure!(
                        executable.replace(key.to_owned()).is_none(),
                        "incompatible_index: duplicate executable input"
                    );
                }
            }
            (key, crate::capture::CaptureInputObservation::Absent)
                if key.starts_with("config:")
                    || key.starts_with("toolchain:")
                    || key.starts_with("ignore:")
                    || key.starts_with("presentation-scip:")
                    || key.starts_with("presentation-manifest:") => {}
            _ => {
                return Err(ControlIntegrity(
                    "incompatible_index: capture input role mismatch".into(),
                )
                .into());
            }
        }
    }

    control_ensure!(
        directories.contains(""),
        "incompatible_index: missing root directory inventory"
    );
    for source in &source_paths {
        let mut parent = Path::new(source).parent();
        while let Some(path) = parent {
            let relative = path
                .to_str()
                .ok_or_else(|| {
                    ControlIntegrity("incompatible_index: non-UTF8 source parent".into())
                })?
                .replace('\\', "/");
            control_ensure!(
                directories.contains(&relative),
                "incompatible_index: missing source directory inventory"
            );
            parent = path.parent();
        }
    }
    let mut expected = BTreeSet::from(["root:.".to_owned()]);
    for name in &crate::capture::ROOT_INPUTS[..22] {
        expected.insert(format!("config:{name}"));
    }
    for name in &crate::capture::ROOT_INPUTS[22..] {
        expected.insert(format!("toolchain:{name}"));
    }
    for directory in &directories {
        for name in [".gitignore", ".ignore"] {
            expected.insert(if directory.is_empty() {
                format!("ignore:{name}")
            } else {
                format!("ignore:{directory}/{name}")
            });
        }
        expected.insert(format!("directory:{directory}"));
    }
    expected.insert(
        executable.ok_or_else(|| {
            ControlIntegrity("incompatible_index: missing executable input".into())
        })?,
    );
    match &decoded_options.scip_path {
        Some(path) => {
            control_ensure!(!path.is_empty(), "incompatible_index: empty SCIP option");
            expected.insert(format!("presentation-scip:{path}"));
        }
        None => control_ensure!(
            !actual
                .iter()
                .any(|key| key.starts_with("presentation-scip:")),
            "incompatible_index: unconfigured SCIP input"
        ),
    }
    match &decoded_options.manifest_path {
        Some(path) => {
            control_ensure!(
                !path.is_empty(),
                "incompatible_index: empty manifest option"
            );
            expected.insert(format!("presentation-manifest:{path}"));
        }
        None => control_ensure!(
            !actual
                .iter()
                .any(|key| key.starts_with("presentation-manifest:")),
            "incompatible_index: unconfigured manifest input"
        ),
    }
    control_ensure!(
        actual == expected,
        "incompatible_index: incomplete or unknown capture inventory"
    );
    Ok(())
}

fn validate_bounded_control(db: &Connection, root_id: &str) -> Result<()> {
    bounded_graph_pair(
        db,
        "SELECT typeof(stats),length(CAST(stats AS BLOB)),
        typeof(diagnostics),length(CAST(diagnostics AS BLOB))
        FROM index_metadata WHERE singleton=1",
    )?;
    let (stats, diagnostics): (String, String) = db.query_row(
        "SELECT stats,diagnostics FROM index_metadata WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let _: IndexStats = serde_json::from_str(&stats)?;
    let _: Vec<Diagnostic> = serde_json::from_str(&diagnostics)?;
    validate_paired_metadata(db, root_id)?;
    validate_reconcile_inventory(db)?;
    let warnings_bytes: Option<i64> = db
        .query_row(
            "SELECT length(CAST(r.class_warnings AS BLOB)) FROM native_revisions r JOIN index_metadata m ON m.index_revision=r.published_index_revision AND r.id='pin:v1:'||m.index_generation||':'||m.index_revision",
            [],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        warnings_bytes.is_some_and(|bytes| (0..=256 * 1024).contains(&bytes)),
        "incompatible_index: class catalog byte budget exceeded or missing"
    );
    Ok(())
}

fn validate_paired_rows(db: &Connection) -> Result<()> {
    let count = |table: &str| -> Result<i64> {
        Ok(db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
    };
    ensure!(
        count("native_producers")? == 1
            && count("native_source_sets")? == 1
            && count("native_revisions")? >= 1,
        "incompatible_index: missing native pair"
    );
    let (manifest_rows, projection_rows, inventory_rows): (i64, i64, i64) = db.query_row(
        "SELECT (SELECT count(*) FROM revision_documents d WHERE d.revision_id=r.id),
                (SELECT count(*) FROM revision_documents d WHERE d.revision_id=r.id AND d.class_projection_id IS NOT NULL),
                json_array_length(r.source_inventory)
         FROM native_revisions r JOIN index_metadata m ON m.singleton=1
           AND r.published_index_revision=m.index_revision
           AND r.id='pin:v1:'||m.index_generation||':'||m.index_revision",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    ensure!(
        manifest_rows == inventory_rows && projection_rows == manifest_rows,
        "incompatible_index: incomplete revision manifest"
    );
    let mut stmt=db.prepare("SELECT d.source_bytes,d.content_hash,d.byte_length,r.path,r.language,r.source_set_id,g.language
        FROM revision_documents r JOIN document_versions d ON d.id=r.document_version_id
        JOIN graph_projections g ON g.id=r.graph_projection_id")?;
    let mut matched = 0;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        use sha2::{Digest, Sha256};
        let bytes: Vec<u8> = row.get(0)?;
        let hash: String = row.get(1)?;
        let length: i64 = row.get(2)?;
        let _ = String::from_utf8(bytes.clone())?;
        ensure!(
            length == bytes.len() as i64
                && hash == hex::encode(Sha256::digest(&bytes))
                && row.get::<_, String>(4)? == row.get::<_, String>(6)?
                && row.get::<_, String>(5)?.starts_with("source-set:v1:")
                && !row.get::<_, String>(3)?.is_empty(),
            "incompatible_index: manifest source mismatch"
        );
        matched += 1;
    }
    ensure!(
        matched == count("revision_documents")?,
        "incompatible_index: missing manifest document"
    );
    ensure!(
        db.prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_none(),
        "incompatible_index: native foreign key mismatch"
    );
    validate_reconcile_inventory(db)?;
    Ok(())
}

#[derive(Clone)]
struct V8NativeScope {
    revision_key: String, // internal pin:v1 generation/revision key
    version_id: String,
    key: crate::native_evidence::DocumentKey,
    native_revision_id: String,
    producer_id: String,
}
impl V8NativeScope {
    fn proof_id(&self) -> String {
        format!(
            "native-proof:v1:{}",
            crate::native_ids::digest(
                b"baleyg.native-proof.v1\0",
                &crate::native_ids::canonical(&serde_json::json!({
                    "producerId":self.producer_id,"document":self.key,
                    "revisionId":self.native_revision_id
                })),
            )
        )
    }
}
fn v8_native_scope_for(
    db: &Connection,
    key: &crate::native_evidence::DocumentKey,
    selected: &ReadRevision,
) -> Result<Option<V8NativeScope>> {
    let mut stmt = db.prepare(
        "SELECT rd.revision_id,rd.document_version_id,r.native_revision_id,d.producer_id
        FROM native_revisions r JOIN revision_documents rd ON rd.revision_id=r.id
        JOIN document_versions d ON d.id=rd.document_version_id
          AND d.source_set_id=rd.source_set_id AND d.language=rd.language AND d.path=rd.path
        WHERE r.id=?4 AND rd.source_set_id=?1 AND rd.language=?2 AND rd.path=?3 LIMIT 2",
    )?;
    let rows = stmt
        .query_map(
            params![key.source_set_id, key.language, key.path, selected.key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        rows.len() <= 1,
        "incompatible_index: duplicate selected document manifest"
    );
    Ok(rows.into_iter().next().map(
        |(revision_key, version_id, native_revision_id, producer_id)| V8NativeScope {
            revision_key,
            version_id,
            key: key.clone(),
            native_revision_id,
            producer_id,
        },
    ))
}

fn v8_owner_scope_for(
    db: &Connection,
    owner: &str,
    selected: &ReadRevision,
) -> Result<Option<V8NativeScope>> {
    use crate::native_evidence::DocumentKey;
    let mut stmt = db.prepare(
        "SELECT m.source_set_id,m.language,m.path
        FROM native_version_declarations d JOIN revision_documents m
          ON m.revision_id=?2 AND m.document_version_id=d.version_id
        WHERE d.syntax_id=?1 LIMIT 2",
    )?;
    let keys: Vec<DocumentKey> = stmt
        .query_map(params![owner, selected.key], |r| {
            Ok(DocumentKey {
                source_set_id: r.get(0)?,
                language: r.get(1)?,
                path: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    ensure!(
        keys.len() <= 1,
        "incompatible_index: duplicate native owner identity"
    );
    keys.first()
        .map(|key| {
            v8_native_scope_for(db, key, selected)?
                .context("incompatible_index: native owner manifest missing")
        })
        .transpose()
}
fn contiguous(ordinal: i64, expected: usize, what: &str) -> Result<()> {
    ensure!(
        ordinal == i64::try_from(expected)?,
        "incompatible_index: {what} ordinal gap"
    );
    Ok(())
}
fn decode_signature(
    types: &mut BTreeMap<String, Vec<String>>,
    id: &str,
    present: bool,
    count: Option<i64>,
    variadic: Option<bool>,
) -> Result<Option<crate::native_evidence::Signature>> {
    let values = types.remove(id).unwrap_or_default();
    if !present {
        ensure!(
            values.is_empty(),
            "incompatible_index: orphan native signature types"
        );
        return Ok(None);
    }
    Ok(Some(crate::native_evidence::Signature {
        parameter_types: values,
        type_parameter_count: usize::try_from(count.context("missing native signature count")?)?,
        variadic: variadic.context("missing native signature variadic")?,
    }))
}
// This reads the WHOLE selected version, checks every child, THEN filters lookup keys.
// It cannot accidentally skip bad children attached to a declaration outside a lookup filter.
fn read_native_declarations_v8(
    db: &Connection,
    scope: &V8NativeScope,
    lookup_key: Option<&str>,
    all_lookup_keys: bool,
) -> Result<Vec<crate::native_evidence::Declaration>> {
    use crate::native_evidence::{Declaration, Header, Key, Parameter, Range};
    let v = &scope.version_id;
    let mut own_types: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in db.prepare("SELECT syntax_id,ordinal,type_name FROM native_version_own_signature_types WHERE version_id=?1 ORDER BY syntax_id,ordinal")?
        .query_map([v],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?)))? {
        let (id,ord,value)=row?;let entry=own_types.entry(id).or_default();
        contiguous(ord,entry.len(),"own signature")?;entry.push(value);
    }
    let mut ancestor_types: BTreeMap<(String, i64), Vec<String>> = BTreeMap::new();
    for row in db.prepare("SELECT syntax_id,ancestor_ordinal,ordinal,type_name FROM native_version_ancestor_signature_types WHERE version_id=?1 ORDER BY syntax_id,ancestor_ordinal,ordinal")?
        .query_map([v],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?)))? {
        let (id,ancestor,ord,value)=row?;let entry=ancestor_types.entry((id,ancestor)).or_default();
        contiguous(ord,entry.len(),"ancestor signature")?;entry.push(value);
    }
    type Ancestor = (
        i64,
        String,
        Option<String>,
        i64,
        bool,
        Option<i64>,
        Option<bool>,
    );
    let mut ancestors: BTreeMap<String, Vec<Ancestor>> = BTreeMap::new();
    for row in db.prepare("SELECT syntax_id,ordinal,kind,name,sibling_ordinal,signature_present,type_parameter_count,variadic FROM native_version_declaration_ancestors WHERE version_id=?1 ORDER BY syntax_id,ordinal")?
        .query_map([v],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,i64>(4)?,r.get::<_,bool>(5)?,r.get::<_,Option<i64>>(6)?,r.get::<_,Option<bool>>(7)?)))? {
        let (id,ord,kind,name,sibling,present,count,variadic)=row?;
        let entry=ancestors.entry(id).or_default();contiguous(ord,entry.len(),"ancestor")?;
        entry.push((ord,kind,name,sibling,present,count,variadic));
    }
    let mut headers: BTreeMap<String, Header> = BTreeMap::new();
    for row in db.prepare("SELECT syntax_id,kind,name,result_type FROM native_version_headers WHERE version_id=?1 ORDER BY syntax_id")?
        .query_map([v],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?)))? {
        let (id,kind,name,result_type)=row?;
        ensure!(headers.insert(id,Header{kind,name,modifiers:vec![],type_parameters:vec![],parameters:vec![],result_type,bases:vec![]}).is_none(),"duplicate native header");
    }
    // Separate ordinal spaces for each (syntax_id,item_kind); don't collapse them.
    let mut item_ordinals: BTreeMap<(String, String), usize> = BTreeMap::new();
    for row in db.prepare("SELECT syntax_id,item_kind,ordinal,value FROM native_version_header_items WHERE version_id=?1 ORDER BY syntax_id,item_kind,ordinal")?
        .query_map([v],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?)))? {
        let (id,kind,ordinal,value)=row?;
        let n=item_ordinals.entry((id.clone(),kind.clone())).or_default();contiguous(ordinal,*n,"header item")?;*n+=1;
        let h=headers.get_mut(&id).context("orphan native header item")?;
        match kind.as_str(){"modifier"=>h.modifiers.push(value),"typeParameter"=>h.type_parameters.push(value),"base"=>h.bases.push(value),_=>anyhow::bail!("unknown header item")}
    }
    let mut param_ordinals: BTreeMap<String, usize> = BTreeMap::new();
    for row in db.prepare("SELECT syntax_id,ordinal,name,type_name,variadic FROM native_version_parameters WHERE version_id=?1 ORDER BY syntax_id,ordinal")?
        .query_map([v],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,bool>(4)?)))? {
        let (id,ordinal,name,type_name,variadic)=row?;
        let n=param_ordinals.entry(id.clone()).or_default();contiguous(ordinal,*n,"header parameter")?;*n+=1;
        headers.get_mut(&id).context("orphan native header parameter")?.parameters.push(Parameter{name,type_name,variadic});
    }
    let proof = scope.proof_id();
    let mut result = Vec::new();
    let mut recorded_owners: BTreeMap<String, Option<String>> = BTreeMap::new();
    for row in db.prepare("SELECT syntax_id,owner_syntax_id,kind,name,lookup_key,key_signature_present,key_type_parameter_count,key_variadic,key_ordinal,start_byte,end_byte,name_start,name_end FROM native_version_declarations WHERE version_id=?1 ORDER BY syntax_id")?
      .query_map([v],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,bool>(5)?,r.get::<_,Option<i64>>(6)?,r.get::<_,Option<bool>>(7)?,r.get::<_,i64>(8)?,r.get::<_,i64>(9)?,r.get::<_,i64>(10)?,r.get::<_,Option<i64>>(11)?,r.get::<_,Option<i64>>(12)?)))? {
        let (id,owner,kind,name,lookup,present,count,variadic,ordinal,start,end,name_start,name_end)=row?;
        ensure!(recorded_owners.insert(id.clone(),owner).is_none(),"duplicate native declaration");
        let key=Key{kind:kind.clone(),name:name.clone(),signature:decode_signature(&mut own_types,&id,present,count,variadic)?,ordinal:usize::try_from(ordinal)?};
        let mut a=Vec::new();
        for (ancestor_ord,kind,name,sibling,present,count,variadic) in ancestors.remove(&id).unwrap_or_default() {
            contiguous(ancestor_ord,a.len(),"ancestor")?;
            let sig_types=ancestor_types.remove(&(id.clone(),ancestor_ord)).unwrap_or_default();
            if !present {ensure!(sig_types.is_empty(),"orphan ancestor signature types");}
            let signature=if present {Some(crate::native_evidence::Signature{parameter_types:sig_types,type_parameter_count:usize::try_from(count.context("missing ancestor signature count")?)?,variadic:variadic.context("missing ancestor signature variadic")?})}else{None};
            a.push(Key{kind,name,signature,ordinal:usize::try_from(sibling)?});
        }
        let name_range=name_start.zip(name_end).map(|(start,end)|->Result<Range>{Ok(Range{start:usize::try_from(start)?,end:usize::try_from(end)?})}).transpose()?;
        ensure!(name_start.is_some()==name_end.is_some(),"half native name range");
        result.push(Declaration{syntax_id:id.clone(),document:scope.key.clone(),revision_id:scope.native_revision_id.clone(),kind,name,lookup_key:lookup,ancestors:a,key,
            range:Range{start:usize::try_from(start)?,end:usize::try_from(end)?},name_range,header:headers.remove(&id).context("missing native header")?,provenance_id:proof.clone()});
    }
    ensure!(
        own_types.is_empty()
            && ancestor_types.is_empty()
            && ancestors.is_empty()
            && headers.is_empty(),
        "orphan native declaration children"
    );
    // Verify stored owner_syntax_id, a column absent from the public DTO. Writer maps parent
    // from serialized ancestor prefix and final Key; compare against complete decoded tree.
    let mut by_key: BTreeMap<(String, String), String> = BTreeMap::new();
    for d in &result {
        let identity = (
            serde_json::to_string(&d.ancestors)?,
            serde_json::to_string(&d.key)?,
        );
        ensure!(
            by_key.insert(identity, d.syntax_id.clone()).is_none(),
            "duplicate native owner key"
        );
    }
    for d in &result {
        let expected = d
            .ancestors
            .last()
            .map(|last| -> Result<String> {
                by_key
                    .get(&(
                        serde_json::to_string(&d.ancestors[..d.ancestors.len() - 1])?,
                        serde_json::to_string(last)?,
                    ))
                    .cloned()
                    .context("missing native parent")
            })
            .transpose()?;
        ensure!(
            recorded_owners.get(&d.syntax_id) == Some(&expected),
            "native parent differs from ancestor key"
        );
    }
    if !all_lookup_keys {
        result.retain(|d| d.lookup_key.as_deref() == lookup_key);
    }
    Ok(result)
}

fn read_native_coverage_v8(
    db: &Connection,
    scope: &V8NativeScope,
) -> Result<crate::native_evidence::Coverage> {
    use crate::native_evidence::Coverage;
    let (requested,selected,state,diagnostic):(bool,bool,String,Option<String>)=
        db.query_row("SELECT coverage_requested,coverage_selected,coverage_state,coverage_diagnostic FROM revision_documents WHERE revision_id=?1 AND source_set_id=?2 AND language=?3 AND path=?4 AND document_version_id=?5",
            params![scope.revision_key,scope.key.source_set_id,scope.key.language,scope.key.path,scope.version_id],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let (mut supported_roles, mut observed_roles) = (vec![], vec![]);
    for row in db.prepare("SELECT role_kind,role,ordinal FROM native_version_coverage_roles WHERE version_id=?1 ORDER BY role_kind,ordinal")?
        .query_map([&scope.version_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))? {
        let (kind,role,ordinal)=row?;
        let out=match kind.as_str(){"supported"=>&mut supported_roles,"observed"=>&mut observed_roles,_=>anyhow::bail!("invalid native coverage role kind")};
        contiguous(ordinal,out.len(),"coverage role")?;out.push(role);
    }
    Ok(Coverage {
        producer_id: scope.producer_id.clone(),
        language: scope.key.language.clone(),
        source_set_id: scope.key.source_set_id.clone(),
        document_path: scope.key.path.clone(),
        revision_id: scope.native_revision_id.clone(),
        requested,
        selected,
        state,
        supported_roles,
        observed_roles,
        diagnostic,
    })
}
// Reads all version rows, verifies all child edges, then selects the requested owner.
fn read_native_calls_v8(
    db: &Connection,
    scope: &V8NativeScope,
    owner: Option<&str>,
) -> Result<Vec<crate::native_evidence::Call>> {
    use crate::native_evidence::{Call, Range};
    let version = &scope.version_id;
    let known: BTreeSet<String> = db
        .prepare("SELECT syntax_id FROM native_version_declarations WHERE version_id=?1")?
        .query_map([version], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let region_owner: BTreeMap<String, String> = db
        .prepare(
            "SELECT id,owner_syntax_id FROM native_version_control_regions WHERE version_id=?1",
        )?
        .query_map([version], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let proof = scope.proof_id();
    let mut calls: Vec<Call> = vec![];
    let mut indexes: BTreeMap<String, usize> = BTreeMap::new();
    let mut ordinals: BTreeMap<String, usize> = BTreeMap::new();
    for row in db.prepare("SELECT id,owner_syntax_id,ordinal,start_byte,end_byte,callee_start,callee_end,spelling FROM native_version_calls WHERE version_id=?1 ORDER BY owner_syntax_id,ordinal")?
        .query_map([version],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,Option<i64>>(5)?,r.get::<_,Option<i64>>(6)?,r.get::<_,Option<String>>(7)?)))? {
        let (id,call_owner,ordinal,start,end,callee_start,callee_end,spelling)=row?;
        ensure!(known.contains(&call_owner),"orphan native call owner");
        let n=ordinals.entry(call_owner.clone()).or_default();contiguous(ordinal,*n,"call")?;*n+=1;
        ensure!(callee_start.is_some()==callee_end.is_some(),"half callee range");
        let callee_range=callee_start.zip(callee_end).map(|(start,end)|->Result<Range>{
            Ok(Range{start:usize::try_from(start)?,end:usize::try_from(end)?})
        }).transpose()?;
        ensure!(indexes.insert(id.clone(),calls.len()).is_none(),"duplicate native call ID");
        calls.push(Call{id,owner_syntax_id:call_owner,ordinal:usize::try_from(ordinal)?,document:scope.key.clone(),
            revision_id:scope.native_revision_id.clone(),range:Range{start:usize::try_from(start)?,end:usize::try_from(end)?},
            callee_range,spelling,region_ids:vec![],provenance_id:proof.clone()});
    }
    for row in db.prepare("SELECT call_id,region_id,ordinal FROM native_version_call_regions WHERE version_id=?1 ORDER BY call_id,ordinal")?
        .query_map([version],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))? {
        let (call_id,region_id,ordinal)=row?;
        let call=&mut calls[*indexes.get(&call_id).context("orphan native call region")?];
        ensure!(region_owner.get(&region_id)==Some(&call.owner_syntax_id),"cross-owner native call region");
        contiguous(ordinal,call.region_ids.len(),"call region")?;
        call.region_ids.push(region_id);
    }
    Ok(calls
        .into_iter()
        .filter(|call| owner.is_none_or(|selected| call.owner_syntax_id == selected))
        .collect())
}
fn read_native_control_regions_v8(
    db: &Connection,
    scope: &V8NativeScope,
    owner: Option<&str>,
) -> Result<Vec<crate::native_evidence::ControlRegion>> {
    use crate::native_evidence::{ControlRegion, Range};
    let known: BTreeSet<String> = db
        .prepare("SELECT syntax_id FROM native_version_declarations WHERE version_id=?1")?
        .query_map([&scope.version_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let proof = scope.proof_id();
    let mut regions = Vec::<ControlRegion>::new();
    let mut indexes = BTreeMap::<String, usize>::new();
    let mut ordinals = BTreeMap::<String, usize>::new();
    for row in db.prepare("SELECT id,owner_syntax_id,ordinal,kind,start_byte,end_byte,parent_id,arm FROM native_version_control_regions WHERE version_id=?1 ORDER BY owner_syntax_id,ordinal")?
        .query_map([&scope.version_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?)))? {
        let (id,region_owner,ordinal,kind,start,end,parent_id,arm)=row?;
        ensure!(known.contains(&region_owner),"orphan native region owner");
        let n=ordinals.entry(region_owner.clone()).or_default();contiguous(ordinal,*n,"region")?;*n+=1;
        ensure!(indexes.insert(id.clone(),regions.len()).is_none(),"duplicate native region ID");
        regions.push(ControlRegion{id,owner_syntax_id:region_owner,ordinal:usize::try_from(ordinal)?,
            document:scope.key.clone(),revision_id:scope.native_revision_id.clone(),kind,
            range:Range{start:usize::try_from(start)?,end:usize::try_from(end)?},parent_id,arm,
            provenance_id:proof.clone()});
    }
    for region in &regions {
        if let Some(parent_id) = &region.parent_id {
            let parent = &regions[*indexes
                .get(parent_id)
                .context("missing native parent region")?];
            ensure!(
                region.owner_syntax_id == parent.owner_syntax_id
                    && parent.range.start <= region.range.start
                    && region.range.end <= parent.range.end,
                "invalid native parent region"
            );
        }
    }
    Ok(regions
        .into_iter()
        .filter(|region| owner.is_none_or(|selected| region.owner_syntax_id == selected))
        .collect())
}

fn native_index_unavailable(error: &anyhow::Error) -> bool {
    error.downcast_ref::<topology::IndexNotReady>().is_some()
}
impl Store {
    pub fn open(
        roots: topology::TopologyRoots,
        identity: topology::WorkspaceIdentity,
    ) -> Result<Self> {
        Self::open_with_stage_hook(roots, identity, |_| Ok(()))
    }
    fn open_with_stage_hook(
        roots: topology::TopologyRoots,
        identity: topology::WorkspaceIdentity,
        before_publish: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<Self> {
        identity.verify()?;
        roots.prepare_index(&identity)?;
        let store = Self {
            workspace_root: identity
                .root
                .to_str()
                .context("workspace path is not UTF-8")?
                .to_owned(),
            roots,
            identity: Arc::new(identity),
            recovery_required: Arc::new(AtomicBool::new(false)),
            recovery_disposition: Arc::new(AtomicU8::new(RecoveryDisposition::Ready as u8)),
            obsolete_format_marker: Arc::new(Mutex::new(None)),
            pending_request_completion: Arc::new(Mutex::new(None)),
            request_file_witness: Arc::new(Mutex::new(None)),
            writer_counters: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            test_queue_before_shared_hook: Arc::new(TestOneShotHook::default()),
            #[cfg(test)]
            test_queue_select_hook: Arc::new(TestOneShotHook::default()),
            #[cfg(test)]
            test_exclusive_recovery_hook: Arc::new(TestOneShotHook::default()),
            #[cfg(test)]
            test_queue_finish_failures: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(test)]
            test_queue_post_commit_failures: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        if !index_path_present(&store.roots.index_db(&store.identity))? {
            let leader = store.roots.leader(&store.identity)?;
            store.initialize(&leader, before_publish)?;
        }
        let admission = (|| -> Result<()> {
            let mut db = store.cache()?;
            let tx = storage_result(db.transaction())?;
            let _ = store.recovery_baseline(&tx)?;
            Ok(())
        })();
        match admission {
            Ok(()) => Ok(store),
            Err(error) if recovery_class(&error) == RecoveryClass::RecreatePending => {
                if let Some(obsolete) = error.downcast_ref::<ObsoleteIndexFormat>() {
                    *store.obsolete_format_marker.lock().unwrap() = Some(obsolete.0.clone());
                }
                store.mark_recovery(RecoveryDisposition::RecreatePending);
                Ok(store)
            }
            Err(error) if error.to_string() == "root_changed: index root identity mismatch" => {
                store.mark_recovery(RecoveryDisposition::RootReplaced);
                Ok(store)
            }
            Err(error) => Err(error),
        }
    }
    /// Isolated roots for integration fixtures; production startup calls `open` with ProjectDirs.
    pub fn open_for_tests(state: &Path, workspace: &Path) -> Result<Self> {
        let identity = topology::WorkspaceIdentity::discover(Some(workspace), workspace)?;
        let roots =
            topology::TopologyRoots::isolated_for_tests(state.join("cache"), state.join("data"));
        Self::open(roots, identity)
    }
    /// Fixture barrier after building a staged index, before its validation and publication.
    pub fn open_for_tests_with_index_stage_hook(
        state: &Path,
        workspace: &Path,
        before_publish: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<Self> {
        let identity = topology::WorkspaceIdentity::discover(Some(workspace), workspace)?;
        let roots =
            topology::TopologyRoots::isolated_for_tests(state.join("cache"), state.join("data"));
        Self::open_with_stage_hook(roots, identity, before_publish)
    }
    /// Build a private schema-8 bootstrap only; the caller must validate and publish it.
    /// The returned guard unlinks only its own stage inode if it is not published.
    fn create_staged_index(&self, leader: &topology::LeaderGuard) -> Result<StagedIndex> {
        use rusqlite::OpenFlags;
        use std::os::unix::fs::OpenOptionsExt;
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        self.identity.verify()?;
        let path = self.roots.index_db(&self.identity);
        let staged_path = path.with_file_name(format!("index.db.tmp-{}", uuid::Uuid::new_v4()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged_path)?;
        let staged = StagedIndex {
            path: staged_path,
            file,
            published: false,
        };
        let db = Connection::open_with_flags(
            &staged.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(Duration::ZERO)?;
        db.pragma_update(None, "journal_mode", "DELETE")?;
        db.pragma_update(None, "synchronous", "FULL")?;
        db.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            db.execute_batch(CACHE_SCHEMA_V8)?;
            let age = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            ensure!(age <= 9_007_199_254_740_991, "invalid_open_age");
            db.execute(
                "INSERT INTO index_metadata VALUES(1,8,?1,?2,?3,?4,?5,0,?6,'',?7,?8,NULL,NULL)",
                params![
                    EXTRACTOR_VERSION,
                    self.workspace_root,
                    self.identity.device.to_string(),
                    self.identity.inode.to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    age as i64,
                    json(&IndexStats::default())?,
                    json(&Vec::<Diagnostic>::new())?
                ],
            )?;
            db.pragma_update(None, "user_version", DATABASE_SCHEMA_VERSION)?;
            db.execute_batch("COMMIT")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = db.execute_batch("ROLLBACK");
        }
        result?;
        drop(db);
        Ok(staged)
    }
    fn validate_replacement_index(
        &self,
        path: &Path,
        stage: &StagedIndex,
        leader: &topology::LeaderGuard,
        pin: IndexPin,
    ) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let named = std::fs::symlink_metadata(path)?;
        let held = stage.file.metadata()?;
        ensure!(
            named.is_file()
                && !named.file_type().is_symlink()
                && (named.dev(), named.ino()) == (held.dev(), held.ino()),
            "unsafe_index: replacement inode changed"
        );
        leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
        self.identity.verify()?;
        let db = open_index(path, false)?;
        let status = self.decode_control_status_raw(&db)?;
        ensure!(
            status.revision == pin && status.evidence_format.is_some(),
            "incompatible_index: staged pair or evidence format changed"
        );
        let marker: String = db.query_row(
            "SELECT reconciled_incarnation FROM index_metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        ensure!(
            marker == leader.incarnation.to_string(),
            "index_not_ready: staged leader marker changed"
        );
        validate_bounded_control(&db, &self.identity.record_id)?;
        validate_paired_rows(&db)?;
        let integrity: String = db.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        ensure!(
            integrity == "ok",
            "incompatible_index: staged integrity check failed"
        );
        drop(db);
        leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
        self.identity.verify()?;
        let named = std::fs::symlink_metadata(path)?;
        ensure!(
            named.is_file()
                && !named.file_type().is_symlink()
                && (named.dev(), named.ino()) == (held.dev(), held.ino()),
            "unsafe_index: replacement inode changed"
        );
        Ok(())
    }
    /// Only an already-classified corrupt index under a fresh exclusive leader may enter.
    pub(crate) fn recreate_index_exclusive(
        &self,
        options: &crate::indexer::IndexOptions,
        leader: &mut topology::LeaderGuard,
        cancel: &CancelFlag,
    ) -> Result<IndexPin> {
        self.recreate_index_exclusive_with_hook(options, leader, cancel, |_| Ok(()))
    }
    fn recreate_index_exclusive_with_hook(
        &self,
        options: &crate::indexer::IndexOptions,
        leader: &mut topology::LeaderGuard,
        cancel: &CancelFlag,
        mut at: impl FnMut(ActivationStage) -> Result<()>,
    ) -> Result<IndexPin> {
        ensure!(
            matches!(
                self.disposition(),
                RecoveryDisposition::RecreatePending | RecoveryDisposition::RootReplaced
            ) && self.recovery_required.load(Ordering::Acquire),
            "recovery_required: exceptional recreation not classified"
        );
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
        self.identity.verify()?;
        ensure!(
            std::fs::canonicalize(&options.workspace_root)? == self.identity.root,
            "root_changed: index options select another workspace"
        );
        check_cancel(cancel)?;
        let path = self.roots.index_db(&self.identity);
        let old = IndexFileWitness::open(&path)?;
        for suffix in ["-wal", "-shm"] {
            let sidecar = path.with_file_name(format!("index.db{suffix}"));
            ensure!(
                !index_path_present(&sidecar)?,
                "recovery_required: unsupported index sidecar: {}",
                sidecar.display()
            );
        }
        let journal_path = path.with_file_name("index.db-journal");
        let journal = index_path_present(&journal_path)?
            .then(|| IndexFileWitness::open(&journal_path))
            .transpose()?;
        // Reclassify after EX admission. A later repair, root mismatch, hot journal,
        // or ordinary format/schema mismatch cannot authorize inode replacement.
        // Bind the marker classified at admission to this EX attempt. A different
        // obsolete marker on the same inode cannot borrow the earlier authority.
        let initial_marker = self.obsolete_format_marker.lock().unwrap().clone();
        let mut obsolete = None;
        let cause = match open_index(&path, false) {
            Err(error) => {
                if let Some(found) = error.downcast_ref::<ObsoleteIndexFormat>() {
                    ensure!(
                        initial_marker.as_ref() == Some(&found.0),
                        "recovery_required: obsolete marker changed since admission"
                    );
                    let db = open_index_marker_probe(&path, false)?;
                    self.verify_metadata_root(&db)?;
                    ensure!(
                        read_index_format_marker(&db)?.eq(&found.0),
                        "recovery_required: obsolete marker changed under EX"
                    );
                    obsolete = Some(found.0.clone());
                }
                recovery_class(&error)
            }
            Ok(db) => {
                let result = self.recovery_baseline(&db);
                drop(db);
                match result {
                    Err(error) => recovery_class(&error),
                    Ok(_) => RecoveryClass::Rebuild,
                }
            }
        };
        if self.disposition() == RecoveryDisposition::RootReplaced {
            // A new inode at the same canonical spelling is a root transition, not
            // corruption. Recheck the old derived index under EX before replacing it.
            let db = open_index(&path, false)?;
            match self.recovery_baseline(&db) {
                Err(error) if error.to_string() == "root_changed: index root identity mismatch" => {
                }
                _ => anyhow::bail!("root_changed: replacement authority changed"),
            }
            ensure!(
                journal.is_none(),
                "recovery_required: root replacement has hot journal"
            );
        } else {
            ensure!(
                cause == RecoveryClass::RecreatePending,
                "recovery_required: corruption/obsolete marker authority not verified"
            );
        }
        old.verify()?;
        leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
        if let Some(marker) = &obsolete {
            old.verify()?;
            let db = open_index_marker_probe(&path, false)?;
            self.verify_metadata_root(&db)?;
            ensure!(
                read_index_format_marker(&db)?.eq(marker),
                "recovery_required: obsolete marker changed before stage"
            );
        }
        let mut stage = self.create_staged_index(leader)?;
        let (graph, native, capture) =
            crate::indexer::index_workspace_bundle(options, self.root_id(), cancel, |_| {})?;
        let pin = self.publish_native_to_stage(
            (&graph, &capture, &native),
            &stage,
            leader,
            cancel,
            |_, _| Ok(()),
        )?;
        ensure!(
            pin.index_revision == 1,
            "recovery_required: replacement must start at revision one"
        );
        self.validate_replacement_index(&stage.path, &stage, leader, pin)?;
        stage.file.sync_all()?;
        stage.verify_path()?;
        old.verify()?;
        if let Some(journal) = &journal {
            journal.verify()?;
        }
        check_cancel(cancel)?;
        capture.verify(cancel)?;
        self.identity.verify()?;
        leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
        at(ActivationStage::BeforeRename)?;
        old.verify()?;
        stage.verify_path()?;
        // The publisher's earlier validation is not enough: a failed or hot
        // staged journal appearing at this barrier must never reach index.db.
        reject_sidecars(&stage.path, true)?;
        check_cancel(cancel)?;
        capture.verify(cancel)?;
        self.identity.verify()?;
        leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
        let dir = self.roots.index_dir(&self.identity);
        let backup = if let Some(journal) = &journal {
            let backup = dir.join(format!("index.db-journal.tmp-{}", uuid::Uuid::new_v4()));
            ensure!(
                !index_path_present(&backup)?,
                "unsafe_index: journal backup exists"
            );
            journal.verify()?;
            std::fs::rename(&journal.path, &backup)?;
            if let Err(error) = journal.verify_at(&backup) {
                restore_index_journal(journal, &backup, &dir)
                    .context("incomplete_recovery: journal restoration failed")?;
                return Err(error);
            }
            // Make the retained journal backup durable before replacing its old
            // index. On any failure before the live rename, restore its pathname.
            let durable_backup = at(ActivationStage::AfterJournalBackupBeforeDirFsync)
                .and_then(|_| reject_sidecars(&stage.path, true))
                .and_then(|_| {
                    std::fs::File::open(&dir)?.sync_all()?;
                    Ok(())
                });
            if let Err(error) = durable_backup {
                restore_index_journal(journal, &backup, &dir)
                    .context("incomplete_recovery: journal restoration failed")?;
                return Err(error);
            }
            Some(backup)
        } else {
            None
        };
        if let Some(marker) = &obsolete {
            let recheck = (|| -> Result<()> {
                old.verify()?;
                let db = open_index_marker_probe(&path, false)?;
                self.verify_metadata_root(&db)?;
                ensure!(
                    read_index_format_marker(&db)?.eq(marker),
                    "recovery_required: obsolete marker changed before rename"
                );
                stage.verify_path()?;
                reject_sidecars(&stage.path, true)?;
                reject_sidecars(&path, true)?;
                leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
                check_cancel(cancel)
            })();
            if let Err(error) = recheck {
                if let (Some(journal), Some(backup)) = (&journal, &backup) {
                    restore_index_journal(journal, backup, &dir)
                        .context("incomplete_recovery: journal restoration failed")?;
                }
                return Err(error);
            }
        }
        if let Err(error) = std::fs::rename(&stage.path, &path) {
            if let (Some(journal), Some(backup)) = (&journal, &backup) {
                restore_index_journal(journal, backup, &dir)
                    .context("incomplete_recovery: journal restoration failed")?;
            }
            return Err(error.into());
        }
        // The atomic rename may already be durable even if any later step fails.
        // Never let the stage guard unlink the now-live replacement inode.
        stage.published = true;
        at(ActivationStage::AfterRenameBeforeDirFsync)?;
        std::fs::File::open(&dir)?.sync_all()?;
        if let (Some(journal), Some(backup)) = (&journal, &backup) {
            journal.verify_at(backup)?;
            std::fs::remove_file(backup)?;
            std::fs::File::open(&dir)?.sync_all()?;
        }
        self.validate_replacement_index(&path, &stage, leader, pin)?;
        leader.downgrade_use_to_shared()?;
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        self.identity.verify()?;
        // This order means every clone still refuses while recovery_required is true.
        self.recovery_disposition
            .store(RecoveryDisposition::Ready as u8, Ordering::Release);
        self.recovery_required.store(false, Ordering::Release);
        Ok(pin)
    }
    fn initialize(
        &self,
        leader: &topology::LeaderGuard,
        before_publish: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<()> {
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        self.identity.verify()?;
        let path = self.roots.index_db(&self.identity);
        reject_sidecars(&path, true)?;
        if index_path_present(&path)? {
            return Ok(());
        }
        let use_guard = self.roots.index_use(&self.identity)?;
        let mut staged = self.create_staged_index(leader)?;
        before_publish(&staged.path)?;
        verify_index_file(&staged.path)?;
        let mut checked = open_index(&staged.path, true)?;
        let checked_snapshot = storage_result(checked.transaction())?;
        self.decode_control_status_raw(&checked_snapshot)?;
        let integrity: String =
            storage_result(checked_snapshot.query_row("PRAGMA quick_check", [], |r| r.get(0)))?;
        ensure!(
            integrity == "ok",
            "incompatible_index: staged integrity check failed"
        );
        drop(checked_snapshot);
        drop(checked);
        staged.file.sync_all()?;
        // The verified pathname must still refer to the inode we created.
        use std::os::unix::fs::MetadataExt;
        let named = std::fs::symlink_metadata(&staged.path)?;
        let opened = staged.file.metadata()?;
        ensure!(
            named.is_file() && named.dev() == opened.dev() && named.ino() == opened.ino(),
            "unsafe_index: staged pathname changed"
        );
        leader.verify()?;
        use_guard.verify()?;
        self.identity.verify()?;
        ensure!(
            !index_path_present(&path)?,
            "incompatible_index: index appeared during initialization"
        );
        std::fs::rename(&staged.path, &path)?;
        staged.published = true;
        std::fs::File::open(self.roots.index_dir(&self.identity))?.sync_all()?;
        Ok(())
    }
    pub fn leader(&self) -> Result<topology::LeaderGuard> {
        self.leader_with_open_hook(|_| Ok(()))
    }
    fn leader_with_open_hook(
        &self,
        before_write: impl FnOnce(&Connection) -> Result<()>,
    ) -> Result<topology::LeaderGuard> {
        self.ensure_not_recreate_pending()?;
        drop(
            self.cache()
                .map_err(|error| self.report_live_read_failure(error))?,
        );
        let leader = self.roots.leader(&self.identity)?;
        // The leader incarnation is already durable. From this point every clone
        // must remain closed unless a complete paired publication commits.
        self.recovery_required.store(true, Ordering::Release);
        // Opening can race a second SQLite writer: repeat validation only AFTER
        // BEGIN IMMEDIATE excludes schema changes and before any metadata UPDATE.
        let mut db = self
            .cache_write()
            .map_err(|error| self.report_live_read_failure(error))?;
        let admitted_version: i64 =
            storage_result(db.pragma_query_value(None, "data_version", |r| r.get(0)))?;
        before_write(&db)?;
        let tx = storage_result(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let compatible = self
            .recovery_baseline(&tx)
            .map_err(|error| self.report_live_read_failure(error))?
            .compatible;
        let locked_version: i64 =
            storage_result(tx.pragma_query_value(None, "data_version", |r| r.get(0)))?;
        ensure!(
            locked_version == admitted_version,
            "incompatible_index: cache changed after admission"
        );
        if compatible {
            let age = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            ensure!(age <= 9_007_199_254_740_991, "invalid_open_age");
            storage_result(tx.execute(
                "UPDATE index_metadata SET last_opened_at=?1 WHERE singleton=1",
                [age as i64],
            ))?;
        }
        storage_result(tx.commit())?;
        drop(db);
        leader.verify()?;
        self.identity.verify()?;
        Ok(leader)
    }
    fn cache(&self) -> Result<IndexConnection> {
        self.connect_index(false)
    }
    fn cache_write(&self) -> Result<IndexConnection> {
        self.connect_index(true)
    }
    fn connect_index(&self, writable: bool) -> Result<IndexConnection> {
        self.ensure_not_recreate_pending()?;
        self.identity.verify()?;
        let use_guard = self.roots.index_use_existing(&self.identity)?;
        let db = open_index(&self.roots.index_db(&self.identity), writable)?;
        self.identity.verify()?;
        use_guard.verify()?;
        Ok(IndexConnection {
            db,
            _use_guard: use_guard,
        })
    }
    fn records(&self) -> topology::DurableRecords<'_> {
        topology::DurableRecords::new(&self.roots, &self.identity)
    }
    fn disposition(&self) -> RecoveryDisposition {
        match self.recovery_disposition.load(Ordering::Acquire) {
            0 => RecoveryDisposition::Ready,
            1 => RecoveryDisposition::Rebuild,
            2 => RecoveryDisposition::RecreatePending,
            3 => RecoveryDisposition::RootReplaced,
            _ => unreachable!("invalid recovery disposition"),
        }
    }
    fn mark_recovery(&self, disposition: RecoveryDisposition) {
        self.recovery_disposition
            .fetch_max(disposition as u8, Ordering::AcqRel);
        self.recovery_required.store(true, Ordering::Release);
    }
    fn ensure_not_recreate_pending(&self) -> Result<()> {
        ensure!(
            !matches!(
                self.disposition(),
                RecoveryDisposition::RecreatePending | RecoveryDisposition::RootReplaced
            ),
            "recovery_required: exceptional index recovery deferred"
        );
        Ok(())
    }
    fn classify_admission_error(&self, error: anyhow::Error, typed: bool) -> Result<bool> {
        let error = if typed {
            selected_integrity(error)
        } else {
            error
        };
        match recovery_class(&error) {
            RecoveryClass::Rebuild => {
                self.mark_recovery(RecoveryDisposition::Rebuild);
                Ok(false)
            }
            RecoveryClass::RecreatePending => {
                self.mark_recovery(RecoveryDisposition::RecreatePending);
                Ok(false)
            }
            RecoveryClass::Hard => Err(error),
        }
    }
    fn report_live_read_failure(&self, error: anyhow::Error) -> anyhow::Error {
        match recovery_class(&error) {
            RecoveryClass::Rebuild => {
                self.mark_recovery(RecoveryDisposition::Rebuild);
                anyhow::anyhow!("incompatible_index: live index decode failed: {error:#}")
            }
            RecoveryClass::RecreatePending => {
                self.mark_recovery(RecoveryDisposition::RecreatePending);
                anyhow::anyhow!("recovery_required: exceptional index recovery deferred")
            }
            RecoveryClass::Hard => error,
        }
    }
    fn report_selected_failure(&self, error: anyhow::Error) -> anyhow::Error {
        match recovery_class(&error) {
            RecoveryClass::Rebuild => {
                self.mark_recovery(RecoveryDisposition::Rebuild);
                anyhow::anyhow!("incompatible_index: selected evidence decode failed: {error:#}")
            }
            RecoveryClass::RecreatePending => {
                self.mark_recovery(RecoveryDisposition::RecreatePending);
                anyhow::anyhow!("incompatible_index: exceptional index recovery deferred")
            }
            RecoveryClass::Hard => error,
        }
    }
    fn validate_recovery_decode_rows(&self, db: &Connection) -> Result<()> {
        fn json_rows<T: DeserializeOwned>(db: &Connection, sql: &str) -> Result<()> {
            let mut statement = db.prepare(sql)?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            for payload in rows {
                let _: T = serde_json::from_str(&payload?)?;
            }
            Ok(())
        }
        let mut stmt = db.prepare(
            "SELECT language,path,content_hash,source_bytes FROM document_versions ORDER BY path",
        )?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })? {
            use sha2::{Digest, Sha256};
            let (language, path, hash, bytes) = row?;
            ensure!(
                hash == hex::encode(Sha256::digest(&bytes)),
                "incompatible_index: stored source hash mismatch"
            );
            let _ = SourceFile {
                language,
                path,
                hash,
                text: String::from_utf8(bytes)?,
            };
        }
        json_rows::<Symbol>(
            db,
            "SELECT n.payload FROM graph_nodes n JOIN revision_documents d ON d.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND d.graph_projection_id=n.projection_id ORDER BY n.id",
        )?;
        json_rows::<CallSite>(
            db,
            "SELECT c.payload FROM graph_calls c JOIN revision_documents d ON d.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND d.graph_projection_id=c.projection_id ORDER BY c.id",
        )?;
        json_rows::<ControlRegion>(
            db,
            "SELECT r.payload FROM graph_regions r JOIN revision_documents d ON d.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND d.graph_projection_id=r.projection_id ORDER BY r.id",
        )?;
        json_rows::<crate::classes::ClassDefinition>(
            db,
            "SELECT c.payload FROM classes c JOIN revision_documents d ON d.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND d.class_projection_id=c.projection_id ORDER BY c.id",
        )?;
        json_rows::<crate::classes::ClassRelation>(
            db,
            "SELECT c.payload FROM class_relations c JOIN revision_documents d ON d.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND d.class_projection_id=c.projection_id ORDER BY c.id",
        )?;
        let (stats, diagnostics, warnings): (String, String, String) = db.query_row(
            "SELECT m.stats,m.diagnostics,r.class_warnings FROM index_metadata m JOIN native_revisions r ON r.published_index_revision=m.index_revision AND r.id='pin:v1:'||m.index_generation||':'||m.index_revision WHERE m.singleton=1",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let _: IndexStats = serde_json::from_str(&stats)?;
        let _: Vec<Diagnostic> = serde_json::from_str(&diagnostics)?;
        let _: Vec<String> = serde_json::from_str(&warnings)?;

        macro_rules! scan {
            ($sql:expr, $( $index:literal => $kind:ty ),+ $(,)?) => {{
                let mut statement = db.prepare($sql)?;
                let rows = statement.query_map([], |row| {
                    $(let _: $kind = row.get($index)?;)+
                    Ok(())
                })?;
                for row in rows { row?; }
            }};
        }
        scan!("SELECT ordinal FROM native_producer_languages", 0 => i64);
        scan!("SELECT ordinal FROM native_source_set_languages", 0 => i64);
        scan!("SELECT ordinal FROM native_source_set_dependencies", 0 => i64);
        scan!("SELECT byte_length FROM document_versions", 0 => i64);
        scan!("SELECT coverage_requested,coverage_selected,coverage_diagnostic FROM revision_documents", 0 => bool, 1 => bool, 2 => Option<String>);
        scan!("SELECT ordinal FROM native_version_coverage_roles", 0 => i64);
        scan!("SELECT key_signature_present,key_type_parameter_count,key_variadic,key_ordinal,start_byte,end_byte,name_start,name_end FROM native_version_declarations",
            0 => bool, 1 => Option<i64>, 2 => Option<bool>, 3 => i64, 4 => i64, 5 => i64, 6 => Option<i64>, 7 => Option<i64>);
        scan!("SELECT ordinal,sibling_ordinal,signature_present,type_parameter_count,variadic FROM native_version_declaration_ancestors",
            0 => i64, 1 => i64, 2 => bool, 3 => Option<i64>, 4 => Option<bool>);
        scan!("SELECT ordinal FROM native_version_own_signature_types", 0 => i64);
        scan!("SELECT ancestor_ordinal,ordinal FROM native_version_ancestor_signature_types", 0 => i64, 1 => i64);
        scan!("SELECT ordinal FROM native_version_header_items", 0 => i64);
        scan!("SELECT ordinal,variadic FROM native_version_parameters", 0 => i64, 1 => bool);
        scan!("SELECT ordinal,start_byte,end_byte,callee_start,callee_end FROM native_version_calls",
            0 => i64, 1 => i64, 2 => i64, 3 => Option<i64>, 4 => Option<i64>);
        scan!("SELECT ordinal FROM native_version_call_regions", 0 => i64);
        scan!("SELECT ordinal,start_byte,end_byte FROM native_version_control_regions", 0 => i64, 1 => i64, 2 => i64);
        Ok(())
    }

    fn verify_metadata_root(&self, db: &Connection) -> Result<()> {
        let (count, typed): (i64, bool) = db.query_row(
            "SELECT count(*),coalesce(min(singleton=1 AND typeof(root_spelling)='text' AND typeof(root_device)='text' AND typeof(root_inode)='text'),0) FROM index_metadata",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            count == 1 && typed,
            "root_key_collision: index root identity is not a typed singleton"
        );
        let device = self.identity.device.to_string();
        let inode = self.identity.inode.to_string();
        for (column, expected, mismatch) in [
            (
                "root_spelling",
                self.workspace_root.as_bytes(),
                "root_key_collision: index belongs to a different spelling",
            ),
            (
                "root_device",
                device.as_bytes(),
                "root_changed: index root identity mismatch",
            ),
            (
                "root_inode",
                inode.as_bytes(),
                "root_changed: index root identity mismatch",
            ),
        ] {
            let blob = db.blob_open(rusqlite::MAIN_DB, "index_metadata", column, 1, true)?;
            ensure!(blob.len() == expected.len(), "{mismatch}");
            for (offset, chunk) in expected.chunks(64 * 1024).enumerate() {
                let offset = offset
                    .checked_mul(64 * 1024)
                    .context("root identity offset overflow")?;
                let mut actual = vec![0; chunk.len()];
                blob.read_at_exact(&mut actual, offset)?;
                ensure!(actual == chunk, "{mismatch}");
            }
            blob.close()?;
        }
        Ok(())
    }

    fn metadata_atom(&self, db: &Connection, column: MetadataColumn) -> Result<MetadataAtom> {
        use rusqlite::types::ValueRef;
        use sha2::{Digest, Sha256};
        let (type_sql, value_sql, column_name, tag) = column.sql();
        let storage: String = db.query_row(type_sql, [], |row| row.get(0))?;
        match storage.as_str() {
            "null" => Ok(MetadataAtom::Null),
            "integer" => db
                .query_row(value_sql, [], |row| match row.get_ref(0)? {
                    ValueRef::Integer(value) => Ok(MetadataAtom::Integer(value)),
                    _ => Err(rusqlite::Error::InvalidQuery),
                })
                .map_err(Into::into),
            "real" => db
                .query_row(value_sql, [], |row| match row.get_ref(0)? {
                    ValueRef::Real(value) => Ok(MetadataAtom::Real(value.to_bits())),
                    _ => Err(rusqlite::Error::InvalidQuery),
                })
                .map_err(Into::into),
            "text" | "blob" => {
                let text = storage == "text";
                let blob =
                    db.blob_open(rusqlite::MAIN_DB, "index_metadata", column_name, 1, true)?;
                let byte_length = blob.len();
                let mut digest = Sha256::new();
                digest.update(b"baleyg-index-metadata-witness-v1\0");
                digest.update(tag);
                digest.update([u8::from(text)]);
                digest.update((byte_length as u64).to_le_bytes());
                let mut offset = 0usize;
                let mut buffer = vec![0; (64 * 1024).min(byte_length)];
                while offset < byte_length {
                    let count = buffer.len().min(byte_length - offset);
                    blob.read_at_exact(&mut buffer[..count], offset)?;
                    digest.update(&buffer[..count]);
                    offset = offset
                        .checked_add(count)
                        .context("metadata offset overflow")?;
                }
                blob.close()?;
                Ok(MetadataAtom::Streamed {
                    column,
                    text,
                    byte_length,
                    sha256: digest.finalize().into(),
                })
            }
            _ => anyhow::bail!("incompatible_index: unknown SQLite metadata storage class"),
        }
    }

    fn metadata_witness(&self, db: &Connection, schema: u32) -> Result<RecoveryWitness> {
        Ok(RecoveryWitness {
            pragma_schema: schema,
            schema_version: self.metadata_atom(db, MetadataColumn::SchemaVersion)?,
            extractor_version: self.metadata_atom(db, MetadataColumn::ExtractorVersion)?,
            index_generation: self.metadata_atom(db, MetadataColumn::IndexGeneration)?,
            index_revision: self.metadata_atom(db, MetadataColumn::IndexRevision)?,
        })
    }

    fn recovery_baseline(&self, db: &Connection) -> Result<RecoveryBaseline> {
        validate_cache_shape(db)?;
        let schema: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        self.verify_metadata_root(db)?;
        let witness = self.metadata_witness(db, schema)?;
        let bounded_text = |atom: &MetadataAtom, max: usize| matches!(atom, MetadataAtom::Streamed { text: true, byte_length, .. } if *byte_length <= max);
        if !bounded_text(&witness.extractor_version, 256)
            || !bounded_text(&witness.index_generation, 64)
            || matches!(
                witness.schema_version,
                MetadataAtom::Streamed { .. } | MetadataAtom::Null
            )
            || matches!(
                witness.index_revision,
                MetadataAtom::Streamed { .. } | MetadataAtom::Null
            )
        {
            self.mark_recovery(RecoveryDisposition::Rebuild);
            return Ok(RecoveryBaseline {
                witness,
                pin: None,
                compatible: false,
            });
        }
        let decoded: rusqlite::Result<(i64, String, String, i64)> = db.query_row(
            "SELECT schema_version,extractor_version,index_generation,index_revision FROM index_metadata WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        );
        let (metadata_schema, extractor, generation, revision) = match decoded {
            Ok(row) => row,
            Err(error) => {
                let compatible = self.classify_admission_error(error.into(), false)?;
                debug_assert!(!compatible);
                return Ok(RecoveryBaseline {
                    witness,
                    pin: None,
                    compatible: false,
                });
            }
        };
        let generation = match uuid::Uuid::parse_str(&generation) {
            Ok(generation) => generation,
            Err(error) => {
                let compatible = self.classify_admission_error(
                    SelectedIntegrity(format!("incompatible_index: invalid generation: {error}"))
                        .into(),
                    false,
                )?;
                debug_assert!(!compatible);
                return Ok(RecoveryBaseline {
                    witness,
                    pin: None,
                    compatible: false,
                });
            }
        };
        if !(0..=9_007_199_254_740_991).contains(&revision) {
            self.mark_recovery(RecoveryDisposition::Rebuild);
            return Ok(RecoveryBaseline {
                witness,
                pin: None,
                compatible: false,
            });
        }
        let pin = IndexPin {
            index_generation: generation,
            index_revision: revision as u64,
        };
        let marker_matches = metadata_schema == i64::from(schema);
        let compatible = if marker_matches
            && schema == DATABASE_SCHEMA_VERSION
            && extractor == EXTRACTOR_VERSION
        {
            // Revision zero is the empty v8 bootstrap. It has no native pair
            // yet; the first successful publication creates that metadata.
            let validation = if pin.index_revision == 0 {
                self.decode_control_status_raw(db).and_then(|status| {
                    control_ensure!(
                        status.evidence_format.is_none(),
                        "incompatible_index: bootstrap exposed evidence"
                    );
                    Ok(())
                })
            } else {
                validate_bounded_control(db, &self.identity.record_id)
            };
            match validation {
                Ok(()) => true,
                Err(error) => self.classify_admission_error(error, true)?,
            }
        } else {
            self.mark_recovery(RecoveryDisposition::Rebuild);
            false
        };
        Ok(RecoveryBaseline {
            witness,
            pin: Some(pin),
            compatible,
        })
    }

    fn decode_control_status_raw(&self, db: &Connection) -> Result<IndexStatus> {
        // This check must run INSIDE the caller's read snapshot or writer lock.
        // The open_index admission check alone cannot protect against later DDL.
        validate_cache_shape(db)?;
        let schema_version: i64 =
            storage_result(db.pragma_query_value(None, "user_version", |r| r.get(0)))?;
        if schema_version == i64::from(DATABASE_SCHEMA_VERSION) {
            bounded_graph_pair(
                db,
                "SELECT typeof(stats),length(CAST(stats AS BLOB)),
                typeof(diagnostics),length(CAST(diagnostics AS BLOB))
                FROM index_metadata WHERE singleton=1",
            )?;
        }
        let row: (i64,String,String,String,String,String,i64,String,String,String) = storage_result(db.query_row(
            "SELECT schema_version,extractor_version,root_spelling,root_device,root_inode,index_generation,index_revision,indexed_at,stats,diagnostics FROM index_metadata WHERE singleton=1",
            [],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?))))?;
        control_ensure!(
            schema_version == i64::from(DATABASE_SCHEMA_VERSION)
                && row.0 == schema_version
                && row.1 == EXTRACTOR_VERSION,
            "incompatible_index: extractor or schema"
        );
        ensure!(
            row.2 == self.workspace_root,
            "root_key_collision: index belongs to a different spelling"
        );
        ensure!(
            row.3 == self.identity.device.to_string() && row.4 == self.identity.inode.to_string(),
            "root_changed: index root identity mismatch"
        );
        let pin: IndexPin = serde_json::from_value(
            serde_json::json!({"indexGeneration":row.5,"indexRevision":row.6}),
        )?;
        if schema_version == i64::from(DATABASE_SCHEMA_VERSION) {
            if pin.index_revision == 0 {
                validate_v8_bootstrap(db)?;
            } else {
                validate_paired_metadata(db, &self.identity.record_id)?;
            }
        }
        Ok(IndexStatus {
            workspace_root: self.workspace_root.clone(),
            revision: pin,
            indexed_at: if row.7.is_empty() { None } else { Some(row.7) },
            stats: serde_json::from_str(&row.8)?,
            diagnostics: serde_json::from_str(&row.9)?,
            evidence_format: (row.0 == i64::from(DATABASE_SCHEMA_VERSION) && row.6 > 0)
                .then(|| EVIDENCE_FORMAT.to_owned()),
        })
    }
    fn ensure_public_read_ready(&self) -> Result<()> {
        if !self.recovery_required.load(Ordering::Acquire) {
            return Ok(());
        }
        match self.disposition() {
            RecoveryDisposition::Ready => {
                Err(topology::IndexNotReady::new("reconciliation required").into())
            }
            RecoveryDisposition::Rebuild => {
                anyhow::bail!(
                    "incompatible_index: reconciliation required after invalid current index"
                )
            }
            RecoveryDisposition::RecreatePending => {
                anyhow::bail!("recovery_required: exceptional index recovery deferred")
            }
            RecoveryDisposition::RootReplaced => {
                anyhow::bail!("recovery_required: replacement-root index recovery deferred")
            }
        }
    }
    fn read_public_control_status(&self, db: &Connection) -> Result<IndexStatus> {
        self.verify_metadata_root(db)
            .map_err(|error| self.report_live_read_failure(error))?;
        self.ensure_public_read_ready()?;
        self.decode_control_status_raw(db)
            .map_err(|error| self.report_live_read_failure(error))
    }

    fn read_status(&self, db: &Connection) -> Result<IndexStatus> {
        let status = self.read_public_control_status(db)?;
        if status.evidence_format.is_none() {
            let schema: i64 =
                storage_result(db.pragma_query_value(None, "user_version", |row| row.get(0)))?;
            // read_public_control_status has already validated the exact empty
            // v8 bootstrap in this snapshot. Only that state can make durable
            // saved reads available without an index; legacy/partial evidence
            // must not take this typed fallback.
            if schema == i64::from(DATABASE_SCHEMA_VERSION) && status.revision.index_revision == 0 {
                return Err(topology::IndexNotReady::new("reindex required").into());
            }
            anyhow::bail!("index_not_ready: reindex required");
        }
        (|| -> Result<()> {
            validate_reconcile_inventory(db)?;
            // Every public v8 derived read needs the same bounded catalog
            // singleton. Missing/oversized live metadata is corruption, never an
            // old-index "requireIndex" fallback. This is one indexed metadata row.
            let warnings_bytes: Option<i64> = db
                .query_row(
                    "SELECT length(CAST(r.class_warnings AS BLOB)) FROM native_revisions r JOIN index_metadata m ON m.index_revision=r.published_index_revision AND r.id='pin:v1:'||m.index_generation||':'||m.index_revision",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            if !warnings_bytes.is_some_and(|bytes| (0..=256 * 1024).contains(&bytes)) {
                return Err(SelectedIntegrity(
                    "incompatible_index: class catalog byte budget exceeded or missing".into(),
                )
                .into());
            }
            Ok(())
        })()
        .map_err(|error| self.report_live_read_failure(error))?;
        Ok(status)
    }

    /// Internal control baseline, never returned by public status or evidence reads.
    pub fn index_baseline(&self) -> Result<IndexPin> {
        self.recovery_index_baseline()?
            .pin
            .context("index_not_ready: prior index pin is not decodable")
    }
    pub(crate) fn recovery_index_baseline(&self) -> Result<RecoveryBaseline> {
        self.ensure_not_recreate_pending()?;
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        self.recovery_baseline(&tx)
    }
    pub fn root_id(&self) -> &str {
        &self.identity.record_id
    }
    pub fn workspace_root(&self) -> &str {
        &self.workspace_root
    }
    pub fn recorded_index_options(&self) -> Result<Option<crate::indexer::IndexOptions>> {
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let schema: u32 =
            storage_result(tx.pragma_query_value(None, "user_version", |row| row.get(0)))?;
        if schema != DATABASE_SCHEMA_VERSION {
            return Ok(None);
        }
        // One snapshot must bind the shape, revision and options. A fresh v8
        // bootstrap has no options or evidence; a published revision never may.
        validate_cache_shape(&tx)?;
        self.verify_metadata_root(&tx)?;
        let revision: i64 = storage_result(tx.query_row(
            "SELECT index_revision FROM index_metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        ))?;
        if revision == 0 {
            validate_v8_bootstrap(&tx)?;
            return Ok(None);
        }
        let payload: Option<String> = storage_result(tx.query_row(
            "SELECT reconcile_options FROM index_metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        ))?;
        let payload = payload.context("incompatible_index: missing reconcile options")?;
        let options: crate::indexer::ReconcileOptions = serde_json::from_str(&payload)
            .context("incompatible_index: invalid reconcile options")?;
        ensure!(
            json(&options)? == payload && options.version == 1,
            "incompatible_index: unsupported reconcile options"
        );
        options.require_absolute_optional_inputs().context(
            "recovery_required: recorded relative index input; run explicit baleyg index",
        )?;
        let mut result =
            crate::indexer::IndexOptions::new(Path::new(&self.workspace_root).to_owned());
        result.max_file_bytes = options.max_file_bytes;
        result.scip_path = options.scip_path.map(Into::into);
        result.manifest_path = options.manifest_path.map(Into::into);
        Ok(Some(result))
    }
    pub fn verify_root(&self) -> Result<()> {
        self.identity.verify()
    }
    pub(crate) fn root_path_replaced(&self) -> Result<bool> {
        self.identity.root_path_replaced()
    }
    pub(crate) fn is_root_replaced(&self) -> bool {
        self.disposition() == RecoveryDisposition::RootReplaced
    }
    pub(crate) fn is_ready_disposition(&self) -> bool {
        self.disposition() == RecoveryDisposition::Ready
    }
    pub(crate) fn is_recreate_pending(&self) -> bool {
        matches!(
            self.disposition(),
            RecoveryDisposition::RecreatePending | RecoveryDisposition::RootReplaced
        ) && self.recovery_required.load(Ordering::Acquire)
    }
    pub(crate) fn recreate_pending_leader_session(
        &self,
        options: &crate::indexer::IndexOptions,
        cancel: &CancelFlag,
    ) -> Result<(IndexPin, Arc<topology::LeaderSession>)> {
        ensure!(
            self.is_recreate_pending(),
            "recovery_required: exceptional recreation not classified"
        );
        if self.disposition() == RecoveryDisposition::RootReplaced {
            // The old index is still fenced by the verified leader under SH. Resolve
            // foreign-root queue rows durably BEFORE attempting index replacement.
            // Dropping this entire scope releases every local SH and SQLite handle.
            {
                let leader = Arc::new(topology::LeaderSession::leader(
                    self.roots.leader(&self.identity)?,
                    self.identity.clone(),
                ));
                self.verify_leader_session(&leader)?;
                let index_path = self.roots.index_db(&self.identity);
                ensure!(
                    !index_path_present(&index_path.with_file_name("index.db-journal"))?,
                    "recovery_required: root replacement has hot journal"
                );
                let db = open_index(&index_path, false)?;
                let mismatch = self.recovery_baseline(&db);
                drop(db);
                ensure!(
                    matches!(mismatch, Err(ref error)
                    if error.to_string() == "root_changed: index root identity mismatch"),
                    "root_changed: replacement authority changed"
                );
                self.fail_changed_root_requests(&leader)?;
            }
        }
        let exclusive = self.roots.index_use_exclusive_existing(&self.identity)?;
        let mut leader = self
            .roots
            .leader_under_exclusive(&self.identity, exclusive)?;
        #[cfg(test)]
        self.test_exclusive_recovery_hook.run();
        let pin = self.recreate_index_exclusive(options, &mut leader, cancel)?;
        let session = Arc::new(topology::LeaderSession::leader(
            leader,
            self.identity.clone(),
        ));
        self.verify_leader_session(&session)?;
        ensure!(
            self.status()?.revision == pin,
            "index_not_ready: exceptional recovery pair not admitted"
        );
        Ok((pin, session))
    }
    pub fn leader_session(&self) -> Result<Arc<topology::LeaderSession>> {
        Ok(Arc::new(topology::LeaderSession::leader(
            self.leader()?,
            self.identity.clone(),
        )))
    }
    pub(crate) fn verify_leader_session(&self, session: &topology::LeaderSession) -> Result<()> {
        session.belongs_to(&self.identity, &self.roots.leader_lock(&self.identity))
    }
    #[allow(dead_code)] // Internal diagnostics; exercised by the writer-lock regression test.
    pub(crate) fn last_writer_counters(&self) -> Option<WriterCounters> {
        *self.writer_counters.lock().unwrap()
    }
    pub(crate) fn begin_leader_publication(&self, session: &topology::LeaderSession) -> Result<()> {
        self.verify_leader_session(session)?;
        self.recovery_required.store(true, Ordering::Release);
        Ok(())
    }
    pub fn follower_session(&self) -> Result<Arc<topology::LeaderSession>> {
        let db = self
            .cache()
            .map_err(|error| self.report_live_read_failure(error))?;
        storage_result(db.execute_batch("BEGIN DEFERRED"))?;
        let (_, marker) = self.admit_evidence_control(&db)?;
        let follower = self.roots.follower(self.identity.clone())?;
        self.verify_follower_marker(&follower, marker)?;
        drop(db);
        Ok(Arc::new(topology::LeaderSession::follower(follower)))
    }
    fn admit_evidence_control(&self, db: &Connection) -> Result<(IndexStatus, uuid::Uuid)> {
        self.verify_metadata_root(db)
            .map_err(|error| self.report_live_read_failure(error))?;
        self.ensure_public_read_ready()?;
        let schema: u32 =
            storage_result(db.pragma_query_value(None, "user_version", |row| row.get(0)))?;
        if schema != DATABASE_SCHEMA_VERSION {
            return Err(topology::IndexNotReady::new("reconciliation required").into());
        }
        let status = self.read_status(db)?;
        let marker: Option<String> = db
            .query_row(
                "SELECT reconciled_incarnation FROM index_metadata WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| self.report_live_read_failure(selected_integrity(error.into())))?;
        let marker = marker
            .context("incompatible_index: missing reconciled incarnation")
            .and_then(|value| {
                uuid::Uuid::parse_str(&value)
                    .context("incompatible_index: invalid reconciled incarnation")
            })
            .map_err(|error| self.report_live_read_failure(selected_integrity(error)))?;
        Ok((status, marker))
    }
    fn verify_follower_marker(
        &self,
        follower: &topology::FollowerGuard,
        marker: uuid::Uuid,
    ) -> Result<()> {
        if follower.incarnation != marker {
            return Err(topology::IndexNotReady::new("reconciled leader mismatch").into());
        }
        self.identity.verify()?;
        follower.verify(marker)
    }
    pub fn evidence_response(&self) -> Result<EvidenceResponse> {
        self.ensure_not_recreate_pending()?;
        let db = self
            .cache()
            .map_err(|error| self.report_live_read_failure(error))?;
        storage_result(db.execute_batch("BEGIN DEFERRED"))?;
        let (_, marker) = self.admit_evidence_control(&db)?;
        let follower = self.roots.follower(self.identity.clone())?;
        self.verify_follower_marker(&follower, marker)?;
        Ok(EvidenceResponse {
            store: self.clone(),
            db,
            follower,
            marker,
        })
    }
    fn with_evidence<T>(&self, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.with_evidence_hook(read, || Ok(()))
    }
    fn with_evidence_hook<T>(
        &self,
        read: impl FnOnce(&Connection) -> Result<T>,
        before_finish: impl FnOnce() -> Result<()>,
    ) -> Result<T> {
        self.with_evidence_observed(read, before_finish, |_| {})
    }
    fn with_evidence_observed<T>(
        &self,
        read: impl FnOnce(&Connection) -> Result<T>,
        before_finish: impl FnOnce() -> Result<()>,
        observe_finish: impl FnOnce(&Result<()>),
    ) -> Result<T> {
        let response = self.evidence_response()?;
        let value = read(&response.db);
        let hook = before_finish();
        let fence = response.finish(());
        observe_finish(&fence);
        match value {
            Err(error) => Err(error),
            Ok(value) => {
                hook?;
                fence?;
                Ok(value)
            }
        }
    }
    pub fn status(&self) -> Result<IndexStatus> {
        self.status_with_open_hook(|_| Ok(()))
    }
    // Private barrier after the admission check, before the read transaction.
    // A second SQLite connection can add DDL here in regression tests.
    fn status_with_open_hook(
        &self,
        before_snapshot: impl FnOnce(&Connection) -> Result<()>,
    ) -> Result<IndexStatus> {
        self.ensure_not_recreate_pending()?;
        let db = self
            .cache()
            .map_err(|error| self.report_live_read_failure(error))?;
        before_snapshot(&db)?;
        drop(db);
        self.with_evidence(|db| self.read_status(db))
    }
    pub fn publish(
        &self,
        graph: &Graph,
        leader: &topology::LeaderGuard,
        expected_revision: IndexPin,
        cancel: &CancelFlag,
    ) -> Result<IndexPin> {
        let _ = (graph, leader, expected_revision, cancel);
        anyhow::bail!("native_evidence_required: graph-only publication refused")
    }
    pub fn publish_captured(
        &self,
        graph: &Graph,
        capture: &crate::capture::Capture,
        leader: &topology::LeaderGuard,
        expected_revision: IndexPin,
        cancel: &CancelFlag,
    ) -> Result<IndexPin> {
        let _ = (graph, capture, leader, expected_revision, cancel);
        anyhow::bail!("native_evidence_required: captured graph-only publication refused")
    }
    pub fn publish_native(
        &self,
        graph: &Graph,
        capture: &crate::capture::Capture,
        native: &crate::native_evidence::Artifact,
        leader: &topology::LeaderGuard,
        expected_revision: IndexPin,
        cancel: &CancelFlag,
    ) -> Result<IndexPin> {
        self.publish_native_expected(
            graph,
            capture,
            native,
            leader,
            ExpectedPublication::Pin(expected_revision),
            cancel,
            false,
        )
    }
    pub(crate) fn publish_native_recovery(
        &self,
        graph: &Graph,
        capture: &crate::capture::Capture,
        native: &crate::native_evidence::Artifact,
        leader: &topology::LeaderGuard,
        expected: RecoveryBaseline,
        cancel: &CancelFlag,
    ) -> Result<IndexPin> {
        self.publish_native_expected(
            graph,
            capture,
            native,
            leader,
            ExpectedPublication::Recovery(Box::new(expected)),
            cancel,
            false,
        )
    }
    pub(crate) fn publish_native_recovery_selective(
        &self,
        graph: &Graph,
        capture: &crate::capture::Capture,
        native: &crate::native_evidence::Artifact,
        leader: &topology::LeaderGuard,
        expected: RecoveryBaseline,
        cancel: &CancelFlag,
    ) -> Result<IndexPin> {
        self.publish_native_expected(
            graph,
            capture,
            native,
            leader,
            ExpectedPublication::Recovery(Box::new(expected)),
            cancel,
            true,
        )
    }
    #[allow(clippy::too_many_arguments)] // The selective proof is internal to this publication seam.
    fn publish_native_expected(
        &self,
        graph: &Graph,
        capture: &crate::capture::Capture,
        native: &crate::native_evidence::Artifact,
        leader: &topology::LeaderGuard,
        expected: ExpectedPublication,
        cancel: &CancelFlag,
        selective: bool,
    ) -> Result<IndexPin> {
        self.validate_native_bundle_with_mode(graph, capture, native, cancel, selective)?;
        self.publish_inner_expected(graph, capture, native, leader, expected, cancel)
    }
    fn validate_native_bundle(
        &self,
        graph: &Graph,
        capture: &crate::capture::Capture,
        native: &crate::native_evidence::Artifact,
        cancel: &CancelFlag,
    ) -> Result<()> {
        self.validate_native_bundle_with_mode(graph, capture, native, cancel, false)
    }
    fn validate_native_bundle_with_mode(
        &self,
        graph: &Graph,
        capture: &crate::capture::Capture,
        native: &crate::native_evidence::Artifact,
        cancel: &CancelFlag,
        selective: bool,
    ) -> Result<()> {
        if selective {
            native.validate_selective(
                capture,
                Path::new(&self.workspace_root),
                &self.identity.record_id,
                cancel,
            )?;
        } else {
            native.validate(
                capture,
                Path::new(&self.workspace_root),
                &self.identity.record_id,
                cancel,
            )?;
        }
        ensure!(
            capture.graph_projection_count() == 1,
            "native_evidence_required: capture must have one graph projection"
        );
        ensure!(
            capture.source_operations.len() == capture.files.len()
                && capture
                    .source_operations
                    .values()
                    .all(|counts| counts.opens == 1
                        && counts.complete_reads == 1
                        && counts.hashes == 1),
            "native_evidence_required: each source must open, read, and hash once"
        );
        crate::indexer::validate_native_graph(graph, capture, native, cancel)?;
        // A capture from a direct library caller must not persist cwd-relative
        // presentation identities even if it bypassed the explicit CLI ingress.
        capture
            .reconcile_options()
            .require_absolute_optional_inputs()
    }
    /// Publish to an unpublished private bootstrap while the caller retains EX use.
    /// This stages the full pair without admitting evidence from the corrupt live DB.
    #[allow(dead_code)] // The exceptional recovery entry point is wired after this scoped seam.
    fn publish_native_to_stage(
        &self,
        bundle: (
            &Graph,
            &crate::capture::Capture,
            &crate::native_evidence::Artifact,
        ),
        stage: &StagedIndex,
        leader: &topology::LeaderGuard,
        cancel: &CancelFlag,
        during_tx: impl FnMut(PublishStage, &Connection) -> Result<()>,
    ) -> Result<IndexPin> {
        let (graph, capture, native) = bundle;
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
        stage.verify_path()?;
        self.identity.verify()?;
        let index_dir = self.roots.index_dir(&self.identity);
        let stage_id = stage
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("index.db.tmp-"));
        ensure!(
            stage.path.parent() == Some(index_dir.as_path())
                && stage_id.is_some_and(|value| uuid::Uuid::parse_str(value).is_ok()),
            "unsafe_index: stage is outside the managed index directory"
        );
        let bootstrap = open_index(&stage.path, false)?;
        let version: u32 = bootstrap.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            version == DATABASE_SCHEMA_VERSION,
            "incompatible_index: stage is not an unpublished bootstrap"
        );
        // The stage must be the exact empty v8 bootstrap we just created.
        // A published or partially written stage is never a recovery source.
        validate_v8_bootstrap(&bootstrap)?;
        let expected = self.recovery_baseline(&bootstrap)?;
        ensure!(
            expected.pin().is_some_and(|pin| pin.index_revision == 0) && expected.compatible,
            "incompatible_index: invalid private stage baseline"
        );
        drop(bootstrap);
        stage.verify_path()?;
        leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
        self.validate_native_bundle(graph, capture, native, cancel)?;
        self.publish_inner_checked_target(
            bundle,
            leader,
            PublicationPlan {
                expected: ExpectedPublication::Recovery(Box::new(expected)),
                target: PublicationTarget::Stage(stage),
            },
            cancel,
            256 * 1024 * 1024 + 16 * 1024,
            during_tx,
        )
    }
    fn publish_inner_expected(
        &self,
        graph: &Graph,
        capture: &crate::capture::Capture,
        native: &crate::native_evidence::Artifact,
        leader: &topology::LeaderGuard,
        expected: ExpectedPublication,
        cancel: &CancelFlag,
    ) -> Result<IndexPin> {
        self.publish_inner_checked_expected(
            (graph, capture, native),
            leader,
            expected,
            cancel,
            256 * 1024 * 1024 + 16 * 1024,
            |_, _| Ok(()),
        )
    }

    // Admission/read parity: only publish SourceFile JSON that pinned reads can
    // select under the same encoded-byte ceiling. This inspects the immutable
    // captured graph; it neither opens source files nor reprojects the graph.
    fn enforce_selected_source_admission(graph: &Graph, max_graph_json_bytes: usize) -> Result<()> {
        fn encoded_len<T: Serialize>(value: &T, max: usize) -> Result<usize> {
            let mut sink = EncodedSourceBudget {
                bytes: 0,
                max_bytes: max,
            };
            serde_json::to_writer(&mut sink, value)?;
            Ok(sink.bytes)
        }
        let mut selected: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for file in &graph.files {
            let raw = file.text.len();
            ensure!(
                raw <= 256 * 1024 * 1024,
                "incompatible_index: selected source byte budget exceeded before publication"
            );
            let envelope =
                max_graph_json_bytes.min(raw.saturating_mul(6).saturating_add(16 * 1024));
            encoded_len(file,envelope).map_err(|e|anyhow::anyhow!(
                "incompatible_index: selected source byte budget exceeded before publication: {e}"))?;
            ensure!(
                selected.insert(&file.path, (raw, 0)).is_none(),
                "incompatible_index: duplicate captured graph source"
            );
        }
        fn account<T: Serialize>(
            selected: &mut BTreeMap<&str, (usize, usize)>,
            path: &str,
            metadata_bytes: usize,
            value: &T,
        ) -> Result<()> {
            let (raw, total) = selected
                .get_mut(path)
                .context("incompatible_index: graph row without captured source")?;
            let row_cap = raw
                .saturating_mul(6)
                .saturating_add(16 * 1024)
                .min(256 * 1024 * 1024 + 16 * 1024);
            let bytes=encoded_len(value,row_cap).map_err(|e|anyhow::anyhow!(
                "incompatible_index: selected graph row byte budget exceeded before publication: {e}"))?;
            let row_bytes = metadata_bytes.checked_add(bytes).context(
                "incompatible_index: selected graph row byte budget exceeded before publication",
            )?;
            *total = total.checked_add(row_bytes).context(
                "incompatible_index: selected graph row byte budget exceeded before publication",
            )?;
            let total_cap = raw
                .saturating_mul(64)
                .saturating_add(256 * 1024)
                .min(512 * 1024 * 1024);
            ensure!(
                row_bytes <= row_cap && *total <= total_cap,
                "incompatible_index: selected graph row byte budget exceeded before publication"
            );
            Ok(())
        }
        for n in &graph.nodes {
            account(
                &mut selected,
                &n.path,
                n.id.len()
                    .saturating_add(n.name.len())
                    .saturating_add(n.path.len()),
                n,
            )?;
        }
        for c in &graph.calls {
            account(
                &mut selected,
                &c.path,
                c.id.len()
                    .saturating_add(c.caller.len())
                    .saturating_add(c.path.len()),
                c,
            )?;
        }
        for r in &graph.regions {
            account(
                &mut selected,
                &r.path,
                r.id.len()
                    .saturating_add(r.owner.len())
                    .saturating_add(r.path.len()),
                r,
            )?;
        }
        Ok(())
    }
    // Private transaction seam used by the in-module rollback tests. Normal callers
    // always pass a no-op; no SQL-fault control is exposed to API or CLI clients.
    #[cfg(test)]
    fn publish_inner_checked(
        &self,
        bundle: (
            &Graph,
            &crate::capture::Capture,
            &crate::native_evidence::Artifact,
        ),
        leader: &topology::LeaderGuard,
        expected_revision: IndexPin,
        cancel: &CancelFlag,
        during_tx: impl FnMut(PublishStage, &Connection) -> Result<()>,
    ) -> Result<IndexPin> {
        self.publish_inner_checked_with_source_cap(
            bundle,
            leader,
            expected_revision,
            cancel,
            256 * 1024 * 1024 + 16 * 1024,
            during_tx,
        )
    }
    // This test-only injection exercises the production admission path with a
    // small graph-JSON cap, without creating a 256MiB escaped source fixture.
    #[cfg(test)]
    fn publish_inner_checked_with_source_cap(
        &self,
        bundle: (
            &Graph,
            &crate::capture::Capture,
            &crate::native_evidence::Artifact,
        ),
        leader: &topology::LeaderGuard,
        expected_revision: IndexPin,
        cancel: &CancelFlag,
        max_graph_json_bytes: usize,
        during_tx: impl FnMut(PublishStage, &Connection) -> Result<()>,
    ) -> Result<IndexPin> {
        self.publish_inner_checked_expected(
            bundle,
            leader,
            ExpectedPublication::Pin(expected_revision),
            cancel,
            max_graph_json_bytes,
            during_tx,
        )
    }
    fn publish_inner_checked_expected(
        &self,
        bundle: (
            &Graph,
            &crate::capture::Capture,
            &crate::native_evidence::Artifact,
        ),
        leader: &topology::LeaderGuard,
        expected: ExpectedPublication,
        cancel: &CancelFlag,
        max_graph_json_bytes: usize,
        during_tx: impl FnMut(PublishStage, &Connection) -> Result<()>,
    ) -> Result<IndexPin> {
        self.publish_inner_checked_target(
            bundle,
            leader,
            PublicationPlan {
                expected,
                target: PublicationTarget::Live,
            },
            cancel,
            max_graph_json_bytes,
            during_tx,
        )
    }
    /// A publishing leader may read its prior snapshot after begin_leader_publication
    /// has made public reads not-ready. This private snapshot never serves clients.
    fn with_prior_publication_snapshot<T>(
        &self,
        read: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        let db = self.cache()?;
        db.execute_batch("BEGIN DEFERRED")?;
        let baseline = self.recovery_baseline(&db)?;
        ensure!(
            baseline.compatible && baseline.pin().is_some_and(|p| p.index_revision > 0),
            "incompatible_index: prior publication is not a current head"
        );
        let result = read(&db);
        let identity = self.identity.verify();
        db.execute_batch("ROLLBACK")?;
        identity?;
        result
    }

    /// Read a prior immutable native version without running the native parser.
    /// Its source bytes must be exactly the newly admitted immutable source. The
    /// normalized rows are checked against the witness written at full admission.
    fn selected_reusable_native(
        &self,
        db: &Connection,
        selected: &ReadRevision,
        file: &SourceFile,
    ) -> Result<crate::native_evidence::SelectedDocument> {
        use crate::native_evidence::{
            Document, DocumentKey, Producer, Provenance, Revision, SelectedDocument, SourceSet,
        };
        use sha2::{Digest, Sha256};
        let key = DocumentKey {
            source_set_id: format!("source-set:v1:{}", self.root_id()),
            language: file.language.clone(),
            path: file.path.clone(),
        };
        let scope = v8_native_scope_for(db, &key, selected)?
            .context("incompatible_index: reusable native version absent")?;
        let (hash, length, bytes, context, producer_id, producer_version, witness): (
            String,
            i64,
            Vec<u8>,
            String,
            String,
            String,
            String,
        ) = db.query_row(
            "SELECT d.content_hash,d.byte_length,d.source_bytes,d.extraction_context,
                d.producer_id,d.producer_version,d.native_witness FROM document_versions d
                WHERE d.id=?1 AND d.source_set_id=?2 AND d.language=?3 AND d.path=?4",
            params![scope.version_id, key.source_set_id, key.language, key.path],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )?;
        ensure!(
            hash == file.hash
                && bytes == file.text.as_bytes()
                && length == bytes.len() as i64
                && hash == hex::encode(Sha256::digest(&bytes))
                && context == crate::native_ids::extraction_context(&file.language, &[])?
                && producer_id == scope.producer_id
                && producer_version == crate::native_evidence::NATIVE_VERSION,
            "incompatible_index: reusable native source/context differs from capture"
        );
        let (executable_hash, kind, position_encoding): (String, String, String) = db.query_row(
            "SELECT executable_hash,kind,position_encoding FROM native_producers
                WHERE id=?1 AND version=?2",
            params![producer_id, producer_version],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let producer = Producer { id:producer_id,version:producer_version,executable_hash,
            kind,languages:db.prepare("SELECT language FROM native_producer_languages WHERE producer_id=?1 AND producer_version=?2 ORDER BY ordinal")?
                .query_map(params![scope.producer_id,crate::native_evidence::NATIVE_VERSION],|r|r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?,position_encoding };
        let source_set = SourceSet { id:key.source_set_id.clone(),root_id:self.root_id().to_owned(),
            languages:db.prepare("SELECT language FROM native_source_set_languages WHERE source_set_id=?1 ORDER BY ordinal")?
                .query_map([&key.source_set_id],|r|r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?,
            dependencies:db.prepare("SELECT dependency_id FROM native_source_set_dependencies WHERE source_set_id=?1 ORDER BY ordinal")?
                .query_map([&key.source_set_id],|r|r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()? };
        let (toolchain_hash,config_hash,dependency_hash):(String,String,String)=db.query_row(
            "SELECT toolchain_hash,config_hash,dependency_hash FROM native_revisions WHERE id=?1",
            [&selected.key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let document = Document {
            key: key.clone(),
            revision_id: scope.native_revision_id.clone(),
            content_hash: hash,
            byte_length: bytes.len(),
        };
        let revision = Revision {
            id: scope.native_revision_id.clone(),
            source_set_id: key.source_set_id.clone(),
            documents: vec![document.clone()],
            toolchain_hash,
            config_hash,
            dependency_hash,
        };
        let coverage = read_native_coverage_v8(db, &scope)?;
        let mut declarations = read_native_declarations_v8(db, &scope, None, true)?;
        // Normalized SQL keys are syntax-ID ordered. Projection expects the
        // producer's preorder (outer ranges and ancestors before children).
        declarations.sort_by_key(|d| {
            (
                d.range.start,
                std::cmp::Reverse(d.range.end),
                d.ancestors.len(),
            )
        });
        let calls = read_native_calls_v8(db, &scope, None)?;
        let control_regions = read_native_control_regions_v8(db, &scope, None)?;
        let observed = crate::native_evidence::document_witness(
            &document,
            &producer,
            &coverage,
            &declarations,
            &calls,
            &control_regions,
        )?;
        ensure!(
            observed == witness,
            "incompatible_index: reusable normalized native witness mismatch"
        );
        let provenance = Provenance {
            id: scope.proof_id(),
            producer_id: producer.id.clone(),
            document: key,
            revision_id: scope.native_revision_id,
            content_hash: file.hash.clone(),
            evidence_kind: "measuredSyntax".into(),
            basis: None,
            freshness: "fresh".into(),
            derived_from: None,
        };
        Ok(SelectedDocument {
            producer,
            source_set,
            revision,
            document,
            coverage,
            provenance,
            declarations,
            calls,
            control_regions,
        })
    }

    /// Produce a selectively assembled native revision only for a proved local edit
    /// (or unchanged captured sources). Other changes use the full native path.
    pub(crate) fn prepare_local_native(
        &self,
        capture: &crate::capture::Capture,
        expected: &RecoveryBaseline,
        cancel: &CancelFlag,
    ) -> Result<Option<crate::native_evidence::Artifact>> {
        use crate::{indexer::CapturedChange, native_evidence};
        if !expected.compatible
            || self.disposition() != RecoveryDisposition::Ready
            || !expected.pin().is_some_and(|p| p.index_revision > 0)
        {
            return Ok(None);
        }
        self.with_prior_publication_snapshot(|db| {
            let selected=ReadRevision::current(db)?;
            ensure!(expected.pin()==Some(selected.pin),
                "revision conflict: prior local snapshot changed");
            self.validate_recovery_decode_rows(db)?;
            let previous_options:crate::indexer::ReconcileOptions = db.query_row(
                "SELECT reconcile_options FROM index_metadata WHERE singleton=1",[],
                |r|r.get::<_,String>(0))?.parse::<serde_json::Value>()
                    .and_then(serde_json::from_value)?;
            let mut previous_inputs=BTreeMap::new();
            for row in db.prepare("SELECT input_key,payload FROM revision_capture_inputs WHERE revision_id=?1 ORDER BY input_key")?
                .query_map([&selected.key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))? {
                let (key,payload)=row?;
                ensure!(previous_inputs.insert(key,serde_json::from_str(&payload)?).is_none(),
                    "incompatible_index: duplicate prior capture input");
            }
            let mut previous=vec![];
            for row in db.prepare("SELECT d.path,d.language,d.content_hash,d.byte_length,d.source_bytes
                FROM revision_documents m JOIN document_versions d ON d.id=m.document_version_id
                WHERE m.revision_id=?1 ORDER BY m.ordinal")?
                .query_map([&selected.key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,
                    r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,Vec<u8>>(4)?)))? {
                use sha2::{Digest,Sha256};
                let (path,language,hash,length,bytes)=row?;
                ensure!(length>=0 && length as usize==bytes.len()
                    && bytes.len()<=256*1024*1024
                    && hex::encode(Sha256::digest(&bytes))==hash,
                    "incompatible_index: prior captured source witness mismatch");
                previous.push(SourceFile {path,language,hash,text:String::from_utf8(bytes)?});
            }
            let decision=crate::indexer::measure_persisted_change(
                &previous,&previous_options,&previous_inputs,capture)?;
            let selected_path=match decision {
                CapturedChange::FullNative {..} => return Ok(None),
                CapturedChange::DocumentLocal {path} => Some(path),
                CapturedChange::Unchanged => None,
            };
            let root=Path::new(&self.workspace_root);
            let mut measured=vec![];
            for file in &capture.files {
                if selected_path.as_deref()==Some(&file.path) {
                    // A previously failed/partial source cannot be transferred as a
                    // successful local proof, even if the changed token is body-only.
                    let previous_file=previous.iter().find(|old|old.path==file.path)
                        .context("prior local source absent")?;
                    let old=self.selected_reusable_native(db,&selected,previous_file).context("incompatible_index: prior native witness invalid")?;
                    if old.coverage.state!="complete" { return Ok(None); }
                    let changed=native_evidence::measure_captured_document(
                        capture,root,self.root_id(),&file.path,cancel, |_| {})?;
                    if changed.coverage.state!="complete" { return Ok(None); }
                    measured.push(changed);
                } else {
                    measured.push(self.selected_reusable_native(db,&selected,file).context("incompatible_index: prior native witness invalid")?);
                }
            }
            let artifact=native_evidence::assemble_selected_revision(
                capture,root,self.root_id(),measured,cancel)?;
            Ok(Some(artifact))
        })
    }

    fn verify_reusable_projection(
        &self,
        db: &Connection,
        ids: &V8DocumentProjection,
        reuse: ReusedFamilies,
        extraction: Option<&crate::classes::FileExtraction>,
    ) -> Result<()> {
        fn payloads<T: DeserializeOwned>(db: &Connection, sql: &str, id: &str) -> Result<Vec<T>> {
            let mut statement = db.prepare(sql)?;
            let mut result = vec![];
            for row in statement.query_map([id], |r| r.get::<_, String>(0))? {
                result.push(serde_json::from_str(&row?)?);
            }
            Ok(result)
        }
        if reuse.graph {
            let (version, hash, state, class_state, stored_f):
                (String,String,String,String,Option<String>) = db.query_row(
                "SELECT document_version_id,graph_hash,state,class_extraction_state,class_extraction_payload
                    FROM graph_projections WHERE id=?1",
                [&ids.graph_id],
                |r| Ok((r.get(0)?, r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
            )?;
            let expected_f = extraction.map(json).transpose()?;
            ensure!(
                state == "ready"
                    && stored_f == expected_f
                    && (if extraction.is_some() {
                        class_state == "ready"
                    } else {
                        class_state == "notApplicable"
                    }),
                "incompatible_index: reusable class extraction witness mismatch"
            );
            let nodes: Vec<Symbol> = payloads(
                db,
                "SELECT payload FROM graph_nodes WHERE projection_id=?1 ORDER BY id",
                &ids.graph_id,
            )?;
            let calls: Vec<CallSite> = payloads(
                db,
                "SELECT payload FROM graph_calls WHERE projection_id=?1 ORDER BY id",
                &ids.graph_id,
            )?;
            let regions: Vec<ControlRegion> = payloads(
                db,
                "SELECT payload FROM graph_regions WHERE projection_id=?1 ORDER BY id",
                &ids.graph_id,
            )?;
            let projected = v8_id(
                "",
                b"baleyg.graph-projection.v1\0",
                serde_json::json!({
                    "documentVersionId":version,"nodes":nodes,"calls":calls,"regions":regions
                }),
            );
            ensure!(
                version == ids.version_id
                    && hash == ids.graph_hash
                    && projected.trim_start_matches(':') == hash,
                "incompatible_index: reusable graph witness mismatch"
            );
        }
        if reuse.class {
            let (graph_id, hash): (String, String) = db.query_row(
                "SELECT graph_projection_id,content_hash FROM class_projections WHERE id=?1",
                [&ids.class_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let classes: Vec<crate::classes::ClassDefinition> = payloads(
                db,
                "SELECT payload FROM classes WHERE projection_id=?1 ORDER BY id",
                &ids.class_id,
            )?;
            let relations: Vec<crate::classes::ClassRelation> = payloads(
                db,
                "SELECT payload FROM class_relations WHERE projection_id=?1 ORDER BY id",
                &ids.class_id,
            )?;
            let projected = v8_id(
                "",
                b"baleyg.class-projection.v1\0",
                serde_json::json!({
                    "graphProjectionId":graph_id,"classes":classes,"relations":relations
                }),
            );
            ensure!(
                graph_id == ids.graph_id
                    && hash == ids.class_hash
                    && projected.trim_start_matches(':') == hash,
                "incompatible_index: reusable class witness mismatch"
            );
        }
        Ok(())
    }

    /// Authenticate candidates in a read snapshot before entering the writer lock.
    /// A missing or corrupt prior witness is never treated as permission to reuse.
    fn preflight_reuse(
        &self,
        graph: &Graph,
        native: &crate::native_evidence::Artifact,
        extractions: &BTreeMap<String, crate::classes::FileExtraction>,
        expected: &ExpectedPublication,
        rows: &BTreeMap<&str, PublicationRows<'_>>,
    ) -> Result<PreflightReuse> {
        if self.disposition() != RecoveryDisposition::Ready
            || !match expected {
                ExpectedPublication::Pin(pin) => pin.index_revision > 0,
                ExpectedPublication::Recovery(baseline) => {
                    baseline.compatible && baseline.pin().is_some_and(|p| p.index_revision > 0)
                }
            }
        {
            return Ok(PreflightReuse::default());
        }
        self.with_prior_publication_snapshot(|db| {
            let selected = ReadRevision::current(db)?;
            if !match expected {
                ExpectedPublication::Pin(pin) => *pin == selected.pin,
                ExpectedPublication::Recovery(baseline) => {
                    baseline.pin() == Some(selected.pin) && baseline.compatible
                }
            } || selected.pin.index_revision == 0
            {
                return Ok(PreflightReuse::default());
            }
            // Decode and verify the current snapshot outside the IMMEDIATE transaction.
            self.validate_recovery_decode_rows(db)?;
            let mut old = BTreeMap::new();
            let mut stmt = db.prepare(
                "SELECT path,document_version_id,graph_projection_id,class_projection_id
                FROM revision_documents WHERE revision_id=?1 ORDER BY path",
            )?;
            for row in stmt.query_map([&selected.key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })? {
                let (path, version, projection, class) = row?;
                ensure!(
                    old.insert(path, (version, projection, class)).is_none(),
                    "incompatible_index: duplicate prior manifest document"
                );
            }
            let mut families = BTreeMap::new();
            for file in &graph.files {
                if let Some((old_version, old_graph, old_class)) = old.get(&file.path) {
                    let grouped = rows
                        .get(file.path.as_str())
                        .context("publication rows missing")?;
                    let ids = v8_document_projection(file, native, grouped)?;
                    let reused = ReusedFamilies {
                        native: *old_version == ids.version_id,
                        graph: *old_graph == ids.graph_id,
                        class: old_class.as_deref() == Some(ids.class_id.as_str()),
                    };
                    ensure!(
                        !reused.graph || reused.native,
                        "incompatible_index: graph reused with different native version"
                    );
                    ensure!(
                        !reused.class || reused.graph,
                        "incompatible_index: class reused with different graph projection"
                    );
                    if reused.native || reused.graph || reused.class {
                        if reused.native {
                            self.selected_reusable_native(db, &selected, file)
                                .context("incompatible_index: prior native witness invalid")?;
                        }
                        // Projection hashes bind all decoded graph/class child rows.
                        self.verify_reusable_projection(
                            db,
                            &ids,
                            reused,
                            extractions.get(&file.path),
                        )?;
                        families.insert(file.path.clone(), reused);
                    }
                }
            }
            Ok(PreflightReuse {
                pin: Some(selected.pin),
                families,
            })
        })
    }
    fn publish_inner_checked_target(
        &self,
        bundle: (
            &Graph,
            &crate::capture::Capture,
            &crate::native_evidence::Artifact,
        ),
        leader: &topology::LeaderGuard,
        plan: PublicationPlan<'_>,
        cancel: &CancelFlag,
        max_graph_json_bytes: usize,
        mut during_tx: impl FnMut(PublishStage, &Connection) -> Result<()>,
    ) -> Result<IndexPin> {
        let PublicationPlan { expected, target } = plan;
        if let PublicationTarget::Live = target {
            self.ensure_not_recreate_pending()?;
        }
        let (graph, capture, native) = bundle;
        Self::enforce_selected_source_admission(graph, max_graph_json_bytes)?;
        ensure!(
            graph.schema_version == SCHEMA_VERSION,
            "unsupported graph schema"
        );
        check_cancel(cancel)?;
        let stats = validate_graph(graph, cancel)?;
        // Parse cached source before taking the writer lock. Projection and graph
        // still publish in one transaction with the same CAS/cancellation guard.
        let limits = crate::classes::Limits::default();
        let class_paths: BTreeSet<_> = graph
            .files
            .iter()
            .filter(|f| matches!(f.language.as_str(), "java" | "python"))
            .map(|f| f.path.as_str())
            .collect();
        // Class extraction only consumes class/method/function symbols. Supply
        // each file's symbols once instead of rescanning the full graph per file.
        let mut class_symbols: BTreeMap<&str, Vec<Symbol>> = BTreeMap::new();
        for node in &graph.nodes {
            if class_paths.contains(node.path.as_str())
                && matches!(
                    node.kind,
                    SymbolKind::Class | SymbolKind::Method | SymbolKind::Function
                )
            {
                class_symbols
                    .entry(node.path.as_str())
                    .or_default()
                    .push(node.clone());
            }
        }
        let extractions: BTreeMap<String, crate::classes::FileExtraction> = graph
            .files
            .iter()
            .filter(|f| matches!(f.language.as_str(), "java" | "python"))
            .map(|file| {
                Ok((
                    file.path.clone(),
                    crate::classes::FileExtraction::extract_file(
                        file,
                        class_symbols
                            .get(file.path.as_str())
                            .map(Vec::as_slice)
                            .unwrap_or(&[]),
                        cancel,
                        limits,
                    )?,
                ))
            })
            .collect::<Result<_>>()?;
        let classes = crate::classes::Catalog::compose(
            &extractions.values().cloned().collect::<Vec<_>>(),
            graph.files.len(),
            graph.nodes.len(),
            limits,
        )?;
        ensure!(
            classes.relations.iter().all(|r| r.target.is_none()
                && r.candidate_ids.is_empty()
                && r.match_kind == "unmatched"),
            "unsafe_index: lexical class relationship"
        );
        // The public read_status guard has this exact singleton JSON ceiling.
        // Refuse over-limit catalog warnings before paired CAS.
        ensure!(
            json(&classes.warnings)?.len() <= 256 * 1024,
            "incompatible_index: class catalog byte budget exceeded before publication"
        );
        let rows = publication_rows(graph, native, &classes)?;
        check_cancel(cancel)?;
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        self.identity.verify()?;
        let mut db = match target {
            PublicationTarget::Live => PublicationConnection::Live(
                self.cache_write()
                    .map_err(|error| self.report_live_read_failure(error))?,
            ),
            PublicationTarget::Stage(stage) => {
                leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
                stage.verify_path()?;
                let db = open_index(&stage.path, true)?;
                stage.verify_path()?;
                PublicationConnection::Stage(db)
            }
        };
        // Keep this exact SQLite connection open across preflight. data_version
        // values from different connections cannot fence a concurrent SQL rewrite.
        let admitted_version: i64 =
            storage_result(db.pragma_query_value(None, "data_version", |r| r.get(0)))?;
        let reuse = if matches!(target, PublicationTarget::Live) {
            self.preflight_reuse(graph, native, &extractions, &expected, &rows)?
        } else {
            PreflightReuse::default()
        };
        let after_preflight: i64 =
            storage_result(db.pragma_query_value(None, "data_version", |r| r.get(0)))?;
        ensure!(
            after_preflight == admitted_version,
            "incompatible_index: cache changed during reuse authentication"
        );
        during_tx(PublishStage::BeforeTransaction, &db)?;
        let tx = storage_result(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let current = match target {
            PublicationTarget::Live => self
                .recovery_baseline(&tx)
                .map_err(|error| self.report_live_read_failure(error))?,
            PublicationTarget::Stage(_) => self.recovery_baseline(&tx)?,
        };
        let locked_version: i64 =
            storage_result(tx.pragma_query_value(None, "data_version", |r| r.get(0)))?;
        ensure!(
            locked_version == admitted_version,
            "incompatible_index: cache changed after admission"
        );
        match &expected {
            ExpectedPublication::Pin(expected_pin) => ensure!(
                current.pin == Some(*expected_pin),
                "revision conflict: expected {expected_pin:?}, found {:?}",
                current.pin
            ),
            ExpectedPublication::Recovery(expected_baseline) => ensure!(
                current.witness == expected_baseline.witness
                    && current.pin == expected_baseline.pin,
                "revision conflict: private recovery baseline changed"
            ),
        }
        ensure!(
            reuse.pin.is_none() || reuse.pin == current.pin,
            "revision conflict: authenticated reuse snapshot changed"
        );
        let compatible = current.compatible;
        let old_pin = current.pin;
        let schema: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        // The compatible old snapshot was decoded under preflight's read snapshot;
        // the data_version and expected-pin CAS above bind it to this transaction.
        let decoded = compatible;
        if let PublicationTarget::Live = target {
            self.ensure_not_recreate_pending()?;
        }
        ensure!(
            schema == DATABASE_SCHEMA_VERSION,
            "incompatible_index: unsupported schema cannot be rebaselined"
        );
        let rebaseline = !compatible
            || !decoded
            || (matches!(target, PublicationTarget::Live)
                && self.disposition() == RecoveryDisposition::Rebuild);
        let prior_scan = (!rebaseline)
            .then(|| compare_capture_snapshot(&tx, capture))
            .transpose()?;
        if let Some(comparison) = prior_scan {
            ensure!(
                comparison.examined > 0,
                "incompatible_index: empty prior scan comparison"
            );
            let _changed = comparison.changed();
        }
        ensure!(
            graph.files.len() == native.revision.documents.len(),
            "graph/native document cardinality mismatch"
        );
        for file in &graph.files {
            ensure!(
                native
                    .revision
                    .documents
                    .iter()
                    .any(|doc| doc.key.path == file.path
                        && doc.key.language == file.language
                        && doc.content_hash == file.hash
                        && doc.byte_length == file.text.len()
                        && capture.files.iter().any(|source| source == file)),
                "graph/native captured source mismatch"
            );
        }
        let revision = IndexPin {
            index_generation: if rebaseline {
                uuid::Uuid::new_v4()
            } else {
                old_pin
                    .context("index_not_ready: prior index pin is not decodable")?
                    .index_generation
            },
            index_revision: if rebaseline {
                1
            } else {
                old_pin
                    .context("index_not_ready: prior index pin is not decodable")?
                    .index_revision
                    .checked_add(1)
                    .filter(|n| *n <= 9_007_199_254_740_991)
                    .context("revision overflow")?
            },
        };
        if rebaseline {
            // An incompatible/corrupt generation is explicitly replaced, never served.
            // Only compatible same-generation publication appends history.
            tx.execute_batch("DELETE FROM revision_documents; DELETE FROM revision_capture_inputs;
                DELETE FROM native_version_call_regions; DELETE FROM native_version_calls;
                DELETE FROM native_version_control_regions; DELETE FROM native_version_own_signature_types;
                DELETE FROM native_version_ancestor_signature_types; DELETE FROM native_version_parameters;
                DELETE FROM native_version_header_items; DELETE FROM native_version_headers;
                DELETE FROM native_version_declaration_ancestors; DELETE FROM native_version_declarations;
                DELETE FROM native_version_coverage_roles; DELETE FROM class_relations; DELETE FROM classes;
                DELETE FROM class_projections; DELETE FROM graph_calls; DELETE FROM graph_regions;
                DELETE FROM graph_nodes; DELETE FROM graph_projections; DELETE FROM document_versions;
                DELETE FROM native_revisions; DELETE FROM native_source_set_dependencies;
                DELETE FROM native_source_set_languages; DELETE FROM native_source_sets;
                DELETE FROM native_producer_inputs; DELETE FROM native_producer_languages;
                DELETE FROM native_producers;")?;
        }
        // The active manifest is full-rewrite, but old same-generation manifests
        // and their immutable version/projection trees remain for retained pins.
        let mut immutable = ImmutableAppend::default();
        let classify_immutable = |error| match target {
            PublicationTarget::Live => self.report_live_read_failure(error),
            PublicationTarget::Stage(_) => error,
        };
        let projections = write_native(
            &tx,
            (native, capture, graph, &classes, &extractions),
            revision,
            &stats,
            leader,
            cancel,
            &mut immutable,
            &reuse,
            &rows,
        )
        .map_err(&classify_immutable)?;
        for f in &graph.files {
            check_cancel(cancel)?;
            during_tx(PublishStage::AfterFile, &tx)?;
            let ids = projections
                .get(&f.path)
                .context("missing graph projection")?;
            let grouped = rows
                .get(f.path.as_str())
                .context("publication rows missing")?;
            for n in &grouped.nodes {
                if reuse.for_path(&f.path).graph {
                    continue;
                }
                immutable
                    .insert(
                        &tx,
                        "INSERT INTO graph_nodes VALUES(?1,?2,?3,?4,?5)",
                        params![ids.graph_id, n.id, n.name, n.path, json(n)?],
                    )
                    .map_err(&classify_immutable)?;
            }
            for c in &grouped.calls {
                if reuse.for_path(&f.path).graph {
                    continue;
                }
                immutable
                    .insert(
                        &tx,
                        "INSERT INTO graph_calls VALUES(?1,?2,?3,?4,?5,?6)",
                        params![
                            ids.graph_id,
                            c.id,
                            c.caller,
                            Option::<String>::None,
                            c.path,
                            json(c)?
                        ],
                    )
                    .map_err(&classify_immutable)?;
            }
            for r in &grouped.regions {
                if reuse.for_path(&f.path).graph {
                    continue;
                }
                immutable
                    .insert(
                        &tx,
                        "INSERT INTO graph_regions VALUES(?1,?2,?3,?4,?5)",
                        params![ids.graph_id, r.id, r.owner, r.path, json(r)?],
                    )
                    .map_err(&classify_immutable)?;
            }
            for class in &grouped.classes {
                if reuse.for_path(&f.path).class {
                    continue;
                }
                immutable
                    .insert(
                        &tx,
                        "INSERT INTO classes VALUES(?1,?2,?3,?4,?5,?6,?7)",
                        params![
                            ids.class_id,
                            ids.graph_id,
                            class.symbol.id,
                            class.symbol.name,
                            class.qualified_name,
                            class.symbol.path,
                            json(class)?,
                        ],
                    )
                    .map_err(&classify_immutable)?;
            }
            for relation in &grouped.relations {
                if reuse.for_path(&f.path).class {
                    continue;
                }
                immutable
                    .insert(
                        &tx,
                        "INSERT INTO class_relations VALUES(?1,?2,?3,?4,?5)",
                        params![
                            ids.class_id,
                            relation.id,
                            relation.owner,
                            relation.target,
                            json(relation)?,
                        ],
                    )
                    .map_err(&classify_immutable)?;
            }
        }
        immutable.finish(&tx).map_err(&classify_immutable)?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .to_string();
        tx.execute("UPDATE index_metadata SET schema_version=8,extractor_version=?1,index_generation=?2,index_revision=?3,indexed_at=?4,stats=?5,diagnostics=?6,reconciled_incarnation=?7,reconcile_options=?8 WHERE singleton=1",
            params![EXTRACTOR_VERSION, revision.index_generation.to_string(), revision.index_revision as i64,timestamp,json(&stats)?,json(&graph.diagnostics)?,leader.incarnation.to_string(),json(capture.reconcile_options())?])?;
        immutable.counters.record(
            "manifest",
            &[
                &EXTRACTOR_VERSION,
                &revision.index_generation.to_string(),
                &(revision.index_revision as i64),
                &timestamp,
                &json(&stats)?,
                &json(&graph.diagnostics)?,
                &leader.incarnation.to_string(),
                &json(capture.reconcile_options())?,
            ],
        )?;
        if rebaseline {
            tx.pragma_update(None, "user_version", DATABASE_SCHEMA_VERSION)?;
        }
        validate_paired_metadata(&tx, &self.identity.record_id)?;
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM revision_documents WHERE revision_id=?1",
            [format!(
                "pin:v1:{}:{}",
                revision.index_generation, revision.index_revision
            )],
            |r| r.get(0),
        )?;
        ensure!(
            count == graph.files.len() as i64,
            "incompatible_index: incomplete new revision manifest"
        );
        check_cancel(cancel)?;
        capture.verify(cancel)?;
        leader.verify()?;
        self.identity.verify()?;
        if let PublicationTarget::Stage(stage) = target {
            stage.verify_path()?;
            leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
        }
        during_tx(PublishStage::BeforeCommit, &tx)?;
        check_cancel(cancel)?;
        capture.verify(cancel)?;
        leader.verify()?;
        self.identity.verify()?;
        if let PublicationTarget::Stage(stage) = target {
            stage.verify_path()?;
            leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
        }
        storage_result(tx.commit())?;
        *self.writer_counters.lock().unwrap() = Some(immutable.counters);
        match target {
            PublicationTarget::Live => {
                self.recovery_disposition
                    .store(RecoveryDisposition::Ready as u8, Ordering::Release);
                self.recovery_required.store(false, Ordering::Release);
            }
            PublicationTarget::Stage(stage) => {
                stage.verify_path()?;
                leader.verify_exclusive_use(&self.roots.index_use_lock(&self.identity))?;
            }
        }
        Ok(revision)
    }

    /// A selected manifest is bound to the same admitted SQLite snapshot as its
    /// consumers. Only typed UUID/integer pin components enter these SQL literals;
    /// the caller never supplies SQL or a raw revision key.
    fn read_revision(&self, db: &Connection, expected: Option<IndexPin>) -> Result<ReadRevision> {
        let head = self.read_status(db)?.revision;
        let pin = expected.unwrap_or(head);
        ensure!(
            pin.index_generation == head.index_generation
                && pin.index_revision > 0
                && pin.index_revision <= head.index_revision,
            "revision conflict: foreign or missing native pin"
        );
        let key = format!("pin:v1:{}:{}", pin.index_generation, pin.index_revision);
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_revisions r WHERE r.id=?1 AND r.published_index_revision=?2
                AND NOT EXISTS (SELECT 1 FROM revision_capture_inputs i
                    WHERE i.revision_id=r.id AND i.input_key='__released:v1'))",
            params![key, pin.index_revision as i64],
            |r| r.get(0),
        )?;
        ensure!(exists, "revision conflict: released or missing native pin");
        Ok(ReadRevision { pin, key })
    }

    fn native_at<T>(
        &self,
        pin: IndexPin,
        read: impl FnOnce(&Connection, &ReadRevision) -> Result<T>,
    ) -> Result<T> {
        self.with_evidence(|db| {
            let selected = self.read_revision(db, Some(pin))?;
            read(db, &selected).map_err(|error| self.report_selected_failure(error))
        })
    }

    /// Remove one non-head manifest. Physical versions are collected separately;
    /// a crash between these commits can only leave unreferenced immutable rows.
    pub fn release_revision(&self, pin: IndexPin, leader: &topology::LeaderGuard) -> Result<()> {
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        self.identity.verify()?;
        self.with_evidence(|db| {
            let head = self.read_revision(db, None)?.pin;
            ensure!(pin != head, "revision conflict: cannot release head");
            self.read_revision(db, Some(pin))?;
            Ok(())
        })?;
        let mut db = self.cache_write()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let head = self.read_revision(&tx, None)?.pin;
        ensure!(pin != head, "revision conflict: cannot release head");
        let selected = self.read_revision(&tx, Some(pin))?;
        leader.verify()?;
        self.identity.verify()?;
        tx.execute(
            "DELETE FROM revision_documents WHERE revision_id=?1",
            [&selected.key],
        )?;
        tx.execute(
            "DELETE FROM revision_capture_inputs WHERE revision_id=?1",
            [&selected.key],
        )?;
        ensure!(
            tx.execute(
                "INSERT INTO revision_capture_inputs(revision_id,input_key,payload)
            SELECT id,'__released:v1','released:v1' FROM native_revisions
            WHERE id=?1 AND published_index_revision=?2",
                params![selected.key, pin.index_revision as i64]
            )? == 1,
            "revision conflict: retained header changed"
        );
        Self::check_retained_foreign_keys(&tx)?;
        leader.verify()?;
        self.identity.verify()?;
        tx.commit()?;
        Ok(())
    }

    fn check_retained_foreign_keys(db: &Connection) -> Result<()> {
        ensure!(
            db.prepare("PRAGMA foreign_key_check")?
                .query([])?
                .next()?
                .is_none(),
            "incompatible_index: retained reference violates foreign key"
        );
        Ok(())
    }

    /// Collect only trees with no references from ANY remaining manifest.
    /// All dependent rows and the FK check share the fenced immediate transaction.
    pub fn collect_unreferenced(&self, leader: &topology::LeaderGuard) -> Result<()> {
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        self.identity.verify()?;
        self.with_evidence(|db| {
            self.read_revision(db, None)?;
            Ok(())
        })?;
        let mut db = self.cache_write()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.read_revision(&tx, None)?;
        leader.verify()?;
        let unreferenced_class = "SELECT p.id FROM class_projections p WHERE NOT EXISTS
            (SELECT 1 FROM revision_documents m WHERE m.class_projection_id=p.id)";
        for table in ["class_relations", "classes"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE projection_id IN ({unreferenced_class})"),
                [],
            )?;
        }
        tx.execute(
            &format!("DELETE FROM class_projections WHERE id IN ({unreferenced_class})"),
            [],
        )?;
        let unreferenced_graph = "SELECT p.id FROM graph_projections p WHERE NOT EXISTS
            (SELECT 1 FROM revision_documents m WHERE m.graph_projection_id=p.id)";
        for table in ["graph_calls", "graph_regions", "graph_nodes"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE projection_id IN ({unreferenced_graph})"),
                [],
            )?;
        }
        tx.execute(
            &format!("DELETE FROM graph_projections WHERE id IN ({unreferenced_graph})"),
            [],
        )?;
        let unreferenced_version = "SELECT v.id FROM document_versions v WHERE NOT EXISTS
            (SELECT 1 FROM revision_documents m WHERE m.document_version_id=v.id)";
        for table in [
            "native_version_call_regions",
            "native_version_calls",
            "native_version_control_regions",
            "native_version_header_items",
            "native_version_parameters",
            "native_version_headers",
            "native_version_ancestor_signature_types",
            "native_version_own_signature_types",
            "native_version_declaration_ancestors",
            "native_version_declarations",
            "native_version_coverage_roles",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE version_id IN ({unreferenced_version})"),
                [],
            )?;
        }
        tx.execute(
            &format!("DELETE FROM document_versions WHERE id IN ({unreferenced_version})"),
            [],
        )?;
        Self::check_retained_foreign_keys(&tx)?;
        leader.verify()?;
        self.identity.verify()?;
        tx.commit()?;
        Ok(())
    }

    /// Reparse exactly one selected, paired source in this SQLite snapshot.
    /// This never rereads a workspace path or projects the whole graph.
    fn selected_native_witness_for(
        &self,
        db: &Connection,
        path: &str,
        selected: &ReadRevision,
    ) -> Result<crate::native_evidence::Artifact> {
        use crate::native_evidence::{Document, DocumentKey, Producer, Revision, SourceSet};
        let (source_set_id, revision_id): (String, String) = db
            .query_row(
                "SELECT m.source_set_id,r.native_revision_id FROM revision_documents m
                 JOIN native_revisions r ON r.id=m.revision_id
                 WHERE m.path=?1 AND m.revision_id=?2",
                params![path, selected.key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .context("incompatible_index: selected native document absent")?;
        ensure!(
            source_set_id == format!("source-set:v1:{}", self.root_id()),
            "incompatible_index: selected source set mismatch"
        );
        let file = Self::selected_source_row_bounded_for(db, path, 256 * 1024 * 1024, selected)?
            .context("incompatible_index: selected graph source absent")?;
        let mut producer:Producer=db.query_row(
            "SELECT id,version,executable_hash,kind,position_encoding FROM native_producers LIMIT 1",[],
            |r|Ok(Producer{id:r.get(0)?,version:r.get(1)?,executable_hash:r.get(2)?,kind:r.get(3)?,languages:vec![],position_encoding:r.get(4)?}),
        )?;
        producer.languages=db.prepare(
            "SELECT language FROM native_producer_languages WHERE producer_id=?1 AND producer_version=?2 ORDER BY ordinal"
        )?.query_map(params![producer.id,producer.version],|r|r.get::<_,String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut source_set: SourceSet = db.query_row(
            "SELECT id,root_id FROM native_source_sets WHERE id=?1",
            [&source_set_id],
            |r| {
                Ok(SourceSet {
                    id: r.get(0)?,
                    root_id: r.get(1)?,
                    languages: vec![],
                    dependencies: vec![],
                })
            },
        )?;
        source_set.languages=db.prepare(
            "SELECT language FROM native_source_set_languages WHERE source_set_id=?1 ORDER BY ordinal"
        )?.query_map([&source_set_id],|r|r.get::<_,String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        source_set.dependencies=db.prepare(
            "SELECT dependency_id FROM native_source_set_dependencies WHERE source_set_id=?1 ORDER BY ordinal"
        )?.query_map([&source_set_id],|r|r.get::<_,String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut revision:Revision=db.query_row(
            "SELECT native_revision_id,source_set_id,toolchain_hash,config_hash,dependency_hash FROM native_revisions WHERE native_revision_id=?1 AND id=?2",
            params![revision_id, selected.key],|r|Ok(Revision{id:r.get(0)?,source_set_id:r.get(1)?,
                documents:vec![],toolchain_hash:r.get(2)?,config_hash:r.get(3)?,dependency_hash:r.get(4)?}),
        )?;
        revision.documents.push(Document {
            key: DocumentKey {
                source_set_id,
                language: file.language.clone(),
                path: path.into(),
            },
            revision_id,
            content_hash: file.hash.clone(),
            byte_length: file.text.len(),
        });
        crate::native_evidence::selected_source_witness(&file, producer, source_set, revision)
            .context("incompatible_index: selected source extraction failed")
    }

    /// Authenticate one selected document's normalized records and graph DTOs
    /// against its paired BLOB in the same pinned SQLite transaction.
    // SQL aggregate touches only the selected path's ancillary native and class rows.
    // SourceFile JSON and graph node/call/region payloads have separate envelopes.
    fn selected_ancillary_byte_usage(
        db: &Connection,
        version_id: &str,
        class_id: &str,
    ) -> Result<(i64, i64)> {
        Ok(db.query_row(
            SELECTED_ANCILLARY_BYTE_SQL,
            params![version_id, class_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }
    fn attest_selected_document(&self, db: &Connection, path: &str) -> Result<()> {
        let selected = self.read_revision(db, None)?;
        self.attest_selected_document_for(db, path, &selected)
    }
    fn attest_selected_document_for(
        &self,
        db: &Connection,
        path: &str,
        selected: &ReadRevision,
    ) -> Result<()> {
        self.attest_selected_document_inner(db, path, selected)
            .map_err(selected_integrity)
            .map_err(|error| self.report_selected_failure(error))
    }
    fn attest_selected_document_inner(
        &self,
        db: &Connection,
        path: &str,
        selected: &ReadRevision,
    ) -> Result<()> {
        use crate::native_evidence::DocumentKey;
        let sql = "SELECT m.source_set_id,m.language,m.path,m.document_version_id,m.graph_projection_id,
            m.class_projection_id,d.content_hash,d.byte_length,length(d.source_bytes),
            g.graph_hash,g.state,g.class_extraction_state,length(CAST(g.class_extraction_payload AS BLOB)),
            c.content_hash,c.state,r.native_revision_id
            FROM revision_documents m JOIN native_revisions r ON r.id=m.revision_id
            JOIN document_versions d ON d.id=m.document_version_id
              AND d.source_set_id=m.source_set_id AND d.language=m.language AND d.path=m.path
            JOIN graph_projections g ON g.id=m.graph_projection_id
              AND g.document_version_id=d.id AND g.language=d.language
            LEFT JOIN class_projections c ON c.id=m.class_projection_id AND c.graph_projection_id=g.id
            WHERE m.path=?1 AND m.revision_id=?2 LIMIT 2";
        let mut stmt = db.prepare(sql)?;
        let rows = stmt
            .query_map(params![path, selected.key], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, String>(9)?,
                    r.get::<_, String>(10)?,
                    r.get::<_, String>(11)?,
                    r.get::<_, Option<i64>>(12)?,
                    r.get::<_, Option<String>>(13)?,
                    r.get::<_, Option<String>>(14)?,
                    r.get::<_, String>(15)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            rows.len() == 1,
            "incompatible_index: selected document missing or ambiguous"
        );
        let (
            source_set,
            language,
            stored_path,
            version_id,
            graph_id,
            class_id,
            hash,
            length,
            bytes,
            graph_hash,
            graph_state,
            class_state,
            f_length,
            class_hash,
            class_projection_state,
            native_revision_id,
        ) = rows.into_iter().next().unwrap();
        ensure!(
            stored_path == path
                && source_set == format!("source-set:v1:{}", self.root_id())
                && length == bytes
                && (0..=256 * 1024 * 1024).contains(&bytes)
                && [
                    &source_set,
                    &language,
                    &stored_path,
                    &version_id,
                    &graph_id,
                    &hash,
                    &graph_hash,
                    &native_revision_id
                ]
                .iter()
                .all(|value| value.len() <= 16 * 1024),
            "incompatible_index: selected source byte budget exceeded"
        );
        let class_id = class_id.context("incompatible_index: selected class projection missing")?;
        ensure!(
            graph_state == "ready"
                && class_projection_state.as_deref() == Some("ready")
                && class_hash.is_some()
                && class_id.len() <= 16 * 1024
                && (if matches!(language.as_str(), "java" | "python") {
                    f_length.is_some_and(|n| (0..=64 * 1024 * 1024).contains(&n))
                } else {
                    f_length.is_none()
                }),
            "incompatible_index: selected class projection invalid"
        );
        ensure!(
            matches!(
                (language.as_str(), class_state.as_str()),
                ("java" | "python", "ready") | ("javascript" | "rust", "notApplicable")
            ),
            "incompatible_index: class extraction state invalid"
        );
        let key = DocumentKey {
            source_set_id: source_set,
            language: language.clone(),
            path: path.into(),
        };
        let scope = v8_native_scope_for(db, &key, selected)?
            .context("incompatible_index: selected native version missing")?;
        ensure!(
            scope.version_id == version_id && scope.native_revision_id == native_revision_id,
            "incompatible_index: selected native manifest mismatch"
        );
        let row_limit = bytes.saturating_mul(16).clamp(1024, 1_000_000);
        let counts_sql = "SELECT (SELECT count(*) FROM graph_nodes WHERE projection_id=?1),
            (SELECT count(*) FROM graph_calls WHERE projection_id=?1),
            (SELECT count(*) FROM graph_regions WHERE projection_id=?1),
            (SELECT count(*) FROM classes WHERE projection_id=?2),
            (SELECT count(*) FROM class_relations WHERE projection_id=?2),
            (SELECT count(*) FROM native_version_coverage_roles WHERE version_id=?3),
            (SELECT count(*) FROM native_version_declarations WHERE version_id=?3),
            (SELECT count(*) FROM native_version_declaration_ancestors WHERE version_id=?3),
            (SELECT count(*) FROM native_version_own_signature_types WHERE version_id=?3),
            (SELECT count(*) FROM native_version_ancestor_signature_types WHERE version_id=?3),
            (SELECT count(*) FROM native_version_headers WHERE version_id=?3),
            (SELECT count(*) FROM native_version_header_items WHERE version_id=?3),
            (SELECT count(*) FROM native_version_parameters WHERE version_id=?3),
            (SELECT count(*) FROM native_version_calls WHERE version_id=?3),
            (SELECT count(*) FROM native_version_control_regions WHERE version_id=?3),
            (SELECT count(*) FROM native_version_call_regions WHERE version_id=?3)";
        let counts: Vec<i64> =
            db.query_row(counts_sql, params![graph_id, class_id, version_id], |r| {
                (0..16)
                    .map(|i| r.get(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?;
        ensure!(
            counts.iter().all(|&n| n >= 0 && n <= row_limit),
            "incompatible_index: selected evidence row budget exceeded"
        );
        let (max_row, total_bytes) =
            Self::selected_ancillary_byte_usage(db, &version_id, &class_id)?;
        ensure!(
            max_row >= 0
                && total_bytes >= 0
                && max_row
                    <= bytes
                        .saturating_mul(32)
                        .saturating_add(16 * 1024)
                        .min(256 * 1024 * 1024)
                && total_bytes
                    <= bytes
                        .saturating_mul(64)
                        .saturating_add(256 * 1024)
                        .min(512 * 1024 * 1024),
            "incompatible_index: selected evidence byte budget exceeded"
        );
        let graph_byte_sql="SELECT COALESCE(max(row_bytes),0),COALESCE(sum(row_bytes),0) FROM (
            SELECT length(CAST(id AS BLOB))+length(CAST(name AS BLOB))+length(CAST(path AS BLOB))+length(CAST(payload AS BLOB)) AS row_bytes FROM graph_nodes WHERE projection_id=?1
            UNION ALL SELECT length(CAST(id AS BLOB))+length(CAST(caller AS BLOB))+COALESCE(length(CAST(target AS BLOB)),0)+length(CAST(path AS BLOB))+length(CAST(payload AS BLOB)) FROM graph_calls WHERE projection_id=?1
            UNION ALL SELECT length(CAST(id AS BLOB))+length(CAST(owner AS BLOB))+length(CAST(path AS BLOB))+length(CAST(payload AS BLOB)) FROM graph_regions WHERE projection_id=?1)";
        let (graph_row, graph_total): (i64, i64) =
            db.query_row(graph_byte_sql, [&graph_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        ensure!(
            graph_row >= 0
                && graph_total >= 0
                && graph_row
                    <= bytes
                        .saturating_mul(6)
                        .saturating_add(16 * 1024)
                        .min(256 * 1024 * 1024 + 16 * 1024)
                && graph_total
                    <= bytes
                        .saturating_mul(64)
                        .saturating_add(256 * 1024)
                        .min(512 * 1024 * 1024),
            "incompatible_index: selected graph row byte budget exceeded"
        );
        let file = Self::selected_source_row_bounded_for(db, path, 256 * 1024 * 1024, selected)?
            .context("incompatible_index: selected source missing")?;
        ensure!(
            file.hash == hash && file.language == language && file.text.len() == bytes as usize,
            "incompatible_index: selected source identity mismatch"
        );
        let witness = self.selected_native_witness_for(db, path, selected)?;
        let coverage = read_native_coverage_v8(db, &scope)?;
        ensure!(
            witness.coverage == vec![coverage],
            "incompatible_index: selected native coverage differs from source"
        );
        let mut declarations = read_native_declarations_v8(db, &scope, None, true)?;
        let mut expected = witness.declarations.clone();
        declarations.sort_by(|a, b| a.syntax_id.cmp(&b.syntax_id));
        expected.sort_by(|a, b| a.syntax_id.cmp(&b.syntax_id));
        ensure!(
            declarations == expected,
            "incompatible_index: selected native declarations differ from source"
        );
        let mut owners: BTreeSet<_> = expected.iter().map(|d| d.syntax_id.clone()).collect();
        for table in ["native_version_calls", "native_version_control_regions"] {
            let query = format!("SELECT DISTINCT owner_syntax_id FROM {table} WHERE version_id=?1");
            for row in db
                .prepare(&query)?
                .query_map([&version_id], |r| r.get::<_, String>(0))?
            {
                owners.insert(row?);
            }
        }
        for owner in owners {
            let calls = read_native_calls_v8(db, &scope, Some(&owner))?;
            let mut expected_calls: Vec<_> = witness
                .calls
                .iter()
                .filter(|c| c.owner_syntax_id == owner)
                .cloned()
                .collect();
            expected_calls.sort_by_key(|c| c.ordinal);
            ensure!(
                calls == expected_calls,
                "incompatible_index: selected native calls differ from source"
            );
            let regions = read_native_control_regions_v8(db, &scope, Some(&owner))?;
            let mut expected_regions: Vec<_> = witness
                .control_regions
                .iter()
                .filter(|r| r.owner_syntax_id == owner)
                .cloned()
                .collect();
            expected_regions.sort_by_key(|r| r.ordinal);
            ensure!(
                regions == expected_regions,
                "incompatible_index: selected native regions differ from source"
            );
        }
        let mut nodes = Vec::new();
        for row in db
            .prepare(
                "SELECT id,name,path,payload FROM graph_nodes WHERE projection_id=?1 ORDER BY id",
            )?
            .query_map([&graph_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
        {
            let (id, name, path, payload) = row?;
            let node: Symbol = serde_json::from_str(&payload)?;
            ensure!(
                node.id == id && node.name == name && node.path == path && path == file.path,
                "incompatible_index: selected graph node materialization differs"
            );
            nodes.push(node);
        }
        let mut calls = Vec::new();
        for row in db.prepare("SELECT id,caller,target,path,payload FROM graph_calls WHERE projection_id=?1 ORDER BY id")?
            .query_map([&graph_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)))? {
            let (id,caller,target,path,payload)=row?;
            let call:CallSite=serde_json::from_str(&payload)?;
            ensure!(call.id==id && call.caller==caller && call.path==path && path==file.path && target.is_none(),
                "incompatible_index: selected graph call materialization differs");
            calls.push(call);
        }
        let mut regions = Vec::new();
        for row in db.prepare("SELECT id,owner,path,payload FROM graph_regions WHERE projection_id=?1 ORDER BY id")?
            .query_map([&graph_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))? {
            let (id,owner,path,payload)=row?;
            let region:ControlRegion=serde_json::from_str(&payload)?;
            ensure!(region.id==id && region.owner==owner && region.path==path && path==file.path,
                "incompatible_index: selected graph region materialization differs");
            regions.push(region);
        }
        let mut class_rows = Vec::new();
        for row in db.prepare("SELECT id,name,qualified_name,path,payload FROM classes WHERE projection_id=?1 ORDER BY id")?
            .query_map([&class_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)))? {
            let (id,name,qualified,path,payload)=row?;
            let class:crate::classes::ClassDefinition=serde_json::from_str(&payload)?;
            ensure!(class.symbol.id==id && class.symbol.name==name && class.symbol.path==path
                && class.qualified_name==qualified && path==file.path,
                "incompatible_index: selected class materialization differs");
            class_rows.push(class);
        }
        let mut class_relations = Vec::new();
        for row in db.prepare("SELECT id,owner,target,payload FROM class_relations WHERE projection_id=?1 ORDER BY id")?
            .query_map([&class_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?)))? {
            let (id,owner,target,payload)=row?;
            let relation:crate::classes::ClassRelation=serde_json::from_str(&payload)?;
            ensure!(relation.id==id && relation.owner==owner && relation.target==target,
                "incompatible_index: selected class relation materialization differs");
            class_relations.push(relation);
        }
        let mut graph = Graph {
            files: vec![file],
            nodes,
            calls,
            regions,
            ..Graph::default()
        };
        graph.stats.files = 1;
        graph.stats.symbols = graph.nodes.len();
        graph.stats.calls = graph.calls.len();
        graph.stats.regions = graph.regions.len();
        graph.stats.unresolved = graph.calls.len();
        for coverage in &witness.coverage {
            if coverage.state != "complete" {
                let recovered = coverage
                    .diagnostic
                    .as_deref()
                    .is_some_and(|d| d.contains("parser recovered"));
                if recovered {
                    graph.stats.parse_error_files += 1;
                }
                graph.diagnostics.push(Diagnostic {
                    path: Some(coverage.document_path.clone()),
                    code: if recovered {
                        "parse-error"
                    } else {
                        "native-coverage-partial"
                    }
                    .into(),
                    message: coverage
                        .diagnostic
                        .clone()
                        .unwrap_or_else(|| "Native extraction is incomplete".into()),
                });
            }
        }
        crate::indexer::validate_native_graph_records(
            &graph,
            &witness,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )?;
        let stored_f: Option<String> = db.query_row(
            "SELECT class_extraction_payload FROM graph_projections WHERE id=?1",
            [&graph_id],
            |r| r.get(0),
        )?;
        if matches!(language.as_str(), "java" | "python") {
            let expected_f = crate::classes::FileExtraction::extract_file(
                &graph.files[0],
                &graph.nodes,
                &Arc::new(std::sync::atomic::AtomicBool::new(false)),
                crate::classes::Limits::default(),
            )?;
            ensure!(
                stored_f
                    .as_deref()
                    .and_then(|f| serde_json::from_str::<crate::classes::FileExtraction>(f).ok())
                    .as_ref()
                    == Some(&expected_f),
                "incompatible_index: selected F differs from authenticated source and graph"
            );
        } else {
            ensure!(
                stored_f.is_none(),
                "incompatible_index: non-class graph carries F"
            );
        }
        let catalog = crate::classes::Catalog {
            classes: class_rows,
            relations: class_relations,
            warnings: vec![],
            truncated: false,
        };
        let rows = publication_rows(&graph, &witness, &catalog)?;
        let grouped = rows
            .get(graph.files[0].path.as_str())
            .context("selected rows missing")?;
        let ids = v8_document_projection(&graph.files[0], &witness, grouped)?;
        ensure!(
            ids.version_id == version_id
                && ids.graph_id == graph_id
                && ids.class_id == class_id
                && ids.graph_hash == graph_hash
                && Some(ids.class_hash) == class_hash,
            "incompatible_index: selected projection fingerprint mismatch"
        );
        Ok(())
    }

    /// Class DTOs are selected presentation projections; compare only this
    /// document's rows with a bounded in-memory class build from attested bytes.
    fn attest_selected_class_for(
        &self,
        db: &Connection,
        path: &str,
        selected: &ReadRevision,
    ) -> Result<()> {
        self.attest_selected_document_for(db, path, selected)?;
        let graph = self.read_graph_for(db, selected)?;
        let file = graph
            .files
            .iter()
            .find(|file| file.path == path)
            .context("incompatible_index: selected class source missing")?;
        if !matches!(file.language.as_str(), "java" | "python") {
            return Ok(());
        }
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let limits = crate::classes::Limits::default();
        let extracts = graph
            .files
            .iter()
            .filter(|f| matches!(f.language.as_str(), "java" | "python"))
            .map(|f| crate::classes::FileExtraction::extract_file(f, &graph.nodes, &cancel, limits))
            .collect::<Result<Vec<_>>>()?;
        let selected_extraction = extracts
            .iter()
            .find(|f| f.path == path)
            .context("incompatible_index: selected class extraction missing")?;
        let stored: String = db.query_row(
            "SELECT g.class_extraction_payload FROM revision_documents m
             JOIN graph_projections g ON g.id=m.graph_projection_id
             WHERE m.revision_id=?2 AND m.path=?1",
            params![path, selected.key],
            |r| r.get(0),
        )?;
        ensure!(
            serde_json::from_str::<crate::classes::FileExtraction>(&stored)?
                == *selected_extraction,
            "incompatible_index: selected class F differs from authenticated source and graph"
        );
        let catalog = crate::classes::Catalog::compose(
            &extracts,
            graph.files.len(),
            graph.nodes.len(),
            limits,
        )?;
        let class_id: String = db.query_row(
            "SELECT m.class_projection_id FROM revision_documents m
             WHERE m.revision_id=?2 AND m.path=?1",
            params![path, selected.key],
            |r| r.get(0),
        )?;
        let mut expected: Vec<_> = catalog
            .classes
            .into_iter()
            .filter(|c| c.symbol.path == path)
            .collect();
        let mut actual: Vec<crate::classes::ClassDefinition> = db
            .prepare("SELECT payload FROM classes WHERE projection_id=?1 ORDER BY id")?
            .query_map([&class_id], |r| r.get::<_, String>(0))?
            .map(|payload| Ok(serde_json::from_str(&payload?)?))
            .collect::<Result<_>>()?;
        expected.sort_by(|a, b| a.symbol.id.cmp(&b.symbol.id));
        actual.sort_by(|a, b| a.symbol.id.cmp(&b.symbol.id));
        ensure!(
            actual == expected,
            "incompatible_index: selected class projection differs from source"
        );
        let mut expected_relations: Vec<_> = catalog
            .relations
            .into_iter()
            .filter(|r| r.path == path)
            .collect();
        let mut actual_relations: Vec<crate::classes::ClassRelation> = db
            .prepare("SELECT payload FROM class_relations WHERE projection_id=?1 ORDER BY id")?
            .query_map([&class_id], |r| r.get::<_, String>(0))?
            .map(|payload| Ok(serde_json::from_str(&payload?)?))
            .collect::<Result<_>>()?;
        expected_relations.sort_by(|a, b| a.id.cmp(&b.id));
        actual_relations.sort_by(|a, b| a.id.cmp(&b.id));
        ensure!(
            actual_relations == expected_relations,
            "incompatible_index: selected class relationships differ from source"
        );
        Ok(())
    }

    pub fn native_declarations_at(
        &self,
        pin: IndexPin,
        language: &str,
        lookup_key: &str,
    ) -> Result<Vec<crate::native_evidence::Declaration>> {
        self.native_at(pin, |db, selected| {
            let mut paths=db.prepare(
                "SELECT DISTINCT m.path FROM native_version_declarations d JOIN revision_documents m
                 ON m.revision_id=?3 AND m.document_version_id=d.version_id
                 WHERE m.language=?1 AND d.lookup_key=?2 ORDER BY m.path LIMIT 5001"
            )?.query_map(params![language,lookup_key,selected.key],|r|r.get::<_,String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ensure!(
                paths.len() <= 5000,
                "incompatible_index: selected lookup document budget exceeded"
            );
            for path in paths.drain(..) {
                self.attest_selected_document_for(db, &path, selected)?;
            }
            Self::read_native_declarations_for(
                db,
                language,
                Some(lookup_key),
                None,
                false,
                selected,
            )
        })
    }
    fn read_native_declarations_for(
        db: &Connection,
        language: &str,
        lookup_key: Option<&str>,
        selected_path: Option<&str>,
        all_lookup_keys: bool,
        selected: &ReadRevision,
    ) -> Result<Vec<crate::native_evidence::Declaration>> {
        use crate::native_evidence::DocumentKey;
        let mut stmt=db.prepare("SELECT DISTINCT m.source_set_id,m.language,m.path
            FROM revision_documents m JOIN native_version_declarations d ON d.version_id=m.document_version_id
            WHERE m.revision_id=?5 AND m.language=?1
              AND (?4 OR ((?2 IS NULL AND d.lookup_key IS NULL) OR d.lookup_key=?2))
              AND (?3 IS NULL OR m.path=?3) ORDER BY m.path LIMIT 5001")?;
        let keys: Vec<DocumentKey> = stmt
            .query_map(
                params![
                    language,
                    lookup_key,
                    selected_path,
                    all_lookup_keys,
                    selected.key
                ],
                |r| {
                    Ok(DocumentKey {
                        source_set_id: r.get(0)?,
                        language: r.get(1)?,
                        path: r.get(2)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<_>>()?;
        ensure!(
            keys.len() <= 5000,
            "incompatible_index: selected native lookup document budget exceeded"
        );
        let mut declarations = vec![];
        for key in keys {
            let scope = v8_native_scope_for(db, &key, selected)?
                .context("incompatible_index: lookup manifest missing")?;
            declarations.extend(read_native_declarations_v8(
                db,
                &scope,
                lookup_key,
                all_lookup_keys,
            )?);
        }
        declarations.sort_by(|a, b| a.syntax_id.cmp(&b.syntax_id));
        Ok(declarations)
    }

    pub fn native_source_at(
        &self,
        pin: IndexPin,
        key: &crate::native_evidence::DocumentKey,
    ) -> Result<Option<(crate::native_evidence::Document, Vec<u8>)>> {
        use crate::native_evidence::Document;
        self.native_at(pin, |db, selected| {
            let Some(scope) = v8_native_scope_for(db, key, selected)? else {
                let path_exists: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM revision_documents WHERE revision_id=?2 AND path=?1)",
                    params![key.path, selected.key],
                    |r| r.get(0),
                )?;
                ensure!(
                    !path_exists,
                    "incompatible_index: selected native source absent"
                );
                return Ok(None);
            };
            self.attest_selected_document_for(db, &key.path, selected)?;
            let source = self
                .selected_source_row_for(db, &key.path, selected)?
                .context("incompatible_index: paired graph source missing")?;
            ensure!(
                source.path == key.path && source.language == key.language,
                "incompatible_index: native source identity differs from manifest"
            );
            let bytes = source.text.into_bytes();
            let document = Document {
                key: key.clone(),
                revision_id: scope.native_revision_id,
                content_hash: source.hash,
                byte_length: bytes.len(),
            };
            Ok(Some((document, bytes)))
        })
    }

    pub fn native_coverage_at(
        &self,
        pin: IndexPin,
        key: &crate::native_evidence::DocumentKey,
    ) -> Result<Option<crate::native_evidence::Coverage>> {
        self.native_at(pin, |db, selected| {
            let scope = v8_native_scope_for(db, key, selected)?;
            if scope.is_some() {
                self.attest_selected_document_for(db, &key.path, selected)?;
            }
            Self::read_native_coverage_for(db, key, selected)
        })
    }
    fn read_native_coverage_for(
        db: &Connection,
        key: &crate::native_evidence::DocumentKey,
        selected: &ReadRevision,
    ) -> Result<Option<crate::native_evidence::Coverage>> {
        v8_native_scope_for(db, key, selected)?
            .map(|scope| read_native_coverage_v8(db, &scope))
            .transpose()
    }

    pub fn native_calls_at(
        &self,
        pin: IndexPin,
        owner: &str,
    ) -> Result<Vec<crate::native_evidence::Call>> {
        self.native_at(pin, |db, selected| {
            if let Some(scope) = v8_owner_scope_for(db, owner, selected)? {
                self.attest_selected_document_for(db, &scope.key.path, selected)?;
                read_native_calls_v8(db, &scope, Some(owner))
            } else {
                let dangling: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM native_version_calls c JOIN revision_documents m ON m.document_version_id=c.version_id AND m.revision_id=?2 WHERE c.owner_syntax_id=?1)",
                    params![owner, selected.key],
                    |r| r.get(0),
                )?;
                ensure!(!dangling, "incompatible_index: native owner absent");
                Ok(vec![])
            }
        })
    }
    pub fn native_control_regions_at(
        &self,
        pin: IndexPin,
        owner: &str,
    ) -> Result<Vec<crate::native_evidence::ControlRegion>> {
        self.native_at(pin, |db, selected| {
            if let Some(scope)=v8_owner_scope_for(db,owner,selected)? {
                self.attest_selected_document_for(db,&scope.key.path,selected)?;
                read_native_control_regions_v8(db,&scope,Some(owner))
            } else {
                let dangling:bool=db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM native_version_control_regions c JOIN revision_documents m ON m.document_version_id=c.version_id AND m.revision_id=?2 WHERE c.owner_syntax_id=?1)",
                    params![owner, selected.key],|r|r.get(0),
                )?;
                ensure!(!dangling,"incompatible_index: native owner absent");
                Ok(vec![])
            }
        })
    }
    /// Search only the persisted projection. Wildcards are literal user text.
    /// One read snapshot and revision guard; no source reads or catalog rebuilds.
    pub fn navigation_at(
        &self,
        request: &crate::navigation::NavigationRequest,
    ) -> Result<crate::navigation::NavigationResult> {
        request.validate()?;
        self.with_evidence(|tx| {
        let selected = self.read_revision(tx, Some(request.expected_revision()))?;
        let revision = selected.pin;
        // Navigation's source selector counts lines from the stored source BLOB,
        // and its member selector reads versioned class/node rows. Before either
        // consumes a selected document, authenticate it against that source BLOB
        // in this same read transaction. Never scan the entire workspace here.
        let selected_path: Option<String> = match request {
            crate::navigation::NavigationRequest::Source(s) => Some(s.path.clone()),
            crate::navigation::NavigationRequest::Member(s) => {
                let selected: Option<(String, bool, Option<String>)> = tx
                    .query_row(
                        "SELECT n.path,json_valid(n.payload),CASE WHEN json_valid(n.payload) THEN json_extract(n.payload,'$.kind') ELSE NULL END FROM graph_nodes n JOIN revision_documents m ON m.revision_id=?2 AND m.graph_projection_id=n.projection_id WHERE n.id=?1 LIMIT 1",
                        params![s.class_id, selected.key],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(|error| self.report_selected_failure(error.into()))?;
                match selected {
                    Some((_path, false, _)) => {
                        return Err(self.report_selected_failure(
                            SelectedIntegrity(
                                "incompatible_index: selected navigation node JSON invalid".into(),
                            )
                            .into(),
                        ));
                    }
                    Some((path, true, Some(kind))) if kind == "class" => Some(path),
                    _ => None,
                }
            }
        };
        if let Some(path) = selected_path {
            // Gate allocation of the selected JSON/BLOB before decoding either.
            // This reads only SQLite byte lengths, not every workspace document.
            let sizes: Option<(i64, i64)> = tx.query_row(
                "SELECT length(d.source_bytes),length(d.source_bytes) FROM revision_documents m
                 JOIN document_versions d ON d.id=m.document_version_id
                 WHERE m.revision_id=?2 AND m.path=?1",
                params![path, selected.key], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            if let Some((graph_len, native_len)) = sizes {
                if graph_len > 2 * 1024 * 1024 || native_len > 2 * 1024 * 1024 {
                    return Err(self.report_selected_failure(
                        SelectedIntegrity(
                            "incompatible_index: selected navigation source exceeds budget".into(),
                        )
                        .into(),
                    ));
                }
                self.attest_selected_document_for(tx, &path, &selected)?;
            } else {
                let graph_file: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM revision_documents WHERE revision_id=?2 AND path=?1)",
                    params![path, selected.key],
                    |row| row.get(0),
                )?;
                if graph_file {
                    return Err(self.report_selected_failure(
                        SelectedIntegrity(
                            "incompatible_index: selected native document missing".into(),
                        )
                        .into(),
                    ));
                }
            }
        }
        crate::navigation::navigate(tx, request, revision, &selected.key)
            })
    }

    pub fn classes_at(
        &self,
        path: Option<&str>,
        query: &str,
        expected: Option<IndexPin>,
        offset: usize,
        limit: usize,
    ) -> Result<crate::class_diagram::ClassPage> {
        use crate::class_diagram::{ClassPage, InvalidRequest};
        let path = path.filter(|path| !path.is_empty());
        ensure!(
            (1..=100).contains(&limit)
                && offset <= 1_000_000
                && query.len() <= 512
                && !query.contains('\0'),
            InvalidRequest("Class search allows 1–100 results and a query of at most 512 bytes.")
        );
        if let Some(path) = path {
            ensure!(
                !path.is_empty()
                    && path.len() <= 8192
                    && !path.contains(['\0', '\\', ':'])
                    && !path
                        .split('/')
                        .any(|part| part.is_empty() || part == "." || part == ".."),
                InvalidRequest("Choose a workspace-relative class source path.")
            );
        }
        self.with_evidence(|tx| {
        let selected_revision = self.read_revision(tx, expected)?;
        let revision = selected_revision.pin;
        let (mut warnings, truncated) = class_metadata(tx, &selected_revision)
            .map_err(|error| self.report_selected_failure(selected_integrity(error)))?;
        let pattern = format!(
            "%{}%",
            query
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut stmt = tx.prepare("SELECT c.id FROM classes c JOIN revision_documents m ON m.revision_id=?6 AND m.class_projection_id=c.projection_id WHERE (?1 IS NULL OR c.path=?1)
            AND (c.name LIKE ?2 ESCAPE '\\' OR c.qualified_name LIKE ?2 ESCAPE '\\' OR c.id=?3)
            ORDER BY CASE WHEN lower(c.name)=lower(?3) THEN 0 ELSE 1 END,c.qualified_name,c.path,c.id LIMIT ?4 OFFSET ?5")?;
        let ids = stmt
            .query_map(
                params![path, pattern, query, (limit + 1) as i64, offset as i64, selected_revision.key],
                |r| r.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| self.report_selected_failure(error.into()))?;
        let mut items = Vec::new();
        let mut consumed = 0;
        let mut bytes = 0;
        let mut byte_limited = false;
        for id in ids.iter().take(limit) {
            let Some((class, size, clipped)) = presentation_class(self, tx, id, &selected_revision)? else {
                // Consume an individually oversized row so pagination always progresses.
                consumed += 1;
                byte_limited = true;
                continue;
            };
            if bytes + size > crate::class_diagram::PAGE_BYTES {
                byte_limited = true;
                break;
            }
            bytes += size;
            consumed += 1;
            byte_limited |= clipped;
            items.push(class);
        }
        let next_offset = (consumed < ids.len()).then_some(offset + consumed);
        if byte_limited {
            warnings.push(crate::class_diagram::BYTE_NOTICE.into());
        }
        let truncated = truncated || byte_limited;
        if let Some(path) = path
            && !path.ends_with(".java")
            && !path.ends_with(".py")
        {
            warnings
                .push("Class diagrams currently support Java and Python declarations only.".into());
        }
        let mut paths: BTreeSet<_> = items
            .iter()
            .map(|class| class.symbol.path.as_str())
            .collect();
        if let Some(selected) = path
            && matches!(selected.rsplit('.').next(), Some("java" | "py"))
        {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM revision_documents WHERE revision_id=?2 AND path=?1)",
                params![selected, selected_revision.key], |r| r.get(0),
            )?;
            if exists { paths.insert(selected); }
        }
        for path in paths {
            self.attest_selected_class_for(tx, path, &selected_revision)?;
        }
        Ok(ClassPage {
            revision,
            items,
            next_offset,
            truncated,
            warnings,
            require_index: false,
        })
            })
    }
    /// Bounded one-hop relation reads and seed resolution share a revision-pinned
    /// read transaction. No filesystem access or graph/provider augmentation.
    pub fn class_diagram_at(
        &self,
        request: &crate::class_diagram::ClassDiagramRequest,
    ) -> Result<crate::class_diagram::ClassDiagram> {
        use crate::class_diagram::{self, InvalidRequest};
        request.validate()?;
        self.with_evidence(|tx| {
            let selected = self.read_revision(tx, Some(request.expected_revision))?;
            let revision = selected.pin;
            let (warnings, truncated) = class_metadata(tx, &selected)
                .map_err(|error| self.report_selected_failure(selected_integrity(error)))?;
            let (seed, clipped) = resolve_class(self, tx, &request.seed, &selected)?;
            // Explicitly selected measured declarations are independent roots, never
            // connected by lexical type-name matches or candidate relationships.
            let mut seeds = vec![seed.symbol.id.clone()];
            let mut classes = BTreeMap::from([(seed.symbol.id.clone(), seed)]);
            for expanded in &request.expanded {
                let (class, _) = resolve_class(self, tx, expanded, &selected)?;
                if !classes.contains_key(&class.symbol.id) {
                    ensure!(
                        classes.len() < class_diagram::MAX_NODES,
                        InvalidRequest("Too many selected classes.")
                    );
                    seeds.push(class.symbol.id.clone());
                    classes.insert(class.symbol.id.clone(), class);
                }
            }
            let paths: BTreeSet<_> = classes
                .values()
                .map(|class| class.symbol.path.as_str())
                .collect();
            for path in paths {
                self.attest_selected_class_for(tx, path, &selected)?;
            }
            class_diagram::project(
                revision,
                &seeds,
                &classes,
                vec![],
                vec![],
                warnings,
                truncated || clipped,
            )
        })
    }
    pub fn symbols(&self, query: &str, limit: usize) -> Result<Vec<Symbol>> {
        Ok(self.symbols_at(query, limit)?.1)
    }
    pub fn symbols_at(&self, query: &str, limit: usize) -> Result<(IndexPin, Vec<Symbol>)> {
        ensure!(query.len() <= 8192, "search query too long");
        self.with_evidence(|tx| {
        let revision = self.read_status(tx)?.revision;
        let selected_paths: BTreeSet<String> = tx.prepare("SELECT n.path FROM graph_nodes n JOIN revision_documents m ON m.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND m.graph_projection_id=n.projection_id WHERE instr(lower(n.name),lower(?1)) > 0 OR instr(lower(n.id),lower(?1)) > 0 ORDER BY CASE WHEN lower(n.name)=lower(?1) THEN 0 WHEN instr(lower(n.name),lower(?1))=1 THEN 1 ELSE 2 END,n.name,n.id LIMIT ?2")?
            .query_map(params![query,limit.min(150) as i64],|r|r.get::<_,String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        for path in selected_paths {
            self.attest_selected_document(tx, &path)?;
        }
        let mut stmt = tx.prepare("SELECT n.payload FROM graph_nodes n JOIN revision_documents m ON m.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND m.graph_projection_id=n.projection_id WHERE instr(lower(n.name),lower(?1)) > 0 OR instr(lower(n.id),lower(?1)) > 0 ORDER BY CASE WHEN lower(n.name)=lower(?1) THEN 0 WHEN instr(lower(n.name),lower(?1))=1 THEN 1 ELSE 2 END,n.name,n.id LIMIT ?2")?;
        let values = stmt.query_map(params![query, limit.min(150) as i64], |r| {
            r.get::<_, String>(0)
        })?;
        let values = values
            .map(|v| Ok(serde_json::from_str(&v?)?))
            .collect::<Result<Vec<Symbol>>>()?;
        let paths: BTreeSet<_> = values.iter().map(|node| node.path.as_str()).collect();
        for path in paths {
            self.attest_selected_document(tx, path)?;
        }
        Ok((revision, values))
            })
    }
    pub fn symbol(&self, id: &str) -> Result<Option<Symbol>> {
        Ok(self.symbol_at(id, None)?.map(|(_, v)| v))
    }
    pub fn source(&self, path: &str) -> Result<Option<SourceFile>> {
        Ok(self.source_at(path, None)?.map(|(_, v)| v))
    }
    pub fn symbol_at(
        &self,
        id: &str,
        expected_revision: Option<IndexPin>,
    ) -> Result<Option<(IndexPin, Symbol)>> {
        self.with_evidence(|tx| {
            let selected = self.read_revision(tx, expected_revision)?;
            let revision = selected.pin;
            let selected_path: Option<String> = tx
                .query_row("SELECT n.path FROM graph_nodes n JOIN revision_documents m ON m.revision_id=?2 AND m.graph_projection_id=n.projection_id WHERE n.id=?1", params![id, selected.key], |r| r.get(0))
                .optional()?;
            if let Some(path) = selected_path {
                self.attest_selected_document_for(tx, &path, &selected)?;
            }
            let node: Option<Symbol> = one_at(tx, "SELECT n.payload FROM graph_nodes n JOIN revision_documents m ON m.revision_id=?2 AND m.graph_projection_id=n.projection_id WHERE n.id=?1", id, &selected.key)?;
            Ok(node.map(|node| (revision, node)))
        })
    }
    fn selected_source_row_for(
        &self,
        db: &Connection,
        path: &str,
        selected: &ReadRevision,
    ) -> Result<Option<SourceFile>> {
        Self::selected_source_row_bounded_for(db, path, 256 * 1024 * 1024, selected)
            .map_err(selected_integrity)
            .map_err(|error| self.report_selected_failure(error))
    }
    #[cfg(test)]
    fn selected_source_row_bounded(
        db: &Connection,
        path: &str,
        max_bytes: i64,
    ) -> Result<Option<SourceFile>> {
        let selected = ReadRevision::current(db)?;
        Self::selected_source_row_bounded_for(db, path, max_bytes, &selected)
    }
    fn selected_source_row_bounded_for(
        db: &Connection,
        path: &str,
        max_bytes: i64,
        selected: &ReadRevision,
    ) -> Result<Option<SourceFile>> {
        let selected_sql = "FROM revision_documents m JOIN document_versions d
            ON d.id=m.document_version_id AND d.source_set_id=m.source_set_id
              AND d.language=m.language AND d.path=m.path
            WHERE m.revision_id=?2 AND m.path=?1";
        let sql = format!(
            "SELECT length(d.source_bytes),length(CAST(d.source_set_id AS BLOB)),length(CAST(d.language AS BLOB)),length(CAST(d.path AS BLOB)),length(CAST(d.content_hash AS BLOB)) {selected_sql} LIMIT 2"
        );
        let mut stmt = db.prepare(&sql)?;
        let mut sizes = stmt.query(params![path, selected.key])?;
        let mut present = false;
        while let Some(row) = sizes.next()? {
            ensure!(!present, "incompatible_index: ambiguous manifest source");
            present = true;
            let bytes: i64 = row.get(0)?;
            ensure!(
                bytes >= 0
                    && bytes <= max_bytes
                    && (1usize..5).all(|i| -> bool {
                        row.get::<_, i64>(i)
                            .is_ok_and(|length| (0..=16 * 1024).contains(&length))
                    }),
                "incompatible_index: selected source byte budget exceeded"
            );
        }
        if !present {
            return Ok(None);
        }
        let sql = format!(
            "SELECT d.source_set_id,d.language,d.path,d.content_hash,d.byte_length,d.source_bytes {selected_sql}"
        );
        let checked: Result<(String, String, String, String, Vec<u8>)> =
            db.query_row(&sql, params![path, selected.key], |r| {
                Ok((|| -> Result<_> {
                    use sha2::{Digest, Sha256};
                    let source_set: String = r.get(0)?;
                    let language: String = r.get(1)?;
                    let stored_path: String = r.get(2)?;
                    let hash: String = r.get(3)?;
                    let length: i64 = r.get(4)?;
                    let raw = r.get_ref(5)?.as_blob()?;
                    ensure!(
                        source_set.starts_with("source-set:v1:")
                            && stored_path == path
                            && length == raw.len() as i64
                            && hash == hex::encode(Sha256::digest(raw)),
                        "incompatible_index: source hash mismatch"
                    );
                    Ok((source_set, language, stored_path, hash, raw.to_vec()))
                })())
            })?;
        let (_source_set, language, stored_path, hash, bytes) = checked?;
        let file = SourceFile {
            path: stored_path,
            hash,
            language,
            text: String::from_utf8(bytes)?,
        };
        let mut budget = EncodedSourceBudget {
            bytes: 0,
            max_bytes: usize::try_from(max_bytes)?.saturating_add(16 * 1024),
        };
        serde_json::to_writer(&mut budget, &file)?;
        Ok(Some(file))
    }
    pub fn source_at(
        &self,
        path: &str,
        expected_revision: Option<IndexPin>,
    ) -> Result<Option<(IndexPin, SourceFile)>> {
        self.with_evidence(|tx| {
            let selected = self.read_revision(tx, expected_revision)?;
            let source = self.selected_source_row_for(tx, path, &selected)?;
            if source.is_some() {
                self.attest_selected_document_for(tx, path, &selected)?;
            }
            Ok(source.map(|file| (selected.pin, file)))
        })
    }
    /// Catalog reads pin revision and rows to one SQLite read transaction.
    /// Enrich only the visible tree page from one cached index snapshot.
    pub fn tree_metadata(
        &self,
        root: &Path,
        items: &mut [crate::file_tree::Entry],
    ) -> Result<(IndexPin, String)> {
        self.tree_metadata_with_finish_hook(root, items, || Ok(()))
    }
    fn tree_metadata_with_finish_hook(
        &self,
        root: &Path,
        items: &mut [crate::file_tree::Entry],
        before_finish: impl FnOnce() -> Result<()>,
    ) -> Result<(IndexPin, String)> {
        let (revision, workspace_root, overlays) = self.with_evidence_hook(|tx| {
            let revision = self.read_status(tx)?.revision;
            let workspace = Path::new(&self.workspace_root);
            let mut valid_stmt = tx.prepare(
                "SELECT NOT EXISTS(SELECT 1 FROM graph_nodes n WHERE n.projection_id=m.graph_projection_id AND json_valid(n.payload)=0) FROM revision_documents m JOIN index_metadata active_manifest ON active_manifest.singleton=1 AND m.revision_id='pin:v1:'||active_manifest.index_generation||':'||active_manifest.index_revision WHERE m.path=?1",
            )?;
            let mut count_stmt = tx.prepare(
                "SELECT (SELECT count(*) FROM graph_nodes n WHERE n.projection_id=m.graph_projection_id AND CASE WHEN json_valid(n.payload) THEN json_extract(n.payload,'$.kind') IN ('function','method') ELSE 0 END) FROM revision_documents m JOIN index_metadata active_manifest ON active_manifest.singleton=1 AND m.revision_id='pin:v1:'||active_manifest.index_generation||':'||active_manifest.index_revision WHERE m.path=?1",
            )?;
            let mut overlays = Vec::new();
            for (index, item) in items.iter().enumerate().filter(|(_, entry)| entry.kind == "file") {
                let absolute = root.join(&item.path);
                let Ok(relative) = absolute.strip_prefix(workspace) else { continue };
                let Some(relative) = relative.to_str() else { continue };
                let valid: Option<bool> = valid_stmt
                    .query_row([relative], |row| row.get(0))
                    .optional()
                    .map_err(|error| self.report_selected_failure(error.into()))?;
                let Some(valid) = valid else { continue };
                if !valid {
                    return Err(self.report_selected_failure(
                        SelectedIntegrity("incompatible_index: selected tree node JSON invalid".into()).into(),
                    ));
                }
                let count: i64 = count_stmt
                    .query_row([relative], |row| row.get(0))
                    .map_err(|error| self.report_selected_failure(error.into()))?;
                overlays.push((index, relative.to_owned(), usize::try_from(count)?));
            }
            Ok((revision, self.workspace_root.clone(), overlays))
        }, before_finish)?;
        for (index, path, count) in overlays {
            items[index].indexed_path = Some(path);
            items[index].method_count = Some(count);
        }
        Ok((revision, workspace_root))
    }
    pub fn files_at(
        &self,
        expected: Option<IndexPin>,
        offset: usize,
        limit: usize,
    ) -> Result<serde_json::Value> {
        ensure!(
            (1..=200).contains(&limit) && offset <= i64::MAX as usize,
            "invalid catalog pagination"
        );
        self.with_evidence(|tx| {
            let selected = self.read_revision(tx, expected)?;
            let revision = selected.pin;
            let mut stmt = tx.prepare(
                "SELECT m.path,d.language,m.graph_projection_id FROM revision_documents m
                 JOIN document_versions d ON d.id=m.document_version_id
                 WHERE m.revision_id=?3 ORDER BY m.path LIMIT ?1 OFFSET ?2",
            )?;
            let source_rows: Vec<(String, String, String)> = stmt
                .query_map(
                    params![(limit + 1) as i64, offset as i64, selected.key],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?
                .collect::<rusqlite::Result<_>>()?;
            let mut items = Vec::new();
            for (path, language, projection_id) in source_rows {
                self.attest_selected_document_for(tx, &path, &selected)?;
                let methods: i64 = tx.query_row(
                    "SELECT count(*) FROM graph_nodes WHERE projection_id=?1
                    AND json_extract(payload,'$.kind') IN ('function','method')",
                    [&projection_id],
                    |r| r.get(0),
                )?;
                items.push(
                    serde_json::json!({"path":path,"language":language,"methodCount":methods}),
                );
            }
            let next = (items.len() > limit).then_some(offset + limit);
            items.truncate(limit);
            Ok(serde_json::json!({"revision":revision,"items":items,"nextOffset":next}))
        })
    }
    pub fn methods_at(
        &self,
        path: &str,
        expected: Option<IndexPin>,
    ) -> Result<Option<serde_json::Value>> {
        self.with_evidence(|tx| {
            let selected = self.read_revision(tx, expected)?;
            let revision = selected.pin;
            let projection_id: Option<String> = tx
                .query_row(
                    "SELECT m.graph_projection_id FROM revision_documents m
                     WHERE m.revision_id=?2 AND m.path=?1",
                    params![path, selected.key],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(projection_id) = projection_id else {
                return Ok(None);
            };
            self.attest_selected_document_for(tx, path, &selected)?;
            let mut stmt = tx.prepare(
                "SELECT payload FROM graph_nodes WHERE projection_id=?1
                AND json_extract(payload,'$.kind') IN ('function','method')
                ORDER BY json_extract(payload,'$.range.startByte'),id LIMIT 1001",
            )?;
            let mut items = Vec::new();
            for payload in stmt.query_map([&projection_id], |r| r.get::<_, String>(0))? {
                let symbol: Symbol = serde_json::from_str(&payload?)?;
                items.push(serde_json::json!({"symbol":symbol,"consequential":true,
                    "reason":"Conservative heuristic: retained; triviality is not proven"}));
            }
            let truncated = items.len() > 1000;
            items.truncate(1000);
            Ok(Some(
                serde_json::json!({"revision":revision,"items":items,"truncated":truncated}),
            ))
        })
    }
    /// Only cached source and measured calls from the same snapshot are used.
    pub fn sequence_at(
        &self,
        seed: &str,
        expected: IndexPin,
        show_all: bool,
    ) -> Result<Option<crate::behavior::SequenceView>> {
        self.with_evidence(|tx| {
            let selected_revision = self.read_revision(tx, Some(expected))?;
            let revision = selected_revision.pin;
            let selected: Option<(String, String)> = tx
                .query_row(
                    "SELECT n.path,n.projection_id FROM graph_nodes n JOIN revision_documents m
                 ON m.revision_id=?2 AND m.graph_projection_id=n.projection_id
                 WHERE n.id=?1",
                    params![seed, selected_revision.key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((path, graph_id)) = selected else {
                return Ok(None);
            };
            self.attest_selected_document_for(tx, &path, &selected_revision)?;
            let payload: String = tx.query_row(
                "SELECT payload FROM graph_nodes WHERE projection_id=?1 AND id=?2",
                params![graph_id, seed],
                |r| r.get(0),
            )?;
            let symbol: Symbol = serde_json::from_str(&payload)?;
            ensure!(
                matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method),
                "invalid sequence symbol kind"
            );
            let file = self
                .selected_source_row_for(tx, &path, &selected_revision)?
                .context("sequence source missing")?;
            let mut stmt = tx.prepare(
                "SELECT payload FROM graph_calls WHERE projection_id=?1
                ORDER BY json_extract(payload,'$.range.startByte'),id",
            )?;
            let calls: Vec<CallSite> = stmt
                .query_map([&graph_id], |r| r.get::<_, String>(0))?
                .map(|payload| Ok(serde_json::from_str(&payload?)?))
                .collect::<Result<_>>()?;
            crate::behavior::build_sequence(revision, &symbol, &file, &calls, show_all).map(Some)
        })
    }

    fn read_graph_for(&self, db: &Connection, selected: &ReadRevision) -> Result<Graph> {
        let (stats_type, stats_len, diagnostics_type, diagnostics_len): (String, i64, String, i64) =
            db.query_row(
                "SELECT typeof(graph_stats),length(CAST(graph_stats AS BLOB)),
                typeof(graph_diagnostics),length(CAST(graph_diagnostics AS BLOB))
             FROM native_revisions WHERE id=?1",
                [&selected.key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|error| self.report_selected_failure(error.into()))?;
        ensure!(
            stats_type == "text"
                && diagnostics_type == "text"
                && (0..=GRAPH_FIELD_MAX_BYTES).contains(&stats_len)
                && (0..=GRAPH_FIELD_MAX_BYTES).contains(&diagnostics_len)
                && stats_len + diagnostics_len <= GRAPH_PAIR_MAX_BYTES,
            "incompatible_index: retained graph metadata byte budget exceeded"
        );
        let (raw_stats, raw_diagnostics): (String, String) = db
            .query_row(
                "SELECT graph_stats,graph_diagnostics FROM native_revisions WHERE id=?1",
                [&selected.key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| self.report_selected_failure(error.into()))?;
        let stats: IndexStats = serde_json::from_str(&raw_stats)
            .map_err(|error| self.report_selected_failure(selected_integrity(error.into())))?;
        let diagnostics: Vec<Diagnostic> = serde_json::from_str(&raw_diagnostics)
            .map_err(|error| self.report_selected_failure(selected_integrity(error.into())))?;
        let paths: Vec<String> = db
            .prepare(
                "SELECT m.path FROM revision_documents m WHERE m.revision_id=?1 ORDER BY m.path",
            )?
            .query_map([&selected.key], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for path in &paths {
            self.attest_selected_document_for(db, path, selected)?;
        }
        let files = paths
            .iter()
            .map(|path| {
                self.selected_source_row_for(db, path, selected)?
                    .context("missing manifest graph source")
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Graph {
            schema_version: SCHEMA_VERSION,
            files,
            nodes: rows_at(db, "SELECT n.payload FROM graph_nodes n JOIN revision_documents d
                ON d.revision_id=?1 AND d.graph_projection_id=n.projection_id ORDER BY n.id", &selected.key)?,
            calls: rows_at(db, "SELECT c.payload FROM graph_calls c JOIN revision_documents d
                ON d.revision_id=?1 AND d.graph_projection_id=c.projection_id
                ORDER BY c.path,json_extract(c.payload,'$.range.startByte'),json_extract(c.payload,'$.range.endByte') DESC,c.id", &selected.key)?,
            regions: rows_at(db, "SELECT g.payload FROM graph_regions g JOIN revision_documents d
                ON d.revision_id=?1 AND d.graph_projection_id=g.projection_id ORDER BY g.id", &selected.key)?,
            stats,
            diagnostics,
        })
    }
    pub fn graph(&self) -> Result<Graph> {
        self.graph_at(None)
    }
    pub fn graph_at(&self, expected: Option<IndexPin>) -> Result<Graph> {
        self.with_evidence(|tx| {
            let selected = self.read_revision(tx, expected)?;
            self.read_graph_for(tx, &selected)
        })
    }
    /// Reauthenticate only a cached packet's selected evidence under one pin.
    /// Unrelated documents are not read; status still validates persisted capture inventory.
    pub fn validate_selected_view(&self, view: &ViewResult, sources: &[SourceFile]) -> Result<()> {
        self.with_evidence(|tx| self.validate_selected_view_in(tx, view, sources))
    }
    fn validate_selected_view_in(
        &self,
        tx: &Connection,
        view: &ViewResult,
        sources: &[SourceFile],
    ) -> Result<()> {
        let selected = self.read_revision(tx, Some(view.revision))?;
        let paths: BTreeSet<_> = view
            .nodes
            .iter()
            .map(|n| n.path.as_str())
            .chain(view.calls.iter().map(|c| c.path.as_str()))
            .chain(view.regions.iter().map(|r| r.path.as_str()))
            .chain(sources.iter().map(|f| f.path.as_str()))
            .collect();
        for path in paths {
            self.attest_selected_document_for(tx, path, &selected)?;
        }
        for node in &view.nodes {
            let actual: Option<Symbol> = one_at(
                tx,
                "SELECT n.payload FROM graph_nodes n JOIN revision_documents m ON m.revision_id=?2 AND m.graph_projection_id=n.projection_id WHERE n.id=?1",
                &node.id,
                &selected.key,
            )?;
            ensure!(
                actual.as_ref() == Some(node),
                "incompatible_index: cached packet selected graph declaration changed"
            );
        }
        for call in &view.calls {
            let actual: Option<CallSite> = one_at(
                tx,
                "SELECT c.payload FROM graph_calls c JOIN revision_documents m ON m.revision_id=?2 AND m.graph_projection_id=c.projection_id WHERE c.id=?1",
                &call.id,
                &selected.key,
            )?;
            ensure!(
                actual.as_ref() == Some(call),
                "incompatible_index: cached packet selected graph call changed"
            );
        }
        for region in &view.regions {
            let actual: Option<ControlRegion> = one_at(
                tx,
                "SELECT g.payload FROM graph_regions g JOIN revision_documents m ON m.revision_id=?2 AND m.graph_projection_id=g.projection_id WHERE g.id=?1",
                &region.id,
                &selected.key,
            )?;
            ensure!(
                actual.as_ref() == Some(region),
                "incompatible_index: cached packet selected graph region changed"
            );
        }
        for source in sources {
            ensure!(
                self.selected_source_row_for(tx, &source.path, &selected)?
                    .as_ref()
                    == Some(source),
                "incompatible_index: cached packet selected source changed"
            );
        }
        Ok(())
    }

    pub fn query_view(&self, query: &ViewQuery) -> Result<Option<ViewResult>> {
        self.query_view_at(query, None)
    }

    pub fn query_view_at(
        &self,
        query: &ViewQuery,
        expected_pin: Option<&IndexPin>,
    ) -> Result<Option<ViewResult>> {
        query.validate()?;
        self.with_evidence(|tx| self.query_view_in(tx, query, expected_pin))
    }
    fn query_view_in(
        &self,
        tx: &Connection,
        query: &ViewQuery,
        expected_pin: Option<&IndexPin>,
    ) -> Result<Option<ViewResult>> {
        let selected = self.read_revision(tx, expected_pin.copied())?;
        let revision = selected.pin;
        let selected_path: Option<String> = tx
            .query_row("SELECT n.path FROM graph_nodes n JOIN revision_documents m ON m.revision_id=?2 AND m.graph_projection_id=n.projection_id WHERE n.id=?1", params![query.seed, selected.key], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(path) = selected_path {
            self.attest_selected_document_for(tx, &path, &selected)?;
        }
        let seed: Option<Symbol> = one_at(
            tx,
            "SELECT n.payload FROM graph_nodes n JOIN revision_documents m ON m.revision_id=?2 AND m.graph_projection_id=n.projection_id WHERE n.id=?1",
            &query.seed,
            &selected.key,
        )?;
        let Some(seed) = seed else { return Ok(None) };
        let mut calls = Vec::new();
        let mut region_ids = BTreeSet::new();
        let mut truncated = false;
        if !query.exclude_paths.iter().any(|p| seed.path.starts_with(p)) {
            let mut stmt = tx.prepare("SELECT c.payload FROM graph_calls c JOIN revision_documents m ON m.revision_id=?3 AND m.graph_projection_id=c.projection_id WHERE c.caller=?1 ORDER BY c.path,json_extract(c.payload,'$.range.startByte'),json_extract(c.payload,'$.range.endByte') DESC,c.id LIMIT ?2")?;
            let rows = stmt.query_map(
                params![seed.id, (query.max_calls + 1) as i64, selected.key],
                |r| r.get::<_, String>(0),
            )?;
            for payload in rows {
                let call: CallSite = serde_json::from_str(&payload?)?;
                if query.exclude_paths.iter().any(|p| call.path.starts_with(p)) {
                    continue;
                }
                if calls.len() >= query.max_calls {
                    truncated = true;
                    break;
                }
                region_ids.extend(call.regions.iter().cloned());
                calls.push(call);
            }
        }
        let mut regions = BTreeMap::new();
        while let Some(id) = region_ids.pop_first() {
            let region: Option<ControlRegion> = one_at(
                tx,
                "SELECT g.payload FROM graph_regions g JOIN revision_documents m ON m.revision_id=?2 AND m.graph_projection_id=g.projection_id WHERE g.id=?1",
                &id,
                &selected.key,
            )?;
            if let Some(region) = region {
                if let Some(parent) = &region.parent {
                    region_ids.insert(parent.clone());
                }
                regions.insert(id, region);
            }
        }
        let paths: BTreeSet<_> = std::iter::once(seed.path.as_str())
            .chain(calls.iter().map(|c| c.path.as_str()))
            .chain(regions.values().map(|r| r.path.as_str()))
            .collect();
        for path in paths {
            self.attest_selected_document_for(tx, path, &selected)?;
        }
        Ok(Some(ViewResult {
            revision,
            query: query.clone(),
            nodes: vec![seed],
            calls,
            regions: regions.into_values().collect(),
            truncated,
            omitted_nodes: 0,
            warnings: if truncated {
                vec!["Measured calls truncated at the request limit.".into()]
            } else {
                vec![]
            },
        }))
    }

    pub fn put_view(&self, view: &SavedView) -> Result<()> {
        view.validate()?;
        self.records().put_view(view)?;
        Ok(())
    }

    fn selected_anchor_in(db: &Connection, store: &Self, target: &str) -> Result<DurableAnchor> {
        let revision = store.read_revision(db, None)?;
        let selected: Option<(String, String)> = db
            .query_row(
                "SELECT m.language,m.path FROM native_version_declarations d
                 JOIN revision_documents m ON m.revision_id=?2 AND m.document_version_id=d.version_id
                 WHERE d.syntax_id=?1",
                params![target, revision.key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (language, path) = selected.context("native declaration target missing")?;
        store.attest_selected_document_for(db, &path, &revision)?;
        let declarations =
            Self::read_native_declarations_for(db, &language, None, Some(&path), true, &revision)?;
        let focused = anchors::find_selected(target, &declarations)?;
        anchors::capture_anchor(focused, &declarations)
    }

    fn declarations_for_anchor(
        db: &Connection,
        store: &Self,
        anchor: &DurableAnchor,
        selected: &ReadRevision,
    ) -> Result<Option<(String, Vec<crate::native_evidence::Declaration>)>> {
        let revision: Option<String> = db
            .query_row(
                "SELECT r.native_revision_id FROM revision_documents m
             JOIN native_revisions r ON r.id=m.revision_id
             WHERE m.revision_id=?4 AND m.source_set_id=?1 AND m.language=?2 AND m.path=?3",
                params![
                    anchor.document.source_set_id,
                    anchor.document.language,
                    anchor.document.path,
                    selected.key
                ],
                |row| row.get(0),
            )
            .optional()?;
        let Some(revision) = revision else {
            return Ok(None);
        };
        store.attest_selected_document_for(db, &anchor.document.path, selected)?;
        let declarations = Self::read_native_declarations_for(
            db,
            &anchor.document.language,
            None,
            Some(&anchor.document.path),
            true,
            selected,
        )?;
        ensure!(
            anchors::document_matches(&anchor.document, &declarations),
            "invalid anchor document association"
        );
        Ok(Some((revision, declarations)))
    }

    fn anchor_attachment(
        db: &Connection,
        store: &Self,
        raw: Option<&serde_json::value::RawValue>,
        selected: &ReadRevision,
    ) -> Result<AnchorAttachment> {
        let Some(raw) = raw else {
            return Ok(AnchorAttachment {
                availability: AttachmentAvailability::Anchorless,
                result: None,
            });
        };
        let anchor: DurableAnchor = serde_json::from_str(raw.get())?;
        anchor.validate()?;
        let result = match Self::declarations_for_anchor(db, store, &anchor, selected)? {
            Some((revision, declarations)) => {
                let current = declarations
                    .iter()
                    .find(|row| row.syntax_id == anchor.syntax_id);
                anchors::audit_anchor(&anchor, &revision, current, &declarations, None)?
            }
            None => AnchorResult::orphaned(AnchorReason::Missing),
        };
        Ok(AnchorAttachment {
            availability: AttachmentAvailability::Ready,
            result: Some(result),
        })
    }

    fn resolve_view(
        db: &Connection,
        store: &Self,
        view: SavedViewRecord,
        pin: Option<IndexPin>,
    ) -> Result<SavedViewState> {
        let selected = store.read_revision(db, pin)?;
        let ids: BTreeSet<&String> = std::iter::once(&view.query.seed)
            .chain(view.pins.keys())
            .chain(view.hidden.iter())
            .collect();
        let mut orphaned_ids = vec![];
        for id in ids {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM graph_nodes n JOIN revision_documents m ON m.revision_id=?2 AND m.graph_projection_id=n.projection_id WHERE n.id=?1)",
                params![id, selected.key],
                |r| r.get(0),
            )?;
            if !exists {
                orphaned_ids.push(id.clone());
            }
        }
        let attachment = Self::anchor_attachment(db, store, view.anchor.as_deref(), &selected)?;
        if attachment
            .result
            .as_ref()
            .is_none_or(|result| result.status != AnchorStatus::Attached)
            && !orphaned_ids.contains(&view.query.seed)
        {
            orphaned_ids.push(view.query.seed.clone());
        }
        Ok(SavedViewState {
            view,
            orphaned_ids,
            index_generation: pin.map(|p| p.index_generation.to_string()),
            index_revision: pin.map(|p| p.index_revision),
            attachment,
        })
    }
    // Durable records are not index evidence. Admit even an empty result before returning it.
    fn saved_pin(response: &EvidenceResponse, expected_pin: Option<IndexPin>) -> Result<IndexPin> {
        Ok(response
            .store
            .read_revision(&response.db, expected_pin)?
            .pin)
    }

    fn finish_saved<T>(&self, response: &EvidenceResponse, value: T) -> Result<T> {
        // A publication in flight must not turn an old attachment into a Ready response.
        self.ensure_public_read_ready()?;
        response.finish(value)
    }

    fn unavailable_view(view: SavedViewRecord) -> SavedViewState {
        let orphaned_ids = std::iter::once(view.query.seed.clone())
            .chain(view.pins.keys().cloned())
            .chain(view.hidden.iter().cloned())
            .collect();
        SavedViewState {
            view,
            orphaned_ids,
            index_generation: None,
            index_revision: None,
            attachment: AnchorAttachment {
                availability: AttachmentAvailability::IndexUnavailable,
                result: None,
            },
        }
    }

    pub fn saved_views_at(&self, expected_pin: Option<IndexPin>) -> Result<Vec<SavedViewState>> {
        let views = self.records().view_records()?;
        let response = match self.evidence_response() {
            Ok(response) => response,
            Err(error) if expected_pin.is_none() && native_index_unavailable(&error) => {
                return Ok(views.into_iter().map(Self::unavailable_view).collect());
            }
            Err(error) => return Err(error),
        };
        let pin = Self::saved_pin(&response, expected_pin)?;
        let states = views
            .into_iter()
            .map(|view| Self::resolve_view(&response.db, self, view, Some(pin)))
            .collect::<Result<Vec<_>>>()?;
        self.finish_saved(&response, states)
    }
    pub fn views(&self) -> Result<Vec<SavedViewState>> {
        self.saved_views_at(None)
    }

    pub fn saved_view_at(
        &self,
        id: &str,
        expected_pin: Option<IndexPin>,
    ) -> Result<Option<SavedViewState>> {
        let view = self.records().view_record(id)?;
        let response = match self.evidence_response() {
            Ok(response) => response,
            Err(error) if expected_pin.is_none() && native_index_unavailable(&error) => {
                return Ok(view.map(Self::unavailable_view));
            }
            Err(error) => return Err(error),
        };
        let pin = Self::saved_pin(&response, expected_pin)?;
        let state = view
            .map(|view| Self::resolve_view(&response.db, self, view, Some(pin)))
            .transpose()?;
        self.finish_saved(&response, state)
    }
    pub fn view(&self, id: &str) -> Result<Option<SavedViewState>> {
        self.saved_view_at(id, None)
    }

    pub fn save_view_at(&self, pin: IndexPin, view: &SavedView) -> Result<SavedViewState> {
        view.validate()?;
        let response = self.evidence_response()?;
        ensure!(
            response.status()?.revision == pin,
            "revision conflict: mutation requires head"
        );
        Self::saved_pin(&response, Some(pin))?;
        let record = self.records().update_view_record(
            &SavedViewRecord::from_base(view.clone(), None),
            || {
                serde_json::value::to_raw_value(&Self::selected_anchor_in(
                    &response.db,
                    self,
                    &view.query.seed,
                )?)
                .map_err(Into::into)
            },
        )?;
        let state = Self::resolve_view(&response.db, self, record, Some(pin))?;
        self.finish_saved(&response, state)
    }
    pub fn delete_view(&self, id: &str) -> Result<bool> {
        self.records().delete_view(id)
    }

    pub fn put_annotation(&self, annotation: &Annotation) -> Result<()> {
        annotation.validate()?;
        self.records().put_annotation(annotation)?;
        Ok(())
    }
    fn resolve_annotation(
        db: &Connection,
        store: &Self,
        annotation: AnnotationRecord,
        pin: Option<IndexPin>,
    ) -> Result<AnnotationState> {
        let selected = store.read_revision(db, pin)?;
        let attachment =
            Self::anchor_attachment(db, store, annotation.anchor.as_deref(), &selected)?;
        let orphaned = attachment
            .result
            .as_ref()
            .is_none_or(|result| result.status != AnchorStatus::Attached);
        Ok(AnnotationState {
            annotation,
            orphaned,
            index_generation: pin.map(|p| p.index_generation.to_string()),
            index_revision: pin.map(|p| p.index_revision),
            attachment,
        })
    }
    fn unavailable_annotation(annotation: AnnotationRecord) -> AnnotationState {
        AnnotationState {
            annotation,
            orphaned: true,
            index_generation: None,
            index_revision: None,
            attachment: AnchorAttachment {
                availability: AttachmentAvailability::IndexUnavailable,
                result: None,
            },
        }
    }
    pub fn saved_annotations_at(
        &self,
        expected_pin: Option<IndexPin>,
    ) -> Result<Vec<AnnotationState>> {
        let annotations = self.records().annotation_records()?;
        let response = match self.evidence_response() {
            Ok(response) => response,
            Err(error) if expected_pin.is_none() && native_index_unavailable(&error) => {
                return Ok(annotations
                    .into_iter()
                    .map(Self::unavailable_annotation)
                    .collect());
            }
            Err(error) => return Err(error),
        };
        let pin = Self::saved_pin(&response, expected_pin)?;
        let states = annotations
            .into_iter()
            .map(|item| Self::resolve_annotation(&response.db, self, item, Some(pin)))
            .collect::<Result<Vec<_>>>()?;
        self.finish_saved(&response, states)
    }
    pub fn annotations(&self) -> Result<Vec<AnnotationState>> {
        self.saved_annotations_at(None)
    }

    pub fn save_annotation_at(
        &self,
        pin: IndexPin,
        request: &AnnotationRequest,
    ) -> Result<AnnotationState> {
        request.validate()?;
        let response = self.evidence_response()?;
        ensure!(
            response.status()?.revision == pin,
            "revision conflict: mutation requires head"
        );
        Self::saved_pin(&response, Some(pin))?;
        let title = request
            .title
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let record = self.records().update_annotation_record(
            &AnnotationRecord::from_base(request.base(), title, None),
            request.title.is_none(),
            || {
                serde_json::value::to_raw_value(&Self::selected_anchor_in(
                    &response.db,
                    self,
                    &request.node_id,
                )?)
                .map_err(Into::into)
            },
        )?;
        let state = Self::resolve_annotation(&response.db, self, record, Some(pin))?;
        self.finish_saved(&response, state)
    }
    pub fn delete_annotation(&self, id: &str) -> Result<bool> {
        self.records().delete_annotation(id)
    }
}

#[cfg(test)]
mod rebaseline_fault_tests {
    use super::*;
    use crate::indexer::{IndexOptions, index_workspace_bundle};
    use std::{fs, sync::atomic::AtomicBool};

    #[test]
    fn selective_publication_faults_rollback_and_retry_same_expected_pin() {
        use crate::{capture::Capture, index_coordinator::IndexJobCoordinator, indexer};
        for failure_at in [PublishStage::AfterFile, PublishStage::BeforeCommit] {
            let state = tempfile::tempdir().unwrap();
            let work = tempfile::tempdir().unwrap();
            fs::write(
                work.path().join("local.js"),
                "function local(){return 1;}\n",
            )
            .unwrap();
            fs::write(
                work.path().join("A.java"),
                "class A { int run(){return 1;} }\n",
            )
            .unwrap();
            let options = IndexOptions::new(work.path().to_owned());
            let cancel = Arc::new(AtomicBool::new(false));
            let store = Store::open_for_tests(state.path(), work.path()).unwrap();
            let job = IndexJobCoordinator::prepare(&store, None).unwrap();
            let session = job.session();
            let first = job.run(&options, &cancel, |_| {}).unwrap();
            fs::write(
                work.path().join("local.js"),
                "function local(){return 2;}\n",
            )
            .unwrap();
            store.begin_leader_publication(&session).unwrap();
            let capture = Capture::admit(&options, &cancel, &|_| {}).unwrap();
            let baseline = store.recovery_index_baseline().unwrap();
            let native = store
                .prepare_local_native(&capture, &baseline, &cancel)
                .unwrap()
                .expect("same-leaf source edit selects one native document");
            let graph =
                indexer::project_native(&options, &capture, &native, &cancel, &|_| {}).unwrap();
            store
                .validate_native_bundle_with_mode(&graph, &capture, &native, &cancel, true)
                .unwrap();
            let err = store
                .publish_inner_checked_expected(
                    (&graph, &capture, &native),
                    session.leader_guard().unwrap(),
                    ExpectedPublication::Recovery(Box::new(baseline)),
                    &cancel,
                    256 * 1024 * 1024 + 16 * 1024,
                    |stage, _db| {
                        if stage == failure_at {
                            // A separate reader must still see the committed old pin.
                            let outside = store.cache()?;
                            let before: i64 = outside.query_row(
                                "SELECT index_revision FROM index_metadata",
                                [],
                                |r| r.get(0),
                            )?;
                            assert_eq!(
                                before, first.index_revision as i64,
                                "uncommitted selected rows remain invisible"
                            );
                            anyhow::bail!("injected selective fault");
                        }
                        Ok(())
                    },
                )
                .unwrap_err();
            assert!(
                err.to_string().contains("injected selective fault"),
                "{err:#}"
            );
            let db = store.cache().unwrap();
            let unchanged = ReadRevision::current(&db).unwrap();
            assert_eq!(unchanged.pin, first, "failure preserves old current pin");
            let rows: i64 = db
                .query_row("SELECT count(*) FROM native_revisions", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 1, "no pending revision survives rollback");
            assert_eq!(
                db.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
            assert!(
                db.prepare("PRAGMA foreign_key_check")
                    .unwrap()
                    .query([])
                    .unwrap()
                    .next()
                    .unwrap()
                    .is_none(),
                "selective rollback leaves all foreign keys valid"
            );
            drop(db);
            let retry =
                IndexJobCoordinator::prepare_with_session(&store, Some(first), session.clone())
                    .unwrap()
                    .run(&options, &cancel, |_| {})
                    .unwrap();
            assert_eq!(retry.index_revision, first.index_revision + 1);
            assert!(
                store
                    .source_at("local.js", Some(first))
                    .unwrap()
                    .unwrap()
                    .1
                    .text
                    .contains("return 1")
            );
            assert!(
                store
                    .source_at("local.js", Some(retry))
                    .unwrap()
                    .unwrap()
                    .1
                    .text
                    .contains("return 2")
            );
            assert_eq!(
                store
                    .last_writer_counters()
                    .unwrap()
                    .reused_occurrence_reads,
                0
            );
        }
    }

    #[test]
    fn persisted_local_classifier_falls_back_on_affected_uncertainty_not_unrelated_ambiguity() {
        use crate::{capture::Capture, index_coordinator::IndexJobCoordinator};
        let cases = [
            ("unrelated lexical duplicate", "body", true),
            ("added source", "add", false),
            ("deleted source", "delete", false),
            ("renamed path", "rename", false),
            ("parser recovery", "parse", false),
            ("changed scope", "scope", false),
            ("changed import", "import", false),
            ("changed reexport", "export", false),
            ("changed supertype", "supertype", false),
            ("affected unproved lookup", "lookup", false),
            (
                "affected lexical duplicate candidate",
                "ambiguous_lookup",
                false,
            ),
        ];
        for (label, change, local) in cases {
            let state = tempfile::tempdir().unwrap();
            let workspace = tempfile::tempdir().unwrap();
            let path = workspace.path();
            fs::write(path.join("target.js"), "function target() { return 1; }\n").unwrap();
            // Duplicate lexical candidates are #22 advisory facts, NOT semantic bindings.
            fs::write(
                path.join("duplicate.js"),
                "function target() { return 3; }\nfunction target() { return 4; }\n",
            )
            .unwrap();
            fs::write(
                path.join("A.java"),
                "class A extends Base { int run(){ return 1; } }\nclass Base {}\n",
            )
            .unwrap();
            let options = IndexOptions::new(path.to_owned());
            let cancel = Arc::new(AtomicBool::new(false));
            let store = Store::open_for_tests(state.path(), path).unwrap();
            let job = IndexJobCoordinator::prepare(&store, None).unwrap();
            let session = job.session();
            let first = job.run(&options, &cancel, |_| {}).unwrap();
            let old = store.graph_at(Some(first)).unwrap();
            assert_eq!(
                old.nodes
                    .iter()
                    .filter(|node| node.name == "target")
                    .count(),
                3,
                "#22 lexical duplicate candidates are advisory, not semantic bindings"
            );
            match change {
                "body" => {
                    fs::write(path.join("target.js"), "function target() { return 2; }\n").unwrap()
                }
                "add" => {
                    fs::write(path.join("added.js"), "function added() { return 1; }\n").unwrap()
                }
                "delete" => fs::remove_file(path.join("duplicate.js")).unwrap(),
                "rename" => fs::rename(path.join("duplicate.js"), path.join("moved.js")).unwrap(),
                "parse" => {
                    fs::write(path.join("target.js"), "function target() { return ( ; }\n").unwrap()
                }
                "scope" => fs::write(
                    path.join("target.js"),
                    "function target() { { return 2; } }\n",
                )
                .unwrap(),
                "import" => fs::write(
                    path.join("target.js"),
                    "import { foo } from './missing.js';\nfunction target() { return 1; }\n",
                )
                .unwrap(),
                "export" => fs::write(
                    path.join("target.js"),
                    "export { target };\nfunction target() { return 1; }\n",
                )
                .unwrap(),
                "supertype" => fs::write(
                    path.join("A.java"),
                    "class A extends Other { int run(){ return 1; } }\nclass Base {}\n",
                )
                .unwrap(),
                "lookup" => fs::write(
                    path.join("target.js"),
                    "function target() { return unknown(); }\n",
                )
                .unwrap(),
                "ambiguous_lookup" => fs::write(
                    path.join("target.js"),
                    "function target() { return target(); }\n",
                )
                .unwrap(),
                _ => unreachable!(),
            }
            let capture = Capture::admit(&options, &cancel, &|_| {}).unwrap();
            let expected = store.recovery_index_baseline().unwrap();
            assert_eq!(
                store
                    .prepare_local_native(&capture, &expected, &cancel)
                    .unwrap()
                    .is_some(),
                local,
                "{label}: persisted authenticated classifier"
            );
            let pin =
                IndexJobCoordinator::prepare_with_session(&store, Some(first), session.clone())
                    .unwrap()
                    .run(&options, &cancel, |_| {})
                    .unwrap();
            assert_eq!(
                store.graph_at(Some(first)).unwrap().nodes,
                old.nodes,
                "{label}: old pin preserved"
            );
            let cold_state = tempfile::tempdir().unwrap();
            let cold = Store::open_for_tests(cold_state.path(), path).unwrap();
            let cold_job = IndexJobCoordinator::prepare(&cold, None).unwrap();
            let _cold_session = cold_job.session();
            let cold_pin = cold_job.run(&options, &cancel, |_| {}).unwrap();
            let published = store.graph_at(Some(pin)).unwrap();
            let oracle = cold.graph_at(Some(cold_pin)).unwrap();
            assert_eq!(published.nodes, oracle.nodes, "{label}: cold node parity");
            assert_eq!(published.calls, oracle.calls, "{label}: cold call parity");
            assert_eq!(
                published.regions, oracle.regions,
                "{label}: cold region parity"
            );
        }
    }

    #[test]
    #[ignore = "resource-gated exact canonical medium local-update writer counters"]
    fn canonical_medium_local_update_writes_only_changed_fact_families() {
        use crate::{
            capture::Capture,
            index_coordinator::IndexJobCoordinator,
            indexer::{self, CapturedChange},
        };
        let generated = tempfile::tempdir().unwrap();
        let output = generated.path().join("canonical");
        let status = std::process::Command::new("node")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/synthetic-cohorts/generate.mjs"))
            .arg("--out")
            .arg(&output)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            fs::read(output.join("manifest.json")).unwrap(),
            fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tools/synthetic-cohorts/manifest-v1.json")
            )
            .unwrap()
        );
        let root = output.join("medium");
        let state = tempfile::tempdir().unwrap();
        let store = Store::open_for_tests(state.path(), &root).unwrap();
        let options = IndexOptions::new(root.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        let job = IndexJobCoordinator::prepare(&store, None).unwrap();
        let session = job.session();
        let first_capture = Capture::admit(&options, &cancel, &|_| {}).unwrap();
        let first = job.run(&options, &cancel, |_| {}).unwrap();
        let full = store.last_writer_counters().unwrap();
        let path = root.join("python/Cmedium0000.py");
        let original = fs::read_to_string(&path).unwrap();
        let marker = "def f0(): return ";
        let offset = original.find(marker).unwrap() + marker.len();
        let mut source = original.as_bytes().to_vec();
        assert!(source[offset].is_ascii_digit());
        source[offset] = if source[offset] == b'1' { b'2' } else { b'1' };
        fs::write(&path, source).unwrap();
        let updated_capture = Capture::admit(&options, &cancel, &|_| {}).unwrap();
        let mut visits = vec![];
        let staged = indexer::measure_captured_native_change(
            &first_capture,
            &updated_capture,
            &root,
            store.root_id(),
            &cancel,
            |key| visits.push(key.path.clone()),
        )
        .unwrap();
        assert!(
            matches!(staged.decision,CapturedChange::DocumentLocal{ref path}
            if path=="python/Cmedium0000.py")
        );
        assert_eq!(
            visits,
            ["python/Cmedium0000.py"],
            "selected native extraction visits exactly one document"
        );
        let baseline = store.recovery_index_baseline().unwrap();
        assert!(
            store
                .prepare_local_native(&updated_capture, &baseline, &cancel)
                .unwrap()
                .is_some(),
            "authenticated persisted head also selects one-document native assembly"
        );
        let second =
            IndexJobCoordinator::prepare_with_session(&store, Some(first), session.clone())
                .unwrap()
                .run(&options, &cancel, |_| {})
                .unwrap();
        let local = store.last_writer_counters().unwrap();
        assert!(
            local.native.rows > 0 && local.native.rows < full.native.rows / 10,
            "selected native rows {local:?} vs full {full:?}"
        );
        assert!(local.graph.rows > 0 && local.graph.rows < full.graph.rows / 10);
        assert!(local.class.rows > 0 && local.class.rows < full.class.rows / 10);
        assert_eq!(local.reused_occurrence_reads, 0);
        assert_eq!(
            local.total.rows,
            local.native.rows + local.graph.rows + local.class.rows + local.manifest.rows
        );
        assert_eq!(
            local.total.bytes,
            local.native.bytes + local.graph.bytes + local.class.bytes + local.manifest.bytes
        );
        assert_eq!(
            store
                .source_at("python/Cmedium0000.py", Some(first))
                .unwrap()
                .unwrap()
                .1
                .text,
            original
        );
        let cold_state = tempfile::tempdir().unwrap();
        let cold = Store::open_for_tests(cold_state.path(), &root).unwrap();
        let cold_job = IndexJobCoordinator::prepare(&cold, None).unwrap();
        let _cold_session = cold_job.session();
        let cold_pin = cold_job.run(&options, &cancel, |_| {}).unwrap();
        let actual = store.graph_at(Some(second)).unwrap();
        let oracle = cold.graph_at(Some(cold_pin)).unwrap();
        assert_eq!(actual.nodes, oracle.nodes);
        assert_eq!(actual.calls, oracle.calls);
        assert_eq!(actual.regions, oracle.regions);
        eprintln!("canonical medium initial writer {full:?}; selected update writer {local:?}");
    }

    #[test]
    #[ignore = "resource-gated canonical medium and large produced-index fact floors"]
    fn canonical_cohort_produced_native_fact_floors() {
        use crate::index_coordinator::IndexJobCoordinator;
        let generated = tempfile::tempdir().unwrap();
        let output = generated.path().join("canonical");
        let generator =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/synthetic-cohorts/generate.mjs");
        let status = std::process::Command::new("node")
            .arg(generator)
            .arg("--out")
            .arg(&output)
            .status()
            .unwrap();
        assert!(status.success(), "canonical cohort generator failed");
        assert_eq!(
            fs::read(output.join("manifest.json")).unwrap(),
            fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tools/synthetic-cohorts/manifest-v1.json")
            )
            .unwrap(),
            "only exact pinned cohort bytes are eligible"
        );
        let cancel = Arc::new(AtomicBool::new(false));
        for size in ["medium", "large"] {
            // Explicit diagnostic only; the default proof always measures both pinned cohorts.
            if size == "large" && std::env::var_os("BALEYG_ONLY_MEDIUM_DIAGNOSTIC").is_some() {
                break;
            }
            let state = tempfile::tempdir().unwrap();
            let root = output.join(size);
            let store = Store::open_for_tests(state.path(), &root).unwrap();
            let job = IndexJobCoordinator::prepare(&store, None).unwrap();
            let _session = job.session();
            let started = std::time::Instant::now();
            let pin = job
                .run_observed(
                    &IndexOptions::new(root),
                    &cancel,
                    |_| {},
                    |_| {
                        eprintln!(
                            "{size} prepublication capture/native/graph {:?}",
                            started.elapsed()
                        );
                    },
                )
                .unwrap();
            eprintln!("{size} committed publisher {:?}", started.elapsed());
            let db = Connection::open(store.roots.index_db(&store.identity)).unwrap();
            let revision = format!("pin:v1:{}:{}", pin.index_generation, pin.index_revision);
            let mut counts = BTreeMap::new();
            for row in db.prepare("SELECT m.language,SUM(
                (SELECT count(*) FROM native_version_declarations d WHERE d.version_id=m.document_version_id)+
                (SELECT count(*) FROM native_version_calls c WHERE c.version_id=m.document_version_id)+
                (SELECT count(*) FROM native_version_control_regions r WHERE r.version_id=m.document_version_id))
                FROM revision_documents m WHERE m.revision_id=?1 GROUP BY m.language ORDER BY m.language")
                .unwrap().query_map([&revision],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))
                .unwrap() {
                let (language,facts)=row.unwrap();counts.insert(language,facts);
            }
            assert_eq!(counts.len(), 4, "{size}: count each produced language");
            if size == "medium" {
                for (language, facts) in &counts {
                    assert!(
                        *facts >= 50_000,
                        "{size}/{language}: produced native facts {facts} below floor"
                    );
                }
            } else {
                assert!(
                    counts.values().sum::<i64>() >= 500_000,
                    "{size}: produced native facts {counts:?} below overall floor"
                );
            }
            let measured = store.last_writer_counters().unwrap();
            assert!(
                measured.native.rows > 0
                    && measured.graph.rows > 0
                    && measured.class.rows > 0
                    && measured.manifest.rows >= counts.len() as u64
            );
            assert_eq!(
                measured.total.rows,
                measured.native.rows
                    + measured.graph.rows
                    + measured.class.rows
                    + measured.manifest.rows
            );
            assert_eq!(
                measured.total.bytes,
                measured.native.bytes
                    + measured.graph.bytes
                    + measured.class.bytes
                    + measured.manifest.bytes
            );
            assert_eq!(measured.reused_occurrence_reads, 0);
            eprintln!("{size} produced facts {counts:?}; writer rows/bytes {measured:?}");
        }
    }

    #[test]
    fn local_writer_counters_exclude_unchanged_occurrence_access() {
        use crate::index_coordinator::IndexJobCoordinator;
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        fs::write(
            work.path().join("local.js"),
            "function local() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            work.path().join("A.java"),
            "class A { int run() { return 1; } }\n",
        )
        .unwrap();
        let options = IndexOptions::new(work.path().to_owned());
        let cancel = Arc::new(AtomicBool::new(false));
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let first_job = IndexJobCoordinator::prepare(&store, None).unwrap();
        let session = first_job.session();
        let first = first_job.run(&options, &cancel, |_| {}).unwrap();
        fs::write(
            work.path().join("local.js"),
            "function local() { return 2; }\n",
        )
        .unwrap();
        let second = IndexJobCoordinator::prepare_with_session(&store, Some(first), session)
            .unwrap()
            .run(&options, &cancel, |_| {})
            .unwrap();
        assert_eq!(second.index_revision, first.index_revision + 1);
        let counted = store.last_writer_counters().unwrap();
        assert_eq!(
            counted.reused_occurrence_reads, 0,
            "unchanged occurrence must never be compared under BEGIN IMMEDIATE"
        );
        assert!(counted.native.rows > 0 && counted.graph.rows > 0 && counted.class.rows > 0);
        assert!(
            counted.manifest.rows >= 3,
            "full manifest plus header must be written"
        );
        assert_eq!(
            counted.total.rows,
            counted.manifest.rows + counted.native.rows + counted.graph.rows + counted.class.rows
        );
        assert_eq!(
            counted.total.bytes,
            counted.manifest.bytes
                + counted.native.bytes
                + counted.graph.bytes
                + counted.class.bytes
        );
    }

    #[test]
    fn wal_header_without_sidecars_is_not_corruption_authority() {
        use std::os::unix::fs::MetadataExt;
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let index = store.roots.index_db(&store.identity);
        let lock = store.roots.leader_lock(&store.identity);
        let old_lock = fs::symlink_metadata(&lock).unwrap();
        drop(store);
        let db = Connection::open(&index).unwrap();
        let mode: String = db
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let _: (i64, i64, i64) = db
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        drop(db);
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(!index.with_file_name(format!("index.db{suffix}")).exists());
        }
        let before = fs::read(&index).unwrap();
        assert_eq!((before[18], before[19]), (2, 2));
        let error = verify_index_file(&index).unwrap_err();
        assert_eq!(recovery_class(&error), RecoveryClass::Hard);
        assert!(
            error.to_string().contains("incompatible_index"),
            "{error:#}"
        );
        assert!(Store::open_for_tests(state.path(), work.path()).is_err());
        assert_eq!(fs::read(&index).unwrap(), before);
        let new_lock = fs::symlink_metadata(&lock).unwrap();
        assert_eq!(
            (old_lock.dev(), old_lock.ino()),
            (new_lock.dev(), new_lock.ino())
        );
    }

    #[test]
    fn only_short_or_invalid_magic_headers_request_typed_recreation() {
        for bytes in [
            b"short".as_slice(),
            b"not-a-sqlite-header-with-at-least-20".as_slice(),
        ] {
            let state = tempfile::tempdir().unwrap();
            let work = tempfile::tempdir().unwrap();
            fs::write(work.path().join("a.js"), "function a() {}\n").unwrap();
            let store = Store::open_for_tests(state.path(), work.path()).unwrap();
            let index = store.roots.index_db(&store.identity);
            drop(store);
            fs::write(&index, bytes).unwrap();
            let error = verify_index_file(&index).unwrap_err();
            assert!(error.is::<ExceptionalIndexFormat>());
            assert_eq!(recovery_class(&error), RecoveryClass::RecreatePending);
            let recovering = Store::open_for_tests(state.path(), work.path()).unwrap();
            assert_eq!(
                recovering.disposition(),
                RecoveryDisposition::RecreatePending
            );
            let exclusive = recovering
                .roots
                .index_use_exclusive_existing(&recovering.identity)
                .unwrap();
            let mut leader = recovering
                .roots
                .leader_under_exclusive(&recovering.identity, exclusive)
                .unwrap();
            let cancel = Arc::new(AtomicBool::new(false));
            let pin = recovering
                .recreate_index_exclusive(
                    &IndexOptions::new(work.path().to_owned()),
                    &mut leader,
                    &cancel,
                )
                .unwrap();
            assert_eq!(pin.index_revision, 1);
            assert_eq!(recovering.status().unwrap().revision, pin);
        }
    }

    #[test]
    fn obsolete_metadata_marker_recreates_fresh_pin_and_preserves_requests() {
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        fs::write(work.path().join("a.js"), "function a() {}\n").unwrap();
        let options = IndexOptions::new(work.path().to_owned());
        let cancel = Arc::new(AtomicBool::new(false));
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
        let owner = store.leader_session().unwrap();
        let original = store
            .publish_native(
                &graph,
                &capture,
                &native,
                owner.leader_guard().unwrap(),
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        assert_eq!(original.index_revision, 1);
        assert_eq!(store.graph_at(Some(original)).unwrap().files.len(), 1);
        let request = store.enqueue_request(&options, Some(original)).unwrap();
        let index = store.roots.index_db(&store.identity);
        drop(owner);
        drop(store);
        let db = Connection::open(&index).unwrap();
        db.execute_batch(
            "PRAGMA ignore_check_constraints=ON; UPDATE index_metadata SET schema_version=7;",
        )
        .unwrap();
        drop(db);
        let pending = Store::open_for_tests(state.path(), work.path()).unwrap();
        assert!(pending.is_recreate_pending());
        let (pin, session) = pending
            .recreate_pending_leader_session(&options, &cancel)
            .unwrap();
        assert_eq!(pin.index_revision, 1);
        assert_ne!(pin.index_generation, original.index_generation);
        let queued = pending.request_by_id(&request.id).unwrap().unwrap();
        assert_eq!(queued.expected, Some(original));
        assert_eq!(queued.state, "queued");
        assert!(
            pending
                .graph_at(Some(original))
                .unwrap_err()
                .to_string()
                .contains("revision conflict")
        );
        drop(session);
    }

    #[test]
    fn private_stage_uses_paired_writer_without_admitting_corrupt_live_evidence() {
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        fs::write(
            work.path().join("a.js"),
            "function changed() { return 1; }\n",
        )
        .unwrap();
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let options = IndexOptions::new(work.path().to_owned());
        let cancel = Arc::new(AtomicBool::new(false));
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
        let owner = store.leader_session().unwrap();
        let previous = store
            .publish_native(
                &graph,
                &capture,
                &native,
                owner.leader_guard().unwrap(),
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        assert_eq!(previous.index_revision, 1);
        let live_path = store.roots.index_db(&store.identity);
        drop(owner);
        drop(store);
        fs::write(&live_path, b"corrupt index, not sqlite").unwrap();
        let original = fs::read(&live_path).unwrap();
        let recovering = Store::open_for_tests(state.path(), work.path()).unwrap();
        assert_eq!(
            recovering.disposition(),
            RecoveryDisposition::RecreatePending
        );
        let clone = recovering.clone();
        let exclusive = recovering
            .roots
            .index_use_exclusive_existing(&recovering.identity)
            .unwrap();
        let leader = recovering
            .roots
            .leader_under_exclusive(&recovering.identity, exclusive)
            .unwrap();
        let stage = recovering.create_staged_index(&leader).unwrap();
        let (graph, native, capture) =
            index_workspace_bundle(&options, recovering.root_id(), &cancel, |_| {}).unwrap();
        let replacement = recovering
            .publish_native_to_stage(
                (&graph, &capture, &native),
                &stage,
                &leader,
                &cancel,
                |_, _| Ok(()),
            )
            .unwrap();
        assert_eq!(replacement.index_revision, previous.index_revision);
        assert_ne!(replacement.index_generation, previous.index_generation);
        let db = open_index(&stage.path, false).unwrap();
        let stage_status = recovering.decode_control_status_raw(&db).unwrap();
        assert_eq!(stage_status.revision, replacement);
        let marker: String = db
            .query_row(
                "SELECT reconciled_incarnation FROM index_metadata WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker, leader.incarnation.to_string());
        validate_paired_metadata(&db, recovering.root_id()).unwrap();
        validate_paired_rows(&db).unwrap();
        validate_reconcile_inventory(&db).unwrap();
        let integrity: String = db
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        drop(db);
        assert_eq!(fs::read(&live_path).unwrap(), original);
        assert_eq!(
            recovering.disposition(),
            RecoveryDisposition::RecreatePending
        );
        assert!(clone.recovery_required.load(Ordering::Acquire));
        assert!(
            clone
                .status()
                .unwrap_err()
                .to_string()
                .contains("recovery_required")
        );

        let failing = recovering.create_staged_index(&leader).unwrap();
        let error = recovering
            .publish_native_to_stage(
                (&graph, &capture, &native),
                &failing,
                &leader,
                &cancel,
                |phase, _| {
                    if phase == PublishStage::BeforeCommit {
                        anyhow::bail!("injected stage precommit refusal");
                    }
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected stage precommit refusal")
        );
        let unchanged = open_index(&failing.path, false).unwrap();
        let baseline = recovering.recovery_baseline(&unchanged).unwrap();
        assert_eq!(baseline.pin().unwrap().index_revision, 0);
        assert!(
            baseline.compatible,
            "private stage remains an empty v8 bootstrap"
        );
        validate_v8_bootstrap(&unchanged).unwrap();
        assert_eq!(
            unchanged
                .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            DATABASE_SCHEMA_VERSION
        );
        drop(unchanged);
        assert_eq!(fs::read(&live_path).unwrap(), original);
        assert_eq!(
            recovering.disposition(),
            RecoveryDisposition::RecreatePending
        );
        assert!(clone.recovery_required.load(Ordering::Acquire));
        assert!(
            clone
                .status()
                .unwrap_err()
                .to_string()
                .contains("recovery_required")
        );
    }

    #[test]
    fn staged_hot_journal_after_validation_refuses_without_deleting_sidecar() {
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        fs::write(work.path().join("a.js"), "function a() {}\n").unwrap();
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let index = store.roots.index_db(&store.identity);
        let dir = store.roots.index_dir(&store.identity);
        drop(store);
        fs::write(&index, b"short").unwrap();
        let before = fs::read(&index).unwrap();
        let recovering = Store::open_for_tests(state.path(), work.path()).unwrap();
        let exclusive = recovering
            .roots
            .index_use_exclusive_existing(&recovering.identity)
            .unwrap();
        let mut leader = recovering
            .roots
            .leader_under_exclusive(&recovering.identity, exclusive)
            .unwrap();
        let mut journal = None;
        let cancel = Arc::new(AtomicBool::new(false));
        let error = recovering
            .recreate_index_exclusive_with_hook(
                &IndexOptions::new(work.path().to_owned()),
                &mut leader,
                &cancel,
                |phase| {
                    if phase == ActivationStage::BeforeRename {
                        let stage = fs::read_dir(&dir)?
                            .map(|entry| entry.map(|entry| entry.path()))
                            .collect::<std::io::Result<Vec<_>>>()?
                            .into_iter()
                            .find(|path| {
                                path.file_name()
                                    .and_then(|name| name.to_str())
                                    .is_some_and(|name| name.starts_with("index.db.tmp-"))
                            })
                            .context("missing private stage at activation hook")?;
                        let sidecar = stage.with_file_name(format!(
                            "{}-journal",
                            stage.file_name().unwrap().to_string_lossy()
                        ));
                        fs::write(&sidecar, b"hot staged journal")?;
                        journal = Some(sidecar);
                    }
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("recovery_required"), "{error:#}");
        assert_eq!(fs::read(&index).unwrap(), before);
        assert_eq!(fs::read(journal.unwrap()).unwrap(), b"hot staged journal");
        assert_eq!(
            recovering.disposition(),
            RecoveryDisposition::RecreatePending
        );
        leader
            .verify_exclusive_use(&recovering.roots.index_use_lock(&recovering.identity))
            .unwrap();
    }

    #[test]
    fn exceptional_activation_is_atomic_and_keeps_pending_on_ambiguous_failure() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        for fault in [
            Some(ActivationStage::BeforeRename),
            Some(ActivationStage::AfterJournalBackupBeforeDirFsync),
            Some(ActivationStage::AfterRenameBeforeDirFsync),
            None,
        ] {
            let state = tempfile::tempdir().unwrap();
            let work = tempfile::tempdir().unwrap();
            fs::write(work.path().join("a.js"), "function a() { return 1; }\n").unwrap();
            let store = Store::open_for_tests(state.path(), work.path()).unwrap();
            let options = IndexOptions::new(work.path().to_owned());
            let cancel = Arc::new(AtomicBool::new(false));
            let (graph, native, capture) =
                index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
            let original_owner = store.leader_session().unwrap();
            let old_pin = store
                .publish_native(
                    &graph,
                    &capture,
                    &native,
                    original_owner.leader_guard().unwrap(),
                    store.index_baseline().unwrap(),
                    &cancel,
                )
                .unwrap();
            store
                .put_annotation(&Annotation {
                    id: "saved".into(),
                    node_id: "unattached".into(),
                    body: "keep me".into(),
                })
                .unwrap();
            let index = store.roots.index_db(&store.identity);
            let dir = store.roots.index_dir(&store.identity);
            let leader_path = store.roots.leader_lock(&store.identity);
            let record = store.roots.record_db(&store.identity);
            let requests = dir.join("requests.db");
            let facts = dir.join("facts.db");
            let neighbor = dir.join("unrelated-record");
            fs::write(&requests, b"retained requests").unwrap();
            fs::write(&facts, b"retained facts").unwrap();
            fs::write(&neighbor, b"unrelated").unwrap();
            let durable_bytes = fs::read(&record).unwrap();
            let old_lock = fs::symlink_metadata(&leader_path).unwrap();
            drop(original_owner);
            drop(store);
            fs::write(&index, b"corrupt-index-header").unwrap();
            let old_bytes = fs::read(&index).unwrap();
            let recovering = Store::open_for_tests(state.path(), work.path()).unwrap();
            assert_eq!(
                recovering.disposition(),
                RecoveryDisposition::RecreatePending
            );
            let shared = recovering
                .roots
                .index_use_existing(&recovering.identity)
                .unwrap();
            assert!(
                recovering
                    .roots
                    .index_use_exclusive_existing(&recovering.identity)
                    .is_err()
            );
            drop(shared);
            let exclusive = recovering
                .roots
                .index_use_exclusive_existing(&recovering.identity)
                .unwrap();
            let mut leader = recovering
                .roots
                .leader_under_exclusive(&recovering.identity, exclusive)
                .unwrap();
            if fault.is_none() || fault == Some(ActivationStage::AfterJournalBackupBeforeDirFsync) {
                let journal = index.with_file_name("index.db-journal");
                fs::write(&journal, b"old journal bytes").unwrap();
                fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();
            }
            let result = recovering.recreate_index_exclusive_with_hook(
                &options,
                &mut leader,
                &cancel,
                |phase| {
                    if Some(phase) == fault {
                        anyhow::bail!("injected activation boundary");
                    }
                    Ok(())
                },
            );
            match fault {
                Some(ActivationStage::BeforeRename) => {
                    assert!(
                        result
                            .unwrap_err()
                            .to_string()
                            .contains("injected activation boundary")
                    );
                    assert_eq!(fs::read(&index).unwrap(), old_bytes);
                    let staged = fs::read_dir(&dir)
                        .unwrap()
                        .filter_map(|entry| {
                            let name = entry.unwrap().file_name().into_string().ok()?;
                            name.starts_with("index.db.tmp-").then_some(name)
                        })
                        .count();
                    assert_eq!(staged, 0);
                    let wal = index.with_file_name("index.db-wal");
                    fs::write(&wal, b"unsafe sidecar").unwrap();
                    assert!(
                        recovering
                            .recreate_index_exclusive(&options, &mut leader, &cancel)
                            .is_err()
                    );
                    assert_eq!(fs::read(&wal).unwrap(), b"unsafe sidecar");
                    assert_eq!(fs::read(&index).unwrap(), old_bytes);
                }
                Some(ActivationStage::AfterJournalBackupBeforeDirFsync) => {
                    assert!(
                        result
                            .unwrap_err()
                            .to_string()
                            .contains("injected activation boundary")
                    );
                    assert_eq!(fs::read(&index).unwrap(), old_bytes);
                    assert_eq!(
                        fs::read(index.with_file_name("index.db-journal")).unwrap(),
                        b"old journal bytes"
                    );
                    assert!(!fs::read_dir(&dir).unwrap().any(|entry| {
                        entry
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with("index.db-journal.tmp-")
                    }));
                }
                Some(ActivationStage::AfterRenameBeforeDirFsync) => {
                    assert!(
                        result
                            .unwrap_err()
                            .to_string()
                            .contains("injected activation boundary")
                    );
                    assert_ne!(fs::read(&index).unwrap(), old_bytes);
                    assert!(
                        leader
                            .verify_exclusive_use(
                                &recovering.roots.index_use_lock(&recovering.identity)
                            )
                            .is_ok()
                    );
                }
                None => {
                    let pin = result.unwrap();
                    assert_eq!(pin.index_revision, old_pin.index_revision);
                    assert_ne!(pin.index_generation, old_pin.index_generation);
                    assert_eq!(recovering.status().unwrap().revision, pin);
                    assert!(
                        recovering
                            .source_at("a.js", Some(old_pin))
                            .unwrap_err()
                            .to_string()
                            .contains("revision conflict")
                    );
                    assert!(!index.with_file_name("index.db-journal").exists());
                    assert!(
                        leader
                            .verify_exclusive_use(
                                &recovering.roots.index_use_lock(&recovering.identity)
                            )
                            .is_err()
                    );
                }
            }
            assert_eq!(fs::read(&requests).unwrap(), b"retained requests");
            assert_eq!(fs::read(&facts).unwrap(), b"retained facts");
            assert_eq!(fs::read(&neighbor).unwrap(), b"unrelated");
            assert_eq!(fs::read(&record).unwrap(), durable_bytes);
            let new_lock = fs::symlink_metadata(&leader_path).unwrap();
            assert_eq!(
                (new_lock.dev(), new_lock.ino()),
                (old_lock.dev(), old_lock.ino())
            );
            if fault.is_some() {
                assert_eq!(
                    recovering.disposition(),
                    RecoveryDisposition::RecreatePending
                );
                assert!(
                    recovering
                        .status()
                        .unwrap_err()
                        .to_string()
                        .contains("recovery_required")
                );
            }
        }
    }

    #[test]
    fn cancellation_at_before_commit_rolls_back_entire_native_class_graph_pair() {
        use crate::class_diagram::ClassDiagramRequest;
        use std::{
            sync::{atomic::Ordering, mpsc},
            thread::JoinHandle,
            time::Duration,
        };

        // The guard cancels, releases the callback, and joins on every early
        // failure, including a timeout or an assertion panic on the main thread.
        struct PublisherGuard {
            cancel: CancelFlag,
            release: Option<mpsc::Sender<()>>,
            worker: Option<JoinHandle<Result<IndexPin>>>,
        }
        impl Drop for PublisherGuard {
            fn drop(&mut self) {
                self.cancel.store(true, Ordering::Release);
                if let Some(release) = self.release.take() {
                    let _ = release.send(());
                }
                if let Some(worker) = self.worker.take() {
                    let _ = worker.join();
                }
            }
        }
        fn worker_outcome(result: std::thread::Result<Result<IndexPin>>) -> String {
            match result {
                Ok(Ok(pin)) => format!("unexpected publisher success: {pin:?}"),
                Ok(Err(error)) => format!("publisher error: {error:#}"),
                Err(panic) => format!(
                    "publisher panic: {}",
                    panic
                        .downcast_ref::<String>()
                        .map(String::as_str)
                        .or_else(|| panic.downcast_ref::<&str>().copied())
                        .unwrap_or("non-string panic payload")
                ),
            }
        }

        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("Types.java");
        let original = "class A { void go() { helper(); } void helper() {} }\nclass B {}\n";
        fs::write(&path, original).unwrap();
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let options = IndexOptions::new(work.path().to_owned());
        let initial_cancel = Arc::new(AtomicBool::new(false));
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &initial_cancel, |_| {}).unwrap();
        let session = store.leader_session().unwrap();
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                session.leader_guard().unwrap(),
                store.index_baseline().unwrap(),
                &initial_cancel,
            )
            .unwrap();
        let seed = graph
            .nodes
            .iter()
            .find(|node| node.name == "A" && node.kind == SymbolKind::Class)
            .unwrap()
            .id
            .clone();
        let question = ClassDiagramRequest {
            seed,
            expected_revision: pin,
            expanded: vec![],
            include_unmatched: false,
            include_hierarchy: false,
        };
        let old_graph = store.graph().unwrap();
        let old_diagram = serde_json::to_value(store.class_diagram_at(&question).unwrap()).unwrap();
        let old_key = native
            .revision
            .documents
            .iter()
            .find(|doc| doc.key.path == "Types.java")
            .unwrap()
            .key
            .clone();
        let old_source = store.source_at("Types.java", Some(pin)).unwrap().unwrap();
        let old_native_source = store.native_source_at(pin, &old_key).unwrap().unwrap();
        let old_coverage = store.native_coverage_at(pin, &old_key).unwrap().unwrap();
        let old_declarations = store.native_declarations_at(pin, "java", "go").unwrap();
        assert!(!old_declarations.is_empty());
        let go_owner = &old_declarations[0].syntax_id;
        let old_calls = store.native_calls_at(pin, go_owner).unwrap();
        assert!(
            !old_calls.is_empty(),
            "real Java call is paired native evidence"
        );
        let old_regions = store.native_control_regions_at(pin, go_owner).unwrap();

        let changed = format!("{original}class Added {{ void added() {{}} }}\n");
        fs::write(&path, &changed).unwrap();
        let (next, next_native, next_capture) =
            index_workspace_bundle(&options, store.root_id(), &initial_cancel, |_| {}).unwrap();
        assert!(
            next.nodes
                .iter()
                .any(|node| node.name == "Added" && node.kind == SymbolKind::Class)
        );
        // The private seam skips the public publish_native wrapper: retain each
        // wrapper admission check on this genuine immutable captured bundle.
        let canonical_work = fs::canonicalize(work.path()).unwrap();
        next_native
            .validate(
                &next_capture,
                &canonical_work,
                store.root_id(),
                &initial_cancel,
            )
            .unwrap();
        assert!(
            next_capture.graph_projection_count() == 1,
            "capture must contain exactly one graph projection"
        );
        assert!(
            next_capture.source_operations.len() == next_capture.files.len()
                && next_capture
                    .source_operations
                    .values()
                    .all(|counts| counts.opens == 1
                        && counts.complete_reads == 1
                        && counts.hashes == 1),
            "each captured source must open/read/hash exactly once"
        );
        crate::indexer::validate_native_graph(&next, &next_capture, &next_native, &initial_cancel)
            .unwrap();

        // The leader was acquired before r1; snapshot after all leader activity.
        let baseline_db = fs::read(store.roots.index_db(&store.identity)).unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel::<()>(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker_store = store.clone();
        let worker_session = session.clone();
        let worker = std::thread::spawn(move || {
            worker_store.publish_inner_checked(
                (&next, &next_capture, &next_native),
                worker_session.leader_guard().unwrap(),
                pin,
                &worker_cancel,
                |stage, tx| {
                    if stage != PublishStage::BeforeCommit {
                        return Ok(());
                    }
                    // BeforeCommit runs after graph/native/class inserts and
                    // paired validation, before the final cancellation check.
                    let new_revision: i64 = tx.query_row(
                        "SELECT index_revision FROM index_metadata WHERE singleton=1",
                        [],
                        |row| row.get(0),
                    )?;
                    ensure!(
                        new_revision == i64::try_from(pin.index_revision + 1)?,
                        "new revision not staged inside writer transaction"
                    );
                    let staged_pin = format!("pin:v1:{}:{}", pin.index_generation, pin.index_revision + 1);
                    let added: i64 = tx.query_row(
                        "SELECT count(*) FROM classes c JOIN revision_documents m ON m.class_projection_id=c.projection_id WHERE m.revision_id=?1 AND c.path='Types.java' AND c.name='Added'",
                        [&staged_pin],
                        |row| row.get(0),
                    )?;
                    ensure!(added == 1, "new class projection not staged");
                    let raw: Vec<u8> = tx.query_row(
                        "SELECT v.source_bytes FROM document_versions v JOIN revision_documents m ON m.document_version_id=v.id WHERE m.revision_id=?1 AND m.path='Types.java' AND v.path='Types.java'",
                        [&staged_pin],
                        |row| row.get(0),
                    )?;
                    ensure!(raw == changed.as_bytes(), "new native source not staged");
                    validate_paired_metadata(tx, worker_store.root_id())?;
                    validate_paired_rows(tx)?;
                    entered_tx
                        .send(())
                        .context("failed to signal BeforeCommit stage")?;
                    release_rx
                        .recv_timeout(Duration::from_secs(30))
                        .context("bounded BeforeCommit release timed out")?;
                    Ok(())
                },
            )
        });
        let mut guard = PublisherGuard {
            cancel,
            release: Some(release_tx),
            worker: Some(worker),
        };
        if let Err(wait) = entered_rx.recv_timeout(Duration::from_secs(30)) {
            let detail = if guard.worker.as_ref().is_some_and(JoinHandle::is_finished) {
                worker_outcome(guard.worker.take().unwrap().join())
            } else {
                format!("publisher still before BeforeCommit stage after bounded wait: {wait}")
            };
            panic!("publisher did not enter the staged writer transaction: {detail}");
        }
        // The worker is blocked inside its own BeforeCommit callback. Probe on
        // a second connection, not on the publisher's Connection reference.
        let probe = Connection::open(store.roots.index_db(&store.identity)).unwrap();
        probe.busy_timeout(Duration::ZERO).unwrap();
        match probe.execute_batch("BEGIN IMMEDIATE") {
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::DatabaseBusy => {}
            Ok(()) => {
                probe.execute_batch("ROLLBACK").unwrap();
                panic!("independent writer obtained lock while publisher paused BeforeCommit");
            }
            Err(error) => panic!("independent writer probe failed unexpectedly: {error}"),
        }
        guard.cancel.store(true, Ordering::Release);
        guard.release.take().unwrap().send(()).unwrap();
        let outcome = guard.worker.take().unwrap().join();
        match outcome {
            Ok(Err(error)) if error.to_string().contains("cancelled") => {}
            other => panic!(
                "publisher failed cancellation contract: {}",
                worker_outcome(other)
            ),
        }
        assert_eq!(
            fs::read(store.roots.index_db(&store.identity)).unwrap(),
            baseline_db,
            "cancelled staged r2 must leave every retained r1 SQLite byte unchanged"
        );
        assert_eq!(store.status().unwrap().revision, pin);
        assert_eq!(store.graph().unwrap(), old_graph);
        assert_eq!(
            serde_json::to_value(store.class_diagram_at(&question).unwrap()).unwrap(),
            old_diagram
        );
        assert_eq!(
            store.source_at("Types.java", Some(pin)).unwrap().unwrap(),
            old_source
        );
        assert_eq!(
            store.native_source_at(pin, &old_key).unwrap().unwrap(),
            old_native_source
        );
        assert_eq!(
            store.native_coverage_at(pin, &old_key).unwrap().unwrap(),
            old_coverage
        );
        assert_eq!(
            store.native_declarations_at(pin, "java", "go").unwrap(),
            old_declarations
        );
        assert_eq!(store.native_calls_at(pin, go_owner).unwrap(), old_calls);
        assert_eq!(
            store.native_control_regions_at(pin, go_owner).unwrap(),
            old_regions
        );
    }

    #[test]
    fn large_real_native_pair_cancels_at_before_commit_without_partial_rows() {
        use std::{
            sync::{atomic::Ordering, mpsc},
            thread::JoinHandle,
            time::Duration,
        };
        // Never leave a live writer or paused callback behind on timeout,
        // assertion failure, unexpected worker error, or worker panic.
        struct PublisherGuard {
            cancel: CancelFlag,
            release: Option<mpsc::Sender<()>>,
            worker: Option<JoinHandle<Result<IndexPin>>>,
        }
        impl Drop for PublisherGuard {
            fn drop(&mut self) {
                self.cancel.store(true, Ordering::Release);
                if let Some(release) = self.release.take() {
                    let _ = release.send(());
                }
                if let Some(worker) = self.worker.take() {
                    let _ = worker.join();
                }
            }
        }
        fn outcome(result: std::thread::Result<Result<IndexPin>>) -> String {
            match result {
                Ok(Ok(pin)) => format!("unexpected publisher success: {pin:?}"),
                Ok(Err(error)) => format!("publisher error: {error:#}"),
                Err(panic) => format!(
                    "publisher panic: {}",
                    panic
                        .downcast_ref::<String>()
                        .map(String::as_str)
                        .or_else(|| panic.downcast_ref::<&str>().copied())
                        .unwrap_or("non-string panic payload")
                ),
            }
        }

        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.js");
        let original = "function a() { b(); c(); }\nfunction b() { c(); }\nfunction c() { a(); }\n";
        fs::write(&path, original).unwrap();
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let options = IndexOptions::new(work.path().to_owned());
        let initial_cancel = Arc::new(AtomicBool::new(false));
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &initial_cancel, |_| {}).unwrap();
        let session = store.leader_session().unwrap();
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                session.leader_guard().unwrap(),
                store.index_baseline().unwrap(),
                &initial_cancel,
            )
            .unwrap();
        let old_graph = store.graph().unwrap();
        let old_source = store.source_at("a.js", Some(pin)).unwrap().unwrap();
        let old_key = native
            .revision
            .documents
            .iter()
            .find(|d| d.key.path == "a.js")
            .unwrap()
            .key
            .clone();
        let old_native_source = store.native_source_at(pin, &old_key).unwrap().unwrap();
        let old_coverage = store.native_coverage_at(pin, &old_key).unwrap().unwrap();
        let old_declarations = store
            .native_declarations_at(pin, "javascript", "a")
            .unwrap();
        assert!(!old_declarations.is_empty());
        let old_owner = &old_declarations[0].syntax_id;
        let old_calls = store.native_calls_at(pin, old_owner).unwrap();
        assert!(
            !old_calls.is_empty(),
            "old JavaScript function has measured native calls"
        );
        let old_regions = store.native_control_regions_at(pin, old_owner).unwrap();

        // Preserve the old test's 5,000 genuine JavaScript function definitions
        // and call sites rather than replacing them with synthetic graph rows.
        let large_source = (0..5_000)
            .map(|i| {
                format!(
                    "function node_{i:05}() {{ node_{:05}(); }}\n",
                    (i + 1) % 5_000
                )
            })
            .collect::<String>();
        fs::write(&path, &large_source).unwrap();
        let (next, next_native, next_capture) =
            index_workspace_bundle(&options, store.root_id(), &initial_cancel, |_| {}).unwrap();
        assert_eq!(
            next.nodes
                .iter()
                .filter(|n| n.kind == SymbolKind::Function)
                .count(),
            5_000
        );
        assert_eq!(next.calls.len(), 5_000);
        assert_eq!(
            next_native
                .declarations
                .iter()
                .filter(|d| d.kind == "function")
                .count(),
            5_000
        );
        assert_eq!(next_native.calls.len(), 5_000);
        // The private publish seam skips the public wrapper. Apply each native,
        // captured-source and graph parity check explicitly before the worker.
        let canonical_work = fs::canonicalize(work.path()).unwrap();
        next_native
            .validate(
                &next_capture,
                &canonical_work,
                store.root_id(),
                &initial_cancel,
            )
            .unwrap();
        assert_eq!(next_capture.graph_projection_count(), 1);
        assert_eq!(
            next_capture.source_operations.len(),
            next_capture.files.len()
        );
        assert!(
            next_capture
                .source_operations
                .values()
                .all(|operations| operations.opens == 1
                    && operations.complete_reads == 1
                    && operations.hashes == 1)
        );
        crate::indexer::validate_native_graph(&next, &next_capture, &next_native, &initial_cancel)
            .unwrap();

        // The baseline is captured after the sole leader acquisition and r1 commit.
        let baseline_db = fs::read(store.roots.index_db(&store.identity)).unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel::<()>(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker_store = store.clone();
        let worker_session = session.clone();
        let worker = std::thread::spawn(move || {
            worker_store.publish_inner_checked(
                (&next,&next_capture,&next_native),worker_session.leader_guard().unwrap(),pin,&worker_cancel,
                |stage,tx|{
                    if stage!=PublishStage::BeforeCommit {return Ok(());}
                    // All graph/native/class inserts and paired checks precede
                    // this callback; the final cancellation check follows it.
                    let revision:i64=tx.query_row(
                        "SELECT index_revision FROM index_metadata WHERE singleton=1",[],|r|r.get(0))?;
                    ensure!(revision==i64::try_from(pin.index_revision+1)?,
                        "new revision not staged in publisher transaction");
                    let staged_pin = format!("pin:v1:{}:{}", pin.index_generation, pin.index_revision + 1);
                    let graph_functions:i64=tx.query_row(
                        "SELECT count(*) FROM graph_nodes n JOIN revision_documents m ON m.graph_projection_id=n.projection_id WHERE m.revision_id=?1 AND m.path='a.js' AND n.path='a.js' AND json_extract(n.payload,'$.kind')='function'",[&staged_pin],|r|r.get(0))?;
                    let native_functions:i64=tx.query_row(
                        "SELECT count(*) FROM native_version_declarations d JOIN document_versions v ON v.id=d.version_id JOIN revision_documents m ON m.document_version_id=v.id WHERE m.revision_id=?1 AND m.path='a.js' AND v.path='a.js' AND d.kind='function'",[&staged_pin],|r|r.get(0))?;
                    let graph_calls:i64=tx.query_row(
                        "SELECT count(*) FROM graph_calls c JOIN revision_documents m ON m.graph_projection_id=c.projection_id WHERE m.revision_id=?1 AND m.path='a.js' AND c.path='a.js'",[&staged_pin],|r|r.get(0))?;
                    let native_calls:i64=tx.query_row(
                        "SELECT count(*) FROM native_version_calls c JOIN document_versions v ON v.id=c.version_id JOIN revision_documents m ON m.document_version_id=v.id WHERE m.revision_id=?1 AND m.path='a.js' AND v.path='a.js'",[&staged_pin],|r|r.get(0))?;
                    ensure!((graph_functions,native_functions,graph_calls,native_calls)==(5_000,5_000,5_000,5_000),
                        "5,000 measured graph/native function and call rows not staged for active pin: graph={graph_functions}/{graph_calls} native={native_functions}/{native_calls}");
                    let source_bytes:Vec<u8>=tx.query_row(
                        "SELECT v.source_bytes FROM document_versions v JOIN revision_documents m ON m.document_version_id=v.id WHERE m.revision_id=?1 AND m.path='a.js' AND v.path='a.js'",[&staged_pin],|r|r.get(0))?;
                    ensure!(source_bytes==large_source.as_bytes(),
                        "new captured native source bytes not staged");
                    validate_paired_metadata(tx,worker_store.root_id())?;
                    validate_paired_rows(tx)?;
                    entered_tx.send(()).context("cannot signal staged BeforeCommit")?;
                    release_rx.recv_timeout(Duration::from_secs(30))
                        .context("bounded BeforeCommit release timed out")?;
                    Ok(())
                },
            )
        });
        let mut guard = PublisherGuard {
            cancel,
            release: Some(release_tx),
            worker: Some(worker),
        };
        // This generous bounded wait is only failure diagnosis/cleanup for the
        // real 5,000-function pre-lock work. It is NOT lock observation.
        if let Err(wait) = entered_rx.recv_timeout(Duration::from_secs(180)) {
            let detail = if guard.worker.as_ref().is_some_and(JoinHandle::is_finished) {
                outcome(guard.worker.take().unwrap().join())
            } else {
                format!("large-source publisher still before BeforeCommit: {wait}")
            };
            panic!("publisher never reached its staged transaction: {detail}");
        }
        let probe = Connection::open(store.roots.index_db(&store.identity)).unwrap();
        probe.busy_timeout(Duration::ZERO).unwrap();
        match probe.execute_batch("BEGIN IMMEDIATE") {
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::DatabaseBusy => {}
            Ok(()) => {
                probe.execute_batch("ROLLBACK").unwrap();
                panic!("independent writer acquired lock while publisher paused BeforeCommit");
            }
            Err(error) => panic!("independent SQLite writer probe failed: {error}"),
        }
        guard.cancel.store(true, Ordering::Release);
        guard.release.take().unwrap().send(()).unwrap();
        match guard.worker.take().unwrap().join() {
            Ok(Err(error)) if error.to_string().contains("cancelled") => {}
            other => panic!(
                "large paired publisher failed cancellation contract: {}",
                outcome(other)
            ),
        }
        assert_eq!(
            fs::read(store.roots.index_db(&store.identity)).unwrap(),
            baseline_db,
            "cancelled staged 5,000-row r2 must leave every retained r1 SQLite byte unchanged"
        );
        assert_eq!(store.status().unwrap().revision, pin);
        assert_eq!(store.graph().unwrap(), old_graph);
        assert_eq!(
            store.source_at("a.js", Some(pin)).unwrap().unwrap(),
            old_source
        );
        assert_eq!(
            store.native_source_at(pin, &old_key).unwrap().unwrap(),
            old_native_source
        );
        assert_eq!(
            store.native_coverage_at(pin, &old_key).unwrap().unwrap(),
            old_coverage
        );
        assert_eq!(
            store
                .native_declarations_at(pin, "javascript", "a")
                .unwrap(),
            old_declarations
        );
        assert_eq!(store.native_calls_at(pin, old_owner).unwrap(), old_calls);
        assert_eq!(
            store.native_control_regions_at(pin, old_owner).unwrap(),
            old_regions
        );
    }

    #[test]
    fn captured_long_call_exceeds_injected_small_graph_cap_and_remains_selectable() {
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let source = format!("function hello() {{ obj.{}(); }}\n", "a".repeat(48 * 1024));
        fs::write(work.path().join("flow.js"), &source).unwrap();
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let options = IndexOptions::new(work.path().to_owned());
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
        assert!(!graph.calls.is_empty(), "test must exercise long call JSON");
        let leader = store.leader().unwrap();
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                &leader,
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        let db = Connection::open(store.roots.index_db(&store.identity)).unwrap();
        let call_bytes: i64 = db
            .query_row(
                "SELECT max(length(CAST(payload AS BLOB))) FROM graph_calls WHERE path='flow.js'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            call_bytes > 32 * 1024,
            "genuine call JSON exceeds an injected 32KiB row cap"
        );
        assert!(
            !store
                .native_declarations_at(pin, "javascript", "hello")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .source_at("flow.js", Some(pin))
                .unwrap()
                .unwrap()
                .1
                .text,
            source
        );
    }

    #[test]
    fn one_byte_over_encoded_source_cap_never_enters_paired_cas() {
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let source_path = work.path().join("flow.js");
        fs::write(&source_path, "function old() {}\n").unwrap();
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let options = IndexOptions::new(work.path().to_owned());
        let (old_graph, old_native, old_capture) =
            index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
        let leader = store.leader().unwrap();
        let pin = store
            .publish_native(
                &old_graph,
                &old_capture,
                &old_native,
                &leader,
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        let db_path = store.roots.index_db(&store.identity);
        let before = fs::read(&db_path).unwrap();
        // Raw capture stays far below the injected budget; sixfold JSON
        // escaping puts its actual canonical stored payload one byte over.
        let source = format!("/*{}*/", "\u{0001}".repeat(5450));
        fs::write(&source_path, &source).unwrap();
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
        let encoded = json(&graph.files[0]).unwrap();
        assert!(encoded.len() > 32 * 1024 && encoded.len() < 36 * 1024);
        let injected_cap = encoded.len() - 1;
        assert!(source.len() < injected_cap);
        let error = store
            .publish_inner_checked_with_source_cap(
                (&graph, &capture, &native),
                &leader,
                pin,
                &cancel,
                injected_cap,
                |_, _| panic!("encoded source must reject before opening the transaction"),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("selected source byte budget exceeded before publication"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&db_path).unwrap(),
            before,
            "pre-CAS budget refusal must preserve the exact prior SQLite bytes"
        );
        assert_eq!(store.status().unwrap().revision, pin);
        assert_eq!(
            store
                .source_at("flow.js", Some(pin))
                .unwrap()
                .unwrap()
                .1
                .text,
            "function old() {}\n"
        );
    }

    #[test]
    fn captured_sixfold_escaped_source_is_selectable_below_explicit_json_ceiling() {
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let source = format!("/*{}*/", "\u{0001}".repeat(64 * 1024));
        fs::write(work.path().join("flow.js"), &source).unwrap();
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let options = IndexOptions::new(work.path().to_owned());
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
        let leader = store.leader().unwrap();
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                &leader,
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        let db = Connection::open(store.roots.index_db(&store.identity)).unwrap();
        let json_bytes: i64 = db
            .query_row(
                "SELECT length(source_bytes) FROM document_versions WHERE path='flow.js'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            json_bytes == source.len() as i64,
            "v8 source bytes must be stored without JSON expansion"
        );
        assert_eq!(
            store
                .source_at("flow.js", Some(pin))
                .unwrap()
                .unwrap()
                .1
                .text,
            source
        );
    }

    #[test]
    fn real_source_above_injected_ancillary_cap_keeps_selected_source_available() {
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let source = format!("/*{}*/", "a".repeat(64 * 1024));
        fs::write(work.path().join("flow.js"), &source).unwrap();
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let options = IndexOptions::new(work.path().to_owned());
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
        let leader = store.leader().unwrap();
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                &leader,
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        let db = Connection::open(store.roots.index_db(&store.identity)).unwrap();
        let (version_id,class_id):(String,String)=db.query_row(
            "SELECT document_version_id,class_projection_id FROM revision_documents WHERE path='flow.js'",[],
            |r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        let (max_ancillary, total_ancillary) =
            Store::selected_ancillary_byte_usage(&db, &version_id, &class_id).unwrap();
        let source_json_bytes: i64 = db
            .query_row(
                "SELECT length(source_bytes) FROM document_versions WHERE path='flow.js'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Small injected 32KiB ancillary cap models a captured source above a
        // narrower ancillary limit without a memory-heavy 33MiB fixture.
        assert!(
            source_json_bytes > 32 * 1024
                && max_ancillary < 32 * 1024
                && total_ancillary < 32 * 1024
        );
        assert!(
            Store::selected_source_row_bounded(&db, "flow.js", 32 * 1024).is_err(),
            "an explicit narrower source API cap must fail closed"
        );
        assert_eq!(
            store
                .source_at("flow.js", Some(pin))
                .unwrap()
                .unwrap()
                .1
                .text,
            source
        );
    }
}

#[cfg(test)]
mod sqlite_schema_race_tests {
    use super::*;
    use crate::indexer::{IndexOptions, index_workspace_bundle};
    use std::{cell::RefCell, fs, os::unix::fs::MetadataExt, sync::atomic::AtomicBool};

    fn ready() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Store,
        Graph,
        crate::capture::Capture,
        crate::native_evidence::Artifact,
        IndexPin,
        CancelFlag,
        Arc<topology::LeaderSession>,
    ) {
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        fs::write(
            work.path().join("flow.js"),
            "function go() { measured(); }\n",
        )
        .unwrap();
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let options = IndexOptions::new(work.path().to_owned());
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
        let session = store.leader_session().unwrap();
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                session.leader_guard().unwrap(),
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        (
            state, work, store, graph, capture, native, pin, cancel, session,
        )
    }

    #[test]
    fn retained_two_revision_tree_survives_partial_third_publication_rollback() {
        let (_state, work, store, graph, capture, native, first, cancel, session) = ready();
        let leader = session.leader_guard().unwrap();
        let second = store
            .publish_native(&graph, &capture, &native, leader, first, &cancel)
            .unwrap();
        assert_eq!(second.index_generation, first.index_generation);
        assert_eq!(second.index_revision, first.index_revision + 1);
        fs::write(
            work.path().join("flow.js"),
            "function go() { changed(); }\n",
        )
        .unwrap();
        let (edited_graph, edited_native, edited_capture) = index_workspace_bundle(
            &IndexOptions::new(work.path().to_owned()),
            store.root_id(),
            &cancel,
            |_| {},
        )
        .unwrap();
        let path = store.roots.index_db(&store.identity);
        let before = fs::read(&path).unwrap();
        let named = fs::symlink_metadata(&path).unwrap();
        let before_inode = (named.dev(), named.ino());
        let sidecars = |p: &Path| {
            ["-wal", "-shm", "-journal"]
                .into_iter()
                .map(|suffix| {
                    let sidecar = p.with_file_name(format!("index.db{suffix}"));
                    fs::symlink_metadata(&sidecar).ok().map(|metadata| {
                        (metadata.dev(), metadata.ino(), fs::read(sidecar).unwrap())
                    })
                })
                .collect::<Vec<_>>()
        };
        let before_sidecars = sidecars(&path);
        let error = store
            .publish_inner_checked(
                (&edited_graph, &edited_capture, &edited_native),
                leader,
                second,
                &cancel,
                |stage, tx| {
                    if stage == PublishStage::AfterFile {
                        // The new r3 header/manifest has already been inserted;
                        // force an actual SQLite error inside its transaction.
                        tx.execute(
                            "INSERT INTO native_revisions SELECT * FROM native_revisions WHERE published_index_revision=?1",
                            [second.index_revision as i64],
                        )?;
                    }
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("UNIQUE constraint failed"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "partial r3 write changed r1 or r2 bytes"
        );
        let named = fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            (named.dev(), named.ino()),
            before_inode,
            "r3 rollback must preserve original SQLite inode"
        );
        assert_eq!(
            sidecars(&path),
            before_sidecars,
            "r3 rollback must preserve SQLite sidecar bytes and inodes"
        );
        assert_eq!(store.status().unwrap().revision, second);
        let db = Connection::open(path).unwrap();
        let headers: i64 = db
            .query_row("SELECT count(*) FROM native_revisions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(headers, 2);
        assert_eq!(
            store
                .source_at("flow.js", Some(first))
                .unwrap()
                .unwrap()
                .1
                .text,
            "function go() { measured(); }\n"
        );
        assert_eq!(
            store
                .source_at("flow.js", Some(second))
                .unwrap()
                .unwrap()
                .1
                .text,
            "function go() { measured(); }\n"
        );
    }

    #[test]
    fn response_post_fence_discards_none_and_empty_results() {
        let (_state, _work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let leader_path = store.roots.leader_lock(&store.identity);
        let none = store.with_evidence_hook(
            |_db| Ok(None::<IndexStatus>),
            || {
                std::fs::write(&leader_path, uuid::Uuid::new_v4().to_string())?;
                Ok(())
            },
        );
        assert!(none.is_err(), "post-fence failure must discard None");

        let (_state, _work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let leader_path = store.roots.leader_lock(&store.identity);
        let empty = store.with_evidence_hook(
            |_db| Ok(Vec::<Symbol>::new()),
            || {
                std::fs::write(&leader_path, uuid::Uuid::new_v4().to_string())?;
                Ok(())
            },
        );
        assert!(
            empty.is_err(),
            "post-fence failure must discard an empty collection"
        );
    }

    #[test]
    fn direct_response_pre_fence_rejects_root_lock_and_incarnation_changes() {
        use std::os::unix::fs::PermissionsExt;
        let (_state, work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let original = work.path().to_owned();
        let moved = original.with_extension("moved-before-read");
        std::fs::rename(&original, &moved).unwrap();
        std::fs::create_dir(&original).unwrap();
        let root_error = store.status().unwrap_err();
        assert!(
            root_error.to_string().contains("root_changed"),
            "{root_error:#}"
        );
        std::fs::remove_dir(&original).unwrap();
        std::fs::rename(&moved, &original).unwrap();

        let (_state, _work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let leader_path = store.roots.leader_lock(&store.identity);
        std::fs::remove_file(&leader_path).unwrap();
        std::fs::write(&leader_path, uuid::Uuid::new_v4().to_string()).unwrap();
        std::fs::set_permissions(&leader_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let lock_error = store.symbol_at("missing", None).unwrap_err();
        assert!(
            lock_error.to_string().contains("managed file")
                || lock_error.to_string().contains("incarnation")
                || lock_error.to_string().contains("leader lock is not held"),
            "{lock_error:#}"
        );

        let (_state, _work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let leader_path = store.roots.leader_lock(&store.identity);
        std::fs::write(&leader_path, uuid::Uuid::new_v4().to_string()).unwrap();
        let incarnation_error = store.symbols_at("never", 10).unwrap_err();
        assert!(
            incarnation_error.to_string().contains("incarnation")
                || incarnation_error.to_string().contains("mismatch"),
            "{incarnation_error:#}"
        );
    }

    #[test]
    fn response_fences_root_lock_and_error_results_before_release() {
        use std::os::unix::fs::PermissionsExt;
        let (_state, work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let original = work.path().to_owned();
        let moved = original.with_extension("moved-for-fence");
        let root_error = store
            .with_evidence_hook(
                |_db| Ok(Some("owned".to_owned())),
                || {
                    std::fs::rename(&original, &moved)?;
                    std::fs::create_dir(&original)?;
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(
            root_error.to_string().contains("root_changed"),
            "{root_error:#}"
        );
        std::fs::remove_dir(&original).unwrap();
        std::fs::rename(&moved, &original).unwrap();

        let (_state, _work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let leader_path = store.roots.leader_lock(&store.identity);
        let lock_error = store
            .with_evidence_hook(
                |_db| Ok(Vec::<Symbol>::new()),
                || {
                    std::fs::remove_file(&leader_path)?;
                    std::fs::write(&leader_path, uuid::Uuid::new_v4().to_string())?;
                    std::fs::set_permissions(&leader_path, std::fs::Permissions::from_mode(0o600))?;
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(
            lock_error.to_string().contains("managed file")
                || lock_error.to_string().contains("incarnation"),
            "{lock_error:#}"
        );

        let (_state, _work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let leader_path = store.roots.leader_lock(&store.identity);
        let finish_observed = std::cell::Cell::new(false);
        let error = store
            .with_evidence_observed::<()>(
                |_db| anyhow::bail!("materialization sentinel"),
                || {
                    std::fs::write(&leader_path, uuid::Uuid::new_v4().to_string())?;
                    Ok(())
                },
                |fence| {
                    finish_observed.set(true);
                    assert!(
                        fence.is_err(),
                        "concurrent incarnation change must fail the final fence"
                    );
                },
            )
            .unwrap_err();
        assert!(finish_observed.get(), "final fence was not attempted");
        assert!(
            error.to_string().contains("materialization sentinel"),
            "original materialization error must win: {error:#}"
        );

        let (_state, _work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let leader_path = store.roots.leader_lock(&store.identity);
        let finish_observed = std::cell::Cell::new(false);
        let io_error = store
            .with_evidence_observed::<()>(
                |_db| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into()),
                || {
                    std::fs::write(&leader_path, uuid::Uuid::new_v4().to_string())?;
                    Ok(())
                },
                |fence| {
                    finish_observed.set(true);
                    assert!(fence.is_err());
                },
            )
            .unwrap_err();
        assert!(finish_observed.get());
        assert!(
            io_error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
        );
        assert!(!native_index_unavailable(&io_error));
    }

    #[test]
    fn tree_metadata_applies_no_overlay_before_post_fence() {
        let (_state, work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let mut entries = vec![crate::file_tree::Entry {
            name: "flow.js".into(),
            path: "flow.js".into(),
            kind: "file",
            indexed_path: None,
            method_count: None,
            unindexed_reason: None,
        }];
        let leader_path = store.roots.leader_lock(&store.identity);
        let error = store
            .tree_metadata_with_finish_hook(work.path(), &mut entries, || {
                std::fs::write(&leader_path, uuid::Uuid::new_v4().to_string())?;
                Ok(())
            })
            .unwrap_err();
        assert!(error.to_string().contains("incarnation"), "{error:#}");
        assert!(entries[0].indexed_path.is_none());
        assert!(entries[0].method_count.is_none());
    }

    #[test]
    fn durable_fallback_classifier_excludes_operational_and_current_corruption_errors() {
        let operational: anyhow::Error =
            std::io::Error::from(std::io::ErrorKind::PermissionDenied).into();
        assert!(!native_index_unavailable(&operational));
        let sqlite: anyhow::Error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
            None,
        )
        .into();
        assert!(!native_index_unavailable(&sqlite));
        let corruption: anyhow::Error =
            ControlIntegrity("incompatible_index: current marker".into()).into();
        assert!(!native_index_unavailable(&corruption));
        let safe: anyhow::Error = topology::IndexNotReady::new("reconciliation required").into();
        assert!(native_index_unavailable(&safe));
    }

    #[test]
    fn recovery_classifier_separates_corruption_from_operational_storage_errors() {
        let classify = |extended_code| {
            let error: anyhow::Error =
                rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(extended_code), None)
                    .into();
            recovery_class(&error)
        };
        assert_eq!(
            classify(rusqlite::ffi::SQLITE_CORRUPT),
            RecoveryClass::RecreatePending
        );
        assert_eq!(
            classify(rusqlite::ffi::SQLITE_NOTADB),
            RecoveryClass::RecreatePending
        );
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_FULL,
            rusqlite::ffi::SQLITE_READONLY,
            rusqlite::ffi::SQLITE_CANTOPEN,
            rusqlite::ffi::SQLITE_IOERR,
            rusqlite::ffi::SQLITE_NOMEM,
            rusqlite::ffi::SQLITE_ERROR,
        ] {
            assert_eq!(classify(code), RecoveryClass::Hard);
        }
    }

    #[test]
    fn live_read_reporter_separates_root_storage_corruption_from_operational_errors() {
        let sqlite = |code| -> anyhow::Error {
            rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None).into()
        };
        for code in [rusqlite::ffi::SQLITE_CORRUPT, rusqlite::ffi::SQLITE_NOTADB] {
            let (_state, _work, store, _graph, _capture, _native, _pin, _cancel, _session) =
                ready();
            let clone = store.clone();
            let error = store.report_live_read_failure(sqlite(code));
            assert!(
                error.to_string().starts_with("recovery_required:"),
                "{error:#}"
            );
            assert_eq!(store.disposition(), RecoveryDisposition::RecreatePending);
            assert!(
                clone
                    .status()
                    .unwrap_err()
                    .to_string()
                    .starts_with("recovery_required:")
            );
        }
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_IOERR,
            rusqlite::ffi::SQLITE_ERROR,
        ] {
            let (_state, _work, store, _graph, _capture, _native, pin, _cancel, _session) = ready();
            let error = store.report_live_read_failure(sqlite(code));
            assert!(error.downcast_ref::<rusqlite::Error>().is_some());
            assert_eq!(store.status().unwrap().revision, pin);
        }
        let (_state, _work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let clone = store.clone();
        let error = store.report_live_read_failure(
            rusqlite::Error::InvalidColumnType(
                0,
                "root_spelling".into(),
                rusqlite::types::Type::Real,
            )
            .into(),
        );
        assert!(error.to_string().starts_with("incompatible_index:"));
        assert!(
            clone
                .status()
                .unwrap_err()
                .to_string()
                .contains("incompatible_index")
        );

        let (_state, _work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let clone = store.clone();
        let error = store.report_live_read_failure(
            ControlIntegrity("incompatible_index: existing structural check".into()).into(),
        );
        assert!(error.to_string().starts_with("incompatible_index:"));
        assert!(
            clone
                .status()
                .unwrap_err()
                .to_string()
                .contains("incompatible_index")
        );

        let (_state, _work, store, _graph, _capture, _native, pin, _cancel, _session) = ready();
        let error = store.report_live_read_failure(anyhow::anyhow!(
            "root_key_collision: index belongs to a different spelling"
        ));
        assert!(error.to_string().starts_with("root_key_collision:"));
        assert_eq!(store.status().unwrap().revision, pin);
    }

    #[test]
    fn selected_operational_errors_do_not_latch_but_integrity_failure_closes_clones() {
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_FULL,
            rusqlite::ffi::SQLITE_READONLY,
            rusqlite::ffi::SQLITE_CANTOPEN,
            rusqlite::ffi::SQLITE_IOERR,
            rusqlite::ffi::SQLITE_NOMEM,
            rusqlite::ffi::SQLITE_ERROR,
        ] {
            let (_state, _work, store, _graph, _capture, _native, pin, _cancel, _session) = ready();
            let clone = store.clone();
            let operational: anyhow::Error =
                rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None).into();
            let returned = store.report_selected_failure(operational);
            assert!(returned.downcast_ref::<rusqlite::Error>().is_some());
            assert_eq!(store.status().unwrap().revision, pin);
            assert_eq!(clone.status().unwrap().revision, pin);
        }

        let (_state, _work, store, _graph, _capture, _native, pin, _cancel, _session) = ready();
        let clone = store.clone();
        let io: anyhow::Error =
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "injected").into();
        let returned = store.report_selected_failure(io);
        assert!(returned.downcast_ref::<std::io::Error>().is_some());
        assert_eq!(store.status().unwrap().revision, pin);
        assert_eq!(clone.status().unwrap().revision, pin);

        let integrity: anyhow::Error = SelectedIntegrity("selected mismatch".into()).into();
        let returned = store.report_selected_failure(integrity);
        assert!(returned.to_string().contains("incompatible_index"));
        assert!(store.status().is_err());
        assert!(clone.status().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn persisted_scan_classifies_same_size_preserved_mtime_edit_by_ctime() {
        use std::ffi::CString;
        use std::os::unix::fs::MetadataExt;

        let (_state, work, store, _graph, _capture, _native, _pin, cancel, _session) = ready();
        let path = work.path().join("flow.js");
        let before_meta = fs::metadata(&path).unwrap();
        let before_stat: crate::capture::CaptureStat = {
            let db = store.cache().unwrap();
            let payload: String = db
                .query_row(
                    "SELECT capture_stat FROM revision_documents WHERE path='flow.js'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            serde_json::from_str(&payload).unwrap()
        };
        fs::write(&path, "function go() { observed(); }\n").unwrap();
        let c_path = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let times = [
            libc::timespec {
                tv_sec: before_meta.atime(),
                tv_nsec: before_meta.atime_nsec(),
            },
            libc::timespec {
                tv_sec: before_meta.mtime(),
                tv_nsec: before_meta.mtime_nsec(),
            },
        ];
        assert_eq!(
            unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) },
            0
        );
        let options = IndexOptions::new(work.path().to_owned());
        let (_next, _native, capture) =
            index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
        let after_stat = capture.source_stat("flow.js").unwrap();
        assert_eq!(before_stat.size, after_stat.size);
        assert_eq!(before_stat.device, after_stat.device);
        assert_eq!(before_stat.inode, after_stat.inode);
        assert_eq!(before_stat.mtime_seconds, after_stat.mtime_seconds);
        assert_eq!(before_stat.mtime_nanoseconds, after_stat.mtime_nanoseconds);
        assert_ne!(
            (before_stat.ctime_seconds, before_stat.ctime_nanoseconds),
            (after_stat.ctime_seconds, after_stat.ctime_nanoseconds)
        );
        let db = store.cache().unwrap();
        let comparison = compare_capture_snapshot(&db, &capture).unwrap();
        assert_eq!(comparison.ctime_changed, 1);
        assert_eq!(comparison.hash_changed, 1);
        assert!(comparison.changed());
        assert_eq!(capture.files[0].text, "function go() { observed(); }\n");
        assert_eq!(
            capture.source_operations["flow.js"],
            crate::capture::SourceOperations {
                opens: 1,
                complete_reads: 1,
                hashes: 1,
            }
        );
    }

    #[test]
    fn real_revision_private_recovery_before_commit_error_rolls_back_same_inode() {
        use rusqlite::types::ValueRef;
        use std::os::unix::fs::MetadataExt;

        let (_state, _work, store, graph, capture, native, _old, cancel, session) = ready();
        let clone = store.clone();
        let path = store.roots.index_db(&store.identity);
        let inode = fs::metadata(&path).unwrap().ino();
        let db = Connection::open(&path).unwrap();
        db.execute(
            "UPDATE index_metadata SET index_revision=CAST(1.5 AS REAL)",
            [],
        )
        .unwrap();
        drop(db);

        let logical = || {
            let db = Connection::open(&path).unwrap();
            let (schema, extractor, generation, files, nodes, documents, revisions, classes):
                (i64, String, String, i64, i64, i64, i64, i64) = db
                .query_row(
                    "SELECT schema_version,extractor_version,index_generation,(SELECT count(*) FROM revision_documents),(SELECT count(*) FROM graph_nodes),(SELECT count(*) FROM document_versions),(SELECT count(*) FROM native_revisions),(SELECT count(*) FROM class_projections) FROM index_metadata",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
                )
                .unwrap();
            let revision_bits = db
                .query_row(
                    "SELECT index_revision FROM index_metadata",
                    [],
                    |row| match row.get_ref(0)? {
                        ValueRef::Real(value) => Ok(value.to_bits()),
                        _ => Err(rusqlite::Error::InvalidQuery),
                    },
                )
                .unwrap();
            assert!(files > 0, "rollback fixture must contain a file row");
            assert!(
                documents > 0,
                "rollback fixture must contain a native document row"
            );
            let selected_file: (String, String) = db
                .query_row(
                    "SELECT content_hash,path FROM document_versions ORDER BY path LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let selected_native: (String, Vec<u8>) = db
                .query_row(
                    "SELECT content_hash,source_bytes FROM document_versions ORDER BY language,path LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            (
                schema,
                extractor,
                generation,
                revision_bits,
                files,
                nodes,
                documents,
                revisions,
                classes,
                selected_file,
                selected_native,
            )
        };
        let before = logical();
        let baseline = {
            let mut db = store.cache().unwrap();
            let tx = db.transaction().unwrap();
            store.recovery_baseline(&tx).unwrap()
        };
        assert!(baseline.pin().is_none());
        let error = store
            .publish_inner_checked_expected(
                (&graph, &capture, &native),
                session.leader_guard().unwrap(),
                ExpectedPublication::Recovery(Box::new(baseline)),
                &cancel,
                256 * 1024 * 1024 + 16 * 1024,
                |stage, _tx| {
                    if stage == PublishStage::BeforeCommit {
                        anyhow::bail!("injected private metadata recovery failure");
                    }
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected private metadata recovery failure"),
            "{error:#}"
        );
        assert_eq!(logical(), before, "failed recovery changed logical pair");
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert!(
            store
                .status()
                .unwrap_err()
                .to_string()
                .contains("incompatible_index")
        );
        assert!(
            clone
                .status()
                .unwrap_err()
                .to_string()
                .contains("incompatible_index")
        );
    }

    #[test]
    fn failed_takeover_before_writer_transaction_closes_every_clone() {
        let (state, work, store, _graph, _capture, _native, pin, _cancel, session) = ready();
        let clone = store.clone();
        drop(session);
        let error = store
            .leader_with_open_hook(|_| anyhow::bail!("injected pre-transaction takeover failure"))
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "injected pre-transaction takeover failure"
        );
        assert!(store.status().is_err());
        assert!(clone.status().is_err());
        let separately_opened = Store::open_for_tests(state.path(), work.path()).unwrap();
        assert_eq!(separately_opened.index_baseline().unwrap(), pin);
        assert!(!separately_opened.recovery_required.load(Ordering::Acquire));
    }

    #[test]
    fn second_connection_adds_legacy_trigger_after_admission_before_publish_lock() {
        let (_state, _work, store, graph, capture, native, old, cancel, session) = ready();
        let clone = store.clone();
        let path = store.roots.index_db(&store.identity);
        let after_external = RefCell::new(None);
        let error = store.publish_inner_checked((&graph, &capture, &native), session.leader_guard().unwrap(), old, &cancel,
            |stage, _checked_connection| {
                if stage == PublishStage::BeforeTransaction {
                    // The Store connection has passed open_index's exact object check,
                    // but has NOT acquired the SQLite writer lock yet.
                    let attacker = Connection::open(&path)?;
                    attacker.execute_batch("CREATE TRIGGER forged_after_admission AFTER INSERT ON graph_calls BEGIN
                        UPDATE graph_calls SET payload=json_set(payload,'$.calleeText','FORGED-NOT-MEASURED') WHERE projection_id=NEW.projection_id AND id=NEW.id; END;")?;
                    drop(attacker);
                    *after_external.borrow_mut() = Some(fs::read(&path)?);
                }
                Ok(())
            }).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("incompatible_index: unknown cache object"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            after_external.into_inner().unwrap(),
            "Store must not change DB bytes after the external CREATE TRIGGER"
        );
        let db = Connection::open(&path).unwrap();
        let (schema, marker, generation, revision): (i64, String, String, i64) = db.query_row(
            "SELECT schema_version,extractor_version,index_generation,index_revision FROM index_metadata", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(
            (schema, marker.as_str(), generation, revision),
            (
                8,
                "native-v4-delta-v1",
                old.index_generation.to_string(),
                old.index_revision as i64
            )
        );
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            8
        );
        let forged: i64 = db
            .query_row(
                "SELECT count(*) FROM graph_calls WHERE payload LIKE '%FORGED-NOT-MEASURED%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(forged, 0, "post-admission trigger must never execute");
        assert!(
            store
                .status()
                .unwrap_err()
                .to_string()
                .contains("incompatible_index")
        );
        db.execute_batch("DROP TRIGGER forged_after_admission")
            .unwrap();
        drop(db);
        for closed in [store.status().unwrap_err(), clone.status().unwrap_err()] {
            assert_eq!(
                closed.to_string(),
                "incompatible_index: reconciliation required after invalid current index"
            );
        }
    }

    #[test]
    fn changed_metadata_between_admission_and_leader_lock_refuses_before_update() {
        let (_state, _work, store, _graph, _capture, _native, pin, _cancel, session) = ready();
        let path = store.roots.index_db(&store.identity);
        let after_external = RefCell::new(None);
        drop(session);
        let error = store
            .leader_with_open_hook(|_checked| {
                let attacker = Connection::open(&path)?;
                attacker.execute(
                    "UPDATE index_metadata SET last_opened_at=last_opened_at+1",
                    [],
                )?;
                drop(attacker);
                *after_external.borrow_mut() = Some(fs::read(&path)?);
                Ok(())
            })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("incompatible_index: cache changed after admission"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            after_external.into_inner().unwrap()
        );
        assert_eq!(store.index_baseline().unwrap(), pin);
        let closed = store.status().unwrap_err();
        assert!(closed.to_string().contains("index_not_ready"), "{closed:#}");
    }

    #[test]
    fn second_connection_adds_view_after_admission_before_status_and_leader_snapshot() {
        let (_state, _work, store, _graph, _capture, _native, _pin, _cancel, _session) = ready();
        let status_clone = store.clone();
        let path = store.roots.index_db(&store.identity);
        let after_status_ddl = RefCell::new(None);
        let status_error = store
            .status_with_open_hook(|_checked| {
                let attacker = Connection::open(&path)?;
                attacker.execute_batch("CREATE VIEW status_after_admission AS SELECT 1")?;
                drop(attacker);
                *after_status_ddl.borrow_mut() = Some(fs::read(&path)?);
                Ok(())
            })
            .unwrap_err();
        assert!(
            status_error
                .to_string()
                .contains("incompatible_index: unknown cache object")
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            after_status_ddl.into_inner().unwrap()
        );
        let attacker = Connection::open(&path).unwrap();
        attacker
            .execute_batch("DROP VIEW status_after_admission")
            .unwrap();
        drop(attacker);
        for closed in [
            store.status().unwrap_err(),
            status_clone.status().unwrap_err(),
        ] {
            assert_eq!(
                closed.to_string(),
                "incompatible_index: reconciliation required after invalid current index"
            );
        }

        let (
            _leader_state,
            _leader_work,
            leader_store,
            _leader_graph,
            _leader_capture,
            _leader_native,
            leader_pin,
            _leader_cancel,
            leader_session,
        ) = ready();
        let leader_clone = leader_store.clone();
        let leader_path = leader_store.roots.index_db(&leader_store.identity);
        let after_leader_ddl = RefCell::new(None);
        drop(leader_session);
        let leader_error = leader_store
            .leader_with_open_hook(|_checked| {
                let attacker = Connection::open(&leader_path)?;
                attacker.execute_batch("CREATE VIEW leader_after_admission AS SELECT 1")?;
                drop(attacker);
                *after_leader_ddl.borrow_mut() = Some(fs::read(&leader_path)?);
                Ok(())
            })
            .unwrap_err();
        assert!(
            leader_error
                .to_string()
                .contains("incompatible_index: unknown cache object")
        );
        assert_eq!(
            fs::read(&leader_path).unwrap(),
            after_leader_ddl.into_inner().unwrap(),
            "leader metadata update must not write after external DDL"
        );
        let attacker = Connection::open(&leader_path).unwrap();
        let (schema, marker, generation, revision): (i64,String,String,i64) = attacker.query_row(
            "SELECT schema_version,extractor_version,index_generation,index_revision FROM index_metadata", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(
            (schema, marker.as_str(), generation, revision),
            (
                8,
                "native-v4-delta-v1",
                leader_pin.index_generation.to_string(),
                leader_pin.index_revision as i64
            )
        );
        assert!(
            leader_store
                .status()
                .unwrap_err()
                .to_string()
                .contains("incompatible_index: unknown cache object type, name or shape")
        );
        attacker
            .execute_batch("DROP VIEW leader_after_admission")
            .unwrap();
        drop(attacker);
        for closed in [
            leader_store.status().unwrap_err(),
            leader_clone.status().unwrap_err(),
        ] {
            assert_eq!(
                closed.to_string(),
                "incompatible_index: reconciliation required after invalid current index"
            );
        }
    }
}

#[cfg(test)]
mod selected_source_budget_tests {
    use super::*;
    use crate::indexer::{IndexOptions, index_workspace_bundle};
    use std::{fs, sync::atomic::AtomicBool};
    #[test]
    fn selected_length_guard_precedes_json_and_blob_decode() {
        let state = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("one.js"), "function one() {}\n").unwrap();
        let store = Store::open_for_tests(state.path(), root.path()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (graph, native, capture) = index_workspace_bundle(
            &IndexOptions::new(root.path().into()),
            store.root_id(),
            &cancel,
            |_| {},
        )
        .unwrap();
        store
            .publish_native(
                &graph,
                &capture,
                &native,
                &store.leader().unwrap(),
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        let db = Connection::open(store.roots.index_db(&store.identity)).unwrap();
        let (node_id, original): (String, String) = db
            .query_row(
                "SELECT id,payload FROM graph_nodes WHERE path='one.js' LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        db.execute(
            "UPDATE graph_nodes SET payload=?1 WHERE id=?2 AND projection_id=(SELECT graph_projection_id FROM revision_documents WHERE path='one.js')",
            params!["not-valid-graph-json".repeat(2000), node_id],
        )
        .unwrap();
        assert!(
            store
                .attest_selected_document(&db, "one.js")
                .unwrap_err()
                .to_string()
                .contains("graph row byte budget exceeded")
        );
        db.execute(
            "UPDATE graph_nodes SET payload=?1 WHERE id=?2 AND projection_id=(SELECT graph_projection_id FROM revision_documents WHERE path='one.js')",
            params![original, node_id],
        )
        .unwrap();
        // A tiny test limit exercises the same SQL size gate without a 256 MiB fixture.
        assert!(
            Store::selected_source_row_bounded(&db, "one.js", 4)
                .unwrap_err()
                .to_string()
                .contains("byte budget exceeded")
        );
        db.execute(
            "UPDATE document_versions SET source_bytes=?1,byte_length=length(?1) WHERE id=(SELECT document_version_id FROM revision_documents WHERE path='one.js')",
            ["not-valid-json".repeat(2000)],
        )
        .unwrap();
        assert!(
            Store::selected_source_row_bounded(&db, "one.js", 64)
                .unwrap_err()
                .to_string()
                .contains("byte budget exceeded")
        );
        db.execute(
            "UPDATE document_versions SET source_bytes=x'ff',byte_length=1 WHERE id=(SELECT document_version_id FROM revision_documents WHERE path='one.js')",
            [],
        )
        .unwrap();
        assert!(
            !Store::selected_source_row_bounded(&db, "one.js", 256 * 1024 * 1024)
                .unwrap_err()
                .to_string()
                .contains("byte budget exceeded")
        );
    }
}
