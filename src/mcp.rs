use crate::*;

pub(crate) fn run_mcp_server(db_path: &Path) -> Result<()> {
    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin.lock());
    let mut stdout = io::stdout();
    while let Some(message) = read_mcp_message(&mut reader)? {
        let request: Value = match serde_json::from_str(&message) {
            Ok(value) => value,
            Err(err) => {
                write_mcp_error(
                    &mut stdout,
                    Value::Null,
                    -32700,
                    &format!("Parse error: {err}"),
                )?;
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if method.starts_with("notifications/") {
            continue;
        }
        match handle_mcp_request(
            db_path,
            method,
            request.get("params").cloned().unwrap_or(Value::Null),
        ) {
            Ok(result) => write_mcp_result(&mut stdout, id, result)?,
            Err(err) => write_mcp_error(&mut stdout, id, -32603, &err.to_string())?,
        }
    }
    Ok(())
}

pub(crate) fn read_mcp_message<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut first = String::new();
    if reader.read_line(&mut first)? == 0 {
        return Ok(None);
    }
    if first.trim_start().starts_with('{') {
        return Ok(Some(first));
    }
    let mut content_length = None;
    let mut line = first;
    loop {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>()?);
        }
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
    }
    let len = content_length.ok_or_else(|| anyhow!("MCP message without Content-Length"))?;
    let mut bytes = vec![0u8; len];
    reader.read_exact(&mut bytes)?;
    Ok(Some(String::from_utf8(bytes)?))
}

pub(crate) fn write_mcp_result<W: Write>(writer: &mut W, id: Value, result: Value) -> Result<()> {
    write_mcp_message(
        writer,
        &serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}),
    )
}

pub(crate) fn write_mcp_error<W: Write>(
    writer: &mut W,
    id: Value,
    code: i32,
    message: &str,
) -> Result<()> {
    write_mcp_message(
        writer,
        &serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
    )
}

pub(crate) fn write_mcp_message<W: Write>(writer: &mut W, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

pub(crate) fn handle_mcp_request(db_path: &Path, method: &str, params: Value) -> Result<Value> {
    match method {
        "initialize" => Ok(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "lab-mcp", "version": env!("CARGO_PKG_VERSION") }
        })),
        "tools/list" => Ok(serde_json::json!({ "tools": mcp_tools() })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("tools/call missing name"))?;
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let value = call_mcp_tool(db_path, name, &args)?;
            Ok(serde_json::json!({
                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value)? }]
            }))
        }
        _ => Err(anyhow!("unsupported MCP method: {method}")),
    }
}

pub(crate) fn mcp_tools() -> Value {
    serde_json::json!([
        {
            "name": "sync",
            "description": "Sync invoice data from Gmail/PDF, KSeF, and/or Saldeo. Without source flags, syncs all three sources.",
            "inputSchema": {"type":"object","properties":{
                "ksef":{"type":"boolean","description":"Sync only KSeF"},
                "mail":{"type":"boolean","description":"Sync only Gmail/PDF (fetch attachments, parse, filter, store)"},
                "saldeo":{"type":"boolean","description":"Sync only Saldeo"},
                "year":{"type":"integer","default":2026},
                "ksef_input":{"type":"string","description":"Path to KSeF export directory/file"},
                "gmail_client_secret":{"type":"string","description":"Google OAuth Desktop Client JSON for token refresh"},
                "gmail_token_file":{"type":"string","description":"Path to Gmail token file"},
                "productmesh_nip":{"type":"string","default":"5242920020","description":"NIP filter for mail scanning"},
                "store":{"type":"boolean","default":false,"description":"Store records in SQLite"}
            }}
        },
        {
            "name": "reconcile",
            "description": "Compare Gmail/PDF, KSeF, and Saldeo records (tri-reconcile). Defaults refresh KSeF/Saldeo metadata before comparing.",
            "inputSchema": {"type":"object","properties":{
                "mail":{"type":"string","description":"Path to Gmail/PDF records JSON/JSONL; defaults to cached mail candidates for year"},
                "ksef":{"type":"string","description":"Path to KSeF records JSON/JSONL; defaults to configured KSeF data and refreshes it first"},
                "saldeo":{"type":"string","description":"Path to Saldeo records JSON/JSONL or raw documents.json; defaults to fetched Saldeo records and refreshes them first"},
                "review_score":{"type":"integer","default":45,"description":"Minimum match score"},
                "store":{"type":"boolean","default":false,"description":"Store temporal snapshot in SQLite"},
                "year":{"type":"integer","default":2026}
            }}
        },
        {
            "name": "reconcile_status",
            "description": "Show the last tri-reconcile report from the database for a given year.",
            "inputSchema": {"type":"object","properties":{
                "year":{"type":"integer","default":2026}
            }}
        },
        {
            "name": "upload",
            "description": "Upload invoices missing in Saldeo. Requires tri_report path or mail+ksef+saldeo paths.",
            "inputSchema": {"type":"object","properties":{
                "year":{"type":"integer","default":2026},
                "tri_report":{"type":"string","description":"Path to tri-reconcile report JSON"},
                "mail":{"type":"string","description":"Path to Gmail/PDF records JSON/JSONL"},
                "ksef":{"type":"string","description":"Path to KSeF records JSON/JSONL"},
                "saldeo":{"type":"string","description":"Path to Saldeo records JSON/JSONL"},
                "review_score":{"type":"integer","default":70,"description":"Minimum match score when computing from sources"}
            }}
        },
        {
            "name": "db_stats",
            "description": "Return SQLite record counts.",
            "inputSchema": {"type":"object","properties":{}}
        },
        {
            "name": "tri_runs",
            "description": "List temporal tri-reconcile runs and diff counters.",
            "inputSchema": {"type":"object","properties":{
                "limit":{"type":"integer","default":20}
            }}
        }
    ])
}

