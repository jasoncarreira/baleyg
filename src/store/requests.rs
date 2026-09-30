//! Durable explicit indexing requests. Each operation has its own short-lived, protected connection.
use super::Store;
use crate::{
    indexer::{IndexOptions, ReconcileOptions},
    model::IndexPin,
    store::topology::{LeaderSession, UseGuard},
};
use anyhow::{Result, ensure};
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CompletionOutcome {
    Done(IndexPin),
    Failed(&'static str),
}
impl CompletionOutcome {
    fn from_result(result: Result<IndexPin>) -> Self {
        match result {
            Ok(pin) => Self::Done(pin),
            Err(error) if error.to_string().starts_with("revision conflict") => {
                Self::Failed("revision_conflict")
            }
            Err(_) => Self::Failed("index_failed"),
        }
    }
    fn matches_terminal(&self, row: &Request) -> bool {
        match self {
            Self::Done(pin) => {
                row.state == "done" && row.revision == Some(*pin) && row.error_code.is_none()
            }
            Self::Failed(code) => {
                row.state == "failed"
                    && row.error_code.as_deref() == Some(*code)
                    && row.revision.is_none()
            }
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct PendingCompletion {
    request: Request,
    outcome: CompletionOutcome,
    incarnation: String,
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
        // A current queue read needs no reserved SQLite writer lock. The first opener
        // alone enters IMMEDIATE, then rechecks after the lock to race safely with
        // another process initializing the same private file. Even read-only callers
        // still open SQLite normally (hot-journal recovery), validate user_version,
        // root singleton and quick_check before interpreting any row.
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 0 {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let locked_version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
            if locked_version == 0 {
                let count: i64 = tx.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |r|r.get(0))?;
                ensure!(count == 0, "incompatible_queue: unexpected schema");
                tx.execute_batch(SCHEMA)?;
                tx.execute(
                    "INSERT INTO queue_identity VALUES (1,?1,?2)",
                    params![self.workspace_root, self.identity.root_key],
                )?;
            } else {
                ensure!(locked_version == 1, "incompatible_queue: schema version");
            }
            tx.commit()?;
        } else {
            ensure!(version == 1, "incompatible_queue: schema version");
        }
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
        let submitted_at = now();
        let root_device = self.identity.device.to_string();
        let root_inode = self.identity.inode.to_string();
        tx.execute("INSERT INTO requests (id,root_device,root_inode,options_json,expected_generation,expected_revision,state,submitted_at) VALUES (?1,?2,?3,?4,?5,?6,'queued',?7)", params![id,root_device,root_inode,encoded,expected.map(|p|p.index_generation.to_string()),expected.map(|p|p.index_revision as i64),submitted_at])?;
        let seq = tx.last_insert_rowid();
        tx.commit()?;
        self.identity.verify()?;
        // Return the exact accepted row constructed under the INSERT transaction.
        // A leader may claim immediately after COMMIT; reading it back would race
        // and mislabel the acknowledgement as already running or terminal.
        Ok(Request {
            seq,
            id,
            state: "queued".into(),
            root_device,
            root_inode,
            options_json: encoded,
            expected,
            claim_incarnation: None,
            revision: None,
            error_code: None,
            submitted_at,
            started_at: None,
            finished_at: None,
        })
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
    pub(crate) fn record_and_finish_request(
        &self,
        session: &LeaderSession,
        request: &Request,
        result: Result<IndexPin>,
    ) -> Result<()> {
        self.verify_leader_session(session)?;
        {
            let mut slot = self.pending_request_completion.lock().unwrap();
            ensure!(slot.is_none(), "storage_busy: unresolved FIFO completion");
            *slot = Some(PendingCompletion {
                request: request.clone(),
                outcome: CompletionOutcome::from_result(result),
                incarnation: session.incarnation().to_string(),
            });
        }
        // Cache the exact result before attempting any SQLite terminal write. A transient
        // failure must not let a subsequent tick re-publish this already executed head.
        self.retry_recorded_completion(session)?;
        Ok(())
    }

    /// Resolve the one in-flight FIFO head before claiming any newer row. A terminal
    /// reread handles an ambiguous SQLite COMMIT; it must match our cached result.
    pub(crate) fn retry_recorded_completion(&self, session: &LeaderSession) -> Result<bool> {
        self.verify_leader_session(session)?;
        let mut slot = self.pending_request_completion.lock().unwrap();
        let Some(pending) = slot.as_ref() else {
            return Ok(false);
        };
        if pending.incarnation != session.incarnation().to_string() {
            // A successor has a new incarnation and must have reconciled before drain.
            // Its ordinary claim path reclaims the old running head under that fence.
            *slot = None;
            return Ok(false);
        }
        let row = self
            .request_by_id(&pending.request.id)?
            .ok_or_else(|| anyhow::anyhow!("storage_busy: cached request disappeared"))?;
        ensure!(
            row.id == pending.request.id
                && row.seq == pending.request.seq
                && row.root_device == pending.request.root_device
                && row.root_inode == pending.request.root_inode
                && row.claim_incarnation.as_deref() == Some(pending.incarnation.as_str()),
            "storage_busy: cached claim changed"
        );
        if row.state == "done" || row.state == "failed" {
            ensure!(
                pending.outcome.matches_terminal(&row),
                "revision conflict: cached completion differs from durable terminal"
            );
            *slot = None;
            return Ok(true);
        }
        ensure!(
            row.state == "running",
            "storage_busy: cached FIFO head is not running"
        );
        if let Err(error) = self.finish_request_outcome(session, &pending.request, &pending.outcome)
        {
            // The write may have committed before returning an error. Confirm the full
            // id/seq/root/incarnation and exact terminal result before treating it as done.
            let reread = self
                .request_by_id(&pending.request.id)?
                .ok_or_else(|| anyhow::anyhow!("storage_busy: cached request disappeared"))?;
            ensure!(
                reread.id == pending.request.id
                    && reread.seq == pending.request.seq
                    && reread.root_device == pending.request.root_device
                    && reread.root_inode == pending.request.root_inode
                    && reread.claim_incarnation.as_deref() == Some(pending.incarnation.as_str()),
                "storage_busy: cached claim changed"
            );
            if reread.state == "done" || reread.state == "failed" {
                ensure!(
                    pending.outcome.matches_terminal(&reread),
                    "revision conflict: cached completion differs from durable terminal"
                );
            } else {
                return Err(error);
            }
        }
        *slot = None;
        Ok(true)
    }

    pub fn finish_request(
        &self,
        session: &LeaderSession,
        request: &Request,
        result: Result<IndexPin>,
    ) -> Result<()> {
        self.finish_request_outcome(session, request, &CompletionOutcome::from_result(result))
    }
    fn finish_request_outcome(
        &self,
        session: &LeaderSession,
        request: &Request,
        outcome: &CompletionOutcome,
    ) -> Result<()> {
        self.verify_leader_session(session)?;
        #[cfg(test)]
        if self
            .test_queue_finish_failures
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |count| (count > 0).then_some(count - 1),
            )
            .is_ok()
        {
            anyhow::bail!("storage_busy: injected terminal write failure");
        }
        let (_guard, mut db) = self.request_connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.verify_leader_session(session)?;
        if let CompletionOutcome::Done(pin) = outcome {
            ensure!(
                self.status()?.revision == *pin,
                "revision conflict: request result is not current index"
            );
        }
        let (generation, revision, code) = match outcome {
            CompletionOutcome::Done(pin) => (
                Some(pin.index_generation.to_string()),
                Some(pin.index_revision as i64),
                None,
            ),
            CompletionOutcome::Failed(code) => (None, None, Some(*code)),
        };
        let changed=tx.execute("UPDATE requests SET state=?1,result_generation=?2,result_revision=?3,error_code=?4,finished_at=?5 WHERE seq=?6 AND state='running' AND claim_incarnation=?7 AND root_device=?8 AND root_inode=?9",params![if code.is_some(){"failed"}else{"done"},generation,revision,code,now(),request.seq,session.incarnation().to_string(),self.identity.device.to_string(),self.identity.inode.to_string()])?;
        ensure!(changed == 1, "storage_busy: request claim changed");
        tx.commit()?;
        #[cfg(test)]
        if self
            .test_queue_post_commit_failures
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |count| (count > 0).then_some(count - 1),
            )
            .is_ok()
        {
            anyhow::bail!("storage_busy: injected ambiguous terminal commit");
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn inject_queue_finish_failure(&self, after_commit: bool) {
        let counter = if after_commit {
            &self.test_queue_post_commit_failures
        } else {
            &self.test_queue_finish_failures
        };
        counter.store(1, std::sync::atomic::Ordering::Release);
    }
}
