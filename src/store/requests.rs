//! Durable explicit indexing requests. Each operation has its own short-lived, protected connection.
use super::Store;
use crate::{
    indexer::{IndexOptions, ReconcileOptions},
    model::IndexPin,
    store::topology::{LeaderSession, UseGuard},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const SCHEMA: &str = "CREATE TABLE requests (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, root_device TEXT NOT NULL, root_inode TEXT NOT NULL, options_json TEXT NOT NULL, expected_generation TEXT, expected_revision INTEGER, state TEXT NOT NULL CHECK(state IN ('queued','running','done','failed')), claim_incarnation TEXT, result_generation TEXT, result_revision INTEGER, error_code TEXT, submitted_at TEXT NOT NULL, started_at TEXT, finished_at TEXT, CHECK ((expected_generation IS NULL) = (expected_revision IS NULL)), CHECK ((result_generation IS NULL) = (result_revision IS NULL)), CHECK ((state='queued' AND claim_incarnation IS NULL AND started_at IS NULL AND finished_at IS NULL AND result_generation IS NULL AND error_code IS NULL) OR (state='running' AND claim_incarnation IS NOT NULL AND started_at IS NOT NULL AND finished_at IS NULL AND result_generation IS NULL AND error_code IS NULL) OR (state='done' AND claim_incarnation IS NOT NULL AND started_at IS NOT NULL AND finished_at IS NOT NULL AND result_generation IS NOT NULL AND error_code IS NULL) OR (state='failed' AND claim_incarnation IS NOT NULL AND started_at IS NOT NULL AND finished_at IS NOT NULL AND result_generation IS NULL AND error_code IS NOT NULL))); CREATE INDEX requests_state_seq ON requests(state,seq); CREATE INDEX requests_root_state_seq ON requests(root_device,root_inode,state,seq); CREATE TABLE queue_identity (singleton INTEGER PRIMARY KEY CHECK(singleton=1), root_spelling TEXT NOT NULL, root_key TEXT NOT NULL); PRAGMA user_version=1";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    #[serde(skip)]
    pub seq: i64,
    pub id: String,
    pub state: String,
    #[serde(skip)]
    pub root_device: String,
    #[serde(skip)]
    pub root_inode: String,
    #[serde(skip)]
    pub options_json: String,
    #[serde(skip)]
    pub expected: Option<IndexPin>,
    #[serde(skip)]
    pub claim_incarnation: Option<String>,
    pub revision: Option<IndexPin>,
    pub error_code: Option<String>,
    pub submitted_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}
