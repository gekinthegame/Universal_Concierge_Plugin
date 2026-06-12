//! Layer 4 — **MCP server**. Exposes the Concierge's tools + resources over the
//! Model Context Protocol (JSON-RPC 2.0 over stdio), so a host AI (Claude Code,
//! Cursor, …) can drive the Concierge the way the architecture intends
//! (`CONCIERGE_MCP.md`). Grounded in the MCP spec, protocol version `2025-11-25`.
//!
//! Two safety rules are baked in:
//! - **Write tools are opt-in** (`write_enabled`, Decision 0028's write-enabled
//!   mode). In read-only mode they are not even listed, and a call is rejected.
//! - **No tool publishes / egresses.** `concierge.write_site` only *stages* a draft
//!   the user previews and publishes from the GUI — publishing stays the user's
//!   explicit, password-gated act (Decision 0026). The AI prepares; the user ships.

use std::io::{BufRead, Write};

use concierge_core::{Cid, CidOrName, CoreBinding, MemCli, Node, Record};
use serde_json::{json, Value};

/// Marker kept for callers that probed the old deferred stub.
pub const STATUS: &str = "implemented (JSON-RPC 2.0 / stdio, protocol 2025-11-25)";

const PROTOCOL_VERSION: &str = "2025-11-25";
const SERVER_NAME: &str = "concierge";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Serve the Concierge over MCP on stdio until stdin closes. **stdout carries only
/// newline-delimited JSON-RPC**; all logging goes to stderr.
pub fn serve_stdio(mem: MemCli, write_enabled: bool) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    eprintln!("[concierge-mcp] stdio · protocol {PROTOCOL_VERSION} · write_enabled={write_enabled}");
    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                write_msg(&mut out, &error_object(&Value::Null, -32700, &format!("parse error: {error}")))?;
                continue;
            }
        };
        // A message with no `id` is a notification (e.g. notifications/initialized);
        // the protocol forbids a reply.
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let response = dispatch(&mem, write_enabled, method, request.get("params"), &id);
        write_msg(&mut out, &response)?;
    }
    Ok(())
}

fn write_msg(out: &mut impl Write, msg: &Value) -> std::io::Result<()> {
    out.write_all(serde_json::to_string(msg).unwrap_or_default().as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

fn result(id: &Value, value: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": value })
}

fn error_object(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A `tools/call` result: a single text block, with the tool-level `isError` flag.
fn tool_result(id: &Value, text: String, is_error: bool) -> Value {
    result(id, json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
}

fn dispatch(mem: &MemCli, write_enabled: bool, method: &str, params: Option<&Value>, id: &Value) -> Value {
    match method {
        "initialize" => result(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {}, "resources": {} },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION,
                    "title": "Universal Concierge Plugin",
                },
                "instructions": "The Universal Concierge Plugin's memory + site-building tools. \
Read tools recall stored memory (concierge.recall / concierge.resolve / concierge.get). \
When write is enabled, concierge.write_site stages a website the user previews live in the \
Studio and publishes themselves — publishing is never automatic. Never assume a tool published \
anything; report only what the result says.",
            }),
        ),
        "ping" => result(id, json!({})),
        "tools/list" => result(id, json!({ "tools": tools_list(write_enabled) })),
        "tools/call" => tools_call(mem, write_enabled, params, id),
        "resources/list" => result(id, json!({ "resources": resources_list() })),
        "resources/read" => resources_read(mem, params, id),
        other => error_object(id, -32601, &format!("method not found: {other}")),
    }
}

// ── Tools ───────────────────────────────────────────────────────────────────

fn tool_def(name: &str, description: &str, schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": schema })
}

fn str_schema(props: &[(&str, &str)], required: &[&str]) -> Value {
    let mut map = serde_json::Map::new();
    for (name, desc) in props {
        map.insert((*name).to_string(), json!({ "type": "string", "description": desc }));
    }
    json!({ "type": "object", "properties": Value::Object(map), "required": required })
}

