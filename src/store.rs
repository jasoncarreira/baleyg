//! SQLite snapshots and durable user data. Connections are never shared between threads.
pub mod anchors;
pub mod topology;
use crate::model::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::{Deref, DerefMut},
    path::Path,
    sync::Arc,
    sync::atomic::Ordering,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct Store {
    roots: topology::TopologyRoots,
    identity: Arc<topology::WorkspaceIdentity>,
    workspace_root: String,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum PublishStage {
    BeforeTransaction,
    AfterFile,
    BeforeCommit,
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
SELECT COALESCE(length(CAST(k.id AS BLOB)),0)+COALESCE(length(CAST(k.name AS BLOB)),0)+COALESCE(length(CAST(k.qualified_name AS BLOB)),0)+COALESCE(length(CAST(k.path AS BLOB)),0)+COALESCE(length(CAST(k.payload AS BLOB)),0) AS row_bytes FROM classes k WHERE k.path=?1
UNION ALL
SELECT COALESCE(length(CAST(r.id AS BLOB)),0)+COALESCE(length(CAST(r.owner AS BLOB)),0)+COALESCE(length(CAST(r.target AS BLOB)),0)+COALESCE(length(CAST(r.payload AS BLOB)),0) AS row_bytes FROM class_relations r JOIN classes k ON k.id=r.owner WHERE k.path=?1
UNION ALL
SELECT COALESCE(length(CAST(p.id AS BLOB)),0)+COALESCE(length(CAST(p.version AS BLOB)),0)+COALESCE(length(CAST(p.executable_hash AS BLOB)),0)+COALESCE(length(CAST(p.kind AS BLOB)),0)+COALESCE(length(CAST(p.position_encoding AS BLOB)),0) AS row_bytes FROM native_producers p WHERE p.id=(SELECT id FROM native_producers LIMIT 1)
UNION ALL
SELECT COALESCE(length(CAST(p.producer_id AS BLOB)),0)+COALESCE(length(CAST(p.language AS BLOB)),0) AS row_bytes FROM native_producer_languages p WHERE p.producer_id=(SELECT id FROM native_producers LIMIT 1)
UNION ALL
SELECT COALESCE(length(CAST(s.id AS BLOB)),0)+COALESCE(length(CAST(s.root_id AS BLOB)),0) AS row_bytes FROM native_source_sets s WHERE s.id=?2
UNION ALL
SELECT COALESCE(length(CAST(s.source_set_id AS BLOB)),0)+COALESCE(length(CAST(s.language AS BLOB)),0) AS row_bytes FROM native_source_set_languages s WHERE s.source_set_id=?2
UNION ALL
SELECT COALESCE(length(CAST(s.source_set_id AS BLOB)),0)+COALESCE(length(CAST(s.dependency_id AS BLOB)),0) AS row_bytes FROM native_source_set_dependencies s WHERE s.source_set_id=?2
UNION ALL
SELECT COALESCE(length(CAST(r.id AS BLOB)),0)+COALESCE(length(CAST(r.source_set_id AS BLOB)),0)+COALESCE(length(CAST(r.toolchain_hash AS BLOB)),0)+COALESCE(length(CAST(r.config_hash AS BLOB)),0)+COALESCE(length(CAST(r.dependency_hash AS BLOB)),0) AS row_bytes FROM native_revisions r WHERE r.id=?4 AND r.source_set_id=?2
UNION ALL
SELECT COALESCE(length(CAST(d.source_set_id AS BLOB)),0)+COALESCE(length(CAST(d.language AS BLOB)),0)+COALESCE(length(CAST(d.path AS BLOB)),0)+COALESCE(length(CAST(d.revision_id AS BLOB)),0)+COALESCE(length(CAST(d.content_hash AS BLOB)),0) AS row_bytes FROM native_documents d WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1
UNION ALL
SELECT COALESCE(length(CAST(v.producer_id AS BLOB)),0)+COALESCE(length(CAST(v.language AS BLOB)),0)+COALESCE(length(CAST(v.source_set_id AS BLOB)),0)+COALESCE(length(CAST(v.document_path AS BLOB)),0)+COALESCE(length(CAST(v.revision_id AS BLOB)),0)+COALESCE(length(CAST(v.state AS BLOB)),0)+COALESCE(length(CAST(v.diagnostic AS BLOB)),0) AS row_bytes FROM native_coverage v WHERE v.source_set_id=?2 AND v.language=?3 AND v.revision_id=?4 AND v.document_path=?1
UNION ALL
SELECT COALESCE(length(CAST(v.producer_id AS BLOB)),0)+COALESCE(length(CAST(v.revision_id AS BLOB)),0)+COALESCE(length(CAST(v.language AS BLOB)),0)+COALESCE(length(CAST(v.document_path AS BLOB)),0)+COALESCE(length(CAST(v.role_kind AS BLOB)),0)+COALESCE(length(CAST(v.role AS BLOB)),0) AS row_bytes FROM native_coverage_roles v WHERE v.revision_id=?4 AND v.language=?3 AND v.document_path=?1 AND v.producer_id=(SELECT id FROM native_producers LIMIT 1)
UNION ALL
SELECT COALESCE(length(CAST(v.id AS BLOB)),0)+COALESCE(length(CAST(v.producer_id AS BLOB)),0)+COALESCE(length(CAST(v.source_set_id AS BLOB)),0)+COALESCE(length(CAST(v.language AS BLOB)),0)+COALESCE(length(CAST(v.path AS BLOB)),0)+COALESCE(length(CAST(v.revision_id AS BLOB)),0)+COALESCE(length(CAST(v.content_hash AS BLOB)),0)+COALESCE(length(CAST(v.evidence_kind AS BLOB)),0)+COALESCE(length(CAST(v.basis AS BLOB)),0)+COALESCE(length(CAST(v.derived_from AS BLOB)),0)+COALESCE(length(CAST(v.freshness AS BLOB)),0) AS row_bytes FROM native_provenance v WHERE v.source_set_id=?2 AND v.language=?3 AND v.revision_id=?4 AND v.path=?1
UNION ALL
SELECT COALESCE(length(CAST(d.syntax_id AS BLOB)),0)+COALESCE(length(CAST(d.source_set_id AS BLOB)),0)+COALESCE(length(CAST(d.language AS BLOB)),0)+COALESCE(length(CAST(d.path AS BLOB)),0)+COALESCE(length(CAST(d.revision_id AS BLOB)),0)+COALESCE(length(CAST(d.owner_syntax_id AS BLOB)),0)+COALESCE(length(CAST(d.kind AS BLOB)),0)+COALESCE(length(CAST(d.name AS BLOB)),0)+COALESCE(length(CAST(d.lookup_key AS BLOB)),0)+COALESCE(length(CAST(d.provenance_id AS BLOB)),0) AS row_bytes FROM native_declarations d WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1
UNION ALL
SELECT COALESCE(length(CAST(a.syntax_id AS BLOB)),0)+COALESCE(length(CAST(a.kind AS BLOB)),0)+COALESCE(length(CAST(a.name AS BLOB)),0) AS row_bytes FROM native_declaration_ancestors a JOIN native_declarations d ON d.syntax_id=a.syntax_id WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1
UNION ALL
SELECT COALESCE(length(CAST(a.syntax_id AS BLOB)),0)+COALESCE(length(CAST(a.type_name AS BLOB)),0) AS row_bytes FROM native_signature_parameter_types a JOIN native_declarations d ON d.syntax_id=a.syntax_id WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1
UNION ALL
SELECT COALESCE(length(CAST(a.syntax_id AS BLOB)),0)+COALESCE(length(CAST(a.kind AS BLOB)),0)+COALESCE(length(CAST(a.name AS BLOB)),0)+COALESCE(length(CAST(a.result_type AS BLOB)),0) AS row_bytes FROM native_headers a JOIN native_declarations d ON d.syntax_id=a.syntax_id WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1
UNION ALL
SELECT COALESCE(length(CAST(a.syntax_id AS BLOB)),0)+COALESCE(length(CAST(a.item_kind AS BLOB)),0)+COALESCE(length(CAST(a.value AS BLOB)),0) AS row_bytes FROM native_header_items a JOIN native_declarations d ON d.syntax_id=a.syntax_id WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1
UNION ALL
SELECT COALESCE(length(CAST(a.syntax_id AS BLOB)),0)+COALESCE(length(CAST(a.name AS BLOB)),0)+COALESCE(length(CAST(a.type_name AS BLOB)),0) AS row_bytes FROM native_parameters a JOIN native_declarations d ON d.syntax_id=a.syntax_id WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1
UNION ALL
SELECT COALESCE(length(CAST(c.id AS BLOB)),0)+COALESCE(length(CAST(c.owner_syntax_id AS BLOB)),0)+COALESCE(length(CAST(c.source_set_id AS BLOB)),0)+COALESCE(length(CAST(c.language AS BLOB)),0)+COALESCE(length(CAST(c.path AS BLOB)),0)+COALESCE(length(CAST(c.revision_id AS BLOB)),0)+COALESCE(length(CAST(c.spelling AS BLOB)),0)+COALESCE(length(CAST(c.provenance_id AS BLOB)),0) AS row_bytes FROM native_calls c WHERE c.source_set_id=?2 AND c.language=?3 AND c.revision_id=?4 AND c.path=?1
UNION ALL
SELECT COALESCE(length(CAST(a.call_id AS BLOB)),0)+COALESCE(length(CAST(a.region_id AS BLOB)),0) AS row_bytes FROM native_call_regions a JOIN native_calls c ON c.id=a.call_id WHERE c.source_set_id=?2 AND c.language=?3 AND c.revision_id=?4 AND c.path=?1
UNION ALL
SELECT COALESCE(length(CAST(c.id AS BLOB)),0)+COALESCE(length(CAST(c.owner_syntax_id AS BLOB)),0)+COALESCE(length(CAST(c.source_set_id AS BLOB)),0)+COALESCE(length(CAST(c.language AS BLOB)),0)+COALESCE(length(CAST(c.path AS BLOB)),0)+COALESCE(length(CAST(c.revision_id AS BLOB)),0)+COALESCE(length(CAST(c.kind AS BLOB)),0)+COALESCE(length(CAST(c.parent_id AS BLOB)),0)+COALESCE(length(CAST(c.arm AS BLOB)),0)+COALESCE(length(CAST(c.provenance_id AS BLOB)),0) AS row_bytes FROM native_control_regions c WHERE c.source_set_id=?2 AND c.language=?3 AND c.revision_id=?4 AND c.path=?1
)"#;
const DATABASE_SCHEMA_VERSION: u32 = 6;
const EXTRACTOR_VERSION: &str = "native-paired-v1";
const GRAPH_SCHEMA_VERSION: u32 = 5;
const GRAPH_EXTRACTOR_VERSION: &str = "native-no-lexical-v1";
const LEGACY_SCHEMA_VERSION: u32 = 4;
const LEGACY_EXTRACTOR_VERSION: &str = "native-v1";
const EVIDENCE_FORMAT: &str = "terminal-native-graph-v1";
const CLASS_SCHEMA: &str = "
CREATE TABLE class_catalog(singleton INTEGER PRIMARY KEY CHECK(singleton=1), warnings TEXT NOT NULL, truncated INTEGER NOT NULL);
CREATE TABLE classes(id TEXT PRIMARY KEY REFERENCES nodes(id) DEFERRABLE INITIALLY DEFERRED, name TEXT NOT NULL, qualified_name TEXT NOT NULL, path TEXT NOT NULL, payload TEXT NOT NULL);
CREATE INDEX classes_path ON classes(path,id);
CREATE TABLE class_relations(id TEXT PRIMARY KEY, owner TEXT NOT NULL REFERENCES classes(id) DEFERRABLE INITIALLY DEFERRED, target TEXT REFERENCES classes(id) DEFERRABLE INITIALLY DEFERRED, payload TEXT NOT NULL);
CREATE INDEX class_relations_owner ON class_relations(owner,id);
CREATE INDEX class_relations_target ON class_relations(target,id);
";
const CACHE_SCHEMA: &str = "
CREATE TABLE index_metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1), schema_version INTEGER NOT NULL, extractor_version TEXT NOT NULL, root_spelling TEXT NOT NULL, root_device TEXT NOT NULL, root_inode TEXT NOT NULL, index_generation TEXT NOT NULL, index_revision INTEGER NOT NULL CHECK(index_revision BETWEEN 0 AND 9007199254740991), last_opened_at INTEGER NOT NULL CHECK(last_opened_at BETWEEN 0 AND 9007199254740991), indexed_at TEXT NOT NULL, stats TEXT NOT NULL, diagnostics TEXT NOT NULL);
CREATE TABLE files(path TEXT PRIMARY KEY, hash TEXT NOT NULL, payload TEXT NOT NULL);
CREATE TABLE nodes(id TEXT PRIMARY KEY, name TEXT NOT NULL, path TEXT NOT NULL REFERENCES files(path) DEFERRABLE INITIALLY DEFERRED, payload TEXT NOT NULL);
CREATE INDEX nodes_name ON nodes(name);
CREATE TABLE calls(id TEXT PRIMARY KEY, caller TEXT NOT NULL REFERENCES nodes(id) DEFERRABLE INITIALLY DEFERRED, target TEXT, path TEXT NOT NULL REFERENCES files(path) DEFERRABLE INITIALLY DEFERRED, payload TEXT NOT NULL);
CREATE INDEX calls_caller ON calls(caller);
CREATE TABLE regions(id TEXT PRIMARY KEY, owner TEXT NOT NULL REFERENCES nodes(id) DEFERRABLE INITIALLY DEFERRED, path TEXT NOT NULL REFERENCES files(path) DEFERRABLE INITIALLY DEFERRED, payload TEXT NOT NULL);
";

