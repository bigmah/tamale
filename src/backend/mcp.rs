//! A minimal MCP server (streamable HTTP transport) that offers tamale's tools to a
//! Claude Code or Codex run. Each call goes through the turn's permission checks and
//! checkpoints, exactly like a call made by the API backend.
//!
//! The server listens on 127.0.0.1 for the length of one run. Its URL contains a random
//! token, so other local processes can't call the tools.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio::task::{JoinHandle, JoinSet};

use crate::agent::{AgentEvent, TurnCtx};
use crate::tools;
use crate::workspace::Workspace;

/// Name of the server in Claude Code's MCP config; its tools appear as `mcp__tamale__<tool>`.
pub const SERVER: &str = "tamale";
pub const TOOL_PREFIX: &str = "mcp__tamale__";
/// Claude Code permission rule allowing every tool of the server.
pub const SERVER_RULE: &str = "mcp__tamale";
/// Requests larger than this are refused (tool arguments are small).
const MAX_BODY: usize = 16 << 20;
/// SSE comments sent while a tool runs, so long commands don't hit client idle timeouts.
const KEEPALIVE: Duration = Duration::from_secs(15);

/// What tool calls can do during a run.
pub struct Tools {
    /// `None` refuses every call (used while compacting).
    pub ctx: Option<Mutex<TurnCtx>>,
    pub workspace: Workspace,
    pub tx: mpsc::UnboundedSender<AgentEvent>,
}

impl Tools {
    async fn call(&self, name: &str, args: Value) -> (String, bool) {
        let Some(ctx) = &self.ctx else {
            return (
                "Tools are unavailable right now; respond with text only.".into(),
                true,
            );
        };
        // One call at a time, like the API backend (TurnCtx tracks checkpoints).
        let mut ctx = ctx.lock().await;
        ctx.call_tool(name, args, &self.workspace, &self.tx).await
    }
}

/// A running server; dropping it stops the server and any calls in progress.
pub struct Bridge {
    pub url: String,
    task: JoinHandle<()>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Bridge {
    pub async fn start(tools: Tools) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("starting the tool server for the CLI backend")?;
        let path = format!("/mcp/{}", token());
        let url = format!("http://{}{path}", listener.local_addr()?);
        let tools = Arc::new(tools);
        let task = tokio::spawn(async move {
            // Connections live in the set, so aborting the server aborts them too.
            let mut conns = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { continue };
                        conns.spawn(serve(stream, tools.clone(), path.clone()));
                    }
                    Some(_) = conns.join_next() => {}
                }
            }
        });
        Ok(Self { url, task })
    }

    /// The `--mcp-config` value pointing Claude Code at this server.
    pub fn config(&self) -> String {
        json!({ "mcpServers": { SERVER: { "type": "http", "url": self.url } } }).to_string()
    }
}

/// 128 unguessable bits from the standard library's randomly keyed hasher.
fn token() -> String {
    use std::hash::BuildHasher;
    let now = std::time::SystemTime::now();
    (0..2u8)
        .map(|i| format!("{:016x}", std::hash::RandomState::new().hash_one((i, now))))
        .collect()
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

/// Reads one HTTP/1.1 request; `None` when the client closed the connection.
async fn read_request(reader: &mut BufReader<TcpStream>) -> Result<Option<Request>> {
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut length = 0;
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().context("invalid Content-Length")?;
        }
    }
    anyhow::ensure!(length <= MAX_BODY, "request body too large");
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    Ok(Some(Request { method, path, body }))
}

async fn serve(stream: TcpStream, tools: Arc<Tools>, path: String) {
    let mut reader = BufReader::new(stream);
    while let Ok(Some(request)) = read_request(&mut reader).await {
        let keep_open = handle(&mut reader, request, &tools, &path).await;
        if !matches!(keep_open, Ok(true)) {
            break;
        }
    }
}

/// Answers one request. `Ok(true)` keeps the connection open for the next one.
async fn handle(
    reader: &mut BufReader<TcpStream>,
    request: Request,
    tools: &Tools,
    path: &str,
) -> Result<bool> {
    let stream = reader.get_mut();
    if request.path != path {
        write_response(stream, "404 Not Found", None).await?;
        return Ok(true);
    }
    if request.method != "POST" {
        // No server-initiated stream (GET) or session to end (DELETE).
        write_response(stream, "405 Method Not Allowed", None).await?;
        return Ok(true);
    }
    let Ok(message) = serde_json::from_slice::<Value>(&request.body) else {
        let error = rpc_error(Value::Null, -32700, "Parse error");
        write_response(stream, "400 Bad Request", Some(&error)).await?;
        return Ok(true);
    };
    let Some(id) = message.get("id").cloned() else {
        // Notifications (and responses) need no answer.
        write_response(stream, "202 Accepted", None).await?;
        return Ok(true);
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match message.get("method").and_then(Value::as_str).unwrap_or("") {
        "tools/call" => {
            call_tool(stream, id, params, tools).await?;
            Ok(false)
        }
        method => {
            let reply = respond(id, method, &params);
            write_response(stream, "200 OK", Some(&reply)).await?;
            Ok(true)
        }
    }
}

/// Replies to every method except `tools/call`.
fn respond(id: Value, method: &str, params: &Value) -> Value {
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params
                .get("protocolVersion")
                .cloned()
                .unwrap_or_else(|| json!("2025-06-18")),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": SERVER, "version": env!("CARGO_PKG_VERSION") },
        }),
        "ping" => json!({}),
        "tools/list" => {
            let list: Vec<Value> = tools::definitions()
                .into_iter()
                .map(|d| {
                    json!({
                        "name": d.name,
                        "description": d.description,
                        "inputSchema": d.parameters,
                    })
                })
                .collect();
            json!({ "tools": list })
        }
        _ => return rpc_error(id, -32601, "Method not found"),
    };
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Runs a tool and streams the result as a server-sent event, with keepalive comments
/// while it runs (approval prompts and long commands can take minutes).
async fn call_tool(stream: &mut TcpStream, id: Value, params: Value, tools: &Tools) -> Result<()> {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
Cache-Control: no-cache\r\nConnection: close\r\n\r\n",
        )
        .await?;
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let call = tools.call(name, args);
    tokio::pin!(call);
    let mut keepalive = tokio::time::interval(KEEPALIVE);
    keepalive.tick().await;
    let (output, is_error) = loop {
        tokio::select! {
            result = &mut call => break result,
            _ = keepalive.tick() => stream.write_all(b": keepalive\n\n").await?,
        }
    };
    let reply = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": output }],
            "isError": is_error,
        },
    });
    stream
        .write_all(format!("event: message\ndata: {reply}\n\n").as_bytes())
        .await?;
    stream.flush().await?;
    Ok(())
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

async fn write_response(stream: &mut TcpStream, status: &str, body: Option<&Value>) -> Result<()> {
    let body = body.map(Value::to_string).unwrap_or_default();
    let content_type = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/json\r\n"
    };
    stream
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\n{content_type}Content-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    Ok(())
}
