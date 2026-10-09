//! Optional, bounded test-only causal witness stream for the MCP fixture.
//!
//! The producers never open a socket or wait for a reader. A single detached
//! task sends one JSON object per connection, in FIFO order. On a lost event or
//! transport error the stream stops: later ACKs must not certify a gap.
use crate::model::IndexPin;
use serde::{Serialize, Serializer};
use std::{
    env, fs,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncWriteExt, net::UnixStream, sync::mpsc};
use uuid::Uuid;

const QUEUE_CAPACITY: usize = 64;
const MAX_RECORD_BYTES: usize = 512;
const IO_DEADLINE: Duration = Duration::from_millis(500);

/// The owner and watcher identities which make generation numbers comparable.
#[derive(Clone, Copy, Debug)]
pub struct Lineage {
    pub owner_incarnation: Uuid,
    pub watch_epoch: Uuid,
    pub ordinal: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    kind: &'static str,
    root_key: String,
    #[serde(serialize_with = "serialize_uuid")]
    owner_incarnation: Uuid,
    #[serde(serialize_with = "serialize_uuid")]
    watch_epoch: Uuid,
    lineage_ordinal: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    watch_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pin: Option<IndexPin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    queue_empty: Option<bool>,
    #[serde(serialize_with = "serialize_uuid")]
    stream_id: Uuid,
    stream_seq: u64,
}

fn serialize_uuid<S: Serializer>(value: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}

struct QueuedEvent {
    kind: &'static str,
    lineage: Lineage,
    watch_generation: Option<u64>,
    pin: Option<IndexPin>,
    queue_empty: Option<bool>,
}

/// Clone the `Arc<CausalWitness>` into each producer; it holds no root owner.
pub struct CausalWitness {
    sender: mpsc::Sender<QueuedEvent>,
    failed: Arc<AtomicBool>,
}

impl CausalWitness {
    /// Activate only for an absolute socket directly inside this user's private
    /// HOME. An invalid/unset fixture hook is inert in ordinary installations.
    pub fn from_env(root_key: String) -> Option<Arc<Self>> {
        let home = PathBuf::from(env::var_os("HOME")?);
        let socket = PathBuf::from(env::var_os("BALEYG_TEST_MCP_CAUSAL_SOCKET")?);
        if !home.is_absolute() || !socket.is_absolute() || socket.parent()? != home {
            return None;
        }
        let owner = unsafe { libc::geteuid() };
        let home_meta = fs::symlink_metadata(&home).ok()?;
        let socket_meta = fs::symlink_metadata(&socket).ok()?;
        if !home_meta.is_dir()
            || home_meta.file_type().is_symlink()
            || home_meta.uid() != owner
            || home_meta.permissions().mode() & 0o077 != 0
            || !socket_meta.file_type().is_socket()
            || socket_meta.uid() != owner
            || socket_meta.permissions().mode() & 0o077 != 0
        {
            return None;
        }
        let handle = tokio::runtime::Handle::try_current().ok()?;
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let failed = Arc::new(AtomicBool::new(false));
        let reporter = Arc::new(Self {
            sender,
            failed: failed.clone(),
        });
        handle.spawn(write_events(
            socket,
            root_key,
            Uuid::new_v4(),
            receiver,
            failed,
        ));
        Some(reporter)
    }

    pub fn h_ready(&self, lineage: Lineage, pin: IndexPin) {
        self.send(QueuedEvent {
            kind: "H_READY",
            lineage,
            watch_generation: None,
            pin: Some(pin),
            queue_empty: None,
        });
    }

    pub fn watch_pending(&self, lineage: Lineage, generation: u64) {
        self.send(QueuedEvent {
            kind: "WATCH_PENDING",
            lineage,
            watch_generation: Some(generation),
            pin: None,
            queue_empty: None,
        });
    }

    /// A nonempty FIFO is not acknowledged; emit only after inventory drains.
    pub fn watch_ack(&self, lineage: Lineage, generation: u64, pin: IndexPin, queue_empty: bool) {
        if !queue_empty {
            return;
        }
        self.send(QueuedEvent {
            kind: "WATCH_ACK",
            lineage,
            watch_generation: Some(generation),
            pin: Some(pin),
            queue_empty: Some(true),
        });
    }

    fn send(&self, event: QueuedEvent) {
        if !self.failed.load(Ordering::Acquire) && self.sender.try_send(event).is_err() {
            self.failed.store(true, Ordering::Release);
        }
    }
}

async fn write_events(
    socket: PathBuf,
    root_key: String,
    stream_id: Uuid,
    mut receiver: mpsc::Receiver<QueuedEvent>,
    failed: Arc<AtomicBool>,
) {
    let mut stream_seq = 0u64;
    while let Some(queued) = receiver.recv().await {
        if failed.load(Ordering::Acquire) {
            send_error(&socket, &root_key, stream_id, stream_seq, queued.lineage).await;
            break;
        }
        let Some(next) = stream_seq.checked_add(1) else {
            failed.store(true, Ordering::Release);
            break;
        };
        stream_seq = next; // Sequence belongs to dequeue order, never producer scheduling.
        let event = Event {
            kind: queued.kind,
            root_key: root_key.clone(),
            owner_incarnation: queued.lineage.owner_incarnation,
            watch_epoch: queued.lineage.watch_epoch,
            lineage_ordinal: queued.lineage.ordinal,
            watch_generation: queued.watch_generation,
            pin: queued.pin,
            queue_empty: queued.queue_empty,
            stream_id,
            stream_seq,
        };
        if failed.load(Ordering::Acquire) {
            send_error(
                &socket,
                &root_key,
                stream_id,
                stream_seq - 1,
                queued.lineage,
            )
            .await;
            break;
        }
        if !write_one(&socket, &event).await {
            failed.store(true, Ordering::Release);
            // The event may have been partially transmitted. ERROR is only a
            // best-effort signal; a missing or partial event itself cannot
            // certify readiness, and the stream is permanently stopped.
            send_error(&socket, &root_key, stream_id, stream_seq, queued.lineage).await;
            break;
        }
    }
}

async fn write_one(socket: &PathBuf, event: &Event) -> bool {
    let Ok(bytes) = serde_json::to_vec(event) else {
        return false;
    };
    if bytes.is_empty() || bytes.len() > MAX_RECORD_BYTES {
        return false;
    }
    // One bounded connection and one complete object. The fixture reads
    // until EOF, so shutdown and drop happen before the next event.
    matches!(
        tokio::time::timeout(IO_DEADLINE, async {
            let mut connection = UnixStream::connect(socket).await?;
            connection.write_all(&bytes).await?;
            connection.shutdown().await
        })
        .await,
        Ok(Ok(()))
    )
}

async fn send_error(
    socket: &PathBuf,
    root_key: &str,
    stream_id: Uuid,
    prior: u64,
    lineage: Lineage,
) {
    let Some(stream_seq) = prior.checked_add(1) else {
        return;
    };
    let error = Event {
        kind: "ERROR",
        root_key: root_key.to_owned(),
        owner_incarnation: lineage.owner_incarnation,
        watch_epoch: lineage.watch_epoch,
        lineage_ordinal: lineage.ordinal,
        watch_generation: None,
        pin: None,
        queue_empty: None,
        stream_id,
        stream_seq,
    };
    let _ = write_one(socket, &error).await;
}