fn tools_list(write_enabled: bool) -> Vec<Value> {
    let mut tools = vec![
        tool_def(
            "concierge.recall",
            "Recall a stored memory by its bound name (resolve + fetch the record).",
            str_schema(&[("name", "The bound name to recall")], &["name"]),
        ),
        tool_def(
            "concierge.resolve",
            "Resolve a bound name to its content id (CID).",
            str_schema(&[("name", "The bound name to resolve")], &["name"]),
        ),
        tool_def(
            "concierge.get",
            "Fetch a record by its content id (CID).",
            str_schema(&[("cid", "The content id to fetch")], &["cid"]),
        ),
    ];
    if write_enabled {
        tools.push(tool_def(
            "concierge.put_node",
            "Store a memory node. Returns its content id (CID).",
            str_schema(
                &[("kind", "Node kind, e.g. 'memory'"), ("fields_json", "JSON object of the node's fields")],
                &["kind", "fields_json"],
            ),
        ));
        tools.push(tool_def(
            "concierge.put_blob",
            "Store a text blob with a media type. Returns its content id (CID).",
            str_schema(
                &[("text", "The blob's text content"), ("media_type", "MIME type, e.g. 'text/plain'")],
                &["text", "media_type"],
            ),
        ));
        tools.push(tool_def(
            "concierge.bind",
            "Bind a human name to a content id (CID).",
            str_schema(&[("name", "The name to bind"), ("cid", "The target content id")], &["name", "cid"]),
        ));
        tools.push(tool_def(
            "concierge.write_site",
            "Stage a website (its index.html) for the user to preview live in the Studio and \
publish themselves. STAGING ONLY — this never publishes or makes anything public.",
            str_schema(
                &[
                    ("html", "The full index.html for the site"),
                    ("name", "Optional site name (folder); defaults to 'draft'"),
                ],
                &["html"],
            ),
        ));
    }
    tools
}

fn tools_call(mem: &MemCli, write_enabled: bool, params: Option<&Value>, id: &Value) -> Value {
    let name = params.and_then(|p| p.get("name")).and_then(Value::as_str).unwrap_or("");
    let empty = json!({});
    let args = params.and_then(|p| p.get("arguments")).unwrap_or(&empty);

    let is_write = matches!(
        name,
        "concierge.put_node" | "concierge.put_blob" | "concierge.bind" | "concierge.write_site"
    );
    if is_write && !write_enabled {
        return tool_result(
            id,
            format!("'{name}' is a write tool; this server is running read-only. Restart with write enabled to use it."),
            true,
        );
    }

    let outcome: Result<String, String> = match name {
        "concierge.recall" => tool_recall(mem, args),
        "concierge.resolve" => tool_resolve(mem, args),
        "concierge.get" => tool_get(mem, args),
        "concierge.put_node" => tool_put_node(mem, args),
        "concierge.put_blob" => tool_put_blob(mem, args),
        "concierge.bind" => tool_bind(mem, args),
        "concierge.write_site" => tool_write_site(mem, args),
        other => return error_object(id, -32602, &format!("unknown tool: {other}")),
    };
    match outcome {
        Ok(text) => tool_result(id, text, false),
        Err(error) => tool_result(id, error, true),
    }
}

fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required argument '{key}'"))
}

fn record_text(record: &Record) -> String {
    match record {
        Record::Live { kind, body_json, .. } => format!("[{kind}]\n{body_json}"),
        Record::Tombstone { receipt_json, .. } => format!("[tombstone]\n{receipt_json}"),
    }
}

fn tool_recall(mem: &MemCli, args: &Value) -> Result<String, String> {
    let name = arg(args, "name")?;
    let cid = mem.resolve(name).map_err(|e| e.to_string())?;
    let record = mem.get(&CidOrName::Cid(cid.clone())).map_err(|e| e.to_string())?;
    Ok(format!("{}\n{}", cid.0, record_text(&record)))
}

