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

/// Attach the original verified launch identity to every daemon generation.
fn attach_mcp(
    mut stream: UnixStream,
    workspace: &crate::store::topology::WorkspaceIdentity,
) -> io::Result<UnixStream> {
    let request = Request {
        id: 1,
        operation: "mcp".into(),
        payload: serde_json::json!({
            "workspace": workspace.root,
            "device": workspace.device,
            "inode": workspace.inode,
        }),
    };
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    protocol::write_frame(&mut stream, &request)?;
    let reply: Reply = protocol::read_frame(&mut stream)?;
    if reply.id != request.id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "daemon reply ID mismatch",
        ));
    }
    if let Some(error) = reply.payload.get("error").and_then(|v| v.as_str()) {
        return Err(io::Error::other(error.to_owned()));
    }
    if reply.payload.get("result") != Some(&serde_json::Value::Bool(true)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid MCP attach reply",
        ));
    }
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(stream)
}

fn mcp_failure(
    value: &serde_json::Value,
    code: &str,
    workspace: &crate::store::topology::WorkspaceIdentity,
    modern: bool,
) -> serde_json::Value {
    use serde_json::json;
    let id = value.get("id").cloned().unwrap_or(serde_json::Value::Null);
    if value.get("method").and_then(|v| v.as_str()) == Some("tools/call")
        && value
            .pointer("/params/name")
            .and_then(|v| v.as_str())
            .is_some_and(|name| crate::mcp::catalog::NAMES.contains(&name))
    {
        let root = value
            .pointer("/params/arguments/workspace")
            .and_then(|v| v.as_str())
            .map(std::path::Path::new)
            .unwrap_or(&workspace.root);
        let envelope =
            crate::mcp::tools::attributed_root(crate::mcp::tools::failure(&id, code), root, false);
        json!({"jsonrpc":"2.0","id":id,
            "result":crate::mcp::tools::result(envelope, modern)})
    } else {
        json!({"jsonrpc":"2.0","id":id,"error":{
            "code":-32603,"message":"Internal error","data":{"code":code}}})
    }
}