pub(crate) fn call_mcp_tool(db_path: &Path, name: &str, args: &Value) -> Result<Value> {
    match name {
        "sync" => {
            let year = json_i32(args, "year", 2026);
            let auto_store_online_ksef = json_path_arg(args, "ksef_input").is_none()
                && (json_bool(args, "ksef", false)
                    || (!json_bool(args, "ksef", false)
                        && !json_bool(args, "mail", false)
                        && !json_bool(args, "saldeo", false)));
            let conn = if json_bool(args, "store", false) || auto_store_online_ksef {
                Some(open_db(db_path)?)
            } else {
                None
            };
            let nip = json_string_arg(args, "productmesh_nip")
                .unwrap_or_else(|| DEFAULT_PRODUCTMESH_NIP.to_string());
            let summary = run_sync_sources(
                year,
                json_bool(args, "ksef", false),
                json_bool(args, "mail", false),
                json_bool(args, "saldeo", false),
                json_path_arg(args, "ksef_input").as_deref(),
                json_path_arg(args, "gmail_client_secret").as_deref(),
                json_path_arg(args, "gmail_token_file").as_deref(),
                &nip,
                conn.as_ref(),
            )?;
            Ok(serde_json::to_value(summary)?)
        }
        "reconcile" => {
            let year = json_i32(args, "year", 2026);
            let mail =
                json_path_arg(args, "mail").unwrap_or_else(|| default_mail_candidates_path(year));
            let ksef_arg = json_path_arg(args, "ksef");
            let saldeo_arg = json_path_arg(args, "saldeo");
            sync_reconcile_metadata(year, ksef_arg.is_none(), saldeo_arg.is_none(), db_path)?;
            let ksef = ksef_arg.unwrap_or_else(|| configured_ksef_out_path(year));
            let saldeo = saldeo_arg.unwrap_or_else(|| default_saldeo_records_path(year));
            let report = tri_reconcile(
                load_records(SourceKind::Mail, &mail)?,
                load_records(SourceKind::Ksef, &ksef)?,
                load_saldeo_records(&saldeo)?,
                json_u8(args, "review_score", 45),
            );
            if json_bool(args, "store", false) {
                let conn = open_db(db_path)?;
                let diff = store_tri_reconcile_report(&conn, year, &report)?;
                return Ok(serde_json::json!({"report": report, "temporal_diff": diff}));
            }
            Ok(serde_json::to_value(report)?)
        }
        "reconcile_status" => {
            let year = json_i32(args, "year", 2026);
            let conn = open_db(db_path)?;
            let report = load_last_tri_report(&conn, year)?;
            Ok(serde_json::to_value(report)?)
        }
        "upload" => {
            let year = json_i32(args, "year", 2026);
            let tri_report = json_path_arg(args, "tri_report");
            let mail = json_path_arg(args, "mail");
            let ksef = json_path_arg(args, "ksef");
            let saldeo = json_path_arg(args, "saldeo");
            let mut plan = saldeo_sync_plan(SaldeoSyncPlanConfig {
                year,
                tri_report: tri_report.as_deref(),
                mail: mail.as_deref(),
                ksef: ksef.as_deref(),
                saldeo: saldeo.as_deref(),
                review_score: json_u8(args, "review_score", 70),
                confirm: true,
                upload_url: None,
            })?;
            saldeo_upload_plan(
                &mut plan,
                &default_saldeo_storage_state_path(),
                DEFAULT_SALDEO_UPLOAD_URL,
                "file",
            )?;
            Ok(serde_json::to_value(plan)?)
        }
        "db_stats" => {
            let conn = open_db(db_path)?;
            Ok(serde_json::to_value(db_stats(&conn)?)?)
        }
        "tri_runs" => {
            let conn = open_db(db_path)?;
            Ok(list_tri_runs(&conn, json_usize(args, "limit", 20))?)
        }
        _ => Err(anyhow!("unknown MCP tool: {name}")),
    }
}

pub(crate) fn json_string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn json_path_arg(args: &Value, key: &str) -> Option<PathBuf> {
    json_string_arg(args, key).map(PathBuf::from)
}

pub(crate) fn json_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

pub(crate) fn json_i32(args: &Value, key: &str, default: i32) -> i32 {
    args.get(key)
        .and_then(|v| v.as_i64())
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(default)
}

pub(crate) fn json_usize(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(default)
}

pub(crate) fn json_u8(args: &Value, key: &str, default: u8) -> u8 {
    args.get(key)
        .and_then(|v| v.as_u64())
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(default)
}
