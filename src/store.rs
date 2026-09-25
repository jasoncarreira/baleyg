//! SQLite snapshots and durable user data. Connections are never shared between threads.
pub mod topology;
use crate::model::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
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
const DATABASE_SCHEMA_VERSION: u32 = 4;
const EXTRACTOR_VERSION: &str = "native-v1";
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
        let sidecar = path.with_file_name(format!("index.db{suffix}"));
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
        version == DATABASE_SCHEMA_VERSION,
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
        match call.resolution {
            Resolution::Internal => {
                ensure!(
                    call.target
                        .as_ref()
                        .is_some_and(|t| nodes.contains_key(t.as_str())),
                    "dangling internal call target"
                );
                stats.internal += 1;
            }
            Resolution::External => stats.external += 1,
            Resolution::Unresolved => stats.unresolved += 1,
            Resolution::Ambiguous => stats.ambiguous += 1,
        }
        // Candidate strings include external SCIP identities and unresolved lexical
        // bindings. They are evidence, not guaranteed graph node references.
        for target in &call.callback_arguments {
            ensure!(
                nodes.contains_key(target.as_str()),
                "dangling callback target"
            );
        }
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
fn presentation_relation(
    db: &Connection,
    id: &str,
) -> Result<Option<crate::classes::ClassRelation>> {
    let payload: Option<String> = db.query_row("SELECT CASE WHEN length(CAST(payload AS BLOB))<=?2 THEN payload ELSE NULL END FROM class_relations WHERE id=?1",
        params![id, crate::class_diagram::RELATION_BYTES as i64], |r| r.get(0)).optional()?.flatten();
    payload
        .map(|payload| Ok(serde_json::from_str(&payload)?))
        .transpose()
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

// Hierarchy planning reads only indexed relationship IDs/endpoints. It does not
// deserialize a catalog or load class members until a bounded plan is selected.
const HIERARCHY_QUERIES: usize = 48;
const HIERARCHY_RECORDS: usize = 256;
const HIERARCHY_DEPTH: usize = 24;
const HIERARCHY_NOTICE: &str = "Hierarchy is partial: depth (24), adjacency query (48), representative row (64 per query), planning edge (256), or relationship-load (256) limits were reached.";
#[derive(Clone)]
struct ClassLink {
    id: String,
    owner: String,
    target: Option<String>,
    occurrences: i64,
}
fn class_links(db: &Connection, seed: &str, kind: &str) -> Result<Vec<ClassLink>> {
    // Filter inheritance BEFORE grouping/capping, so arbitrarily many field,
    // parameter or return references cannot starve extends/implements evidence.
    let filter = match kind {
        "ancestors" => {
            "target IS NOT NULL AND owner=?1 AND json_extract(payload,'$.kind') IN ('extends','implements')"
        }
        "descendants" => {
            "target IS NOT NULL AND target=?1 AND json_extract(payload,'$.kind') IN ('extends','implements')"
        }
        "hints" => "owner=?1 AND target IS NULL",
        _ => {
            "target IS NOT NULL AND (owner=?1 OR target=?1) AND json_extract(payload,'$.kind') NOT IN ('extends','implements')"
        }
    };
    let sql = format!("SELECT r.id,r.owner,r.target,g.occurrences FROM class_relations r JOIN (
        SELECT min(id) AS representative,count(*) AS occurrences,
            CASE WHEN owner=?1 THEN 0 ELSE 1 END AS direction FROM class_relations
        WHERE {filter}
        GROUP BY owner,target,CASE WHEN target IS NULL THEN json_extract(payload,'$.typeName') ELSE '' END,
            json_extract(payload,'$.kind'),json_extract(payload,'$.matchKind')
        ORDER BY direction,representative LIMIT 65
        ) g ON r.id=g.representative ORDER BY g.direction,r.id");
    Ok(db
        .prepare(&sql)?
        .query_map([seed], |r| {
            Ok(ClassLink {
                id: r.get(0)?,
                owner: r.get(1)?,
                target: r.get(2)?,
                occurrences: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
/// Undirected reachability over actual recorded links, with a shortest real
/// predecessor edge. The bounded thin graph can include disconnected roots;
/// they are never considered connected merely because the user supplied them.
fn class_paths(seed: &str, links: &[ClassLink]) -> BTreeMap<String, Option<(String, usize)>> {
    let mut paths = BTreeMap::from([(seed.to_owned(), None)]);
    let mut queue = VecDeque::from([seed.to_owned()]);
    while let Some(node) = queue.pop_front() {
        for (index, link) in links.iter().enumerate() {
            let Some(target) = &link.target else { continue };
            let next = if link.owner == node {
                target
            } else if target == &node {
                &link.owner
            } else {
                continue;
            };
            if !paths.contains_key(next) {
                paths.insert(next.clone(), Some((node.clone(), index)));
                queue.push_back(next.clone());
            }
        }
    }
    paths
}

#[allow(clippy::too_many_arguments)]
fn hierarchy_diagram(
    db: &Connection,
    revision: IndexPin,
    request: &crate::class_diagram::ClassDiagramRequest,
    mut seeds: Vec<String>,
    mut classes: BTreeMap<String, crate::classes::ClassDefinition>,
    mut warnings: Vec<String>,
    mut truncated: bool,
    mut byte_limited: bool,
) -> Result<crate::class_diagram::ClassDiagram> {
    use crate::class_diagram::{self, InvalidRequest, MAX_EDGES, MAX_NODES};
    // Resolve IDs first, deduplicating even method/class aliases before loading
    // members. All manual roots count against the same 24 class-load budget.
    for id in &request.expanded {
        let id = resolve_class_id(db, id)?;
        if !seeds.contains(&id) {
            seeds.push(id);
        }
    }
    // Do not descend again from an ancestor: Derived -> Base <- Peer must not
    // pull Peer into Derived's diagram merely because they share a base.
    let mut queue: VecDeque<_> = seeds
        .iter()
        .flat_map(|id| [(id.clone(), 0, "ancestors"), (id.clone(), 0, "descendants")])
        .collect();
    let mut queued: BTreeSet<_> = queue
        .iter()
        .map(|(id, _, direction)| (id.clone(), *direction))
        .collect();
    let mut hierarchy = Vec::new();
    let mut link_ids = BTreeSet::new();
    let mut queries = 0;
    let mut hierarchy_limited = false;
    let mut grouped = false;
    while let Some((id, depth, direction)) = queue.pop_front() {
        if queries >= HIERARCHY_QUERIES || hierarchy.len() >= HIERARCHY_RECORDS {
            hierarchy_limited = true;
            break;
        }
        queries += 1;
        let links = class_links(db, &id, direction)?;
        hierarchy_limited |= links.len() > MAX_EDGES;
        for link in links.into_iter().take(MAX_EDGES) {
            grouped |= link.occurrences > 1;
            if hierarchy.len() >= HIERARCHY_RECORDS {
                hierarchy_limited = true;
                break;
            }
            let target = link.target.as_ref().expect("hierarchy target");
            let next = if link.owner == id {
                target
            } else {
                &link.owner
            };
            if !queued.contains(&(next.clone(), direction)) {
                if depth >= HIERARCHY_DEPTH {
                    hierarchy_limited = true;
                    continue;
                }
                queued.insert((next.clone(), direction));
                queue.push_back((next.clone(), depth + 1, direction));
            }
            if link_ids.insert(link.id.clone()) {
                hierarchy.push(link);
            }
        }
    }
    // Validate expansions against the actual seed-connected hierarchy, in user
    // order. A field neighbor of a deep automatic class needs no manual anchor.
    let mut connections = hierarchy.clone();
    let mut mandatory_indices = BTreeSet::new();
    let mut mandatory = Vec::new();
    for root in seeds.iter().skip(1) {
        let mut paths = class_paths(&seeds[0], &connections);
        if !paths.contains_key(root) {
            let connected = serde_json::to_string(&paths.keys().collect::<Vec<_>>())?;
            let bridge = db
                .query_row(
                    "SELECT id,owner,target FROM class_relations
                WHERE (owner=?1 AND target IN (SELECT value FROM json_each(?2)))
                   OR (target=?1 AND owner IN (SELECT value FROM json_each(?2)))
                ORDER BY id LIMIT 1",
                    params![root, connected],
                    |r| {
                        Ok(ClassLink {
                            id: r.get(0)?,
                            owner: r.get(1)?,
                            target: r.get(2)?,
                            occurrences: 1,
                        })
                    },
                )
                .optional()?;
            let bridge = bridge.ok_or(InvalidRequest(
                "Expand a related class connected to the visible hierarchy. The connection may exceed the bounded hierarchy search; choose a nearer class.",
            ))?;
            connections.push(bridge);
            paths = class_paths(&seeds[0], &connections);
        }
        let mut current = root.clone();
        let mut path = Vec::new();
        while let Some(Some((previous, index))) = paths.get(&current) {
            path.push(*index);
            current = previous.clone();
        }
        for index in path.into_iter().rev() {
            if mandatory_indices.insert(index) {
                mandatory.push(connections[index].clone());
            }
        }
    }
    let mut required: BTreeSet<_> = seeds.iter().cloned().collect();
    for link in &mandatory {
        required.insert(link.owner.clone());
        required.extend(link.target.iter().cloned());
    }
    ensure!(
        required.len() <= MAX_NODES && mandatory.len() <= MAX_EDGES,
        InvalidRequest(
            "Selected class connections exceed the 24 node or 64 edge limit. Remove an expansion or choose a nearer class.",
        )
    );
    let mut loaded: BTreeSet<_> = classes.keys().cloned().collect();
    for id in required {
        if loaded.insert(id.clone()) {
            let (class, _, clipped) = presentation_class(db, &id)?.ok_or(InvalidRequest(
                "A selected class connection exceeds the presentation byte limit.",
            ))?;
            byte_limited |= clipped;
            classes.insert(id, class);
        }
    }
    let mut bridges = Vec::new();
    let mut shown = BTreeSet::new();
    for link in mandatory {
        let relation = presentation_relation(db, &link.id)?.ok_or(InvalidRequest(
            "A selected class connection exceeds the presentation byte limit.",
        ))?;
        shown.insert(link.id);
        bridges.push(relation);
    }
    // BFS from the reserved connected subgraph; each optional edge attaches to
    // it. Members are loaded at most once, including failed oversize records.
    let mut relations = Vec::new();
    let mut frontier: VecDeque<_> = classes.keys().cloned().collect();
    let mut visited = BTreeSet::new();
    let mut relation_loads = bridges.len();
    while let Some(node) = frontier.pop_front() {
        if !visited.insert(node.clone()) {
            continue;
        }
        for link in &hierarchy {
            if link.owner != node && link.target.as_ref() != Some(&node) {
                continue;
            }
            if shown.contains(&link.id) {
                continue;
            }
            if shown.len() >= MAX_EDGES {
                truncated = true;
                continue;
            }
            let target = link.target.as_ref().expect("hierarchy target");
            let next = if link.owner == node {
                target
            } else {
                &link.owner
            };
            if !classes.contains_key(next) {
                if loaded.len() >= MAX_NODES {
                    truncated = true;
                    continue;
                }
                if !loaded.insert(next.clone()) {
                    continue;
                }
                if let Some((class, _, clipped)) = presentation_class(db, next)? {
                    byte_limited |= clipped;
                    classes.insert(next.clone(), class);
                } else {
                    byte_limited = true;
                    continue;
                }
            }
            if relation_loads >= HIERARCHY_RECORDS {
                hierarchy_limited = true;
                continue;
            }
            relation_loads += 1;
            if let Some(relation) = presentation_relation(db, &link.id)? {
                shown.insert(link.id.clone());
                relations.push(relation);
                frontier.push_back(next.clone());
            } else {
                byte_limited = true;
            }
        }
    }
    // Offer one-hop associations from all automatic hierarchy classes, but only
    // after hierarchy reservation. These never become recursive hierarchy roots.
    // Use reached endpoints, not all loaded classes (a relation can be oversize).
    let mut neighborhood: BTreeSet<_> = seeds.iter().cloned().collect();
    for edge in bridges.iter().chain(&relations) {
        neighborhood.insert(edge.owner.clone());
        neighborhood.extend(edge.target.iter().cloned());
    }
    for kind in ["associations", "hints"] {
        if kind == "hints" && !request.include_unmatched {
            continue;
        }
        for node in &neighborhood {
            let links = class_links(db, node, kind)?;
            if links.len() > MAX_EDGES {
                truncated = true;
            }
            for link in links.into_iter().take(MAX_EDGES) {
                grouped |= link.occurrences > 1;
                if shown.contains(&link.id) {
                    continue;
                }
                if shown.len() >= MAX_EDGES {
                    truncated = true;
                    continue;
                }
                let mut present = true;
                for id in std::iter::once(&link.owner).chain(link.target.iter()) {
                    if classes.contains_key(id) {
                        continue;
                    }
                    if loaded.len() >= MAX_NODES {
                        truncated = true;
                        present = false;
                        break;
                    }
                    if !loaded.insert(id.clone()) {
                        present = false;
                        break;
                    }
                    if let Some((class, _, clipped)) = presentation_class(db, id)? {
                        byte_limited |= clipped;
                        classes.insert(id.clone(), class);
                    } else {
                        byte_limited = true;
                        present = false;
                    }
                }
                if !present {
                    continue;
                }
                if relation_loads >= HIERARCHY_RECORDS {
                    hierarchy_limited = true;
                    continue;
                }
                relation_loads += 1;
                if let Some(relation) = presentation_relation(db, &link.id)? {
                    shown.insert(link.id);
                    relations.push(relation);
                } else {
                    byte_limited = true;
                }
            }
        }
    }
    if hierarchy_limited {
        truncated = true;
        warnings.push(HIERARCHY_NOTICE.into());
    }
    if byte_limited {
        truncated = true;
        warnings.push(class_diagram::BYTE_NOTICE.into());
    }
    if grouped {
        warnings.push("Repeated same-kind references are grouped; each diagram edge shows one representative source range.".into());
    }
    class_diagram::project(
        revision, &seeds, &classes, bridges, relations, warnings, truncated,
    )
}

impl Store {
    pub fn open(
        roots: topology::TopologyRoots,
        identity: topology::WorkspaceIdentity,
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
        if !store.roots.index_db(&store.identity).exists() {
            let leader = store.roots.leader(&store.identity)?;
            store.initialize(&leader)?;
        }
        store.status()?;
        Ok(store)
    }
    /// Isolated roots for integration fixtures; production startup calls `open` with ProjectDirs.
    pub fn open_for_tests(state: &Path, workspace: &Path) -> Result<Self> {
        let identity = topology::WorkspaceIdentity::discover(Some(workspace), workspace)?;
        let roots =
            topology::TopologyRoots::isolated_for_tests(state.join("cache"), state.join("data"));
        Self::open(roots, identity)
    }
    fn initialize(&self, leader: &topology::LeaderGuard) -> Result<()> {
        use rusqlite::OpenFlags;
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        self.identity.verify()?;
        let path = self.roots.index_db(&self.identity);
        reject_sidecars(&path, true)?;
        if path.exists() {
            return Ok(());
        }
        let _use_guard = self.roots.index_use(&self.identity)?;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let db = Connection::open_with_flags(
            &path,
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
                "INSERT INTO index_metadata VALUES(1,4,?1,?2,?3,?4,?5,0,?6,'',?7,?8)",
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
        verify_index_file(&path)?;
        std::fs::File::open(&path)?.sync_all()?;
        std::fs::File::open(self.roots.index_dir(&self.identity))?.sync_all()?;
        self.identity.verify()?;
        Ok(())
    }
    pub fn leader(&self) -> Result<topology::LeaderGuard> {
        drop(self.cache()?);
        let leader = self.roots.leader(&self.identity)?;
        let mut db = self.cache_write()?;
        let tx = storage_result(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let age = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        ensure!(age <= 9_007_199_254_740_991, "invalid_open_age");
        storage_result(tx.execute(
            "UPDATE index_metadata SET last_opened_at=?1 WHERE singleton=1",
            [age as i64],
        ))?;
        storage_result(tx.commit())?;
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
    fn read_status(&self, db: &Connection) -> Result<IndexStatus> {
        let row: (i64,String,String,String,String,String,i64,String,String,String) = storage_result(db.query_row(
            "SELECT schema_version,extractor_version,root_spelling,root_device,root_inode,index_generation,index_revision,indexed_at,stats,diagnostics FROM index_metadata WHERE singleton=1",
            [],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?))))?;
        ensure!(
            row.0 == 4 && row.1 == EXTRACTOR_VERSION,
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
        })
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
        ensure!(
            graph.schema_version == SCHEMA_VERSION,
            "unsupported graph schema"
        );
        check_cancel(cancel)?;
        let stats = validate_graph(graph, cancel)?;
        // Parse cached source before taking the writer lock. Projection and graph
        // still publish in one transaction with the same CAS/cancellation guard.
        let classes = crate::classes::Catalog::build(&graph.files, &graph.nodes, cancel)?;
        check_cancel(cancel)?;
        leader.belongs_to(&self.roots.leader_lock(&self.identity))?;
        self.identity.verify()?;
        let mut db = self.cache_write()?;
        let tx = storage_result(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let old = self.read_status(&tx)?.revision;
        ensure!(
            expected_revision == old,
            "revision conflict: expected {expected_revision:?}, found {old:?}"
        );
        let revision = IndexPin {
            index_generation: old.index_generation,
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
                params![c.id, c.caller, c.target, c.path, json(c)?],
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
        tx.execute("UPDATE index_metadata SET index_revision=?1,indexed_at=?2,stats=?3,diagnostics=?4 WHERE singleton=1",
            params![revision.index_revision as i64,timestamp,json(&stats)?,json(&graph.diagnostics)?])?;
        check_cancel(cancel)?;
        leader.verify()?;
        self.identity.verify()?;
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
        use crate::class_diagram::{self, InvalidRequest, MAX_EDGES};
        request.validate()?;
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
        let revision = self.read_status(&tx)?.revision;
        ensure!(revision == request.expected_revision, "revision conflict");
        let Some((mut warnings, mut truncated)) = class_metadata(&tx)? else {
            return Ok(class_diagram::ClassDiagram::unindexed(
                revision,
                request.seed.clone(),
            ));
        };
        let (seed, mut byte_limited) = resolve_class(&tx, &request.seed)?;
        let mut seeds = vec![seed.symbol.id.clone()];
        let mut classes = BTreeMap::from([(seed.symbol.id.clone(), seed)]);
        if request.include_hierarchy {
            return hierarchy_diagram(
                &tx,
                revision,
                request,
                seeds,
                classes,
                warnings,
                truncated,
                byte_limited,
            );
        }
        let mut bridges = Vec::new();
        for id in &request.expanded {
            let (class, clipped) = resolve_class(&tx, id)?;
            byte_limited |= clipped;
            let id = &class.symbol.id;
            if seeds.contains(id) {
                continue;
            }
            // Expansion is permitted only from the connected session history.
            // Reserve its bridge before neighbors, so node/edge caps cannot strand it.
            let mut bridge = None;
            for previous in &seeds {
                bridge = tx
                    .query_row(
                        "SELECT id FROM class_relations
                    WHERE (owner=?1 AND target=?2) OR (owner=?2 AND target=?1) ORDER BY id LIMIT 1",
                        params![previous, id],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                    .map(|id| presentation_relation(&tx, &id))
                    .transpose()?
                    .flatten();
                if bridge.is_some() {
                    break;
                }
            }
            let bridge = bridge.ok_or(InvalidRequest(
                "Expand a related class with evidence within the presentation byte limit.",
            ))?;
            bridges.push(bridge);
            seeds.push(id.clone());
            classes.insert(id.clone(), class);
        }
        let mut relations = Vec::new();
        let mut relation_ids: BTreeSet<String> =
            bridges.iter().map(|relation| relation.id.clone()).collect();
        let mut grouped_references = false;
        // Process real class links for ALL seeds before optional terminal hints.
        for hints in [false, true] {
            if hints && !request.include_unmatched {
                continue;
            }
            for seed in &seeds {
                // Group BEFORE the row cap so repeated field/parameter references
                // cannot crowd out a different related class. The source catalog
                // is bounded at build time; owner/target indexes restrict this scan.
                // Join back by the minimum actual ID: range and certainty belong
                // to that one recorded occurrence, never an invented union range.
                let filter = if hints {
                    "owner=?1 AND target IS NULL"
                } else {
                    "target IS NOT NULL AND (owner=?1 OR target=?1)"
                };
                let sql = format!("SELECT r.id,g.occurrences FROM class_relations r JOIN (
                    SELECT min(id) AS representative,count(*) AS occurrences,
                        CASE WHEN owner=?1 THEN 0 ELSE 1 END AS direction FROM class_relations
                    WHERE {filter}
                    GROUP BY owner,target,CASE WHEN target IS NULL THEN json_extract(payload,'$.typeName') ELSE '' END,
                        json_extract(payload,'$.kind'),json_extract(payload,'$.matchKind')
                    ORDER BY direction,representative LIMIT ?2
                    ) g ON r.id=g.representative ORDER BY g.direction,r.id");
                let mut stmt = tx.prepare(&sql)?;
                let candidates = stmt
                    .query_map(params![seed, (MAX_EDGES + 1) as i64], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if candidates.len() > MAX_EDGES {
                    truncated = true;
                }
                for (id, occurrences) in candidates.into_iter().take(MAX_EDGES) {
                    grouped_references |= occurrences > 1;
                    if relation_ids.contains(&id) {
                        continue;
                    }
                    if relations.len() >= MAX_EDGES {
                        truncated = true;
                        break;
                    }
                    relation_ids.insert(id.clone());
                    let Some(relation) = presentation_relation(&tx, &id)? else {
                        byte_limited = true;
                        continue;
                    };
                    let mut endpoints_present = true;
                    for id in std::iter::once(&relation.owner).chain(relation.target.iter()) {
                        if classes.contains_key(id) {
                            continue;
                        }
                        if classes.len() >= class_diagram::MAX_NODES {
                            truncated = true;
                            endpoints_present = false;
                            break;
                        }
                        if let Some((class, _, clipped)) = presentation_class(&tx, id)? {
                            byte_limited |= clipped;
                            classes.insert(id.clone(), class);
                        } else {
                            byte_limited = true;
                            endpoints_present = false;
                        }
                    }
                    if endpoints_present {
                        relations.push(relation);
                    }
                }
            }
        }
        if byte_limited {
            truncated = true;
            warnings.push(class_diagram::BYTE_NOTICE.into());
        }
        if grouped_references {
            warnings.push("Repeated same-kind references are grouped; each diagram edge shows one representative source range.".into());
        }
        class_diagram::project(
            revision, &seeds, &classes, bridges, relations, warnings, truncated,
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
        let excluded = |path: &str| query.exclude_paths.iter().any(|p| path.starts_with(p));
        let mut nodes = BTreeMap::from([(seed.id.clone(), seed)]);
        let mut queue = VecDeque::from([(query.seed.clone(), 0usize)]);
        let mut calls = BTreeMap::new();
        let mut region_ids = BTreeSet::new();
        let mut omitted = BTreeSet::new();
        let mut truncated = false;
        while let Some((id, depth)) = queue.pop_front() {
            if depth >= query.depth {
                continue;
            }
            let mut stmt =
                tx.prepare("SELECT payload FROM calls WHERE caller=?1 ORDER BY path,json_extract(payload,'$.range.startByte'),json_extract(payload,'$.range.endByte') DESC,id")?;
            let values = stmt.query_map([&id], |r| r.get::<_, String>(0))?;
            for value in values {
                let call: CallSite = serde_json::from_str(&value?)?;
                if excluded(&call.path) {
                    continue;
                }
                if calls.len() >= query.max_calls {
                    truncated = true;
                    break;
                }
                let mut targets = Vec::new();
                if call.resolution == Resolution::Internal
                    && let Some(t) = &call.target
                {
                    targets.push(t.clone());
                }
                if query.include_callbacks {
                    targets.extend(call.callback_arguments.iter().cloned());
                }
                targets.sort();
                targets.dedup();
                for target in targets {
                    if nodes.contains_key(&target) {
                        continue;
                    }
                    let node: Option<Symbol> =
                        one(&tx, "SELECT payload FROM nodes WHERE id=?1", &target)?;
                    let Some(node) = node else { continue };
                    if excluded(&node.path) {
                        continue;
                    }
                    if nodes.len() >= query.max_nodes {
                        truncated = true;
                        omitted.insert(target);
                        continue;
                    }
                    queue.push_back((target.clone(), depth + 1));
                    nodes.insert(target, node);
                }
                region_ids.extend(call.regions.iter().cloned());
                calls.insert(call.id.clone(), call);
            }
        }
        let mut regions = BTreeMap::new();
        while let Some(id) = region_ids.pop_first() {
            if regions.contains_key(&id) {
                continue;
            }
            let region: Option<ControlRegion> =
                one(&tx, "SELECT payload FROM regions WHERE id=?1", &id)?;
            if let Some(region) = region {
                if excluded(&region.path) {
                    continue;
                }
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
            nodes: nodes.into_values().collect(),
            calls: {
                let mut sites: Vec<CallSite> = calls.into_values().collect();
                sites.sort_by(|a, b| {
                    (
                        &a.path,
                        a.range.start_byte,
                        std::cmp::Reverse(a.range.end_byte),
                        &a.id,
                    )
                        .cmp(&(
                            &b.path,
                            b.range.start_byte,
                            std::cmp::Reverse(b.range.end_byte),
                            &b.id,
                        ))
                });
                sites
            },
            regions: regions.into_values().collect(),
            truncated,
            omitted_nodes: omitted.len(),
            warnings: if truncated {
                vec!["View truncated by node or call limits; omittedNodes counts discovered omitted targets only.".into()]
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
        views
            .into_iter()
            .map(|v| Self::resolve_view(&tx, v))
            .collect()
    }
    pub fn view(&self, id: &str) -> Result<Option<SavedViewState>> {
        let view = self.records().view(id)?;
        let mut db = self.cache()?;
        let tx = storage_result(db.transaction())?;
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
