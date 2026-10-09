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

fn mcp_failure(value: &serde_json::Value, code: &str) -> serde_json::Value {
    use serde_json::json;
    let mut data = json!({"code":code});
    if let Some(selected) = value.pointer("/params/arguments/workspace") {
        data["attemptedWorkspace"] = crate::mcp::tools::attempted(selected);
    }
    json!({"jsonrpc":"2.0","id":value.get("id").cloned().unwrap_or_default(),
        "error":{"code":-32603,"message":"Internal error","data":data}})
}

#[derive(Default)]
struct McpInputBuffer {
    queue: std::collections::VecDeque<crate::mcp::wire::Frame>,
    cancelled_queued: Vec<serde_json::Value>,
}

/// One bounded stdin reader and one response writer survive daemon generations.
/// Only an interrupted read can be replayed; unknown methods are never retried.
pub fn relay_stdio(
    stream: UnixStream,
    workspace: &crate::store::topology::WorkspaceIdentity,
    mut reconnect: impl FnMut() -> io::Result<UnixStream>,
) -> io::Result<()> {
    use crate::mcp::wire::{self, Frame};
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
    let mut buffer = McpInputBuffer::default();
    let mut receive_bytes = Vec::new();
    let mut stdout = io::stdout().lock();
    let mut legacy_init: Option<Vec<u8>> = None;
    let mut legacy_ready = false;
    let mut eof = false;
    loop {
        let frame = if let Some(frame) = buffer.queue.pop_front() {
            frame
        } else if eof {
            break;
        } else {
            receiver.recv().unwrap_or(Frame::Eof)
        };
        let line = match frame {
            Frame::Eof => break,
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
        if has_id
            && let Some(at) = buffer
                .cancelled_queued
                .iter()
                .position(|id| Some(id) == value.get("id"))
        {
            buffer.cancelled_queued.remove(at);
            continue;
        }
        let read_only = matches!(
            method,
            "server/discover" | "tools/list" | "tools/call" | "initialize"
        );
        if method == "initialize" && has_id {
            legacy_init = Some(line.clone());
            legacy_ready = false;
        }
        if method == "notifications/initialized" {
            legacy_ready = true;
        }
        let mut sent = line.clone();
        sent.push(b'\n');
        let mut retry = 0;
        loop {
            if current.is_none() {
                receive_bytes.clear();
                current = reconnect()
                    .and_then(|stream| attach_mcp(stream, workspace))
                    .ok();
                if let (Some(stream), true, Some(init)) =
                    (current.as_mut(), legacy_ready, legacy_init.as_ref())
                    && method != "initialize"
                {
                    let mut setup = init.clone();
                    setup.push(b'\n');
                    let init_id = serde_json::from_slice::<serde_json::Value>(init)
                        .ok()
                        .and_then(|value| value.get("id").cloned())
                        .unwrap_or_default();
                    let setup_result = stream
                        .write_all(&setup)
                        .and_then(|()| {
                            read_mcp_line(
                                stream,
                                &receiver,
                                &mut buffer,
                                &mut stdout,
                                &mut eof,
                                Some(&init_id),
                                &mut receive_bytes,
                            )
                            .and_then(|reply| {
                                reply.ok_or_else(|| io::Error::other("MCP setup canceled"))
                            })
                            .map(|_| ())
                        })
                        .and_then(|()| {
                            stream.write_all(
                                b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
                            )
                        });
                    if setup_result.is_err() {
                        current = None;
                    }
                }
            }
            if eof {
                break;
            }
            let Some(stream) = current.as_mut() else {
                if has_id {
                    wire::write_response(&mut stdout, &mcp_failure(&value, "daemon_unavailable"))?;
                }
                break;
            };
            if stream.write_all(&sent).is_err() {
                current = None;
            } else if !has_id {
                break;
            } else {
                match read_mcp_line(
                    stream,
                    &receiver,
                    &mut buffer,
                    &mut stdout,
                    &mut eof,
                    value.get("id"),
                    &mut receive_bytes,
                ) {
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
                    Ok(None) => {
                        if !eof {
                            // No response from the canceled generation can reach a later ID.
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                            current = None;
                            receive_bytes.clear();
                        }
                        break;
                    }
                    Err(error) => {
                        let partial = error.kind() == io::ErrorKind::InvalidData;
                        current = None;
                        receive_bytes.clear();
                        if eof {
                            break;
                        }
                        // Never replay a possibly delivered mutation or a truncated reply.
                        if partial || !read_only || retry != 0 {
                            wire::write_response(
                                &mut stdout,
                                &mcp_failure(
                                    &value,
                                    if read_only {
                                        "daemon_unavailable"
                                    } else {
                                        "outcome_unknown"
                                    },
                                ),
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
                    wire::write_response(
                        &mut stdout,
                        &mcp_failure(
                            &value,
                            if read_only {
                                "daemon_unavailable"
                            } else {
                                "outcome_unknown"
                            },
                        ),
                    )?;
                }
                break;
            }
            retry += 1;
        }
        if eof {
            break;
        }
    }
    if let Some(stream) = current {
        let _ = stream.shutdown(std::net::Shutdown::Write);
    }
    Ok(())
}

fn cancellation_id(frame: &crate::mcp::wire::Frame) -> Option<serde_json::Value> {
    use crate::mcp::wire::{self, Frame};
    let Frame::Line(line) = frame else {
        return None;
    };
    let request = wire::decode(Frame::Line(line.clone())).ok()??;
    if request.id.is_some() || request.method != "notifications/cancelled" {
        return None;
    }
    let params = request.params?.as_object()?.clone();
    if !params
        .keys()
        .all(|key| key == "requestId" || key == "reason")
        || params.get("reason").is_some_and(|value| !value.is_string())
    {
        return None;
    }
    match params.get("requestId")? {
        serde_json::Value::String(text) if wire::compact_string_token_len(text) <= 256 => {
            Some(serde_json::Value::String(text.clone()))
        }
        serde_json::Value::Number(number) => {
            wire::exact_safe_integer(&number.to_string()).map(serde_json::Value::from)
        }
        _ => None,
    }
}

/// Check all ready input before committing a reply. Once the buffer.queue is full,
/// reject excess admissions explicitly; never let them hide a later cancellation.
fn drain_mcp_input(
    stream: &mut UnixStream,
    receiver: &std::sync::mpsc::Receiver<crate::mcp::wire::Frame>,
    buffer: &mut McpInputBuffer,
    stdout: &mut impl std::io::Write,
    eof: &mut bool,
    active_id: Option<&serde_json::Value>,
    canceled: &mut bool,
) -> io::Result<()> {
    use crate::mcp::wire::{self, Frame};
    use std::io::Write;
    use std::sync::mpsc::TryRecvError;
    loop {
        match receiver.try_recv() {
            Ok(Frame::Eof) => {
                *eof = true;
                return Ok(());
            }
            Ok(frame) => {
                if let Some(id) = cancellation_id(&frame) {
                    if active_id == Some(&id) {
                        *canceled = true;
                    } else if buffer.queue.iter().any(|frame| {
                        matches!(frame,
                        Frame::Line(line) if wire::decode(Frame::Line(line.clone())).ok()
                            .flatten().and_then(|request| request.id).as_ref() == Some(&id))
                    }) && !buffer.cancelled_queued.contains(&id)
                    {
                        buffer.cancelled_queued.push(id);
                    }
                    if let Frame::Line(mut line) = frame {
                        line.push(b'\n');
                        // Cancellation takes effect locally even if the old socket died.
                        let _ = stream.write_all(&line);
                    }
                } else if buffer.queue.len() < 16 {
                    buffer.queue.push_back(frame);
                } else {
                    let response = match wire::decode(frame) {
                        Err(error) => Some(error.response()),
                        Ok(Some(request)) if request.id.is_some() => Some(serde_json::json!({
                            "jsonrpc":"2.0","id":request.id,"error":{
                                "code":-32603,"message":"Internal error",
                                "data":{"code":"too_many_requests"}}})),
                        _ => None,
                    };
                    if let Some(response) = response {
                        wire::write_response(stdout, &response)?;
                    }
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    Ok(())
}

/// Read exactly one complete bounded reply, including when several arrive together.
fn read_mcp_line(
    stream: &mut UnixStream,
    receiver: &std::sync::mpsc::Receiver<crate::mcp::wire::Frame>,
    buffer: &mut McpInputBuffer,
    stdout: &mut impl std::io::Write,
    eof: &mut bool,
    expected_id: Option<&serde_json::Value>,
    receive_bytes: &mut Vec<u8>,
) -> io::Result<Option<serde_json::Value>> {
    use crate::mcp::wire;
    use std::io::Read;
    let mut line = Vec::new();
    let mut canceled = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        drain_mcp_input(
            stream,
            receiver,
            buffer,
            stdout,
            eof,
            expected_id,
            &mut canceled,
        )?;
        if *eof || canceled {
            return Ok(None);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "daemon_unavailable: MCP response deadline",
            ));
        }
        let mut buf = [0u8; 4096];
        let chunk = if receive_bytes.is_empty() {
            match stream.read(&mut buf) {
                Ok(0) => {
                    drain_mcp_input(
                        stream,
                        receiver,
                        buffer,
                        stdout,
                        eof,
                        expected_id,
                        &mut canceled,
                    )?;
                    if *eof || canceled {
                        return Ok(None);
                    }
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
            drain_mcp_input(
                stream,
                receiver,
                buffer,
                stdout,
                eof,
                expected_id,
                &mut canceled,
            )?;
            if *eof || canceled {
                return Ok(None);
            }
            return Ok(Some(reply));
        }
    }
}