fn tool_resolve(mem: &MemCli, args: &Value) -> Result<String, String> {
    let name = arg(args, "name")?;
    Ok(mem.resolve(name).map_err(|e| e.to_string())?.0)
}

fn tool_get(mem: &MemCli, args: &Value) -> Result<String, String> {
    let cid = arg(args, "cid")?;
    let record = mem
        .get(&CidOrName::Cid(Cid(cid.to_string())))
        .map_err(|e| e.to_string())?;
    Ok(record_text(&record))
}

fn tool_put_node(mem: &MemCli, args: &Value) -> Result<String, String> {
    let kind = arg(args, "kind")?;
    let fields_json = arg(args, "fields_json")?;
    // Validate it parses as JSON so we never store a malformed node.
    serde_json::from_str::<Value>(fields_json).map_err(|e| format!("fields_json is not valid JSON: {e}"))?;
    let cid = mem
        .put_node(&Node { kind: kind.to_string(), fields_json: fields_json.to_string() })
        .map_err(|e| e.to_string())?;
    Ok(cid.0)
}

fn tool_put_blob(mem: &MemCli, args: &Value) -> Result<String, String> {
    let text = arg(args, "text")?;
    let media_type = arg(args, "media_type")?;
    let cid = mem.put_blob(text.as_bytes(), media_type).map_err(|e| e.to_string())?;
    Ok(cid.0)
}

fn tool_bind(mem: &MemCli, args: &Value) -> Result<String, String> {
    let name = arg(args, "name")?;
    let cid = arg(args, "cid")?;
    mem.bind(name, &Cid(cid.to_string())).map_err(|e| e.to_string())?;
    Ok(format!("bound '{name}' → {cid}"))
}

fn tool_write_site(mem: &MemCli, args: &Value) -> Result<String, String> {
    let html = arg(args, "html")?;
    let name = args.get("name").and_then(Value::as_str).unwrap_or("draft");
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(48)
        .collect();
    let safe = if safe.is_empty() { "draft".to_string() } else { safe };
    let store = mem.store_dir().map_err(|e| e.to_string())?;
    let folder = store.join("canvas").join(&safe);
    std::fs::create_dir_all(&folder).map_err(|e| format!("create draft dir: {e}"))?;
    std::fs::write(folder.join("index.html"), html).map_err(|e| format!("write draft: {e}"))?;
    Ok(format!(
        "Staged site '{safe}' ({} bytes) at {}. It appears live in the Concierge Studio (Write tab). \
The user previews it and publishes it themselves — nothing has been published or made public.",
        html.len(),
        folder.join("index.html").display()
    ))
}

// ── Resources (side-effect-free reads) ──────────────────────────────────────

fn resources_list() -> Vec<Value> {
    // Names and CIDs are addressable by URI template; we advertise the stable head.
    vec![json!({
        "uri": "concierge://name/latest",
        "name": "latest",
        "description": "The latest checkpoint of the memory store.",
        "mimeType": "text/plain",
    })]
}

fn resources_read(mem: &MemCli, params: Option<&Value>, id: &Value) -> Value {
    let uri = params.and_then(|p| p.get("uri")).and_then(Value::as_str).unwrap_or("");
    let target = if let Some(name) = uri.strip_prefix("concierge://name/") {
        mem.resolve(name).map(CidOrName::Cid).map_err(|e| e.to_string())
    } else if let Some(cid) = uri.strip_prefix("concierge://cid/") {
        Ok(CidOrName::Cid(Cid(cid.to_string())))
    } else {
        return error_object(id, -32602, &format!("unsupported resource uri: {uri}"));
    };
    match target.and_then(|t| mem.get(&t).map_err(|e| e.to_string())) {
        Ok(record) => result(
            id,
            json!({ "contents": [{ "uri": uri, "mimeType": "text/plain", "text": record_text(&record) }] }),
        ),
        Err(error) => error_object(id, -32603, &error),
    }
}
