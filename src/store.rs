//! SQLite snapshots and durable user data. Connections are never shared between threads.
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
    AfterFile,
    BeforeCommit,
}
const DATABASE_SCHEMA_VERSION: u32 = 5;
const EXTRACTOR_VERSION: &str = "native-no-lexical-v1";
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
        version == DATABASE_SCHEMA_VERSION || version == LEGACY_SCHEMA_VERSION,
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
            || (version == DATABASE_SCHEMA_VERSION && metadata == (5, EXTRACTOR_VERSION.into())),
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

fn class_metadata(db: &Connection) -> Result<Option<(Vec<String>, bool)>> {
    let metadata: Option<(String, bool)> = db
        .query_row(
            "SELECT warnings,truncated FROM class_catalog WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    metadata
        .map(|(warnings, mut truncated)| {
            let warnings: Vec<String> = serde_json::from_str(&warnings)?;
            let mut bytes = 0;
            let mut visible = Vec::new();
            for warning in warnings {
                bytes += serde_json::to_vec(&warning)?.len() + 1;
                if bytes > 256 * 1024 {
                    truncated = true;
                    visible.push(
                        "Further catalog warnings omitted by the presentation byte limit.".into(),
                    );
                    break;
                }
                visible.push(warning);
            }
            Ok((visible, truncated))
        })
        .transpose()
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
        let db = store.cache()?;
        store.read_control_status(&db)?;
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
        before_publish(&staged.path)?;
        verify_index_file(&staged.path)?;
        let checked = open_index(&staged.path, true)?;
        self.read_control_status(&checked)?;
        let integrity: String =
            storage_result(checked.query_row("PRAGMA quick_check", [], |r| r.get(0)))?;
        ensure!(
            integrity == "ok",
            "incompatible_index: staged integrity check failed"
        );
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
        drop(self.cache()?);
        let leader = self.roots.leader(&self.identity)?;
        // Acquiring the leader must not mutate legacy cache bytes: a failed
        // rebaseline leaves the old schema-4 database intact and unreadable.
        let mut db = self.cache_write()?;
        if self.read_control_status(&db)?.evidence_format.is_some() {
            let tx = storage_result(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
            let age = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            ensure!(age <= 9_007_199_254_740_991, "invalid_open_age");
            storage_result(tx.execute(
                "UPDATE index_metadata SET last_opened_at=?1 WHERE singleton=1",
                [age as i64],
            ))?;
            storage_result(tx.commit())?;
        }
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
        let row: (i64,String,String,String,String,String,i64,String,String,String) = storage_result(db.query_row(
            "SELECT schema_version,extractor_version,root_spelling,root_device,root_inode,index_generation,index_revision,indexed_at,stats,diagnostics FROM index_metadata WHERE singleton=1",
            [],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?))))?;
        ensure!(
            (row.0 == i64::from(LEGACY_SCHEMA_VERSION) && row.1 == LEGACY_EXTRACTOR_VERSION)
                || (row.0 == i64::from(DATABASE_SCHEMA_VERSION) && row.1 == EXTRACTOR_VERSION),
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
        Ok(status)
    }
    /// Internal control baseline, never returned by public status or evidence reads.
    pub fn index_baseline(&self) -> Result<IndexPin> {
        let db = self.cache()?;
        Ok(self.read_control_status(&db)?.revision)
    }
    pub fn verify_root(&self) -> Result<()> {
        self.identity.verify()
    }
    pub fn status(&self) -> Result<IndexStatus> {
        let db = self.cache()?;
        self.read_status(&db)
    }
    pub fn publish(
        &self,
        graph: &Graph,
        leader: &topology::LeaderGuard,
        expected_revision: IndexPin,
        cancel: &CancelFlag,
    ) -> Result<IndexPin> {
        self.publish_inner(graph, None, leader, expected_revision, cancel)
    }
    pub fn publish_captured(
        &self,
        graph: &Graph,
        capture: &crate::capture::Capture,
        leader: &topology::LeaderGuard,
        expected_revision: IndexPin,
        cancel: &CancelFlag,
    ) -> Result<IndexPin> {
        self.publish_inner(graph, Some(capture), leader, expected_revision, cancel)
    }
    fn publish_inner(
        &self,
        graph: &Graph,
        capture: Option<&crate::capture::Capture>,
        leader: &topology::LeaderGuard,
        expected_revision: IndexPin,
        cancel: &CancelFlag,
    ) -> Result<IndexPin> {
        self.publish_inner_checked(graph, capture, leader, expected_revision, cancel, |_, _| {
            Ok(())
        })
    }

    // Private transaction seam used by the in-module rollback tests. Normal callers
    // always pass a no-op; no SQL-fault control is exposed to API or CLI clients.
    fn publish_inner_checked(
        &self,
        graph: &Graph,
        capture: Option<&crate::capture::Capture>,
        leader: &topology::LeaderGuard,
        expected_revision: IndexPin,
        cancel: &CancelFlag,
        mut during_tx: impl FnMut(PublishStage, &rusqlite::Transaction<'_>) -> Result<()>,
    ) -> Result<IndexPin> {
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
        check_cancel(cancel)?;
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        self.identity.verify()?;
        let mut db = self.cache_write()?;
        let tx = storage_result(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let baseline = self.read_control_status(&tx)?;
        let old = baseline.revision;
        ensure!(
            expected_revision == old,
            "revision conflict: expected {expected_revision:?}, found {old:?}"
        );
        let rebaseline = baseline.evidence_format.is_none();
        if rebaseline {
            ensure!(
                capture.is_some(),
                "index_not_ready: rebaseline needs verified capture"
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
        tx.execute(
            "INSERT INTO class_catalog VALUES(1,?1,?2)",
            params![json(&classes.warnings)?, classes.truncated],
        )?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .to_string();
        tx.execute("UPDATE index_metadata SET schema_version=5,extractor_version=?1,index_generation=?2,index_revision=?3,indexed_at=?4,stats=?5,diagnostics=?6 WHERE singleton=1",
            params![EXTRACTOR_VERSION, revision.index_generation.to_string(), revision.index_revision as i64,timestamp,json(&stats)?,json(&graph.diagnostics)?])?;
        if rebaseline {
            tx.pragma_update(None, "user_version", DATABASE_SCHEMA_VERSION)?;
        }
        check_cancel(cancel)?;
        if let Some(capture) = capture {
            capture.verify(cancel)?;
        }
        leader.verify()?;
        self.identity.verify()?;
        during_tx(PublishStage::BeforeCommit, &tx)?;
        storage_result(tx.commit())?;
        Ok(revision)
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
        use crate::class_diagram::{ClassPage, INDEX_NOTICE, InvalidRequest};
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
        let Some((mut warnings, truncated)) = class_metadata(&tx)? else {
            return Ok(ClassPage {
                revision,
                items: vec![],
                next_offset: None,
                truncated: false,
                warnings: vec![INDEX_NOTICE.into()],
                require_index: true,
            });
        };
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
        let Some((warnings, truncated)) = class_metadata(&tx)? else {
            return Ok(class_diagram::ClassDiagram::unindexed(
                revision,
                request.seed.clone(),
            ));
        };
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
        let mut stmt = tx.prepare("SELECT payload FROM nodes WHERE instr(lower(name),lower(?1)) > 0 OR instr(lower(id),lower(?1)) > 0 ORDER BY CASE WHEN lower(name)=lower(?1) THEN 0 WHEN instr(lower(name),lower(?1))=1 THEN 1 ELSE 2 END,name,id LIMIT ?2")?;
        let values = stmt.query_map(params![query, limit.min(150) as i64], |r| {
            r.get::<_, String>(0)
        })?;
        let values = values
            .map(|v| Ok(serde_json::from_str(&v?)?))
            .collect::<Result<Vec<Symbol>>>()?;
        Ok((revision, values))
    }
    pub fn symbol(&self, id: &str) -> Result<Option<Symbol>> {
        Ok(self.symbol_at(id, None)?.map(|(_, v)| v))
    }
    pub fn source(&self, path: &str) -> Result<Option<SourceFile>> {
        Ok(self.source_at(path, None)?.map(|(_, v)| v))
    }
    fn entity_at<T: DeserializeOwned>(
        &self,
        sql: &str,
        id: &str,
        expected_revision: Option<IndexPin>,
    ) -> Result<Option<(IndexPin, T)>> {
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        ensure!(
            expected_revision.is_none_or(|r| r == revision),
            "revision conflict: expected {expected_revision:?}, found {revision:?}"
        );
        Ok(one(&tx, sql, id)?.map(|value| (revision, value)))
    }
    pub fn symbol_at(
        &self,
        id: &str,
        expected_revision: Option<IndexPin>,
    ) -> Result<Option<(IndexPin, Symbol)>> {
        self.entity_at(
            "SELECT payload FROM nodes WHERE id=?1",
            id,
            expected_revision,
        )
    }
    pub fn source_at(
        &self,
        path: &str,
        expected_revision: Option<IndexPin>,
    ) -> Result<Option<(IndexPin, SourceFile)>> {
        self.entity_at(
            "SELECT payload FROM files WHERE path=?1",
            path,
            expected_revision,
        )
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
        let Some(symbol) = one::<Symbol>(&tx, "SELECT payload FROM nodes WHERE id=?1", seed)?
        else {
            return Ok(None);
        };
        ensure!(
            matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method),
            "invalid sequence symbol kind"
        );
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
    pub fn query_view(&self, query: &ViewQuery) -> Result<Option<ViewResult>> {
        query.validate()?;
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
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
    fn resolve_view(db: &Connection, view: SavedView) -> Result<SavedViewState> {
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
        Ok(SavedViewState { view, orphaned_ids })
    }
    pub fn views(&self) -> Result<Vec<SavedViewState>> {
        let views = self.records().views()?;
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        if self.read_control_status(&tx)?.evidence_format.is_none() {
            return Ok(views
                .into_iter()
                .map(|view| SavedViewState {
                    orphaned_ids: std::iter::once(view.query.seed.clone())
                        .chain(view.pins.keys().cloned())
                        .chain(view.hidden.iter().cloned())
                        .collect(),
                    view,
                })
                .collect());
        }
        views
            .into_iter()
            .map(|v| Self::resolve_view(&tx, v))
            .collect()
    }
    pub fn view(&self, id: &str) -> Result<Option<SavedViewState>> {
        let view = self.records().view(id)?;
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        if self.read_control_status(&tx)?.evidence_format.is_none() {
            return Ok(view.map(|view| SavedViewState {
                orphaned_ids: std::iter::once(view.query.seed.clone())
                    .chain(view.pins.keys().cloned())
                    .chain(view.hidden.iter().cloned())
                    .collect(),
                view,
            }));
        }
        view.map(|v| Self::resolve_view(&tx, v)).transpose()
    }
    pub fn delete_view(&self, id: &str) -> Result<bool> {
        self.records().delete_view(id)
    }
    pub fn put_annotation(&self, annotation: &Annotation) -> Result<()> {
        annotation.validate()?;
        self.records().put_annotation(annotation)?;
        Ok(())
    }
    pub fn annotations(&self) -> Result<Vec<AnnotationState>> {
        let annotations = self.records().annotations()?;
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        if self.read_control_status(&tx)?.evidence_format.is_none() {
            return Ok(annotations
                .into_iter()
                .map(|annotation| AnnotationState {
                    annotation,
                    orphaned: true,
                })
                .collect());
        }
        annotations
            .into_iter()
            .map(|annotation| {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM nodes WHERE id=?1)",
                    [&annotation.node_id],
                    |r| r.get(0),
                )?;
                Ok(AnnotationState {
                    annotation,
                    orphaned: !exists,
                })
            })
            .collect()
    }
    pub fn delete_annotation(&self, id: &str) -> Result<bool> {
        self.records().delete_annotation(id)
    }
}

#[cfg(test)]
mod rebaseline_fault_tests {
    use super::*;
    use crate::indexer::{IndexOptions, index_workspace_with_capture};
    use std::{fs, ptr, sync::atomic::AtomicBool};

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
        let (graph, capture) = index_workspace_with_capture(&options, &cancel, |_| {}).unwrap();
        let original = store
            .publish_captured(
                &graph,
                &capture,
                &store.leader().unwrap(),
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
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
                    &graph,
                    Some(&capture),
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
            .publish_captured(&graph, &capture, &leader, original, &cancel)
            .unwrap();
        assert_ne!(rotated.index_generation, original.index_generation);
        assert_eq!(rotated.index_revision, original.index_revision + 1);
    }
}
