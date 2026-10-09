//! Reconnection policy: repeat only read-only calls after uncertain delivery.
use super::protocol::{self, Reply, Request};
use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallError {
    DaemonUnavailable,
    OutcomeUnknown,
}
impl CallError {
    pub fn code(self) -> &'static str {
        match self {
            Self::DaemonUnavailable => "daemon_unavailable",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }
}

/// The starter reports election loss separately from unexpected startup failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    Started,
    ElectionInProgress,
}

/// Start is invoked only after a failed initial connect. Concurrent starters may both
/// invoke it; the daemon's flock elects exactly one listener.
pub fn connect_or_start(
    socket: &Path,
    mut start: impl FnMut() -> io::Result<StartOutcome>,
    timeout: Duration,
) -> io::Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(socket) {
        return Ok(stream);
    }
    let _ = start()?; // An election loser waits for the winner within the same deadline.
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(stream) = UnixStream::connect(socket) {
            return Ok(stream);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "daemon unavailable",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub fn call(
    request: &Request,
    read_only: bool,
    mut connect: impl FnMut() -> io::Result<UnixStream>,
) -> Result<Reply, CallError> {
    let attempts = if read_only { 2 } else { 1 };
    for attempt in 0..attempts {
        let mut stream = match connect() {
            Ok(stream) => stream,
            Err(_) if attempt + 1 < attempts => continue,
            Err(_) => return Err(CallError::DaemonUnavailable),
        };
        // A partially written request can still have reached the server. Never replay
        // a mutation after attempting its write, including when the reply is lost.
        let reply = protocol::write_frame(&mut stream, request)
            .and_then(|()| read_reply(&mut stream, request.id));
        match reply {
            Ok(reply) => return Ok(reply),
            Err(_) if read_only && attempt + 1 < attempts => continue,
            Err(_) if read_only => return Err(CallError::DaemonUnavailable),
            Err(_) => return Err(CallError::OutcomeUnknown),
        }
    }
    Err(CallError::DaemonUnavailable)
}

/// One large CLI export is a sequence of individually bounded frames. The
/// final JSON is parsed only after every frame with the same request ID arrives.
fn read_reply(stream: &mut UnixStream, id: u64) -> io::Result<Reply> {
    let mut reply: Reply = protocol::read_frame(stream)?;
    if reply.id != id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "daemon reply ID mismatch",
        ));
    }
    if reply.payload.get("chunk").is_none() {
        return Ok(reply);
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = reply
            .payload
            .get("chunk")
            .and_then(|value| value.as_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid daemon chunk"))?;
        let part = hex::decode(chunk).map_err(io::Error::other)?;
        bytes.extend_from_slice(&part);
        let more = reply
            .payload
            .get("more")
            .and_then(|value| value.as_bool())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "missing daemon chunk flag")
            })?;
        if !more {
            return Ok(Reply {
                id,
                payload: serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            });
        }
        reply = protocol::read_frame(stream)?;
        if reply.id != id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "daemon reply ID mismatch",
            ));
        }
    }
}