/// One bounded stdin reader and one response writer survive daemon generations.
/// Only an interrupted read can be replayed; unknown methods are never retried.
pub fn relay_stdio(
    stream: UnixStream,
    workspace: &crate::store::topology::WorkspaceIdentity,
    mut reconnect: impl FnMut() -> io::Result<UnixStream>,
) -> io::Result<()> {
    use crate::mcp::wire::{self, Frame};
    use std::collections::VecDeque;
    use std::io::{BufReader, Write};
    use std::sync::mpsc;
    let mut current = Some(attach_mcp(stream, workspace)?);
    let (sender, receiver) = mpsc::sync_channel::<Frame>(16);
    thread::Builder::new()
        .name("mcp-stdin-relay".into())
        .spawn(move || {
            let mut reader = BufReader::new(io::stdin());
            loop {
                match wire::read_frame(&mut reader) {
                    Ok(Frame::Eof) => {
                        let _ = sender.send(Frame::Eof);
                        break;
                    }
                    Ok(frame) => {
                        if sender.send(frame).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        eprintln!("MCP stdin read failed: {error}");
                        let _ = sender.send(Frame::Eof);
                        break;
                    }
                }
            }
        })?;
    let mut queue: VecDeque<(Frame, bool)> = VecDeque::new();
    let mut buffered_reply = None;
    let mut receive_bytes = Vec::new();
    let mut stdout = io::stdout().lock();
    let mut legacy_init: Option<Vec<u8>> = None;
    let mut legacy_ready = false;
    let mut modern = false;
    let mut eof = false;
    loop {
        let (frame, mut pre_sent) = if let Some(frame) = queue.pop_front() {
            frame
        } else if eof {
            break;
        } else {
            (
                match receiver.recv() {
                    Ok(frame) => frame,
                    Err(_) => Frame::Eof,
                },
                false,
            )
        };
        let line = match frame {
            Frame::Eof => {
                eof = true;
                continue;
            }
            Frame::Oversized => {
                wire::write_response(
                    &mut stdout,
                    &wire::ProtocolError::new(-32600, serde_json::Value::Null).response(),
                )?;
                continue;
            }
            Frame::Line(line) => line,
        };
        let admitted = match wire::decode(Frame::Line(line.clone())) {
            Ok(Some(request)) => request,
            Ok(None) => continue,
            Err(error) => {
                wire::write_response(&mut stdout, &error.response())?;
                continue;
            }
        };
        let mut value: serde_json::Value = serde_json::from_slice(&line).expect("decoded MCP line");
        if let Some(id) = admitted.id {
            value["id"] = id;
        }
        let method = value.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let has_id = value.get("id").is_some();
        let read_only = matches!(
            method,
            "server/discover" | "tools/list" | "tools/call" | "initialize"
        );
        if method == "initialize" && has_id {
            legacy_init = Some(line.clone());
            legacy_ready = false;
            modern = false;
        }
        if method == "server/discover" && has_id {
            modern = true;
        }
        if method == "notifications/initialized" {
            legacy_ready = true;
        }
        let mut sent = line.clone();
        sent.push(b'\n');
        let mut retry = 0;
        loop {
            if current.is_none() {
                pre_sent = false;
                for (_, sent) in &mut queue {
                    *sent = false;
                }
                buffered_reply = None;
                receive_bytes.clear();
                current = reconnect()
                    .and_then(|stream| attach_mcp(stream, workspace))
                    .ok();
                if current.is_some()
                    && legacy_ready
                    && method != "initialize"
                    && let Some(init) = &legacy_init
                {
                    let stream = current.as_mut().unwrap();
                    let mut setup = init.clone();
                    setup.push(b'\n');
                    if stream.write_all(&setup).is_ok()
                        && read_mcp_line(
                            stream,
                            &receiver,
                            &mut queue,
                            &mut eof,
                            &serde_json::from_slice::<serde_json::Value>(init)
                                .ok()
                                .and_then(|value| value.get("id").cloned())
                                .unwrap_or_default(),
                            &mut buffered_reply,
                            &mut receive_bytes,
                        )
                        .is_ok()
                    {
                        if stream
                            .write_all(
                                b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
                            )
                            .is_err()
                        {
                            current = None;
                        }
                    } else {
                        current = None;
                    }
                }
            }
            let Some(stream) = current.as_mut() else {
                if has_id {
                    wire::write_response(
                        &mut stdout,
                        &mcp_failure(&value, "daemon_unavailable", workspace, modern),
                    )?;
                }
                break;
            };
            if !pre_sent && stream.write_all(&sent).is_err() {
                current = None;
            } else if !has_id {
                break;
            } else {
                let response = if let Some(reply) = buffered_reply.take() {
                    Ok(Some(reply))
                } else {
                    read_mcp_line(
                        stream,
                        &receiver,
                        &mut queue,
                        &mut eof,
                        value.get("id").expect("request ID"),
                        &mut buffered_reply,
                        &mut receive_bytes,
                    )
                };
                match response {
                    Ok(Some(reply)) => {
                        if reply.get("id") != value.get("id") {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "daemon MCP reply ID mismatch",
                            ));
                        }
                        wire::write_response(&mut stdout, &reply)?;
                        break;
                    }
                    Ok(None) => break, // canceled: the next already-sent reply is buffered
                    Err(error) => {
                        let partial = error.kind() == io::ErrorKind::InvalidData;
                        current = None;
                        receive_bytes.clear();
                        buffered_reply = None;
                        // Partial output is never forwarded; avoid retrying a truncated reply.
                        if partial || !read_only || retry != 0 {
                            let code = if read_only {
                                "daemon_unavailable"
                            } else {
                                "outcome_unknown"
                            };
                            wire::write_response(
                                &mut stdout,
                                &mcp_failure(&value, code, workspace, modern),
                            )?;
                            break;
                        }
                        retry += 1;
                        continue;
                    }
                }
            }
            if !read_only || retry != 0 {
                if has_id {
                    let code = if read_only {
                        "daemon_unavailable"
                    } else {
                        "outcome_unknown"
                    };
                    wire::write_response(
                        &mut stdout,
                        &mcp_failure(&value, code, workspace, modern),
                    )?;
                }
                break;
            }
            retry += 1;
        }
    }
    if let Some(stream) = current {
        let _ = stream.shutdown(std::net::Shutdown::Write);
    }
    Ok(())
}