const NATIVE_SCHEMA: &str = "
CREATE INDEX nodes_path ON nodes(path,id);
CREATE INDEX calls_path ON calls(path,id);
CREATE INDEX regions_path ON regions(path,id);
CREATE TABLE native_producers(id TEXT PRIMARY KEY,version TEXT NOT NULL,executable_hash TEXT NOT NULL CHECK(length(executable_hash)=64),kind TEXT NOT NULL CHECK(kind='native'),position_encoding TEXT NOT NULL CHECK(position_encoding='utf8'));
CREATE TABLE native_producer_languages(producer_id TEXT NOT NULL REFERENCES native_producers(id) DEFERRABLE INITIALLY DEFERRED,language TEXT NOT NULL,ordinal INTEGER NOT NULL CHECK(ordinal>=0),PRIMARY KEY(producer_id,language),UNIQUE(producer_id,ordinal));
CREATE TABLE native_source_sets(id TEXT PRIMARY KEY,root_id TEXT NOT NULL);
CREATE TABLE native_source_set_languages(source_set_id TEXT NOT NULL REFERENCES native_source_sets(id) DEFERRABLE INITIALLY DEFERRED,language TEXT NOT NULL,ordinal INTEGER NOT NULL CHECK(ordinal>=0),PRIMARY KEY(source_set_id,language),UNIQUE(source_set_id,ordinal));
CREATE TABLE native_source_set_dependencies(source_set_id TEXT NOT NULL REFERENCES native_source_sets(id) DEFERRABLE INITIALLY DEFERRED,dependency_id TEXT NOT NULL REFERENCES native_source_sets(id) DEFERRABLE INITIALLY DEFERRED,ordinal INTEGER NOT NULL CHECK(ordinal>=0),PRIMARY KEY(source_set_id,dependency_id),UNIQUE(source_set_id,ordinal));
CREATE TABLE native_revisions(id TEXT PRIMARY KEY,source_set_id TEXT NOT NULL REFERENCES native_source_sets(id) DEFERRABLE INITIALLY DEFERRED,toolchain_hash TEXT NOT NULL CHECK(length(toolchain_hash)=64),config_hash TEXT NOT NULL CHECK(length(config_hash)=64),dependency_hash TEXT NOT NULL CHECK(length(dependency_hash)=64));
CREATE TABLE native_documents(source_set_id TEXT NOT NULL REFERENCES native_source_sets(id) DEFERRABLE INITIALLY DEFERRED,language TEXT NOT NULL,path TEXT NOT NULL,revision_id TEXT NOT NULL REFERENCES native_revisions(id) DEFERRABLE INITIALLY DEFERRED,content_hash TEXT NOT NULL CHECK(length(content_hash)=64),byte_length INTEGER NOT NULL CHECK(byte_length>=0 AND byte_length=length(source_bytes)),source_bytes BLOB NOT NULL,PRIMARY KEY(revision_id,language,path),UNIQUE(source_set_id,language,path,revision_id),UNIQUE(source_set_id,language,path,revision_id,content_hash));
CREATE INDEX native_documents_path ON native_documents(path,revision_id);
CREATE TABLE native_coverage(producer_id TEXT NOT NULL REFERENCES native_producers(id) DEFERRABLE INITIALLY DEFERRED,language TEXT NOT NULL,source_set_id TEXT NOT NULL,document_path TEXT NOT NULL,revision_id TEXT NOT NULL,requested INTEGER NOT NULL CHECK(requested IN (0,1)),selected INTEGER NOT NULL CHECK(selected IN (0,1)),state TEXT NOT NULL CHECK(state IN ('notRequested','omitted','unsupported','failed','partial','complete')),diagnostic TEXT,CHECK((state='complete')=(diagnostic IS NULL)),PRIMARY KEY(producer_id,revision_id,language,document_path),FOREIGN KEY(source_set_id,language,document_path,revision_id) REFERENCES native_documents(source_set_id,language,path,revision_id) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX native_coverage_document ON native_coverage(revision_id,language,document_path);
CREATE TABLE native_coverage_roles(producer_id TEXT NOT NULL,revision_id TEXT NOT NULL,language TEXT NOT NULL,document_path TEXT NOT NULL,role_kind TEXT NOT NULL CHECK(role_kind IN ('supported','observed')),role TEXT NOT NULL CHECK(role IN ('definition','call')),ordinal INTEGER NOT NULL CHECK(ordinal>=0),PRIMARY KEY(producer_id,revision_id,language,document_path,role_kind,ordinal),UNIQUE(producer_id,revision_id,language,document_path,role_kind,role),FOREIGN KEY(producer_id,revision_id,language,document_path) REFERENCES native_coverage(producer_id,revision_id,language,document_path) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE native_provenance(id TEXT PRIMARY KEY,producer_id TEXT NOT NULL REFERENCES native_producers(id) DEFERRABLE INITIALLY DEFERRED,source_set_id TEXT NOT NULL,language TEXT NOT NULL,path TEXT NOT NULL,revision_id TEXT NOT NULL,content_hash TEXT NOT NULL,evidence_kind TEXT NOT NULL CHECK(evidence_kind='measuredSyntax'),basis TEXT CHECK(basis IS NULL),derived_from TEXT CHECK(derived_from IS NULL),freshness TEXT NOT NULL CHECK(freshness='fresh'),UNIQUE(revision_id,language,path),UNIQUE(id,source_set_id,language,path,revision_id),FOREIGN KEY(source_set_id,language,path,revision_id) REFERENCES native_documents(source_set_id,language,path,revision_id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(source_set_id,language,path,revision_id,content_hash) REFERENCES native_documents(source_set_id,language,path,revision_id,content_hash) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX native_provenance_document ON native_provenance(revision_id,language,path);
CREATE TABLE native_declarations(syntax_id TEXT PRIMARY KEY,source_set_id TEXT NOT NULL,language TEXT NOT NULL,path TEXT NOT NULL,revision_id TEXT NOT NULL,owner_syntax_id TEXT REFERENCES native_declarations(syntax_id) DEFERRABLE INITIALLY DEFERRED,kind TEXT NOT NULL,name TEXT,lookup_key TEXT,key_signature_present INTEGER NOT NULL CHECK(key_signature_present IN (0,1)),key_type_parameter_count INTEGER,key_variadic INTEGER,key_ordinal INTEGER NOT NULL CHECK(key_ordinal>=0),start_byte INTEGER NOT NULL,end_byte INTEGER NOT NULL,name_start INTEGER,name_end INTEGER,provenance_id TEXT NOT NULL REFERENCES native_provenance(id) DEFERRABLE INITIALLY DEFERRED,CHECK(start_byte>=0 AND end_byte>=start_byte),CHECK((name IS NULL)=(lookup_key IS NULL) AND (name IS NULL)=(name_start IS NULL) AND (name_start IS NULL)=(name_end IS NULL)),CHECK(name_start IS NULL OR (name_start>=start_byte AND name_end<=end_byte AND name_end>name_start)),CHECK((key_signature_present=0 AND key_type_parameter_count IS NULL AND key_variadic IS NULL) OR (key_signature_present=1 AND key_type_parameter_count>=0 AND key_variadic IN (0,1))),UNIQUE(syntax_id,source_set_id,language,path,revision_id),FOREIGN KEY(source_set_id,language,path,revision_id) REFERENCES native_documents(source_set_id,language,path,revision_id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(provenance_id,source_set_id,language,path,revision_id) REFERENCES native_provenance(id,source_set_id,language,path,revision_id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(owner_syntax_id,source_set_id,language,path,revision_id) REFERENCES native_declarations(syntax_id,source_set_id,language,path,revision_id) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX native_declarations_lookup ON native_declarations(revision_id,language,lookup_key,syntax_id);
CREATE INDEX native_declarations_document ON native_declarations(revision_id,language,path);
CREATE INDEX native_declarations_path ON native_declarations(path,syntax_id);
CREATE TABLE native_declaration_ancestors(syntax_id TEXT NOT NULL REFERENCES native_declarations(syntax_id) DEFERRABLE INITIALLY DEFERRED,ordinal INTEGER NOT NULL CHECK(ordinal>=0),kind TEXT NOT NULL,name TEXT,sibling_ordinal INTEGER NOT NULL CHECK(sibling_ordinal>=0),signature_present INTEGER NOT NULL CHECK(signature_present IN (0,1)),type_parameter_count INTEGER,variadic INTEGER,CHECK((signature_present=0 AND type_parameter_count IS NULL AND variadic IS NULL) OR (signature_present=1 AND type_parameter_count>=0 AND variadic IN (0,1))),PRIMARY KEY(syntax_id,ordinal));
CREATE TABLE native_signature_parameter_types(syntax_id TEXT NOT NULL,ancestor_ordinal INTEGER NOT NULL,ordinal INTEGER NOT NULL CHECK(ordinal>=0),type_name TEXT NOT NULL,PRIMARY KEY(syntax_id,ancestor_ordinal,ordinal),FOREIGN KEY(syntax_id) REFERENCES native_declarations(syntax_id) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE native_headers(syntax_id TEXT PRIMARY KEY REFERENCES native_declarations(syntax_id) DEFERRABLE INITIALLY DEFERRED,kind TEXT NOT NULL,name TEXT,result_type TEXT);
CREATE TABLE native_header_items(syntax_id TEXT NOT NULL REFERENCES native_headers(syntax_id) DEFERRABLE INITIALLY DEFERRED,item_kind TEXT NOT NULL CHECK(item_kind IN ('modifier','typeParameter','base')),ordinal INTEGER NOT NULL CHECK(ordinal>=0),value TEXT NOT NULL,PRIMARY KEY(syntax_id,item_kind,ordinal));
CREATE TABLE native_parameters(syntax_id TEXT NOT NULL REFERENCES native_headers(syntax_id) DEFERRABLE INITIALLY DEFERRED,ordinal INTEGER NOT NULL CHECK(ordinal>=0),name TEXT,type_name TEXT,variadic INTEGER NOT NULL CHECK(variadic IN (0,1)),PRIMARY KEY(syntax_id,ordinal));
CREATE TABLE native_calls(id TEXT PRIMARY KEY,owner_syntax_id TEXT NOT NULL REFERENCES native_declarations(syntax_id) DEFERRABLE INITIALLY DEFERRED,ordinal INTEGER NOT NULL CHECK(ordinal>=0),source_set_id TEXT NOT NULL,language TEXT NOT NULL,path TEXT NOT NULL,revision_id TEXT NOT NULL,start_byte INTEGER NOT NULL,end_byte INTEGER NOT NULL,callee_start INTEGER,callee_end INTEGER,spelling TEXT,provenance_id TEXT NOT NULL REFERENCES native_provenance(id) DEFERRABLE INITIALLY DEFERRED,CHECK(start_byte>=0 AND end_byte>start_byte),CHECK((callee_start IS NULL)=(callee_end IS NULL)),CHECK(callee_start IS NULL OR (callee_start>=start_byte AND callee_end<=end_byte AND callee_end>callee_start)),UNIQUE(revision_id,owner_syntax_id,ordinal),FOREIGN KEY(source_set_id,language,path,revision_id) REFERENCES native_documents(source_set_id,language,path,revision_id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(provenance_id,source_set_id,language,path,revision_id) REFERENCES native_provenance(id,source_set_id,language,path,revision_id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(owner_syntax_id,source_set_id,language,path,revision_id) REFERENCES native_declarations(syntax_id,source_set_id,language,path,revision_id) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX native_calls_owner ON native_calls(revision_id,owner_syntax_id,ordinal);
CREATE INDEX native_calls_path ON native_calls(path,id);
CREATE TABLE native_call_regions(call_id TEXT NOT NULL REFERENCES native_calls(id) DEFERRABLE INITIALLY DEFERRED,region_id TEXT NOT NULL REFERENCES native_control_regions(id) DEFERRABLE INITIALLY DEFERRED,ordinal INTEGER NOT NULL CHECK(ordinal>=0),PRIMARY KEY(call_id,ordinal),UNIQUE(call_id,region_id));
CREATE TABLE native_control_regions(id TEXT PRIMARY KEY,owner_syntax_id TEXT NOT NULL REFERENCES native_declarations(syntax_id) DEFERRABLE INITIALLY DEFERRED,ordinal INTEGER NOT NULL CHECK(ordinal>=0),source_set_id TEXT NOT NULL,language TEXT NOT NULL,path TEXT NOT NULL,revision_id TEXT NOT NULL,kind TEXT NOT NULL,start_byte INTEGER NOT NULL,end_byte INTEGER NOT NULL,parent_id TEXT REFERENCES native_control_regions(id) DEFERRABLE INITIALLY DEFERRED,arm TEXT,provenance_id TEXT NOT NULL REFERENCES native_provenance(id) DEFERRABLE INITIALLY DEFERRED,CHECK(start_byte>=0 AND end_byte>start_byte),UNIQUE(revision_id,owner_syntax_id,ordinal),UNIQUE(id,owner_syntax_id,source_set_id,language,path,revision_id),FOREIGN KEY(source_set_id,language,path,revision_id) REFERENCES native_documents(source_set_id,language,path,revision_id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(provenance_id,source_set_id,language,path,revision_id) REFERENCES native_provenance(id,source_set_id,language,path,revision_id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(owner_syntax_id,source_set_id,language,path,revision_id) REFERENCES native_declarations(syntax_id,source_set_id,language,path,revision_id) DEFERRABLE INITIALLY DEFERRED,FOREIGN KEY(parent_id,owner_syntax_id,source_set_id,language,path,revision_id) REFERENCES native_control_regions(id,owner_syntax_id,source_set_id,language,path,revision_id) DEFERRABLE INITIALLY DEFERRED);
CREATE INDEX native_control_regions_owner ON native_control_regions(revision_id,owner_syntax_id,ordinal);
CREATE INDEX native_control_regions_path ON native_control_regions(path,id);
";
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
    file.read_exact(&mut header)
        .context("incompatible_index: invalid database header")?;
    ensure!(
        &header[..16] == b"SQLite format 3\0" && header[18] == 1 && header[19] == 1,
        "incompatible_index: rollback header required"
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

// The staged file is private to this attempt. On failure, only unlink our own inode.
struct StagedIndex {
    path: std::path::PathBuf,
    file: std::fs::File,
    published: bool,
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
    expected.execute_batch(CACHE_SCHEMA)?;
    expected.execute_batch(CLASS_SCHEMA)?;
    let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version == DATABASE_SCHEMA_VERSION {
        expected.execute_batch(NATIVE_SCHEMA)?;
    } else {
        ensure!(
            matches!(version, LEGACY_SCHEMA_VERSION | GRAPH_SCHEMA_VERSION),
            "incompatible_index: unknown schema version"
        );
    }
    ensure!(
        objects(db)? == objects(&expected)?,
        "incompatible_index: unknown cache object type, name or shape"
    );
    Ok(())
}
fn open_index(path: &Path, writable: bool) -> Result<Connection> {
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
    let version: u32 = storage_result(db.pragma_query_value(None, "user_version", |r| r.get(0)))?;
    ensure!(
        matches!(
            version,
            LEGACY_SCHEMA_VERSION | GRAPH_SCHEMA_VERSION | DATABASE_SCHEMA_VERSION
        ),
        "incompatible_index: schema version {version}"
    );
    storage_result(db.prepare(
        "SELECT index_generation,index_revision,indexed_at,stats,diagnostics FROM index_metadata",
    ))?;
    storage_result(db.prepare("SELECT path,hash,payload FROM files"))?;
    storage_result(db.prepare("SELECT id,name,path,payload FROM nodes"))?;
    storage_result(db.prepare("SELECT id,caller,target,path,payload FROM calls"))?;
    storage_result(db.prepare("SELECT id,owner,path,payload FROM regions"))?;
    storage_result(db.prepare("SELECT warnings,truncated FROM class_catalog"))?;
    storage_result(db.prepare("SELECT id,name,qualified_name,path,payload FROM classes"))?;
    storage_result(db.prepare("SELECT id,owner,target,payload FROM class_relations"))?;
    validate_cache_shape(&db)?;
    let metadata: (i64, String) = db.query_row(
        "SELECT schema_version,extractor_version FROM index_metadata WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        (version == LEGACY_SCHEMA_VERSION && metadata == (4, LEGACY_EXTRACTOR_VERSION.into()))
            || (version == GRAPH_SCHEMA_VERSION && metadata == (5, GRAPH_EXTRACTOR_VERSION.into()))
            || (version == DATABASE_SCHEMA_VERSION && metadata == (6, EXTRACTOR_VERSION.into())),
        "incompatible_index: schema and metadata mismatch"
    );
    let count: i64 = db.query_row("SELECT count(*) FROM index_metadata", [], |r| r.get(0))?;
    ensure!(count == 1, "incompatible_index: metadata cardinality");
    Ok(db)
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
fn json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}
fn rows<T: DeserializeOwned>(db: &Connection, sql: &str) -> Result<Vec<T>> {
    let mut stmt = storage_result(db.prepare(sql))?;
    let values = storage_result(stmt.query_map([], |r| r.get::<_, String>(0)))?;
    values
        .map(|v| Ok(serde_json::from_str(&storage_result(v)?)?))
        .collect()
}
fn one<T: DeserializeOwned>(db: &Connection, sql: &str, id: &str) -> Result<Option<T>> {
    let value: Option<String> = storage_result(db.query_row(sql, [id], |r| r.get(0)).optional())?;
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

fn class_metadata(db: &Connection) -> Result<(Vec<String>, bool)> {
    let length: i64 = db
        .query_row(
            "SELECT length(CAST(warnings AS BLOB)) FROM class_catalog WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map_err(|e| anyhow::anyhow!("incompatible_index: class catalog missing: {e}"))?;
    ensure!(
        (0..=256 * 1024).contains(&length),
        "incompatible_index: class catalog byte budget exceeded"
    );
    let (warnings, mut truncated): (String, bool) = db.query_row(
        "SELECT warnings,truncated FROM class_catalog WHERE singleton=1",
        [],
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
    db: &Connection,
    id: &str,
) -> Result<Option<(crate::classes::ClassDefinition, usize, bool)>> {
    use crate::class_diagram::CLASS_BYTES;
    let size: Option<i64> = db
        .query_row(
            "SELECT length(CAST(payload AS BLOB)) FROM classes WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(size) = size else {
        return Ok(None);
    };
    if size <= CLASS_BYTES as i64 {
        let payload: String =
            db.query_row("SELECT payload FROM classes WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
        return Ok(Some((
            serde_json::from_str(&payload)?,
            payload.len(),
            false,
        )));
    }
    for count in [32, 16, 8, 4, 2, 1, 0] {
        let payload: Option<String> = db.query_row("WITH clipped AS (
            SELECT json_set(payload,
                '$.fields',(SELECT json_group_array(json(value)) FROM json_each(classes.payload,'$.fields') WHERE key < ?2),
                '$.methods',(SELECT json_group_array(json(value)) FROM json_each(classes.payload,'$.methods') WHERE key < ?2),
                '$.truncated',json('true')) AS payload FROM classes WHERE id=?1)
            SELECT CASE WHEN length(CAST(payload AS BLOB)) <= ?3 THEN payload ELSE NULL END FROM clipped",
            params![id, count, CLASS_BYTES as i64], |r| r.get(0))?;
        if let Some(payload) = payload {
            return Ok(Some((serde_json::from_str(&payload)?, payload.len(), true)));
        }
    }
    // Pathological non-member metadata cannot fit without changing identity.
    Ok(None)
}
fn resolve_class_id(db: &Connection, id: &str) -> Result<String> {
    use crate::class_diagram::InvalidRequest;
    let mut current = id.to_owned();
    let mut seen = BTreeSet::new();
    for depth in 0..64 {
        ensure!(
            seen.insert(current.clone()),
            InvalidRequest("The selected symbol has a cyclic class ancestry.")
        );
        if db.query_row(
            "SELECT EXISTS(SELECT 1 FROM classes WHERE id=?1)",
            [&current],
            |r| r.get::<_, bool>(0),
        )? {
            return Ok(current);
        }
        let symbol = one::<Symbol>(db, "SELECT payload FROM nodes WHERE id=?1", &current)?.ok_or(
            InvalidRequest("Choose an indexed Java or Python class or method."),
        )?;
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

fn resolve_class(db: &Connection, id: &str) -> Result<(crate::classes::ClassDefinition, bool)> {
    let id = resolve_class_id(db, id)?;
    let (class, _, clipped) = presentation_class(db, &id)?.ok_or(
        crate::class_diagram::InvalidRequest("Class metadata exceeds the presentation byte limit."),
    )?;
    Ok((class, clipped))
}

fn write_native(
    db: &Connection,
    artifact: &crate::native_evidence::Artifact,
    capture: &crate::capture::Capture,
    cancel: &CancelFlag,
) -> Result<()> {
    use crate::native_evidence::Signature;
    let a = artifact;
    db.execute_batch("DELETE FROM native_call_regions; DELETE FROM native_calls; DELETE FROM native_control_regions; DELETE FROM native_signature_parameter_types; DELETE FROM native_declaration_ancestors; DELETE FROM native_parameters; DELETE FROM native_header_items; DELETE FROM native_headers; DELETE FROM native_declarations; DELETE FROM native_provenance; DELETE FROM native_coverage_roles; DELETE FROM native_coverage; DELETE FROM native_documents; DELETE FROM native_revisions; DELETE FROM native_source_set_dependencies; DELETE FROM native_source_set_languages; DELETE FROM native_source_sets; DELETE FROM native_producer_languages; DELETE FROM native_producers;")?;
    db.execute(
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
        db.execute(
            "INSERT INTO native_producer_languages VALUES(?1,?2,?3)",
            params![a.producer.id, language, ordinal as i64],
        )?;
    }
    db.execute(
        "INSERT INTO native_source_sets VALUES(?1,?2)",
        params![a.source_set.id, a.source_set.root_id],
    )?;
    for (ordinal, language) in a.source_set.languages.iter().enumerate() {
        db.execute(
            "INSERT INTO native_source_set_languages VALUES(?1,?2,?3)",
            params![a.source_set.id, language, ordinal as i64],
        )?;
    }
    for (ordinal, dependency) in a.source_set.dependencies.iter().enumerate() {
        db.execute(
            "INSERT INTO native_source_set_dependencies VALUES(?1,?2,?3)",
            params![a.source_set.id, dependency, ordinal as i64],
        )?;
    }
    db.execute(
        "INSERT INTO native_revisions VALUES(?1,?2,?3,?4,?5)",
        params![
            a.revision.id,
            a.revision.source_set_id,
            a.revision.toolchain_hash,
            a.revision.config_hash,
            a.revision.dependency_hash
        ],
    )?;
    let sources: BTreeMap<_, _> = capture
        .files
        .iter()
        .map(|file| ((file.language.as_str(), file.path.as_str()), file))
        .collect();
    for doc in &a.revision.documents {
        check_cancel(cancel)?;
        let file = sources
            .get(&(doc.key.language.as_str(), doc.key.path.as_str()))
            .context("native document missing captured bytes")?;
        ensure!(
            file.hash == doc.content_hash && file.text.len() == doc.byte_length,
            "native document bytes differ from capture"
        );
        db.execute(
            "INSERT INTO native_documents VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                doc.key.source_set_id,
                doc.key.language,
                doc.key.path,
                doc.revision_id,
                doc.content_hash,
                doc.byte_length as i64,
                file.text.as_bytes()
            ],
        )?;
    }
    for coverage in &a.coverage {
        check_cancel(cancel)?;
        db.execute(
            "INSERT INTO native_coverage VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                coverage.producer_id,
                coverage.language,
                coverage.source_set_id,
                coverage.document_path,
                coverage.revision_id,
                coverage.requested,
                coverage.selected,
                coverage.state,
                coverage.diagnostic
            ],
        )?;
        for (kind, roles) in [
            ("supported", &coverage.supported_roles),
            ("observed", &coverage.observed_roles),
        ] {
            for (ordinal, role) in roles.iter().enumerate() {
                db.execute(
                    "INSERT INTO native_coverage_roles VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        coverage.producer_id,
                        coverage.revision_id,
                        coverage.language,
                        coverage.document_path,
                        kind,
                        role,
                        ordinal as i64
                    ],
                )?;
            }
        }
    }
    for proof in &a.provenance {
        check_cancel(cancel)?;
        ensure!(
            proof.basis.is_none() && proof.derived_from.is_none(),
            "native proof must be fresh and direct"
        );
        db.execute(
            "INSERT INTO native_provenance VALUES(?1,?2,?3,?4,?5,?6,?7,?8,NULL,NULL,?9)",
            params![
                proof.id,
                proof.producer_id,
                proof.document.source_set_id,
                proof.document.language,
                proof.document.path,
                proof.revision_id,
                proof.content_hash,
                proof.evidence_kind,
                proof.freshness
            ],
        )?;
    }
    fn signature(
        db: &Connection,
        id: &str,
        ancestor: i64,
        signature: &Option<Signature>,
    ) -> Result<()> {
        if let Some(signature) = signature {
            for (ordinal, parameter) in signature.parameter_types.iter().enumerate() {
                db.execute(
                    "INSERT INTO native_signature_parameter_types VALUES(?1,?2,?3,?4)",
                    params![id, ancestor, ordinal as i64, parameter],
                )?;
            }
        }
        Ok(())
    }
    let mut owners = BTreeMap::new();
    for d in &a.declarations {
        let key = (
            d.document.path.clone(),
            serde_json::to_string(&d.ancestors)?,
            serde_json::to_string(&d.key)?,
        );
        ensure!(
            owners.insert(key, d.syntax_id.clone()).is_none(),
            "duplicate native owner key"
        );
    }
    for declaration in &a.declarations {
        check_cancel(cancel)?;
        let d = declaration;
        let owner = d
            .ancestors
            .last()
            .map(|last| -> Result<String> {
                let prefix = serde_json::to_string(&d.ancestors[..d.ancestors.len() - 1])?;
                let key = serde_json::to_string(last)?;
                owners
                    .get(&(d.document.path.clone(), prefix, key))
                    .cloned()
                    .context("native declaration parent missing")
            })
            .transpose()?;
        let sig = d.key.signature.as_ref();
        db.execute("INSERT INTO native_declarations VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",params![d.syntax_id,d.document.source_set_id,d.document.language,d.document.path,d.revision_id,owner,d.kind,d.name,d.lookup_key,sig.is_some(),sig.map(|s|s.type_parameter_count as i64),sig.map(|s|s.variadic),d.key.ordinal as i64,d.range.start as i64,d.range.end as i64,d.name_range.as_ref().map(|r|r.start as i64),d.name_range.as_ref().map(|r|r.end as i64),d.provenance_id])?;
        signature(db, &d.syntax_id, -1, &d.key.signature)?;
        for (ordinal, key) in d.ancestors.iter().enumerate() {
            let sig = key.signature.as_ref();
            db.execute(
                "INSERT INTO native_declaration_ancestors VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    d.syntax_id,
                    ordinal as i64,
                    key.kind,
                    key.name,
                    key.ordinal as i64,
                    sig.is_some(),
                    sig.map(|s| s.type_parameter_count as i64),
                    sig.map(|s| s.variadic)
                ],
            )?;
            signature(db, &d.syntax_id, ordinal as i64, &key.signature)?;
        }
        db.execute(
            "INSERT INTO native_headers VALUES(?1,?2,?3,?4)",
            params![
                d.syntax_id,
                d.header.kind,
                d.header.name,
                d.header.result_type
            ],
        )?;
        for (kind, values) in [
            ("modifier", &d.header.modifiers),
            ("typeParameter", &d.header.type_parameters),
            ("base", &d.header.bases),
        ] {
            for (ordinal, value) in values.iter().enumerate() {
                db.execute(
                    "INSERT INTO native_header_items VALUES(?1,?2,?3,?4)",
                    params![d.syntax_id, kind, ordinal as i64, value],
                )?;
            }
        }
        for (ordinal, parameter) in d.header.parameters.iter().enumerate() {
            db.execute(
                "INSERT INTO native_parameters VALUES(?1,?2,?3,?4,?5)",
                params![
                    d.syntax_id,
                    ordinal as i64,
                    parameter.name,
                    parameter.type_name,
                    parameter.variadic
                ],
            )?;
        }
    }
    for region in &a.control_regions {
        check_cancel(cancel)?;
        let r = region;
        db.execute(
            "INSERT INTO native_control_regions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                r.id,
                r.owner_syntax_id,
                r.ordinal as i64,
                r.document.source_set_id,
                r.document.language,
                r.document.path,
                r.revision_id,
                r.kind,
                r.range.start as i64,
                r.range.end as i64,
                r.parent_id,
                r.arm,
                r.provenance_id
            ],
        )?;
    }
    for call in &a.calls {
        check_cancel(cancel)?;
        let c = call;
        db.execute(
            "INSERT INTO native_calls VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                c.id,
                c.owner_syntax_id,
                c.ordinal as i64,
                c.document.source_set_id,
                c.document.language,
                c.document.path,
                c.revision_id,
                c.range.start as i64,
                c.range.end as i64,
                c.callee_range.as_ref().map(|r| r.start as i64),
                c.callee_range.as_ref().map(|r| r.end as i64),
                c.spelling,
                c.provenance_id
            ],
        )?;
        for (ordinal, region) in c.region_ids.iter().enumerate() {
            db.execute(
                "INSERT INTO native_call_regions VALUES(?1,?2,?3)",
                params![c.id, region, ordinal as i64],
            )?;
        }
    }
    ensure!(
        db.prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_none(),
        "native foreign key failure"
    );
    Ok(())
}

/// Bounded readiness check for a pair installed by the verified writer. This does not
/// attest every stored BLOB after out-of-band SQLite mutation. Opening the Store checks
/// all rows; pinned source reads verify the selected BLOB inside their read snapshot.
fn validate_paired_metadata(db: &Connection, root_id: &str) -> Result<()> {
    fn one_row(db: &Connection, sql: &str) -> Result<Option<(String, String)>> {
        let mut rows = db
            .prepare(sql)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            rows.len() <= 1,
            "incompatible_index: duplicate native pair metadata"
        );
        Ok(rows.pop())
    }
    let producer = one_row(db, "SELECT id,kind FROM native_producers LIMIT 2")?;
    let source = one_row(db, "SELECT id,root_id FROM native_source_sets LIMIT 2")?;
    let revision = one_row(db, "SELECT id,source_set_id FROM native_revisions LIMIT 2")?;
    let expected_source = format!("source-set:v1:{root_id}");
    ensure!(
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
    Ok(())
}

fn validate_paired_rows(db: &Connection) -> Result<()> {
    let count = |table: &str| -> Result<i64> {
        Ok(db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
    };
    ensure!(
        count("native_producers")? == 1
            && count("native_source_sets")? == 1
            && count("native_revisions")? == 1,
        "incompatible_index: missing native pair"
    );
    ensure!(
        count("native_documents")? == count("files")?
            && count("native_coverage")? == count("files")?
            && count("native_provenance")? == count("files")?,
        "incompatible_index: incomplete native pair"
    );
    let mut stmt=db.prepare("SELECT d.source_bytes,d.content_hash,d.byte_length,f.payload,f.hash FROM native_documents d JOIN files f ON d.path=f.path")?;
    let mut rows = stmt.query([])?;
    let mut matched = 0;
    while let Some(row) = rows.next()? {
        use sha2::{Digest, Sha256};
        let bytes: Vec<u8> = row.get(0)?;
        let hash: String = row.get(1)?;
        let length: i64 = row.get(2)?;
        let file: SourceFile = serde_json::from_str(&row.get::<_, String>(3)?)?;
        ensure!(
            length == bytes.len() as i64
                && hash == hex::encode(Sha256::digest(&bytes))
                && file.hash == hash
                && row.get::<_, String>(4)? == hash
                && file.text.as_bytes() == bytes,
            "incompatible_index: graph/native bytes mismatch"
        );
        matched += 1;
    }
    ensure!(
        matched == count("files")?,
        "incompatible_index: missing graph/native document"
    );
    ensure!(
        db.prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_none(),
        "incompatible_index: native foreign key mismatch"
    );
    Ok(())
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
        };
        if !index_path_present(&store.roots.index_db(&store.identity))? {
            let leader = store.roots.leader(&store.identity)?;
            store.initialize(&leader, before_publish)?;
        }
        let mut db = store.cache()?;
        let tx = storage_result(db.transaction())?;
        store.read_control_status(&tx)?;
        let schema: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if schema == DATABASE_SCHEMA_VERSION {
            validate_paired_rows(&tx)?;
        }
        Ok(store)
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
    fn initialize(
        &self,
        leader: &topology::LeaderGuard,
        before_publish: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<()> {
        use rusqlite::OpenFlags;
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        self.identity.verify()?;
        let path = self.roots.index_db(&self.identity);
        reject_sidecars(&path, true)?;
        if index_path_present(&path)? {
            return Ok(());
        }
        let use_guard = self.roots.index_use(&self.identity)?;
        use std::os::unix::fs::OpenOptionsExt;
        let staged_path = path.with_file_name(format!("index.db.tmp-{}", uuid::Uuid::new_v4()));
        let staged = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged_path)?;
        let mut staged = StagedIndex {
            path: staged_path,
            file: staged,
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
            db.execute_batch(CACHE_SCHEMA)?;
            db.execute_batch(CLASS_SCHEMA)?;
            let age = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            ensure!(age <= 9_007_199_254_740_991, "invalid_open_age");
            db.execute(
                "INSERT INTO index_metadata VALUES(1,5,?1,?2,?3,?4,?5,0,?6,'',?7,?8)",
                params![
                    GRAPH_EXTRACTOR_VERSION,
                    self.workspace_root,
                    self.identity.device.to_string(),
                    self.identity.inode.to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    age as i64,
                    json(&IndexStats::default())?,
                    json(&Vec::<Diagnostic>::new())?
                ],
            )?;
            db.pragma_update(None, "user_version", GRAPH_SCHEMA_VERSION)?;
            db.execute_batch("COMMIT")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = db.execute_batch("ROLLBACK");
        }
        result?;
        drop(db);
        before_publish(&staged.path)?;
        verify_index_file(&staged.path)?;
        let mut checked = open_index(&staged.path, true)?;
        let checked_snapshot = storage_result(checked.transaction())?;
        self.read_control_status(&checked_snapshot)?;
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
        drop(self.cache()?);
        let leader = self.roots.leader(&self.identity)?;
        // Opening can race a second SQLite writer: repeat validation only AFTER
        // BEGIN IMMEDIATE excludes schema changes and before any metadata UPDATE.
        let mut db = self.cache_write()?;
        let admitted_version: i64 =
            storage_result(db.pragma_query_value(None, "data_version", |r| r.get(0)))?;
        before_write(&db)?;
        let tx = storage_result(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let status = self.read_control_status(&tx)?;
        let locked_version: i64 =
            storage_result(tx.pragma_query_value(None, "data_version", |r| r.get(0)))?;
        ensure!(
            locked_version == admitted_version,
            "incompatible_index: cache changed after admission"
        );
        if status.evidence_format.is_some() {
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
    fn read_control_status(&self, db: &Connection) -> Result<IndexStatus> {
        // This check must run INSIDE the caller's read snapshot or writer lock.
        // The open_index admission check alone cannot protect against later DDL.
        validate_cache_shape(db)?;
        let schema_version: i64 =
            storage_result(db.pragma_query_value(None, "user_version", |r| r.get(0)))?;
        let row: (i64,String,String,String,String,String,i64,String,String,String) = storage_result(db.query_row(
            "SELECT schema_version,extractor_version,root_spelling,root_device,root_inode,index_generation,index_revision,indexed_at,stats,diagnostics FROM index_metadata WHERE singleton=1",
            [],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?))))?;
        ensure!(
            (schema_version == i64::from(LEGACY_SCHEMA_VERSION)
                && row.0 == schema_version
                && row.1 == LEGACY_EXTRACTOR_VERSION)
                || (schema_version == i64::from(GRAPH_SCHEMA_VERSION)
                    && row.0 == schema_version
                    && row.1 == GRAPH_EXTRACTOR_VERSION)
                || (schema_version == i64::from(DATABASE_SCHEMA_VERSION)
                    && row.0 == schema_version
                    && row.1 == EXTRACTOR_VERSION),
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
            validate_paired_metadata(db, &self.identity.record_id)?;
        }
        Ok(IndexStatus {
            workspace_root: self.workspace_root.clone(),
            revision: pin,
            indexed_at: if row.7.is_empty() { None } else { Some(row.7) },
            stats: serde_json::from_str(&row.8)?,
            diagnostics: serde_json::from_str(&row.9)?,
            evidence_format: (row.0 == i64::from(DATABASE_SCHEMA_VERSION))
                .then(|| EVIDENCE_FORMAT.to_owned()),
        })
    }
    fn read_status(&self, db: &Connection) -> Result<IndexStatus> {
        let status = self.read_control_status(db)?;
        ensure!(
            status.evidence_format.is_some(),
            "index_not_ready: reindex required"
        );
        // Every public schema-6 derived read needs the same bounded catalog
        // singleton. Missing/oversized live metadata is corruption, never an
        // old-index "requireIndex" fallback. This is one indexed metadata row.
        let warnings_bytes: Option<i64> = db
            .query_row(
                "SELECT length(CAST(warnings AS BLOB)) FROM class_catalog WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        ensure!(
            warnings_bytes.is_some_and(|bytes| (0..=256 * 1024).contains(&bytes)),
            "incompatible_index: class catalog byte budget exceeded or missing"
        );
        Ok(status)
    }
    /// Internal control baseline, never returned by public status or evidence reads.
    pub fn index_baseline(&self) -> Result<IndexPin> {
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        Ok(self.read_control_status(&tx)?.revision)
    }
    pub fn root_id(&self) -> &str {
        &self.identity.record_id
    }
    pub fn verify_root(&self) -> Result<()> {
        self.identity.verify()
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
        let mut db = self.cache()?;
        before_snapshot(&db)?;
        let tx = storage_result(db.transaction())?;
        self.read_status(&tx)
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
        native.validate(
            capture,
            Path::new(&self.workspace_root),
            &self.identity.record_id,
            cancel,
        )?;
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
        self.publish_inner(graph, capture, native, leader, expected_revision, cancel)
    }
    fn publish_inner(
        &self,
        graph: &Graph,
        capture: &crate::capture::Capture,
        native: &crate::native_evidence::Artifact,
        leader: &topology::LeaderGuard,
        expected_revision: IndexPin,
        cancel: &CancelFlag,
    ) -> Result<IndexPin> {
        self.publish_inner_checked(
            (graph, capture, native),
            leader,
            expected_revision,
            cancel,
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
        mut during_tx: impl FnMut(PublishStage, &Connection) -> Result<()>,
    ) -> Result<IndexPin> {
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
        let classes = crate::classes::Catalog::build(&graph.files, &graph.nodes, cancel)?;
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
        check_cancel(cancel)?;
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        self.identity.verify()?;
        let mut db = self.cache_write()?;
        let admitted_version: i64 =
            storage_result(db.pragma_query_value(None, "data_version", |r| r.get(0)))?;
        during_tx(PublishStage::BeforeTransaction, &db)?;
        let tx = storage_result(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let baseline = self.read_control_status(&tx)?;
        let locked_version: i64 =
            storage_result(tx.pragma_query_value(None, "data_version", |r| r.get(0)))?;
        ensure!(
            locked_version == admitted_version,
            "incompatible_index: cache changed after admission"
        );
        let old = baseline.revision;
        ensure!(
            expected_revision == old,
            "revision conflict: expected {expected_revision:?}, found {old:?}"
        );
        let schema: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let rebaseline = schema != DATABASE_SCHEMA_VERSION;
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
                old.index_generation
            },
            index_revision: old
                .index_revision
                .checked_add(1)
                .filter(|n| *n <= 9_007_199_254_740_991)
                .context("revision overflow")?,
        };
        if rebaseline {
            tx.execute_batch(NATIVE_SCHEMA)?;
        }
        tx.execute_batch(
            "DELETE FROM class_relations; DELETE FROM classes; DELETE FROM class_catalog;
             DELETE FROM calls; DELETE FROM regions; DELETE FROM nodes; DELETE FROM files;",
        )?;
        for f in &graph.files {
            check_cancel(cancel)?;
            tx.execute(
                "INSERT INTO files VALUES(?1,?2,?3)",
                params![f.path, f.hash, json(f)?],
            )?;
            during_tx(PublishStage::AfterFile, &tx)?;
        }
        for n in &graph.nodes {
            check_cancel(cancel)?;
            tx.execute(
                "INSERT INTO nodes VALUES(?1,?2,?3,?4)",
                params![n.id, n.name, n.path, json(n)?],
            )?;
        }
        for c in &graph.calls {
            check_cancel(cancel)?;
            tx.execute(
                "INSERT INTO calls VALUES(?1,?2,?3,?4,?5)",
                params![c.id, c.caller, Option::<String>::None, c.path, json(c)?],
            )?;
        }
        for r in &graph.regions {
            check_cancel(cancel)?;
            tx.execute(
                "INSERT INTO regions VALUES(?1,?2,?3,?4)",
                params![r.id, r.owner, r.path, json(r)?],
            )?;
        }
        for class in &classes.classes {
            check_cancel(cancel)?;
            tx.execute(
                "INSERT INTO classes VALUES(?1,?2,?3,?4,?5)",
                params![
                    class.symbol.id,
                    class.symbol.name,
                    class.qualified_name,
                    class.symbol.path,
                    json(class)?
                ],
            )?;
        }
        for relation in &classes.relations {
            check_cancel(cancel)?;
            tx.execute(
                "INSERT INTO class_relations VALUES(?1,?2,?3,?4)",
                params![
                    relation.id,
                    relation.owner,
                    relation.target,
                    json(relation)?
                ],
            )?;
        }
        write_native(&tx, native, capture, cancel)?;
        tx.execute(
            "INSERT INTO class_catalog VALUES(1,?1,?2)",
            params![json(&classes.warnings)?, classes.truncated],
        )?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .to_string();
        tx.execute("UPDATE index_metadata SET schema_version=6,extractor_version=?1,index_generation=?2,index_revision=?3,indexed_at=?4,stats=?5,diagnostics=?6 WHERE singleton=1",
            params![EXTRACTOR_VERSION, revision.index_generation.to_string(), revision.index_revision as i64,timestamp,json(&stats)?,json(&graph.diagnostics)?])?;
        if rebaseline {
            tx.pragma_update(None, "user_version", DATABASE_SCHEMA_VERSION)?;
        }
        validate_paired_metadata(&tx, &self.identity.record_id)?;
        validate_paired_rows(&tx)?;
        check_cancel(cancel)?;
        capture.verify(cancel)?;
        leader.verify()?;
        self.identity.verify()?;
        during_tx(PublishStage::BeforeCommit, &tx)?;
        check_cancel(cancel)?;
        capture.verify(cancel)?;
        leader.verify()?;
        self.identity.verify()?;
        storage_result(tx.commit())?;
        Ok(revision)
    }

    fn native_at<T>(
        &self,
        pin: IndexPin,
        read: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        ensure!(
            self.read_status(&tx)?.revision == pin,
            "revision conflict: stale native pin"
        );
        read(&tx)
    }

    /// Reparse exactly one selected, paired source in this SQLite snapshot.
    /// This never rereads a workspace path or projects the whole graph.
    fn selected_native_witness(
        &self,
        db: &Connection,
        path: &str,
    ) -> Result<crate::native_evidence::Artifact> {
        use crate::native_evidence::{Document, DocumentKey, Producer, Revision, SourceSet};
        let (source_set_id, revision_id): (String, String) = db
            .query_row(
                "SELECT source_set_id,revision_id FROM native_documents WHERE path=?1",
                [path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .context("incompatible_index: selected native document absent")?;
        ensure!(
            source_set_id == format!("source-set:v1:{}", self.root_id()),
            "incompatible_index: selected source set mismatch"
        );
        let file = Self::selected_source_row(db, path)?
            .context("incompatible_index: selected graph source absent")?;
        let mut producer:Producer=db.query_row(
            "SELECT id,version,executable_hash,kind,position_encoding FROM native_producers LIMIT 1",[],
            |r|Ok(Producer{id:r.get(0)?,version:r.get(1)?,executable_hash:r.get(2)?,kind:r.get(3)?,languages:vec![],position_encoding:r.get(4)?}),
        )?;
        producer.languages=db.prepare(
            "SELECT language FROM native_producer_languages WHERE producer_id=?1 ORDER BY ordinal"
        )?.query_map([&producer.id],|r|r.get::<_,String>(0))?
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
            "SELECT id,source_set_id,toolchain_hash,config_hash,dependency_hash FROM native_revisions WHERE id=?1",
            [&revision_id],|r|Ok(Revision{id:r.get(0)?,source_set_id:r.get(1)?,
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
        path: &str,
        source_set_id: &str,
        language: &str,
        revision_id: &str,
    ) -> Result<(i64, i64)> {
        db.query_row(
            SELECTED_ANCILLARY_BYTE_SQL,
            params![path, source_set_id, language, revision_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| {
            anyhow::anyhow!("incompatible_index: selected evidence byte budget exceeded: {e}")
        })
    }
    fn attest_selected_document(&self, db: &Connection, path: &str) -> Result<()> {
        // SQLite lengths and bounded row counts precede BLOB/JSON allocation and
        // selected tree-sitter extraction. All predicates stay on this source set,
        // revision, language and path; no workspace-wide payload scan.
        let mut size_guard=db.prepare("SELECT length(CAST(d.source_set_id AS BLOB)),length(CAST(d.language AS BLOB)),length(CAST(d.revision_id AS BLOB)),length(CAST(d.path AS BLOB)),length(CAST(d.content_hash AS BLOB)),length(CAST(f.hash AS BLOB)),length(d.source_bytes),length(CAST(f.payload AS BLOB)) FROM native_documents d JOIN files f ON f.path=d.path WHERE d.path=?1 LIMIT 2")?;
        let byte_headers: Vec<[i64; 8]> = size_guard
            .query_map([path], |r| {
                Ok([
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ])
            })?
            .collect::<rusqlite::Result<_>>()?;
        ensure!(
            byte_headers.len() == 1
                && byte_headers[0][..6]
                    .iter()
                    .all(|length| *length >= 0 && *length <= 16 * 1024)
                && byte_headers[0][6] >= 0
                && byte_headers[0][6] <= 256 * 1024 * 1024
                && byte_headers[0][7] >= 0
                && byte_headers[0][7] <= 256 * 1024 * 1024 + 16 * 1024
                && byte_headers[0][7]
                    <= byte_headers[0][6]
                        .saturating_mul(6)
                        .saturating_add(16 * 1024),
            "incompatible_index: selected source byte budget exceeded"
        );
        let mut sizes=db.prepare("SELECT d.source_set_id,d.language,d.revision_id,length(d.source_bytes),length(CAST(f.payload AS BLOB)) FROM native_documents d JOIN files f ON f.path=d.path WHERE d.path=?1 LIMIT 2")?;
        let selected: Vec<(String, String, String, i64, i64)> = sizes
            .query_map([path], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        ensure!(
            selected.len() == 1,
            "incompatible_index: selected document identity missing or ambiguous"
        );
        let (source_set_id, language, revision_id, native_bytes, graph_bytes) = &selected[0];
        ensure!(
            source_set_id == &format!("source-set:v1:{}", self.root_id())
                && *native_bytes >= 0
                && *native_bytes <= 256 * 1024 * 1024
                && *graph_bytes >= 0
                && *graph_bytes <= 256 * 1024 * 1024 + 16 * 1024
                && *graph_bytes <= native_bytes.saturating_mul(6).saturating_add(16 * 1024),
            "incompatible_index: selected source byte budget exceeded"
        );
        let row_limit = native_bytes.saturating_mul(16).clamp(1024, 1_000_000);
        let row_sql = r#"SELECT (SELECT count(*) FROM files WHERE path=?1),
(SELECT count(*) FROM nodes WHERE path=?1),
(SELECT count(*) FROM calls WHERE path=?1),
(SELECT count(*) FROM regions WHERE path=?1),
(SELECT count(*) FROM classes WHERE path=?1),
(SELECT count(*) FROM class_relations r JOIN classes k ON k.id=r.owner WHERE k.path=?1),
(SELECT count(*) FROM native_documents WHERE source_set_id=?2 AND language=?3 AND revision_id=?4 AND path=?1),
(SELECT count(*) FROM native_coverage WHERE producer_id=(SELECT id FROM native_producers LIMIT 1) AND revision_id=?4 AND language=?3 AND document_path=?1 AND source_set_id=?2),
(SELECT count(*) FROM native_coverage_roles WHERE producer_id=(SELECT id FROM native_producers LIMIT 1) AND revision_id=?4 AND language=?3 AND document_path=?1),
(SELECT count(*) FROM native_provenance WHERE revision_id=?4 AND language=?3 AND path=?1 AND source_set_id=?2),
(SELECT count(*) FROM native_declarations d WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1),
(SELECT count(*) FROM native_declaration_ancestors x JOIN native_declarations d ON d.syntax_id=x.syntax_id WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1),
(SELECT count(*) FROM native_signature_parameter_types x JOIN native_declarations d ON d.syntax_id=x.syntax_id WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1),
(SELECT count(*) FROM native_headers x JOIN native_declarations d ON d.syntax_id=x.syntax_id WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1),
(SELECT count(*) FROM native_header_items x JOIN native_declarations d ON d.syntax_id=x.syntax_id WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1),
(SELECT count(*) FROM native_parameters x JOIN native_declarations d ON d.syntax_id=x.syntax_id WHERE d.source_set_id=?2 AND d.language=?3 AND d.revision_id=?4 AND d.path=?1),
(SELECT count(*) FROM native_calls WHERE source_set_id=?2 AND language=?3 AND revision_id=?4 AND path=?1),
(SELECT count(*) FROM native_call_regions x JOIN native_calls c ON c.id=x.call_id WHERE c.source_set_id=?2 AND c.language=?3 AND c.revision_id=?4 AND c.path=?1),
(SELECT count(*) FROM native_control_regions WHERE source_set_id=?2 AND language=?3 AND revision_id=?4 AND path=?1),
(SELECT count(*) FROM native_producer_languages WHERE producer_id=(SELECT id FROM native_producers LIMIT 1)),
(SELECT count(*) FROM native_source_set_languages WHERE source_set_id=?2),
(SELECT count(*) FROM native_source_set_dependencies WHERE source_set_id=?2)"#;
        let row_counts: Vec<i64> = db.query_row(
            row_sql,
            params![path, source_set_id, language, revision_id],
            |r| {
                (0..22)
                    .map(|i| r.get::<_, i64>(i))
                    .collect::<rusqlite::Result<_>>()
            },
        )?;
        ensure!(
            row_counts.len() == 22 && row_counts.into_iter().all(|n| n >= 0 && n <= row_limit),
            "incompatible_index: selected evidence row budget exceeded"
        );
        // Materialized native TEXT and class/graph JSON also need a byte budget.
        // One SQL aggregate inspects the SAME selected row sets without copying
        // their TEXT into Rust. Max bounds a single row; SUM prevents count × cap.
        let (max_row, total_bytes) =
            Self::selected_ancillary_byte_usage(db, path, source_set_id, language, revision_id)?;
        let per_row_limit = native_bytes
            .saturating_mul(32)
            .saturating_add(16 * 1024)
            .min(256 * 1024 * 1024);
        let total_limit = native_bytes
            .saturating_mul(64)
            .saturating_add(256 * 1024)
            .min(512 * 1024 * 1024);
        ensure!(
            max_row >= 0
                && total_bytes >= 0
                && max_row <= per_row_limit
                && total_bytes <= total_limit,
            "incompatible_index: selected evidence byte budget exceeded"
        );
        // All graph rows have a capture-aligned pre-allocation byte envelope.
        // Their aggregate is bounded separately from ancillary/native rows.
        let graph_sql = r#"SELECT COALESCE(max(row_bytes),0),COALESCE(sum(row_bytes),0) FROM (
SELECT COALESCE(length(CAST(n.id AS BLOB)),0)+COALESCE(length(CAST(n.name AS BLOB)),0)+COALESCE(length(CAST(n.path AS BLOB)),0)+COALESCE(length(CAST(n.payload AS BLOB)),0) AS row_bytes FROM nodes n WHERE n.path=?1
UNION ALL
SELECT COALESCE(length(CAST(g.id AS BLOB)),0)+COALESCE(length(CAST(g.caller AS BLOB)),0)+COALESCE(length(CAST(g.target AS BLOB)),0)+COALESCE(length(CAST(g.path AS BLOB)),0)+COALESCE(length(CAST(g.payload AS BLOB)),0) AS row_bytes FROM calls g WHERE g.path=?1
UNION ALL
SELECT COALESCE(length(CAST(g.id AS BLOB)),0)+COALESCE(length(CAST(g.owner AS BLOB)),0)+COALESCE(length(CAST(g.path AS BLOB)),0)+COALESCE(length(CAST(g.payload AS BLOB)),0) AS row_bytes FROM regions g WHERE g.path=?1
)"#;
        let (graph_row, graph_total): (i64, i64) = db
            .query_row(graph_sql, [path], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| {
                anyhow::anyhow!("incompatible_index: selected graph row byte budget exceeded: {e}")
            })?;
        // JSON may escape one source byte into six; the absolute row ceiling
        // is an explicit fail-closed selected API limit if capture exceeds it.
        let graph_row_limit = native_bytes
            .saturating_mul(6)
            .saturating_add(16 * 1024)
            .min(256 * 1024 * 1024 + 16 * 1024);
        let graph_total_limit = native_bytes
            .saturating_mul(64)
            .saturating_add(256 * 1024)
            .min(512 * 1024 * 1024);
        ensure!(
            graph_row >= 0
                && graph_total >= 0
                && graph_row <= graph_row_limit
                && graph_total <= graph_total_limit,
            "incompatible_index: selected graph row byte budget exceeded"
        );
        let file = Self::selected_source_row(db, path)?
            .context("incompatible_index: selected graph/native source missing")?;
        let witness = self.selected_native_witness(db, path)?;
        let mut stored:Vec<(String,Option<String>)>=db.prepare(
            "SELECT syntax_id,lookup_key FROM native_declarations WHERE source_set_id=?1 AND language=?2 AND revision_id=?3 AND path=?4 ORDER BY syntax_id"
        )?.query_map(params![source_set_id,language,revision_id,path],|r|Ok((r.get(0)?,r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut expected: Vec<_> = witness
            .declarations
            .iter()
            .map(|d| (d.syntax_id.clone(), d.lookup_key.clone()))
            .collect();
        stored.sort_by(|a, b| a.0.cmp(&b.0));
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        ensure!(
            stored == expected,
            "incompatible_index: selected native declaration inventory differs from source"
        );
        let key = &witness.revision.documents[0].key;
        let coverage = Self::read_native_coverage(db, key)?
            .context("incompatible_index: selected native coverage missing")?;
        ensure!(
            witness.coverage == vec![coverage],
            "incompatible_index: selected native coverage differs from source"
        );
        let proofs: Vec<(String,String,String)> = db.prepare(
            "SELECT id,content_hash,revision_id FROM native_provenance WHERE path=?1 ORDER BY id"
        )?.query_map([path], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let expected_proofs: Vec<_> = witness
            .provenance
            .iter()
            .map(|p| (p.id.clone(), p.content_hash.clone(), p.revision_id.clone()))
            .collect();
        ensure!(
            proofs == expected_proofs,
            "incompatible_index: selected native provenance differs from source"
        );
        let keys: BTreeSet<_> = witness
            .declarations
            .iter()
            .map(|d| d.lookup_key.as_deref())
            .collect();
        for lookup_key in keys {
            let mut actual =
                Self::read_native_declarations(db, &file.language, lookup_key, Some(path), false)?;
            let mut expected: Vec<_> = witness
                .declarations
                .iter()
                .filter(|d| d.lookup_key.as_deref() == lookup_key)
                .cloned()
                .collect();
            actual.sort_by(|a, b| a.syntax_id.cmp(&b.syntax_id));
            expected.sort_by(|a, b| a.syntax_id.cmp(&b.syntax_id));
            ensure!(
                actual == expected,
                "incompatible_index: selected native declarations differ from source"
            );
        }
        let mut owners: BTreeSet<String> = witness
            .declarations
            .iter()
            .map(|d| d.syntax_id.clone())
            .collect();
        for table in ["native_calls", "native_control_regions"] {
            let sql = if table == "native_calls" {
                "SELECT DISTINCT owner_syntax_id FROM native_calls WHERE path=?1"
            } else {
                "SELECT DISTINCT owner_syntax_id FROM native_control_regions WHERE path=?1"
            };
            for row in db
                .prepare(sql)?
                .query_map([path], |r| r.get::<_, String>(0))?
            {
                owners.insert(row?);
            }
        }
        for owner in owners {
            let calls = Self::read_native_calls(db, &owner)?;
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
            let regions = Self::read_native_control_regions(db, &owner)?;
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
        fn rows<T: serde::de::DeserializeOwned>(
            db: &Connection,
            table: &str,
            path: &str,
        ) -> Result<Vec<T>> {
            let sql = match table {
                "nodes" => "SELECT payload FROM nodes WHERE path=?1 ORDER BY id",
                "calls" => "SELECT payload FROM calls WHERE path=?1 ORDER BY id",
                "regions" => "SELECT payload FROM regions WHERE path=?1 ORDER BY id",
                _ => anyhow::bail!("invalid selected table"),
            };
            db.prepare(sql)?
                .query_map([path], |r| r.get::<_, String>(0))?
                .map(|row| Ok(serde_json::from_str::<T>(&row?)?))
                .collect()
        }
        let nodes: Vec<Symbol> = rows(db, "nodes", path)?;
        let calls: Vec<CallSite> = rows(db, "calls", path)?;
        let regions: Vec<ControlRegion> = rows(db, "regions", path)?;
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
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "incompatible_index: selected graph evidence differs from source: {e:#}"
            )
        })
    }

    /// Class DTOs are selected presentation projections; compare only this
    /// document's rows with a bounded in-memory class build from attested bytes.
    fn attest_selected_class(&self, db: &Connection, path: &str) -> Result<()> {
        self.attest_selected_document(db, path)?;
        let file = Self::selected_source_row(db, path)?
            .context("incompatible_index: selected class source missing")?;
        if !matches!(file.language.as_str(), "java" | "python") {
            return Ok(());
        }
        let mut stmt = db.prepare("SELECT payload FROM nodes WHERE path=?1 ORDER BY id")?;
        let nodes: Vec<Symbol> = stmt
            .query_map([path], |row| row.get::<_, String>(0))?
            .map(|payload| Ok(serde_json::from_str(&payload?)?))
            .collect::<Result<_>>()?;
        let mut expected = crate::classes::Catalog::build(
            &[file],
            &nodes,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )?
        .classes;
        let mut actual: Vec<crate::classes::ClassDefinition> = db
            .prepare("SELECT payload FROM classes WHERE path=?1 ORDER BY id")?
            .query_map([path], |row| row.get::<_, String>(0))?
            .map(|payload| Ok(serde_json::from_str(&payload?)?))
            .collect::<Result<_>>()?;
        expected.sort_by(|a, b| a.symbol.id.cmp(&b.symbol.id));
        actual.sort_by(|a, b| a.symbol.id.cmp(&b.symbol.id));
        ensure!(
            actual == expected,
            "incompatible_index: selected class projection differs from source"
        );
        Ok(())
    }

    pub fn native_declarations_at(
        &self,
        pin: IndexPin,
        language: &str,
        lookup_key: &str,
    ) -> Result<Vec<crate::native_evidence::Declaration>> {
        self.native_at(pin, |db| {
            let mut paths=db.prepare(
                "SELECT DISTINCT path FROM native_declarations WHERE revision_id=(SELECT id FROM native_revisions LIMIT 1) AND language=?1 AND lookup_key=?2 ORDER BY path LIMIT 5001"
            )?.query_map(params![language,lookup_key],|r|r.get::<_,String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ensure!(paths.len()<=5000,"incompatible_index: selected lookup document budget exceeded");
            for path in paths.drain(..) { self.attest_selected_document(db,&path)?; }
            Self::read_native_declarations(db, language, Some(lookup_key), None, false)
        })
    }
    fn read_native_declarations(
        db: &Connection,
        language: &str,
        lookup_key: Option<&str>,
        selected_path: Option<&str>,
        all_lookup_keys: bool,
    ) -> Result<Vec<crate::native_evidence::Declaration>> {
        use crate::native_evidence::{
            Declaration, DocumentKey, Header, Key, Parameter, Range, Signature,
        };

        type AncestorRow = (
            i64,
            String,
            Option<String>,
            i64,
            bool,
            Option<i64>,
            Option<bool>,
        );
        let mut types: BTreeMap<(String, i64), Vec<String>> = BTreeMap::new();
        let mut statement = db.prepare(
                "SELECT t.syntax_id,t.ancestor_ordinal,t.type_name FROM native_signature_parameter_types t                  JOIN native_declarations d ON d.syntax_id=t.syntax_id                  WHERE d.language=?1 AND (?4 OR ((?2 IS NULL AND d.lookup_key IS NULL) OR d.lookup_key=?2)) AND (?3 IS NULL OR d.path=?3) ORDER BY t.syntax_id,t.ancestor_ordinal,t.ordinal",
            )?;
        for item in statement.query_map(
            params![language, lookup_key, selected_path, all_lookup_keys],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )? {
            let (id, ordinal, name) = item?;
            types.entry((id, ordinal)).or_default().push(name);
        }
        let mut ancestors: BTreeMap<String, Vec<AncestorRow>> = BTreeMap::new();
        let mut statement = db.prepare(
                "SELECT a.syntax_id,a.ordinal,a.kind,a.name,a.sibling_ordinal,a.signature_present,                 a.type_parameter_count,a.variadic FROM native_declaration_ancestors a                  JOIN native_declarations d ON d.syntax_id=a.syntax_id                  WHERE d.language=?1 AND (?4 OR ((?2 IS NULL AND d.lookup_key IS NULL) OR d.lookup_key=?2)) AND (?3 IS NULL OR d.path=?3) ORDER BY a.syntax_id,a.ordinal",
            )?;
        for item in statement.query_map(
            params![language, lookup_key, selected_path, all_lookup_keys],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, bool>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, Option<bool>>(7)?,
                ))
            },
        )? {
            let (id, ordinal, kind, name, sibling, signature, count, variadic) = item?;
            ancestors
                .entry(id)
                .or_default()
                .push((ordinal, kind, name, sibling, signature, count, variadic));
        }
        let mut headers: BTreeMap<String, Header> = BTreeMap::new();
        let mut statement = db.prepare(
                "SELECT h.syntax_id,h.kind,h.name,h.result_type FROM native_headers h                  JOIN native_declarations d ON d.syntax_id=h.syntax_id                  WHERE d.language=?1 AND (?4 OR ((?2 IS NULL AND d.lookup_key IS NULL) OR d.lookup_key=?2)) AND (?3 IS NULL OR d.path=?3)",
            )?;
        for item in statement.query_map(
            params![language, lookup_key, selected_path, all_lookup_keys],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )? {
            let (id, kind, name, result_type) = item?;
            ensure!(
                headers
                    .insert(
                        id,
                        Header {
                            kind,
                            name,
                            modifiers: vec![],
                            type_parameters: vec![],
                            parameters: vec![],
                            result_type,
                            bases: vec![],
                        }
                    )
                    .is_none(),
                "duplicate native header"
            );
        }
        let mut statement = db.prepare(
                "SELECT i.syntax_id,i.item_kind,i.value FROM native_header_items i                  JOIN native_declarations d ON d.syntax_id=i.syntax_id                  WHERE d.language=?1 AND (?4 OR ((?2 IS NULL AND d.lookup_key IS NULL) OR d.lookup_key=?2)) AND (?3 IS NULL OR d.path=?3)                  ORDER BY i.syntax_id,i.item_kind,i.ordinal",
            )?;
        for item in statement.query_map(
            params![language, lookup_key, selected_path, all_lookup_keys],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )? {
            let (id, kind, value) = item?;
            let header = headers.get_mut(&id).context("missing native header")?;
            match kind.as_str() {
                "modifier" => header.modifiers.push(value),
                "typeParameter" => header.type_parameters.push(value),
                "base" => header.bases.push(value),
                _ => anyhow::bail!("unknown native header item"),
            }
        }
        let mut statement = db.prepare(
                "SELECT p.syntax_id,p.name,p.type_name,p.variadic FROM native_parameters p                  JOIN native_declarations d ON d.syntax_id=p.syntax_id                  WHERE d.language=?1 AND (?4 OR ((?2 IS NULL AND d.lookup_key IS NULL) OR d.lookup_key=?2)) AND (?3 IS NULL OR d.path=?3) ORDER BY p.syntax_id,p.ordinal",
            )?;
        for item in statement.query_map(
            params![language, lookup_key, selected_path, all_lookup_keys],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            },
        )? {
            let (id, name, type_name, variadic) = item?;
            headers
                .get_mut(&id)
                .context("missing native parameter header")?
                .parameters
                .push(Parameter {
                    name,
                    type_name,
                    variadic,
                });
        }
        fn signature(
            types: &mut BTreeMap<(String, i64), Vec<String>>,
            id: &str,
            ordinal: i64,
            present: bool,
            count: Option<i64>,
            variadic: Option<bool>,
        ) -> Result<Option<Signature>> {
            let parameter_types = types.remove(&(id.to_owned(), ordinal)).unwrap_or_default();
            if !present {
                ensure!(parameter_types.is_empty(), "orphan native signature types");
                return Ok(None);
            }
            Ok(Some(Signature {
                parameter_types,
                type_parameter_count: usize::try_from(count.context("missing signature count")?)?,
                variadic: variadic.context("missing signature variadic")?,
            }))
        }
        let mut stmt = db.prepare(
                "SELECT syntax_id,source_set_id,path,revision_id,kind,name,lookup_key,                 key_signature_present,key_type_parameter_count,key_variadic,key_ordinal,                 start_byte,end_byte,name_start,name_end,provenance_id                  FROM native_declarations WHERE revision_id=(SELECT id FROM native_revisions LIMIT 1)                  AND language=?1 AND (?4 OR ((?2 IS NULL AND lookup_key IS NULL) OR lookup_key=?2)) AND (?3 IS NULL OR path=?3) ORDER BY syntax_id",
            )?;
        let rows = stmt.query_map(
            params![language, lookup_key, selected_path, all_lookup_keys],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, bool>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                    r.get::<_, Option<bool>>(9)?,
                    r.get::<_, i64>(10)?,
                    r.get::<_, i64>(11)?,
                    r.get::<_, i64>(12)?,
                    r.get::<_, Option<i64>>(13)?,
                    r.get::<_, Option<i64>>(14)?,
                    r.get::<_, String>(15)?,
                ))
            },
        )?;
        let mut declarations = Vec::new();
        for row in rows {
            let (
                syntax_id,
                source_set_id,
                path,
                revision_id,
                kind,
                name,
                lookup_key,
                present,
                count,
                variadic,
                key_ordinal,
                start,
                end,
                name_start,
                name_end,
                provenance_id,
            ) = row?;
            let key = Key {
                kind: kind.clone(),
                name: name.clone(),
                signature: signature(&mut types, &syntax_id, -1, present, count, variadic)?,
                ordinal: usize::try_from(key_ordinal)?,
            };
            let mut ancestor_keys = Vec::new();
            for (ordinal, kind, name, sibling, present, count, variadic) in
                ancestors.remove(&syntax_id).unwrap_or_default()
            {
                ensure!(
                    ordinal == ancestor_keys.len() as i64,
                    "native ancestor ordinal gap"
                );
                ancestor_keys.push(Key {
                    kind,
                    name,
                    signature: signature(
                        &mut types, &syntax_id, ordinal, present, count, variadic,
                    )?,
                    ordinal: usize::try_from(sibling)?,
                });
            }
            let name_range = name_start
                .zip(name_end)
                .map(|(start, end)| -> Result<Range> {
                    Ok(Range {
                        start: usize::try_from(start)?,
                        end: usize::try_from(end)?,
                    })
                })
                .transpose()?;
            declarations.push(Declaration {
                syntax_id: syntax_id.clone(),
                document: DocumentKey {
                    source_set_id,
                    language: language.to_owned(),
                    path,
                },
                revision_id,
                kind,
                name,
                lookup_key,
                ancestors: ancestor_keys,
                key,
                range: Range {
                    start: usize::try_from(start)?,
                    end: usize::try_from(end)?,
                },
                name_range,
                header: headers
                    .remove(&syntax_id)
                    .context("missing native declaration header")?,
                provenance_id,
            });
        }
        ensure!(
            types.is_empty() && ancestors.is_empty() && headers.is_empty(),
            "orphan native declaration children"
        );
        Ok(declarations)
    }

    pub fn native_source_at(
        &self,
        pin: IndexPin,
        key: &crate::native_evidence::DocumentKey,
    ) -> Result<Option<(crate::native_evidence::Document, Vec<u8>)>> {
        use crate::native_evidence::Document;
        self.native_at(pin, |db| {
            let sizes:Option<[i64;6]>=db.query_row(
                "SELECT length(source_bytes),length(CAST(source_set_id AS BLOB)),length(CAST(language AS BLOB)),length(CAST(path AS BLOB)),length(CAST(revision_id AS BLOB)),length(CAST(content_hash AS BLOB)) FROM native_documents WHERE source_set_id=?1 AND language=?2 AND path=?3",
                params![key.source_set_id,key.language,key.path],|r|Ok([
                    r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?]),
            ).optional()?;
            ensure!(sizes.is_none_or(|bytes|bytes[0]>=0 && bytes[0]<=256*1024*1024
                && bytes[1..].iter().all(|length|*length>=0 && *length<=16*1024)),
                "incompatible_index: selected native source byte budget exceeded");
            type NativeSourceRow=(String,String,i64,Vec<u8>);
            let row:Option<Result<NativeSourceRow>>=db.query_row(
                "SELECT revision_id,content_hash,byte_length,source_bytes FROM native_documents WHERE source_set_id=?1 AND language=?2 AND path=?3",
                params![key.source_set_id,key.language,key.path],|r|Ok((||->Result<_>{
                    use sha2::{Digest,Sha256};
                    let revision_id:String=r.get(0)?;
                    let hash:String=r.get(1)?;
                    let length:i64=r.get(2)?;
                    let raw=r.get_ref(3)?.as_blob()?;
                    ensure!(length==raw.len() as i64 && hash==hex::encode(Sha256::digest(raw)),
                        "incompatible_index: native source hash mismatch");
                    Ok((revision_id,hash,length,r.get(3)?))
                })()),
            ).optional()?;
            let row=row.transpose()?.map(|(revision_id,content_hash,byte_length,bytes)|->Result<_>{
                let graph=Self::selected_source_row(db,&key.path)?
                    .context("incompatible_index: paired graph source missing")?;
                ensure!(graph.path==key.path && graph.language==key.language
                    && graph.hash==content_hash && graph.text.as_bytes()==bytes,
                    "incompatible_index: native source differs from paired graph");
                Ok((Document {key:key.clone(),revision_id,content_hash,byte_length:usize::try_from(byte_length)?},bytes))
            }).transpose()?;
            if row.is_none() {
                let graph_file:bool=db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE path=?1)",[&key.path],|r|r.get(0),
                )?;
                ensure!(!graph_file,"incompatible_index: selected native source absent");
            }
            Ok(row)
        })
    }
    pub fn native_coverage_at(
        &self,
        pin: IndexPin,
        key: &crate::native_evidence::DocumentKey,
    ) -> Result<Option<crate::native_evidence::Coverage>> {
        self.native_at(pin, |db| {
            let selected:bool=db.query_row(
                "SELECT EXISTS(SELECT 1 FROM native_documents WHERE source_set_id=?1 AND language=?2 AND path=?3)",
                params![key.source_set_id,key.language,key.path],|r|r.get(0),
            )?;
            if selected { self.attest_selected_document(db,&key.path)?; }
            let row=Self::read_native_coverage(db,key)?;
            ensure!(!selected || row.is_some(),
                "incompatible_index: selected native coverage missing");
            Ok(row)
        })
    }
    fn read_native_coverage(
        db: &Connection,
        key: &crate::native_evidence::DocumentKey,
    ) -> Result<Option<crate::native_evidence::Coverage>> {
        use crate::native_evidence::Coverage;

        let row: Option<(String,String,bool,bool,String,Option<String>)> = db.query_row("SELECT producer_id,revision_id,requested,selected,state,diagnostic FROM native_coverage WHERE source_set_id=?1 AND language=?2 AND document_path=?3",
                params![key.source_set_id,key.language,key.path],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).optional()?;
        row.map(|(producer_id,revision_id,requested,selected,state,diagnostic)| -> Result<_> {
                let mut roles = db.prepare("SELECT role_kind,role FROM native_coverage_roles WHERE producer_id=?1 AND revision_id=?2 AND language=?3 AND document_path=?4 ORDER BY role_kind,ordinal")?;
                let mut supported_roles=Vec::new();
                let mut observed_roles=Vec::new();
                for role in roles.query_map(params![producer_id,revision_id,key.language,key.path],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))? {
                    let (kind,role)=role?;
                    match kind.as_str() { "supported"=>supported_roles.push(role),"observed"=>observed_roles.push(role),_=>anyhow::bail!("invalid native coverage role kind") }
                }
                Ok(Coverage {producer_id,language:key.language.clone(),source_set_id:key.source_set_id.clone(),document_path:key.path.clone(),revision_id,requested,selected,state,supported_roles,observed_roles,diagnostic})
            }).transpose()
    }
    pub fn native_calls_at(
        &self,
        pin: IndexPin,
        owner: &str,
    ) -> Result<Vec<crate::native_evidence::Call>> {
        self.native_at(pin, |db| {
            let path: Option<String> = db
                .query_row(
                    "SELECT path FROM native_declarations WHERE syntax_id=?1",
                    [owner],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(ref path) = path {
                self.attest_selected_document(db, path)?;
            } else {
                let dangling: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM native_calls WHERE owner_syntax_id=?1)",
                    [owner],
                    |r| r.get(0),
                )?;
                ensure!(!dangling, "incompatible_index: native owner absent");
            }
            Self::read_native_calls(db, owner)
        })
    }
    fn read_native_calls(
        db: &Connection,
        owner: &str,
    ) -> Result<Vec<crate::native_evidence::Call>> {
        use crate::native_evidence::{Call, DocumentKey, Range};

        let mut regions = BTreeMap::<String, Vec<String>>::new();
        let mut region_stmt = db.prepare(
                "SELECT r.call_id,r.region_id FROM native_call_regions r                  JOIN native_calls c ON c.id=r.call_id WHERE c.owner_syntax_id=?1                  ORDER BY c.ordinal,r.ordinal",
            )?;
        for row in region_stmt.query_map([owner], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (call, region) = row?;
            regions.entry(call).or_default().push(region);
        }
        let mut stmt = db.prepare(
                "SELECT id,ordinal,source_set_id,language,path,revision_id,start_byte,end_byte,                 callee_start,callee_end,spelling,provenance_id FROM native_calls                  WHERE owner_syntax_id=?1 ORDER BY ordinal",
            )?;
        let rows = stmt.query_map([owner], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, String>(11)?,
            ))
        })?;
        let mut calls = Vec::new();
        for row in rows {
            let (
                id,
                ordinal,
                source_set_id,
                language,
                path,
                revision_id,
                start,
                end,
                callee_start,
                callee_end,
                spelling,
                provenance_id,
            ) = row?;
            let callee_range = callee_start
                .zip(callee_end)
                .map(|(start, end)| -> Result<Range> {
                    Ok(Range {
                        start: usize::try_from(start)?,
                        end: usize::try_from(end)?,
                    })
                })
                .transpose()?;
            let region_ids = regions.remove(&id).unwrap_or_default();
            calls.push(Call {
                id,
                owner_syntax_id: owner.to_owned(),
                ordinal: usize::try_from(ordinal)?,
                document: DocumentKey {
                    source_set_id,
                    language,
                    path,
                },
                revision_id,
                range: Range {
                    start: usize::try_from(start)?,
                    end: usize::try_from(end)?,
                },
                callee_range,
                spelling,
                region_ids,
                provenance_id,
            });
        }
        ensure!(regions.is_empty(), "native region references missing call");
        Ok(calls)
    }

    pub fn native_control_regions_at(
        &self,
        pin: IndexPin,
        owner: &str,
    ) -> Result<Vec<crate::native_evidence::ControlRegion>> {
        self.native_at(pin, |db| {
            let path: Option<String> = db
                .query_row(
                    "SELECT path FROM native_declarations WHERE syntax_id=?1",
                    [owner],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(ref path) = path {
                self.attest_selected_document(db, path)?;
            } else {
                let dangling: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM native_control_regions WHERE owner_syntax_id=?1)",
                    [owner],
                    |r| r.get(0),
                )?;
                ensure!(!dangling, "incompatible_index: native owner absent");
            }
            Self::read_native_control_regions(db, owner)
        })
    }
    fn read_native_control_regions(
        db: &Connection,
        owner: &str,
    ) -> Result<Vec<crate::native_evidence::ControlRegion>> {
        use crate::native_evidence::{ControlRegion, DocumentKey, Range};

        let mut stmt=db.prepare("SELECT id,ordinal,source_set_id,language,path,revision_id,kind,start_byte,end_byte,parent_id,arm,provenance_id FROM native_control_regions WHERE owner_syntax_id=?1 ORDER BY ordinal")?;
        let rows = stmt.query_map([owner], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, String>(11)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                ordinal,
                source_set_id,
                language,
                path,
                revision_id,
                kind,
                start,
                end,
                parent_id,
                arm,
                provenance_id,
            ) = row?;
            Ok(ControlRegion {
                id,
                owner_syntax_id: owner.to_owned(),
                ordinal: usize::try_from(ordinal)?,
                document: DocumentKey {
                    source_set_id,
                    language,
                    path,
                },
                revision_id,
                kind,
                range: Range {
                    start: usize::try_from(start)?,
                    end: usize::try_from(end)?,
                },
                parent_id,
                arm,
                provenance_id,
            })
        })
        .collect()
    }
    /// Search only the persisted projection. Wildcards are literal user text.
    /// One read snapshot and revision guard; no source reads or catalog rebuilds.
    pub fn navigation_at(
        &self,
        request: &crate::navigation::NavigationRequest,
    ) -> Result<crate::navigation::NavigationResult> {
        request.validate()?;
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        ensure!(request.expected_revision() == revision, "revision conflict");
        // Navigation's source selector counts lines in the graph JSON, and its
        // member selector reads graph class/node rows. Before either consumes a
        // selected document, authenticate that JSON against the paired native BLOB
        // in this same read transaction. Never scan the entire workspace here.
        let selected_path: Option<String> = match request {
            crate::navigation::NavigationRequest::Source(s) => Some(s.path.clone()),
            crate::navigation::NavigationRequest::Member(s) => tx.query_row(
                "SELECT path FROM nodes WHERE id=?1 AND json_extract(payload,'$.kind')='class' LIMIT 1",
                [&s.class_id], |row| row.get(0),
            ).optional()?,
        };
        if let Some(path) = selected_path {
            // Gate allocation of the selected JSON/BLOB before decoding either.
            // This reads only SQLite byte lengths, not every workspace document.
            let sizes: Option<(i64, i64)> = tx.query_row(
                "SELECT length(CAST(f.payload AS BLOB)),length(d.source_bytes) FROM files f JOIN native_documents d ON d.path=f.path WHERE f.path=?1",
                [&path], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            if let Some((graph_len, native_len)) = sizes {
                ensure!(
                    graph_len <= 2 * 1024 * 1024 && native_len <= 2 * 1024 * 1024,
                    "incompatible_index: selected navigation source exceeds budget"
                );
                self.attest_selected_document(&tx, &path)?;
            } else {
                let graph_file: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE path=?1)",
                    [&path],
                    |row| row.get(0),
                )?;
                ensure!(
                    !graph_file,
                    "incompatible_index: selected native document missing"
                );
            }
        }
        crate::navigation::navigate(&tx, request, revision)
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
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        ensure!(expected.is_none_or(|r| r == revision), "revision conflict");
        let (mut warnings, truncated) = class_metadata(&tx)?;
        let pattern = format!(
            "%{}%",
            query
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut stmt = tx.prepare("SELECT id FROM classes WHERE (?1 IS NULL OR path=?1)
            AND (name LIKE ?2 ESCAPE '\\' OR qualified_name LIKE ?2 ESCAPE '\\' OR id=?3)
            ORDER BY CASE WHEN lower(name)=lower(?3) THEN 0 ELSE 1 END,qualified_name,path,id LIMIT ?4 OFFSET ?5")?;
        let ids = stmt
            .query_map(
                params![path, pattern, query, (limit + 1) as i64, offset as i64],
                |r| r.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut items = Vec::new();
        let mut consumed = 0;
        let mut bytes = 0;
        let mut byte_limited = false;
        for id in ids.iter().take(limit) {
            let Some((class, size, clipped)) = presentation_class(&tx, id)? else {
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
        let paths: BTreeSet<_> = items
            .iter()
            .map(|class| class.symbol.path.as_str())
            .collect();
        for path in paths {
            self.attest_selected_class(&tx, path)?;
        }
        Ok(ClassPage {
            revision,
            items,
            next_offset,
            truncated,
            warnings,
            require_index: false,
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
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        ensure!(revision == request.expected_revision, "revision conflict");
        let (warnings, truncated) = class_metadata(&tx)?;
        let (seed, clipped) = resolve_class(&tx, &request.seed)?;
        // Explicitly selected measured declarations are independent roots, never
        // connected by lexical type-name matches or candidate relationships.
        let mut seeds = vec![seed.symbol.id.clone()];
        let mut classes = BTreeMap::from([(seed.symbol.id.clone(), seed)]);
        for expanded in &request.expanded {
            let (class, _) = resolve_class(&tx, expanded)?;
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
            self.attest_selected_class(&tx, path)?;
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
    }
    pub fn symbols(&self, query: &str, limit: usize) -> Result<Vec<Symbol>> {
        Ok(self.symbols_at(query, limit)?.1)
    }
    pub fn symbols_at(&self, query: &str, limit: usize) -> Result<(IndexPin, Vec<Symbol>)> {
        ensure!(query.len() <= 8192, "search query too long");
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        let selected_paths: BTreeSet<String> = tx.prepare("SELECT path FROM nodes WHERE instr(lower(name),lower(?1)) > 0 OR instr(lower(id),lower(?1)) > 0 ORDER BY CASE WHEN lower(name)=lower(?1) THEN 0 WHEN instr(lower(name),lower(?1))=1 THEN 1 ELSE 2 END,name,id LIMIT ?2")?
            .query_map(params![query,limit.min(150) as i64],|r|r.get::<_,String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        for path in selected_paths {
            self.attest_selected_document(&tx, &path)?;
        }
        let mut stmt = tx.prepare("SELECT payload FROM nodes WHERE instr(lower(name),lower(?1)) > 0 OR instr(lower(id),lower(?1)) > 0 ORDER BY CASE WHEN lower(name)=lower(?1) THEN 0 WHEN instr(lower(name),lower(?1))=1 THEN 1 ELSE 2 END,name,id LIMIT ?2")?;
        let values = stmt.query_map(params![query, limit.min(150) as i64], |r| {
            r.get::<_, String>(0)
        })?;
        let values = values
            .map(|v| Ok(serde_json::from_str(&v?)?))
            .collect::<Result<Vec<Symbol>>>()?;
        let paths: BTreeSet<_> = values.iter().map(|node| node.path.as_str()).collect();
        for path in paths {
            self.attest_selected_document(&tx, path)?;
        }
        Ok((revision, values))
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
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        ensure!(
            expected_revision.is_none_or(|pin| pin == revision),
            "revision conflict"
        );
        let selected_path: Option<String> = tx
            .query_row("SELECT path FROM nodes WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        if let Some(path) = selected_path {
            self.attest_selected_document(&tx, &path)?;
        }
        let node: Option<Symbol> = one(&tx, "SELECT payload FROM nodes WHERE id=?1", id)?;
        Ok(node.map(|node| (revision, node)))
    }
    fn selected_source_row(db: &Connection, path: &str) -> Result<Option<SourceFile>> {
        Self::selected_source_row_bounded(db, path, 256 * 1024 * 1024)
    }
    fn selected_source_row_bounded(
        db: &Connection,
        path: &str,
        max_bytes: i64,
    ) -> Result<Option<SourceFile>> {
        // SQL bounds before copying either BLOB/JSON. The 256MiB+16KiB
        // SourceFile JSON ceiling is also enforced on the immutable captured
        // graph BEFORE paired CAS. Escaping above the ceiling refuses the new
        // bundle atomically, so a public ready pair is never unreadable for it.
        // Native-source bytes/hash are checked independently in this snapshot.
        // Check selected row byte lengths before copying a BLOB or decoding JSON.
        let sizes:Option<[i64;9]>=db.query_row(
            "SELECT length(CAST(f.payload AS BLOB)),length(d.source_bytes),length(CAST(f.path AS BLOB)),length(CAST(f.hash AS BLOB)),length(CAST(d.path AS BLOB)),length(CAST(d.content_hash AS BLOB)),length(CAST(d.source_set_id AS BLOB)),length(CAST(d.language AS BLOB)),length(CAST(d.revision_id AS BLOB)) FROM files f JOIN native_documents d ON d.path=f.path WHERE f.path=?1",
            [path],|r|Ok([r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?]),
        ).optional()?;
        if let Some(bytes) = sizes {
            let (graph_bytes, native_bytes) = (bytes[0], bytes[1]);
            ensure!(
                graph_bytes >= 0
                    && native_bytes >= 0
                    && graph_bytes <= max_bytes.saturating_add(16 * 1024)
                    && graph_bytes <= native_bytes.saturating_mul(6).saturating_add(16 * 1024)
                    && native_bytes <= max_bytes
                    && bytes[2..]
                        .iter()
                        .all(|length| *length >= 0 && *length <= 16 * 1024),
                "incompatible_index: selected source byte budget exceeded"
            );
        }
        type PairedSourceRow = (String, String, String, Vec<u8>, String, i64);
        let row:Option<Result<PairedSourceRow>>=db.query_row(
            "SELECT f.payload,f.hash,d.content_hash,d.source_bytes,d.language,d.byte_length FROM native_documents d JOIN files f ON f.path=d.path WHERE d.path=?1",
            [path],|row|Ok((||->Result<_>{
                use sha2::{Digest,Sha256};
                let graph_hash:String=row.get(1)?;
                let hash:String=row.get(2)?;
                let source=row.get_ref(3)?.as_blob()?;
                let length:i64=row.get(5)?;
                // Hash SQLite's borrowed BLOB before allocating the Rust source
                // Vec or graph JSON String. The earlier SQL caps bound this scan.
                ensure!(length==source.len() as i64
                    && hash==hex::encode(Sha256::digest(source))
                    && graph_hash==hash,"incompatible_index: source hash mismatch");
                Ok((row.get(0)?,graph_hash,hash,row.get(3)?,row.get(4)?,length))
            })()),
        ).optional()?;
        row.transpose()?
            .map(
                |(payload, graph_hash, hash, bytes, language, _length)| -> Result<_> {
                    let file: SourceFile = serde_json::from_str(&payload)?;
                    let text = String::from_utf8(bytes)?;
                    ensure!(
                        file.path == path
                            && file.text == text
                            && file.hash == hash
                            && graph_hash == hash
                            && file.language == language,
                        "incompatible_index: source bytes mismatch"
                    );
                    Ok(SourceFile { text, ..file })
                },
            )
            .transpose()
    }
    pub fn source_at(
        &self,
        path: &str,
        expected_revision: Option<IndexPin>,
    ) -> Result<Option<(IndexPin, SourceFile)>> {
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        ensure!(
            expected_revision.is_none_or(|pin| pin == revision),
            "revision conflict"
        );
        Ok(Self::selected_source_row(&tx, path)?.map(|file| (revision, file)))
    }
    /// Catalog reads pin revision and rows to one SQLite read transaction.
    /// Enrich only the visible tree page from one cached index snapshot.
    pub fn tree_metadata(
        &self,
        root: &Path,
        items: &mut [crate::file_tree::Entry],
    ) -> Result<(IndexPin, String)> {
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        let workspace = Path::new(&self.workspace_root);
        let mut stmt = tx.prepare("SELECT (SELECT count(*) FROM nodes n WHERE n.path=f.path AND json_extract(n.payload,'$.kind') IN ('function','method')) FROM files f WHERE f.path=?1")?;
        for item in items.iter_mut().filter(|e| e.kind == "file") {
            let absolute = root.join(&item.path);
            let Ok(relative) = absolute.strip_prefix(workspace) else {
                continue;
            };
            let Some(relative) = relative.to_str() else {
                continue;
            };
            let count: Option<i64> = stmt.query_row([relative], |r| r.get(0)).optional()?;
            if let Some(count) = count {
                item.indexed_path = Some(relative.into());
                item.method_count = Some(usize::try_from(count)?);
            }
        }
        Ok((revision, self.workspace_root.clone()))
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
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        ensure!(expected.is_none_or(|r| r == revision), "revision conflict");
        let mut stmt = tx.prepare("SELECT f.path,json_extract(f.payload,'$.language'),(SELECT count(*) FROM nodes n WHERE n.path=f.path AND json_extract(n.payload,'$.kind') IN ('function','method')) FROM files f ORDER BY f.path LIMIT ?1 OFFSET ?2")?;
        let mut items = stmt.query_map(params![(limit + 1) as i64, offset as i64], |r| {
            Ok(serde_json::json!({"path":r.get::<_,String>(0)?,"language":r.get::<_,String>(1)?,"methodCount":r.get::<_,i64>(2)?}))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        let next = (items.len() > limit).then_some(offset + limit);
        items.truncate(limit);
        Ok(serde_json::json!({"revision":revision,"items":items,"nextOffset":next}))
    }
    pub fn methods_at(
        &self,
        path: &str,
        expected: Option<IndexPin>,
    ) -> Result<Option<serde_json::Value>> {
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        ensure!(expected.is_none_or(|r| r == revision), "revision conflict");
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE path=?1)",
            [path],
            |r| r.get::<_, bool>(0),
        )? {
            return Ok(None);
        }
        let mut stmt = tx.prepare("SELECT payload FROM nodes WHERE path=?1 AND json_extract(payload,'$.kind') IN ('function','method') ORDER BY json_extract(payload,'$.range.startByte'),id LIMIT 1001")?;
        let values = stmt.query_map([path], |r| r.get::<_, String>(0))?;
        let mut items = Vec::new();
        for value in values {
            let symbol: Symbol = serde_json::from_str(&value?)?;
            // Accessor syntax alone does not prove a body is trivial. Without
            // semantic proof retain it, including zero-call validations/checks.
            items.push(serde_json::json!({"symbol":symbol,"consequential":true,"reason":"Conservative heuristic: retained; triviality is not proven"}));
        }
        let truncated = items.len() > 1000;
        items.truncate(1000);
        Ok(Some(
            serde_json::json!({"revision":revision,"items":items,"truncated":truncated}),
        ))
    }
    /// Only cached source and measured calls from the same snapshot are used.
    pub fn sequence_at(
        &self,
        seed: &str,
        expected: IndexPin,
        show_all: bool,
    ) -> Result<Option<crate::behavior::SequenceView>> {
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        ensure!(revision == expected, "revision conflict");
        let selected_path: Option<String> = tx
            .query_row("SELECT path FROM nodes WHERE id=?1", [seed], |r| r.get(0))
            .optional()?;
        if let Some(path) = selected_path {
            self.attest_selected_document(&tx, &path)?;
        }
        let Some(symbol) = one::<Symbol>(&tx, "SELECT payload FROM nodes WHERE id=?1", seed)?
        else {
            return Ok(None);
        };
        ensure!(
            matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method),
            "invalid sequence symbol kind"
        );
        self.attest_selected_document(&tx, &symbol.path)?;
        let file = one::<SourceFile>(&tx, "SELECT payload FROM files WHERE path=?1", &symbol.path)?
            .context("sequence source missing")?;
        let mut stmt = tx.prepare("SELECT payload FROM calls WHERE path=?1 ORDER BY json_extract(payload,'$.range.startByte'),id")?;
        let values = stmt.query_map([&symbol.path], |r| r.get::<_, String>(0))?;
        let calls = values
            .map(|v| Ok(serde_json::from_str::<CallSite>(&v?)?))
            .collect::<Result<Vec<_>>>()?;
        crate::behavior::build_sequence(revision, &symbol, &file, &calls, show_all).map(Some)
    }
    fn read_graph(&self, db: &Connection) -> Result<Graph> {
        let status = self.read_status(db)?;
        Ok(Graph {
            schema_version: SCHEMA_VERSION,
            files: rows(db, "SELECT payload FROM files ORDER BY path")?,
            nodes: rows(db, "SELECT payload FROM nodes ORDER BY id")?,
            calls: rows(
                db,
                "SELECT payload FROM calls ORDER BY path,json_extract(payload,'$.range.startByte'),json_extract(payload,'$.range.endByte') DESC,id",
            )?,
            regions: rows(db, "SELECT payload FROM regions ORDER BY id")?,
            diagnostics: status.diagnostics,
            stats: status.stats,
        })
    }
    pub fn graph(&self) -> Result<Graph> {
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let graph = self.read_graph(&tx)?;
        storage_result(tx.commit())?;
        Ok(graph)
    }
    /// Reauthenticate only a cached packet's selected evidence under one pin.
    /// Unrelated documents are not read, so bounded status stays metadata-only.
    pub fn validate_selected_view(&self, view: &ViewResult, sources: &[SourceFile]) -> Result<()> {
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        ensure!(
            self.read_status(&tx)?.revision == view.revision,
            "revision conflict: cached packet pin changed"
        );
        let paths: BTreeSet<_> = view
            .nodes
            .iter()
            .map(|n| n.path.as_str())
            .chain(view.calls.iter().map(|c| c.path.as_str()))
            .chain(view.regions.iter().map(|r| r.path.as_str()))
            .chain(sources.iter().map(|f| f.path.as_str()))
            .collect();
        for path in paths {
            self.attest_selected_document(&tx, path)?;
        }
        for node in &view.nodes {
            let actual: Option<Symbol> =
                one(&tx, "SELECT payload FROM nodes WHERE id=?1", &node.id)?;
            ensure!(
                actual.as_ref() == Some(node),
                "incompatible_index: cached packet selected graph declaration changed"
            );
        }
        for call in &view.calls {
            let actual: Option<CallSite> =
                one(&tx, "SELECT payload FROM calls WHERE id=?1", &call.id)?;
            ensure!(
                actual.as_ref() == Some(call),
                "incompatible_index: cached packet selected graph call changed"
            );
        }
        for region in &view.regions {
            let actual: Option<ControlRegion> =
                one(&tx, "SELECT payload FROM regions WHERE id=?1", &region.id)?;
            ensure!(
                actual.as_ref() == Some(region),
                "incompatible_index: cached packet selected graph region changed"
            );
        }
        for source in sources {
            ensure!(
                Self::selected_source_row(&tx, &source.path)?.as_ref() == Some(source),
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
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        ensure!(
            expected_pin.is_none_or(|expected| *expected == revision),
            "revision conflict: stale native pin"
        );
        let selected_path: Option<String> = tx
            .query_row("SELECT path FROM nodes WHERE id=?1", [&query.seed], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(path) = selected_path {
            self.attest_selected_document(&tx, &path)?;
        }
        let seed: Option<Symbol> = one(&tx, "SELECT payload FROM nodes WHERE id=?1", &query.seed)?;
        let Some(seed) = seed else { return Ok(None) };
        let mut calls = Vec::new();
        let mut region_ids = BTreeSet::new();
        let mut truncated = false;
        if !query.exclude_paths.iter().any(|p| seed.path.starts_with(p)) {
            let mut stmt = tx.prepare("SELECT payload FROM calls WHERE caller=?1 ORDER BY path,json_extract(payload,'$.range.startByte'),json_extract(payload,'$.range.endByte') DESC,id LIMIT ?2")?;
            let rows = stmt.query_map(params![seed.id, (query.max_calls + 1) as i64], |r| {
                r.get::<_, String>(0)
            })?;
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
            let region: Option<ControlRegion> =
                one(&tx, "SELECT payload FROM regions WHERE id=?1", &id)?;
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
            self.attest_selected_document(&tx, path)?;
        }
        storage_result(tx.commit())?;
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
        let selected: Option<(String, String)> = db
            .query_row(
                "SELECT language,path FROM native_declarations WHERE syntax_id=?1",
                [target],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (language, path) = selected.context("native declaration target missing")?;
        store.attest_selected_document(db, &path)?;
        let declarations = Self::read_native_declarations(db, &language, None, Some(&path), true)?;
        let focused = anchors::find_selected(target, &declarations)?;
        anchors::capture_anchor(focused, &declarations)
    }

    fn declarations_for_anchor(
        db: &Connection,
        store: &Self,
        anchor: &DurableAnchor,
    ) -> Result<(String, Vec<crate::native_evidence::Declaration>)> {
        let revision: Option<String> = db.query_row(
            "SELECT revision_id FROM native_documents WHERE source_set_id=?1 AND language=?2 AND path=?3",
            params![anchor.document.source_set_id, anchor.document.language, anchor.document.path], |row| row.get(0),
        ).optional()?;
        let revision = revision.context("invalid anchor document association")?;
        store.attest_selected_document(db, &anchor.document.path)?;
        let declarations = Self::read_native_declarations(
            db,
            &anchor.document.language,
            None,
            Some(&anchor.document.path),
            true,
        )?;
        ensure!(
            anchors::document_matches(&anchor.document, &declarations),
            "invalid anchor document association"
        );
        Ok((revision, declarations))
    }

    fn anchor_attachment(
        db: &Connection,
        store: &Self,
        raw: Option<&serde_json::value::RawValue>,
    ) -> Result<AnchorAttachment> {
        let Some(raw) = raw else {
            return Ok(AnchorAttachment {
                availability: AttachmentAvailability::Anchorless,
                result: None,
            });
        };
        let anchor: DurableAnchor = serde_json::from_str(raw.get())?;
        anchor.validate()?;
        let (revision, declarations) = Self::declarations_for_anchor(db, store, &anchor)?;
        let current = declarations
            .iter()
            .find(|row| row.syntax_id == anchor.syntax_id);
        let result = anchors::audit_anchor(&anchor, &revision, current, &declarations, None)?;
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
        let ids: BTreeSet<&String> = std::iter::once(&view.query.seed)
            .chain(view.pins.keys())
            .chain(view.hidden.iter())
            .collect();
        let mut orphaned_ids = vec![];
        for id in ids {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE id=?1)",
                [id],
                |r| r.get(0),
            )?;
            if !exists {
                orphaned_ids.push(id.clone());
            }
        }
        let attachment = Self::anchor_attachment(db, store, view.anchor.as_deref())?;
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
        if views.is_empty() && expected_pin.is_none() {
            return Ok(vec![]);
        }
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let control = self.read_control_status(&tx)?;
        if control.evidence_format.is_none() {
            ensure!(
                expected_pin.is_none(),
                "revision conflict: native evidence unavailable"
            );
            return Ok(views.into_iter().map(Self::unavailable_view).collect());
        }
        let pin = self.read_status(&tx)?.revision;
        ensure!(
            expected_pin.is_none_or(|expected| expected == pin),
            "revision conflict: stale native pin"
        );
        views
            .into_iter()
            .map(|view| Self::resolve_view(&tx, self, view, Some(pin)))
            .collect()
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
        let Some(view) = view else {
            if let Some(expected) = expected_pin {
                let mut db = self.cache()?;
                let tx = storage_result(db.transaction())?;
                let status = self.read_control_status(&tx)?;
                ensure!(
                    status.evidence_format.is_some(),
                    "revision conflict: native evidence unavailable"
                );
                ensure!(
                    status.revision == expected,
                    "revision conflict: stale native pin"
                );
                self.read_status(&tx)?;
            }
            return Ok(None);
        };
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let control = self.read_control_status(&tx)?;
        if control.evidence_format.is_none() {
            ensure!(
                expected_pin.is_none(),
                "revision conflict: native evidence unavailable"
            );
            return Ok(Some(Self::unavailable_view(view)));
        }
        let pin = self.read_status(&tx)?.revision;
        ensure!(
            expected_pin.is_none_or(|expected| expected == pin),
            "revision conflict: stale native pin"
        );
        Ok(Some(Self::resolve_view(&tx, self, view, Some(pin))?))
    }
    pub fn view(&self, id: &str) -> Result<Option<SavedViewState>> {
        self.saved_view_at(id, None)
    }

    pub fn save_view_at(&self, pin: IndexPin, view: &SavedView) -> Result<SavedViewState> {
        view.validate()?;
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let status = self.read_control_status(&tx)?;
        ensure!(
            status.evidence_format.is_some(),
            "revision conflict: native evidence unavailable"
        );
        ensure!(
            status.revision == pin,
            "revision conflict: stale native pin"
        );
        self.read_status(&tx)?;
        let record = self.records().update_view_record(
            &SavedViewRecord::from_base(view.clone(), None),
            || {
                serde_json::value::to_raw_value(&Self::selected_anchor_in(
                    &tx,
                    self,
                    &view.query.seed,
                )?)
                .map_err(Into::into)
            },
        )?;
        Self::resolve_view(&tx, self, record, Some(pin))
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
        let attachment = Self::anchor_attachment(db, store, annotation.anchor.as_deref())?;
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
        if annotations.is_empty() && expected_pin.is_none() {
            return Ok(vec![]);
        }
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let control = self.read_control_status(&tx)?;
        if control.evidence_format.is_none() {
            ensure!(
                expected_pin.is_none(),
                "revision conflict: native evidence unavailable"
            );
            return Ok(annotations
                .into_iter()
                .map(Self::unavailable_annotation)
                .collect());
        }
        let pin = self.read_status(&tx)?.revision;
        ensure!(
            expected_pin.is_none_or(|expected| expected == pin),
            "revision conflict: stale native pin"
        );
        annotations
            .into_iter()
            .map(|item| Self::resolve_annotation(&tx, self, item, Some(pin)))
            .collect()
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
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let status = self.read_control_status(&tx)?;
        ensure!(
            status.evidence_format.is_some(),
            "revision conflict: native evidence unavailable"
        );
        ensure!(
            status.revision == pin,
            "revision conflict: stale native pin"
        );
        self.read_status(&tx)?;
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
                    &tx,
                    self,
                    &request.node_id,
                )?)
                .map_err(Into::into)
            },
        )?;
        Self::resolve_annotation(&tx, self, record, Some(pin))
    }
    pub fn delete_annotation(&self, id: &str) -> Result<bool> {
        self.records().delete_annotation(id)
    }
}

#[cfg(test)]
mod rebaseline_fault_tests {
    use super::*;
    use crate::indexer::{IndexOptions, index_workspace_bundle};
    use std::{fs, ptr, sync::atomic::AtomicBool};

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
        let leader = store.leader().unwrap();
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                &leader,
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

        let (entered_tx, entered_rx) = mpsc::sync_channel::<()>(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker_store = store.clone();
        let worker = std::thread::spawn(move || {
            worker_store.publish_inner_checked(
                (&next, &next_capture, &next_native),
                &leader,
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
                    let added: i64 = tx.query_row(
                        "SELECT count(*) FROM classes WHERE path='Types.java' AND name='Added'",
                        [],
                        |row| row.get(0),
                    )?;
                    ensure!(added == 1, "new class projection not staged");
                    let raw: Vec<u8> = tx.query_row(
                        "SELECT source_bytes FROM native_documents WHERE path='Types.java'",
                        [],
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
        let leader = store.leader().unwrap();
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                &leader,
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

        let (entered_tx, entered_rx) = mpsc::sync_channel::<()>(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker_store = store.clone();
        let worker = std::thread::spawn(move || {
            worker_store.publish_inner_checked(
                (&next,&next_capture,&next_native),&leader,pin,&worker_cancel,
                |stage,tx|{
                    if stage!=PublishStage::BeforeCommit {return Ok(());}
                    // All graph/native/class inserts and paired checks precede
                    // this callback; the final cancellation check follows it.
                    let revision:i64=tx.query_row(
                        "SELECT index_revision FROM index_metadata WHERE singleton=1",[],|r|r.get(0))?;
                    ensure!(revision==i64::try_from(pin.index_revision+1)?,
                        "new revision not staged in publisher transaction");
                    let graph_functions:i64=tx.query_row(
                        "SELECT count(*) FROM nodes WHERE path='a.js' AND json_extract(payload,'$.kind')='function'",[],|r|r.get(0))?;
                    let native_functions:i64=tx.query_row(
                        "SELECT count(*) FROM native_declarations WHERE path='a.js' AND kind='function'",[],|r|r.get(0))?;
                    let graph_calls:i64=tx.query_row(
                        "SELECT count(*) FROM calls WHERE path='a.js'",[],|r|r.get(0))?;
                    let native_calls:i64=tx.query_row(
                        "SELECT count(*) FROM native_calls WHERE path='a.js'",[],|r|r.get(0))?;
                    ensure!((graph_functions,native_functions,graph_calls,native_calls)==(5_000,5_000,5_000,5_000),
                        "5,000 measured graph/native function and call rows not staged: graph={graph_functions}/{graph_calls} native={native_functions}/{native_calls}");
                    let source_bytes:Vec<u8>=tx.query_row(
                        "SELECT source_bytes FROM native_documents WHERE path='a.js'",[],|r|r.get(0))?;
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
                "SELECT max(length(CAST(payload AS BLOB))) FROM calls WHERE path='flow.js'",
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
                "SELECT length(CAST(payload AS BLOB)) FROM files WHERE path='flow.js'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            json_bytes > source.len() as i64 * 5,
            "JSON source must exercise near-sixfold control-byte expansion"
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
        let (source_set_id,language,revision_id):(String,String,String)=db.query_row(
            "SELECT source_set_id,language,revision_id FROM native_documents WHERE path='flow.js'",[],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        let (max_ancillary, total_ancillary) = Store::selected_ancillary_byte_usage(
            &db,
            "flow.js",
            &source_set_id,
            &language,
            &revision_id,
        )
        .unwrap();
        let source_json_bytes: i64 = db
            .query_row(
                "SELECT length(CAST(payload AS BLOB)) FROM files WHERE path='flow.js'",
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

    unsafe extern "C" fn abort_commit(_: *mut std::ffi::c_void) -> i32 {
        1
    }

    #[test]
    fn known_old_sqlite_partial_insert_and_commit_failure_keep_exact_bytes_and_pin() {
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        fs::write(work.path().join("flow.js"), "function foo() { bar(); }\n").unwrap();
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let options = IndexOptions::new(work.path().to_owned());
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
        let original = store.index_baseline().unwrap();
        let path = store.roots.index_db(&store.identity);
        let db = Connection::open(&path).unwrap();
        db.execute(
            "UPDATE index_metadata SET schema_version=4,extractor_version='native-v1'",
            [],
        )
        .unwrap();
        db.pragma_update(None, "user_version", 4).unwrap();
        drop(db);
        let before = fs::read(&path).unwrap();
        let leader = store.leader().unwrap();
        for mode in [PublishStage::AfterFile, PublishStage::BeforeCommit] {
            let failure = store
                .publish_inner_checked(
                    (&graph, &capture, &native),
                    &leader,
                    original,
                    &cancel,
                    |stage, tx| {
                        if stage == mode {
                            match mode {
                                PublishStage::AfterFile => {
                                    // Real SQLite UNIQUE error after DELETEs and one inserted file.
                                    tx.execute(
                                        "INSERT INTO files VALUES(?1,'duplicate','{}')",
                                        [&graph.files[0].path],
                                    )?;
                                }
                                PublishStage::BeforeCommit => {
                                    // SQLite aborts COMMIT itself; its transaction is rolled back.
                                    unsafe {
                                        rusqlite::ffi::sqlite3_commit_hook(
                                            tx.handle(),
                                            Some(abort_commit),
                                            ptr::null_mut(),
                                        );
                                    }
                                }
                                PublishStage::BeforeTransaction => unreachable!(),
                            }
                        }
                        Ok(())
                    },
                )
                .unwrap_err();
            assert!(
                failure.to_string().contains(match mode {
                    PublishStage::AfterFile => "UNIQUE constraint failed",
                    PublishStage::BeforeCommit => "constraint failed",
                    PublishStage::BeforeTransaction => unreachable!(),
                }),
                "{failure:#}"
            );
            assert_eq!(
                fs::read(&path).unwrap(),
                before,
                "failed commit changed old bytes"
            );
            assert_eq!(store.index_baseline().unwrap(), original);
            assert!(
                store
                    .status()
                    .unwrap_err()
                    .to_string()
                    .contains("index_not_ready")
            );
            assert!(
                store
                    .graph()
                    .unwrap_err()
                    .to_string()
                    .contains("index_not_ready")
            );
            let db = Connection::open(&path).unwrap();
            let (schema, extractor): (i64, String) = db
                .query_row(
                    "SELECT schema_version,extractor_version FROM index_metadata",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!((schema, extractor.as_str()), (4, "native-v1"));
            assert_eq!(
                db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                    .unwrap(),
                4
            );
        }
        let rotated = store
            .publish_native(&graph, &capture, &native, &leader, original, &cancel)
            .unwrap();
        assert_ne!(rotated.index_generation, original.index_generation);
        assert_eq!(rotated.index_revision, original.index_revision + 1);
    }
}

#[cfg(test)]
mod sqlite_schema_race_tests {
    use super::*;
    use crate::indexer::{IndexOptions, index_workspace_bundle};
    use std::{cell::RefCell, fs, sync::atomic::AtomicBool};

    fn ready() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Store,
        Graph,
        crate::capture::Capture,
        crate::native_evidence::Artifact,
        IndexPin,
        CancelFlag,
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
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                &store.leader().unwrap(),
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        (state, work, store, graph, capture, native, pin, cancel)
    }

    #[test]
    fn second_connection_adds_legacy_trigger_after_admission_before_publish_lock() {
        let (_state, _work, store, graph, capture, native, old, cancel) = ready();
        let path = store.roots.index_db(&store.identity);
        let leader = store.leader().unwrap();
        let after_external = RefCell::new(None);
        let error = store.publish_inner_checked((&graph, &capture, &native), &leader, old, &cancel,
            |stage, _checked_connection| {
                if stage == PublishStage::BeforeTransaction {
                    // The Store connection has passed open_index's exact object check,
                    // but has NOT acquired the SQLite writer lock yet.
                    let attacker = Connection::open(&path)?;
                    attacker.execute_batch("CREATE TRIGGER forged_after_admission AFTER INSERT ON calls BEGIN
                        UPDATE calls SET payload=json_set(payload,'$.calleeText','FORGED-NOT-MEASURED') WHERE id=NEW.id; END;")?;
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
                6,
                "native-paired-v1",
                old.index_generation.to_string(),
                old.index_revision as i64
            )
        );
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            6
        );
        let forged: i64 = db
            .query_row(
                "SELECT count(*) FROM calls WHERE payload LIKE '%FORGED-NOT-MEASURED%'",
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
    }

    #[test]
    fn changed_metadata_between_admission_and_leader_lock_refuses_before_update() {
        let (_state, _work, store, _graph, _capture, _native, pin, _cancel) = ready();
        let path = store.roots.index_db(&store.identity);
        let after_external = RefCell::new(None);
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
        assert_eq!(store.status().unwrap().revision, pin);
    }

    #[test]
    fn second_connection_adds_view_after_admission_before_status_and_leader_snapshot() {
        let (_state, _work, store, _graph, _capture, _native, pin, _cancel) = ready();
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
        assert_eq!(store.status().unwrap().revision, pin);

        let after_leader_ddl = RefCell::new(None);
        let leader_error = store
            .leader_with_open_hook(|_checked| {
                let attacker = Connection::open(&path)?;
                attacker.execute_batch("CREATE VIEW leader_after_admission AS SELECT 1")?;
                drop(attacker);
                *after_leader_ddl.borrow_mut() = Some(fs::read(&path)?);
                Ok(())
            })
            .unwrap_err();
        assert!(
            leader_error
                .to_string()
                .contains("incompatible_index: unknown cache object")
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            after_leader_ddl.into_inner().unwrap(),
            "leader metadata update must not write after external DDL"
        );
        let attacker = Connection::open(&path).unwrap();
        let (schema, marker, generation, revision): (i64,String,String,i64) = attacker.query_row(
            "SELECT schema_version,extractor_version,index_generation,index_revision FROM index_metadata", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(
            (schema, marker.as_str(), generation, revision),
            (
                6,
                "native-paired-v1",
                pin.index_generation.to_string(),
                pin.index_revision as i64
            )
        );
        assert!(
            store
                .status()
                .unwrap_err()
                .to_string()
                .contains("incompatible_index")
        );
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
                "SELECT id,payload FROM nodes WHERE path='one.js' LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        db.execute(
            "UPDATE nodes SET payload=?1 WHERE id=?2",
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
            "UPDATE nodes SET payload=?1 WHERE id=?2",
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
            "UPDATE files SET payload=?1 WHERE path='one.js'",
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
            "UPDATE files SET payload='not-json' WHERE path='one.js'",
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