impl Request {
    pub fn options(&self, root: &Path) -> Result<IndexOptions> {
        ensure!(
            self.options_json.len() <= 4096,
            "invalid request options length"
        );
        let value: ReconcileOptions = serde_json::from_str(&self.options_json)?;
        ensure!(
            value.version == 1 && (1..=16_777_216).contains(&value.max_file_bytes),
            "invalid request options"
        );
        value.require_absolute_optional_inputs()?;
        let mut options = IndexOptions::new(root.to_owned());
        options.max_file_bytes = value.max_file_bytes;
        options.scip_path = value.scip_path.map(Into::into);
        options.manifest_path = value.manifest_path.map(Into::into);
        Ok(options)
    }
}
fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}
fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Request> {
    let generation: Option<String> = row.get(5)?;
    let revision: Option<i64> = row.get(6)?;
    let result_generation: Option<String> = row.get(9)?;
    let result_revision: Option<i64> = row.get(10)?;
    let pin = |generation: Option<String>,
               revision: Option<i64>|
     -> rusqlite::Result<Option<IndexPin>> {
        match (generation, revision) {
            (None, None) => Ok(None),
            (Some(g), Some(r)) if r >= 0 => Ok(Some(IndexPin {
                index_generation: Uuid::parse_str(&g).map_err(|_| rusqlite::Error::InvalidQuery)?,
                index_revision: r as u64,
            })),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    };
    let request = Request {
        seq: row.get(0)?,
        id: row.get(1)?,
        root_device: row.get(2)?,
        root_inode: row.get(3)?,
        options_json: row.get(4)?,
        expected: pin(generation, revision)?,
        state: row.get(7)?,
        claim_incarnation: row.get(8)?,
        revision: pin(result_generation, result_revision)?,
        error_code: row.get(11)?,
        submitted_at: row.get(12)?,
        started_at: row.get(13)?,
        finished_at: row.get(14)?,
    };
    if Uuid::parse_str(&request.id).is_err()
        || !matches!(
            request.state.as_str(),
            "queued" | "running" | "done" | "failed"
        )
        || request.options_json.len() > 4096
        || request.seq < 1
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(request)
}
const COLUMNS: &str = "seq,id,root_device,root_inode,options_json,expected_generation,expected_revision,state,claim_incarnation,result_generation,result_revision,error_code,submitted_at,started_at,finished_at";
impl Store {
    pub fn request_db_path(&self) -> std::path::PathBuf {
        self.roots.requests_db(&self.identity)
    }
    fn request_connection(&self) -> Result<(UseGuard, Connection)> {
        self.identity.verify()?;
        let guard = self.roots.index_use(&self.identity)?;
        let path = self.request_db_path();
        // The protected directory and file must remain private, regular and tied to the pathname.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let metadata = file.metadata()?;
        let named = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file()
                && !named.file_type().is_symlink()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o777 == 0o600
                && metadata.nlink() == 1
                && (metadata.dev(), metadata.ino()) == (named.dev(), named.ino()),
            "unsafe requests.db"
        );
        drop(file);
        let mut db = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        db.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON")?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 0 {
            let count: i64 = tx.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |r|r.get(0))?;
            ensure!(count == 0, "incompatible_queue: unexpected schema");
            tx.execute_batch(SCHEMA)?;
            tx.execute(
                "INSERT INTO queue_identity VALUES (1,?1,?2)",
                params![self.workspace_root, self.identity.root_key],
            )?;
        } else {
            ensure!(version == 1, "incompatible_queue: schema version");
        }
        tx.commit()?;
        let identity: (String, String) = db.query_row(
            "SELECT root_spelling,root_key FROM queue_identity WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            identity == (self.workspace_root.clone(), self.identity.root_key.clone()),
            "root_key_collision: requests.db belongs to different root"
        );
        let check: String = db.query_row("PRAGMA quick_check(1)", [], |r| r.get(0))?;
        ensure!(check == "ok", "incompatible_queue: integrity check failed");
        self.identity.verify()?;
        guard.verify()?;
        Ok((guard, db))
    }
    pub fn enqueue_request(
        &self,
        options: &IndexOptions,
        expected: Option<IndexPin>,
    ) -> Result<Request> {
        self.identity.verify()?;
        let selected = std::fs::symlink_metadata(&options.workspace_root)?;
        ensure!(
            selected.is_dir()
                && !selected.file_type().is_symlink()
                && (selected.dev(), selected.ino()) == (self.identity.device, self.identity.inode)
                && std::fs::canonicalize(&options.workspace_root)? == self.identity.root,
            "root_changed: request workspace mismatch"
        );
        let options = ReconcileOptions::from(options);
        ensure!(
            options.version == 1 && (1..=16_777_216).contains(&options.max_file_bytes),
            "invalid request options"
        );
        options.require_absolute_optional_inputs()?;
        let encoded = serde_json::to_string(&options)?;
        ensure!(encoded.len() <= 4096, "invalid request options length");
        let (_guard, mut db) = self.request_connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.identity.verify()?;
        let id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO requests (id,root_device,root_inode,options_json,expected_generation,expected_revision,state,submitted_at) VALUES (?1,?2,?3,?4,?5,?6,'queued',?7)", params![id,self.identity.device.to_string(),self.identity.inode.to_string(),encoded,expected.map(|p|p.index_generation.to_string()),expected.map(|p|p.index_revision as i64),now()])?;
        tx.commit()?;
        self.request_by_id(&id)?
            .context("queued request missing after commit")
    }
    pub fn request_by_id(&self, id: &str) -> Result<Option<Request>> {
        if Uuid::parse_str(id).is_err() {
            return Ok(None);
        }
        let (_guard, db) = self.request_connection()?;
        let row = db
            .query_row(
                &format!("SELECT {COLUMNS} FROM requests WHERE id=?1"),
                [id],
                read,
            )
            .optional()?;
        self.verify_request_root(&row)?;
        Ok(row)
    }
    pub fn earliest_unfinished_request(&self) -> Result<Option<Request>> {
        let (_guard, db) = self.request_connection()?;
        let row = db.query_row(
            &format!("SELECT {COLUMNS} FROM requests WHERE state IN ('queued','running') ORDER BY seq LIMIT 1"),
            [],
            read,
        ).optional()?;
        self.verify_request_root(&row)?;
        Ok(row)
    }
    pub fn current_request(&self) -> Result<Option<Request>> {
        let (_guard, db) = self.request_connection()?;
        let row = db
            .query_row(
                &format!("SELECT {COLUMNS} FROM requests ORDER BY seq DESC LIMIT 1"),
                [],
                read,
            )
            .optional()?;
        self.verify_request_root(&row)?;
        Ok(row)
    }
    fn verify_request_root(&self, row: &Option<Request>) -> Result<()> {
        self.identity.verify()?;
        if let Some(row) = row {
            ensure!(
                row.root_device == self.identity.device.to_string()
                    && row.root_inode == self.identity.inode.to_string(),
                "root_changed: request belongs to another root"
            );
        }
        Ok(())
    }
    pub fn claim_request(&self, session: &LeaderSession) -> Result<Option<Request>> {
        self.verify_leader_session(session)?;
        let (_guard, mut db) = self.request_connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.verify_leader_session(session)?;
        let row=tx.query_row(&format!("SELECT {COLUMNS} FROM requests WHERE state IN ('queued','running') ORDER BY seq LIMIT 1"), [],read).optional()?;
        let Some(row) = row else { return Ok(None) };
        self.verify_request_root(&Some(row.clone()))?;
        // A running request held by this incarnation belongs to another local driver.
        if row.state == "running"
            && row.claim_incarnation.as_deref() == Some(session.incarnation().to_string().as_str())
        {
            return Ok(None);
        }
        tx.execute("UPDATE requests SET state='running',claim_incarnation=?1,started_at=?2 WHERE seq=?3 AND state IN ('queued','running')", params![session.incarnation().to_string(),now(),row.seq])?;
        let claimed = tx.query_row(
            &format!("SELECT {COLUMNS} FROM requests WHERE seq=?1"),
            [row.seq],
            read,
        )?;
        tx.commit()?;
        Ok(Some(claimed))
    }
    pub fn finish_request(
        &self,
        session: &LeaderSession,
        request: &Request,
        result: Result<IndexPin>,
    ) -> Result<()> {
        self.verify_leader_session(session)?;
        let (_guard, mut db) = self.request_connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.verify_leader_session(session)?;
        if let Ok(pin) = &result {
            ensure!(
                self.status()?.revision == *pin,
                "revision conflict: request result is not current index"
            );
        }
        let (generation, revision, code) = match result {
            Ok(pin) => (
                Some(pin.index_generation.to_string()),
                Some(pin.index_revision as i64),
                None,
            ),
            Err(e) => (
                None,
                None,
                Some(if e.to_string().starts_with("revision conflict") {
                    "revision_conflict"
                } else {
                    "index_failed"
                }),
            ),
        };
        let changed=tx.execute("UPDATE requests SET state=?1,result_generation=?2,result_revision=?3,error_code=?4,finished_at=?5 WHERE seq=?6 AND state='running' AND claim_incarnation=?7 AND root_device=?8 AND root_inode=?9",params![if code.is_some(){"failed"}else{"done"},generation,revision,code,now(),request.seq,session.incarnation().to_string(),self.identity.device.to_string(),self.identity.inode.to_string()])?;
        ensure!(changed == 1, "storage_busy: request claim changed");
        tx.commit()?;
        Ok(())
    }
}