/// Wait only while a reply is outstanding. Drain available input before publishing
/// any outcome, so a cancellation already queued reaches the daemon first.
fn read_mcp_line(
    stream: &mut UnixStream,
    receiver: &std::sync::mpsc::Receiver<crate::mcp::wire::Frame>,
    queue: &mut std::collections::VecDeque<(crate::mcp::wire::Frame, bool)>,
    eof: &mut bool,
    expected_id: &serde_json::Value,
    buffered_reply: &mut Option<serde_json::Value>,
    receive_bytes: &mut Vec<u8>,
) -> io::Result<Option<serde_json::Value>> {
    use crate::mcp::wire::{self, Frame};
    use std::io::{Read, Write};
    use std::sync::mpsc::TryRecvError;
    let mut line = Vec::new();
    let mut cancelled = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "daemon_unavailable: MCP response deadline",
            ));
        }
        while queue.len() < 16 {
            match receiver.try_recv() {
                Ok(Frame::Line(bytes))
                    if serde_json::from_slice::<serde_json::Value>(&bytes)
                        .ok()
                        .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(str::to_owned))
                        .as_deref()
                        == Some("notifications/cancelled") =>
                {
                    let notification: serde_json::Value =
                        serde_json::from_slice(&bytes).expect("decoded notification");
                    if notification.pointer("/params/requestId") == Some(expected_id) {
                        cancelled = true;
                    }
                    let mut sent = bytes;
                    sent.push(b'\n');
                    stream.write_all(&sent)?;
                }
                Ok(Frame::Eof) => {
                    *eof = true;
                }
                Ok(frame) => queue.push_back((frame, false)),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if cancelled {
            for (frame, sent) in queue.iter_mut() {
                if !*sent && let Frame::Line(bytes) = frame {
                    let mut line = bytes.clone();
                    line.push(b'\n');
                    stream.write_all(&line)?;
                    *sent = true;
                }
            }
            if *eof && queue.is_empty() {
                return Ok(None);
            }
        }
        let mut buf = [0u8; 4096];
        let chunk = if receive_bytes.is_empty() {
            match stream.read(&mut buf) {
                Ok(0) => {
                    return Err(io::Error::new(
                        if line.is_empty() {
                            io::ErrorKind::UnexpectedEof
                        } else {
                            io::ErrorKind::InvalidData
                        },
                        "daemon_unavailable: MCP response interrupted",
                    ));
                }
                Ok(n) => buf[..n].to_vec(),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        } else {
            std::mem::take(receive_bytes)
        };
        let end = chunk.iter().position(|byte| *byte == b'\n');
        let take = end.map_or(chunk.len(), |offset| offset + 1);
        if line.len() + take > wire::RESPONSE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP response too large",
            ));
        }
        line.extend_from_slice(&chunk[..take]);
        if end.is_some() {
            receive_bytes.extend_from_slice(&chunk[take..]);
            let reply: serde_json::Value = serde_json::from_slice(&line)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if cancelled && reply.get("id") != Some(expected_id) {
                *buffered_reply = Some(reply);
                return Ok(None);
            }
            return Ok(Some(reply));
        }
    }
}
