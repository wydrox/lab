use crate::cli::{DEFAULT_REVIEW_SCORE, MAX_REVIEW_SCORE, MIN_REVIEW_SCORE, default_year};
use crate::*;

#[cfg(test)]
mod mcp_tests;

pub(crate) const JSONRPC_PARSE_ERROR: i32 = -32700;
pub(crate) const JSONRPC_INVALID_REQUEST: i32 = -32600;
pub(crate) const JSONRPC_METHOD_NOT_FOUND: i32 = -32601;
pub(crate) const JSONRPC_INVALID_PARAMS: i32 = -32602;
/// Górny limit `Content-Length` — chroni przed alokacją absurdalnego bufora.
const MAX_MCP_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// Błąd na poziomie protokołu JSON-RPC (nie błąd wykonania narzędzia).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpError {
    pub(crate) code: i32,
    pub(crate) message: String,
}

impl McpError {
    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: JSONRPC_INVALID_PARAMS,
            message: message.into(),
        }
    }

    fn invalid_argument(key: &str, expected: &str, got: &Value) -> Self {
        Self::invalid_params(format!(
            "invalid argument `{key}`: expected {expected}, got {got}"
        ))
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

/// Sposób ramkowania wiadomości na stdio. Odpowiedź używa tego samego
/// ramkowania co żądanie, na które odpowiada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpFraming {
    /// Standardowy transport MCP stdio: jedna wiadomość JSON na linię.
    Newline,
    /// Zgodność wsteczna: nagłówek `Content-Length: N` + pusta linia + body.
    ContentLength,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpIncoming {
    pub(crate) framing: McpFraming,
    /// Treść wiadomości albo opis błędu ramkowania/kodowania (odpowiedź -32700).
    pub(crate) payload: std::result::Result<String, String>,
}

pub(crate) fn run_mcp_server(db_path: &Path) -> Result<()> {
    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin.lock());
    let mut stdout = io::stdout().lock();
    serve_mcp(db_path, &mut reader, &mut stdout)
}

pub(crate) fn serve_mcp<R: BufRead, W: Write>(
    db_path: &Path,
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    while let Some(incoming) = read_mcp_message(reader)? {
        let response = match incoming.payload {
            Ok(text) => handle_mcp_message(db_path, &text),
            Err(message) => Some(jsonrpc_error(
                Value::Null,
                JSONRPC_PARSE_ERROR,
                &format!("Parse error: {message}"),
            )),
        };
        if let Some(response) = response {
            write_mcp_message(writer, &response, incoming.framing)?;
        }
    }
    Ok(())
}

/// Czyta jedną wiadomość. Puste linie między wiadomościami są pomijane.
/// Linia zaczynająca się od nagłówka (`Content-Length:`/`Content-Type:`)
/// oznacza ramkowanie nagłówkowe; każda inna linia to wiadomość w trybie
/// newline-delimited. `Err` zwraca wyłącznie błędy I/O.
pub(crate) fn read_mcp_message<R: BufRead>(reader: &mut R) -> Result<Option<McpIncoming>> {
    let mut first = Vec::new();
    loop {
        first.clear();
        if reader.read_until(b'\n', &mut first)? == 0 {
            return Ok(None);
        }
        if !first.iter().all(u8::is_ascii_whitespace) {
            break;
        }
    }
    if !is_mcp_header_line(&first) {
        return Ok(Some(McpIncoming {
            framing: McpFraming::Newline,
            payload: String::from_utf8(first)
                .map(|text| text.trim().to_string())
                .map_err(|err| format!("message is not valid UTF-8: {err}")),
        }));
    }

    let mut content_length: Option<std::result::Result<usize, String>> = None;
    let mut line = first;
    loop {
        let text = String::from_utf8_lossy(&line);
        let trimmed = text.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|err| format!("invalid Content-Length {:?}: {err}", value.trim())),
            );
        }
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(None);
        }
    }
    let payload = match content_length {
        None => Err("MCP header block without Content-Length".to_string()),
        Some(Err(message)) => Err(message),
        Some(Ok(len)) if len > MAX_MCP_MESSAGE_BYTES => Err(format!(
            "Content-Length {len} exceeds the {MAX_MCP_MESSAGE_BYTES}-byte limit"
        )),
        Some(Ok(len)) => {
            let mut bytes = vec![0u8; len];
            match reader.read_exact(&mut bytes) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(err) => return Err(err.into()),
            }
            String::from_utf8(bytes).map_err(|err| format!("message is not valid UTF-8: {err}"))
        }
    };
    Ok(Some(McpIncoming {
        framing: McpFraming::ContentLength,
        payload,
    }))
}

fn is_mcp_header_line(line: &[u8]) -> bool {
    let lower = String::from_utf8_lossy(line)
        .trim_start()
        .to_ascii_lowercase();
    lower.starts_with("content-length:") || lower.starts_with("content-type:")
}

pub(crate) fn write_mcp_message<W: Write>(
    writer: &mut W,
    value: &Value,
    framing: McpFraming,
) -> Result<()> {
    // Kompaktowy JSON: znaki nowej linii w stringach są escapowane, więc
    // wiadomość nigdy nie zawiera dosłownego '\n'.
    let body = serde_json::to_vec(value)?;
    match framing {
        McpFraming::Newline => {
            writer.write_all(&body)?;
            writer.write_all(b"\n")?;
        }
        McpFraming::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
            writer.write_all(&body)?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn jsonrpc_error(id: Value, code: i32, message: &str) -> Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

/// Obsługuje jedną wiadomość JSON-RPC. Zwraca odpowiedź albo `None`
/// dla notyfikacji (wiadomość bez `id`) i odpowiedzi klienta.
pub(crate) fn handle_mcp_message(db_path: &Path, text: &str) -> Option<Value> {
    let request: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(err) => {
            return Some(jsonrpc_error(
                Value::Null,
                JSONRPC_PARSE_ERROR,
                &format!("Parse error: {err}"),
            ));
        }
    };
    let Some(object) = request.as_object() else {
        return Some(jsonrpc_error(
            Value::Null,
            JSONRPC_INVALID_REQUEST,
            "Invalid Request: expected a JSON-RPC object",
        ));
    };
    let id = object.get("id").cloned();
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        // Odpowiedź klienta (result/error) albo notyfikacja bez metody: bez odpowiedzi.
        let is_client_response = object.contains_key("result") || object.contains_key("error");
        return match id {
            Some(id) if !is_client_response => Some(jsonrpc_error(
                id,
                JSONRPC_INVALID_REQUEST,
                "Invalid Request: missing method",
            )),
            _ => None,
        };
    };
    // Wiadomość bez `id` to notyfikacja — nigdy nie dostaje odpowiedzi.
    let id = id?;
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    Some(match handle_mcp_request(db_path, method, &params) {
        Ok(result) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(err) => jsonrpc_error(id, err.code, &err.message),
    })
}

pub(crate) fn handle_mcp_request(
    db_path: &Path,
    method: &str,
    params: &Value,
) -> std::result::Result<Value, McpError> {
    match method {
        "initialize" => Ok(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "lab-mcp", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(serde_json::json!({})),
        "tools/list" => Ok(serde_json::json!({ "tools": mcp_tools() })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| McpError::invalid_params("tools/call: missing tool `name`"))?;
            let args = match params.get("arguments") {
                None | Some(Value::Null) => serde_json::json!({}),
                Some(value @ Value::Object(_)) => value.clone(),
                Some(other) => {
                    return Err(McpError::invalid_params(format!(
                        "tools/call: `arguments` must be an object, got {other}"
                    )));
                }
            };
            let call = parse_mcp_tool_call(name, &args)?;
            Ok(match execute_mcp_tool(db_path, call) {
                Ok(output) => output.into_tool_result(),
                Err(err) => mcp_tool_error_result(&format!("{err:#}")),
            })
        }
        _ => Err(McpError {
            code: JSONRPC_METHOD_NOT_FOUND,
            message: format!("Method not found: {method}"),
        }),
    }
}

/// Wynik narzędzia: wartość JSON i opcjonalny błąd kroku po częściowym wykonaniu.
#[derive(Debug)]
pub(crate) struct McpToolOutput {
    pub(crate) value: Value,
    pub(crate) error: Option<String>,
}

impl McpToolOutput {
    fn ok(value: Value) -> Self {
        Self { value, error: None }
    }

    pub(crate) fn into_tool_result(self) -> Value {
        let json = pretty_json(&self.value);
        match self.error {
            None => serde_json::json!({ "content": [{ "type": "text", "text": json }] }),
            Some(error) => serde_json::json!({
                "content": [
                    { "type": "text", "text": error },
                    { "type": "text", "text": json }
                ],
                "isError": true
            }),
        }
    }
}

pub(crate) fn mcp_tool_error_result(message: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true
    })
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

pub(crate) fn mcp_tools() -> Value {
    let review_score = serde_json::json!({
        "type": "integer",
        "default": DEFAULT_REVIEW_SCORE,
        "minimum": MIN_REVIEW_SCORE,
        "maximum": MAX_REVIEW_SCORE,
        "description": format!(
            "Minimum match score ({MIN_REVIEW_SCORE}-{MAX_REVIEW_SCORE}, default {DEFAULT_REVIEW_SCORE})"
        )
    });
    let year = serde_json::json!({
        "type": "integer",
        "description": "Settlement year; defaults to the current year"
    });
    serde_json::json!([
        {
            "name": "sync",
            "description": "Sync invoice data from Gmail/PDF, KSeF, and/or Saldeo. Without source flags, syncs all three sources.",
            "inputSchema": {"type":"object","properties":{
                "ksef":{"type":"boolean","default":false,"description":"Sync only KSeF"},
                "mail":{"type":"boolean","default":false,"description":"Sync only Gmail/PDF (fetch attachments, parse, filter, store)"},
                "amazon_mail":{"type":"boolean","default":false,"description":"Sync only Amazon.it / Amazon.es mail via Gmail search preset (cannot be combined with mail)"},
                "saldeo":{"type":"boolean","default":false,"description":"Sync only Saldeo"},
                "year":year,
                "ksef_input":{"type":"string","description":"Path to KSeF export directory/file"},
                "gmail_client_secret":{"type":"string","description":"Google OAuth Desktop Client JSON for token refresh"},
                "gmail_token_file":{"type":"string","description":"Path to Gmail token file"},
                "productmesh_nip":{"type":"string","default":DEFAULT_PRODUCTMESH_NIP,"description":"NIP filter for mail scanning"},
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
                "review_score":review_score,
                "store":{"type":"boolean","default":false,"description":"Store temporal snapshot in SQLite"},
                "year":year
            }}
        },
        {
            "name": "reconcile_status",
            "description": "Show the last tri-reconcile report from the database for a given year.",
            "inputSchema": {"type":"object","properties":{
                "year":year
            }}
        },
        {
            "name": "upload",
            "description": "Upload invoices missing in Saldeo. Dry run by default: without confirm=true it only returns the upload plan. Uses tri_report, or mail+ksef+saldeo paths, or the cached data for the year.",
            "inputSchema": {"type":"object","properties":{
                "year":year,
                "tri_report":{"type":"string","description":"Path to tri-reconcile report JSON"},
                "mail":{"type":"string","description":"Path to Gmail/PDF records JSON/JSONL"},
                "ksef":{"type":"string","description":"Path to KSeF records JSON/JSONL"},
                "saldeo":{"type":"string","description":"Path to Saldeo records JSON/JSONL"},
                "review_score":review_score,
                "confirm":{"type":"boolean","default":false,"description":"Execute the upload to Saldeo. Default false: dry run, returns the plan only"},
                "approve":{"type":"boolean","default":false,"description":"After upload, approve all unmarked KSeF documents in Saldeo (only executed together with confirm=true; otherwise returns the approval plan)"}
            }}
        },
        {
            "name": "repair",
            "description": "Fill missing Saldeo invoice fields from KSeF/Gmail, report Saldeo duplicates, and optionally run LLM on mail PDFs. Dry run unless confirm=true.",
            "inputSchema": {"type":"object","properties":{
                "year":year,
                "review_score":review_score,
                "llm":{"type":"boolean","default":false,"description":"Also enrich incomplete mail PDFs with LLM/OpenRouter"},
                "confirm":{"type":"boolean","default":false,"description":"Write Saldeo overrides (and LLM results) to SQLite/files. Default false: dry run"}
            }}
        },
        {
            "name": "approve",
            "description": "Approve unmarked KSeF documents in Saldeo. Dry run unless confirm=true: returns the pending document id plan.",
            "inputSchema": {"type":"object","properties":{
                "year":year,
                "review_score":review_score,
                "confirm":{"type":"boolean","default":false,"description":"Execute markAccounting in Saldeo. Default false: dry run"}
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
                "limit":{"type":"integer","minimum":0,"default":20}
            }}
        }
    ])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpSyncArgs {
    pub(crate) year: i32,
    pub(crate) ksef: bool,
    pub(crate) mail: bool,
    pub(crate) amazon_mail: bool,
    pub(crate) saldeo: bool,
    pub(crate) ksef_input: Option<PathBuf>,
    pub(crate) gmail_client_secret: Option<PathBuf>,
    pub(crate) gmail_token_file: Option<PathBuf>,
    pub(crate) productmesh_nip: String,
    pub(crate) store: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpReconcileArgs {
    pub(crate) year: i32,
    pub(crate) mail: Option<PathBuf>,
    pub(crate) ksef: Option<PathBuf>,
    pub(crate) saldeo: Option<PathBuf>,
    pub(crate) review_score: u8,
    pub(crate) store: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpUploadArgs {
    pub(crate) year: i32,
    pub(crate) tri_report: Option<PathBuf>,
    pub(crate) mail: Option<PathBuf>,
    pub(crate) ksef: Option<PathBuf>,
    pub(crate) saldeo: Option<PathBuf>,
    pub(crate) review_score: u8,
    pub(crate) confirm: bool,
    pub(crate) approve: bool,
}

/// Zwalidowane wywołanie narzędzia — parsowanie jest czyste (bez I/O).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpToolCall {
    Sync(McpSyncArgs),
    Reconcile(McpReconcileArgs),
    ReconcileStatus {
        year: i32,
    },
    Upload(McpUploadArgs),
    Repair {
        year: i32,
        review_score: u8,
        llm: bool,
        confirm: bool,
    },
    Approve {
        year: i32,
        review_score: u8,
        confirm: bool,
    },
    DbStats,
    TriRuns {
        limit: usize,
    },
}

pub(crate) fn parse_mcp_tool_call(
    name: &str,
    args: &Value,
) -> std::result::Result<McpToolCall, McpError> {
    Ok(match name {
        "sync" => {
            let mail = json_bool(args, "mail", false)?;
            let amazon_mail = json_bool(args, "amazon_mail", false)?;
            if mail && amazon_mail {
                return Err(McpError::invalid_params(
                    "invalid arguments: `amazon_mail` cannot be combined with `mail`",
                ));
            }
            McpToolCall::Sync(McpSyncArgs {
                year: json_year(args)?,
                ksef: json_bool(args, "ksef", false)?,
                mail,
                amazon_mail,
                saldeo: json_bool(args, "saldeo", false)?,
                ksef_input: json_path_arg(args, "ksef_input")?,
                gmail_client_secret: json_path_arg(args, "gmail_client_secret")?,
                gmail_token_file: json_path_arg(args, "gmail_token_file")?,
                productmesh_nip: json_string_arg(args, "productmesh_nip")?
                    .unwrap_or_else(|| DEFAULT_PRODUCTMESH_NIP.to_string()),
                store: json_bool(args, "store", false)?,
            })
        }
        "reconcile" => McpToolCall::Reconcile(McpReconcileArgs {
            year: json_year(args)?,
            mail: json_path_arg(args, "mail")?,
            ksef: json_path_arg(args, "ksef")?,
            saldeo: json_path_arg(args, "saldeo")?,
            review_score: json_review_score(args)?,
            store: json_bool(args, "store", false)?,
        }),
        "reconcile_status" => McpToolCall::ReconcileStatus {
            year: json_year(args)?,
        },
        "upload" => McpToolCall::Upload(McpUploadArgs {
            year: json_year(args)?,
            tri_report: json_path_arg(args, "tri_report")?,
            mail: json_path_arg(args, "mail")?,
            ksef: json_path_arg(args, "ksef")?,
            saldeo: json_path_arg(args, "saldeo")?,
            review_score: json_review_score(args)?,
            confirm: json_bool(args, "confirm", false)?,
            approve: json_bool(args, "approve", false)?,
        }),
        "repair" => McpToolCall::Repair {
            year: json_year(args)?,
            review_score: json_review_score(args)?,
            llm: json_bool(args, "llm", false)?,
            confirm: json_bool(args, "confirm", false)?,
        },
        "approve" => McpToolCall::Approve {
            year: json_year(args)?,
            review_score: json_review_score(args)?,
            confirm: json_bool(args, "confirm", false)?,
        },
        "db_stats" => McpToolCall::DbStats,
        "tri_runs" => McpToolCall::TriRuns {
            limit: json_usize(args, "limit", 20)?,
        },
        _ => return Err(McpError::invalid_params(format!("Unknown tool: {name}"))),
    })
}

pub(crate) fn execute_mcp_tool(db_path: &Path, call: McpToolCall) -> Result<McpToolOutput> {
    match call {
        McpToolCall::Sync(args) => {
            let auto_store_online_ksef =
                args.ksef_input.is_none() && (args.ksef || (!args.mail && !args.saldeo));
            let conn = if args.store || auto_store_online_ksef {
                Some(open_db(db_path)?)
            } else {
                None
            };
            let summary = run_sync_sources(
                args.year,
                args.ksef,
                args.mail,
                args.amazon_mail,
                args.saldeo,
                args.ksef_input.as_deref(),
                args.gmail_client_secret.as_deref(),
                args.gmail_token_file.as_deref(),
                &args.productmesh_nip,
                Some(db_path),
                conn.as_ref(),
            )?;
            Ok(McpToolOutput::ok(serde_json::to_value(summary)?))
        }
        McpToolCall::Reconcile(args) => {
            let year = args.year;
            let mail = args
                .mail
                .unwrap_or_else(|| default_mail_candidates_path(year));
            sync_reconcile_metadata(year, args.ksef.is_none(), args.saldeo.is_none(), db_path)?;
            let ksef = args.ksef.unwrap_or_else(|| configured_ksef_out_path(year));
            let saldeo = args
                .saldeo
                .unwrap_or_else(|| default_saldeo_records_path(year));
            let report = tri_reconcile(
                load_records(SourceKind::Mail, &mail)?,
                load_records(SourceKind::Ksef, &ksef)?,
                load_saldeo_records(&saldeo, Some(db_path))?,
                args.review_score,
            );
            if args.store {
                let conn = open_db(db_path)?;
                let diff = store_tri_reconcile_report(&conn, year, &report)?;
                return Ok(McpToolOutput::ok(
                    serde_json::json!({"report": report, "temporal_diff": diff}),
                ));
            }
            Ok(McpToolOutput::ok(serde_json::to_value(report)?))
        }
        McpToolCall::ReconcileStatus { year } => {
            let conn = open_db(db_path)?;
            let report = load_last_tri_report(&conn, year)?;
            Ok(McpToolOutput::ok(serde_json::to_value(report)?))
        }
        McpToolCall::Upload(args) => execute_mcp_upload(db_path, args),
        McpToolCall::Repair {
            year,
            review_score,
            llm,
            confirm,
        } => Ok(McpToolOutput::ok(serde_json::to_value(
            saldeo_repair_plan(db_path, year, review_score, llm, confirm)?,
        )?)),
        McpToolCall::Approve {
            year,
            review_score,
            confirm,
        } => Ok(McpToolOutput::ok(serde_json::to_value(
            saldeo_approve_pending_ksef(db_path, year, review_score, confirm)?,
        )?)),
        McpToolCall::DbStats => {
            let conn = open_db(db_path)?;
            Ok(McpToolOutput::ok(serde_json::to_value(db_stats(&conn)?)?))
        }
        McpToolCall::TriRuns { limit } => {
            let conn = open_db(db_path)?;
            Ok(McpToolOutput::ok(list_tri_runs(&conn, limit)?))
        }
    }
}

/// Upload: po rozpoczęciu wysyłki wynik zawsze zawiera plan (co poszło do
/// Saldeo); błędy dalszych kroków (refresh, approve) są dołączane do planu
/// jako `followup_errors` i zgłaszane przez `isError: true`.
fn execute_mcp_upload(db_path: &Path, args: McpUploadArgs) -> Result<McpToolOutput> {
    let year = args.year;
    let mut plan = saldeo_sync_plan(SaldeoSyncPlanConfig {
        year,
        tri_report: args.tri_report.as_deref(),
        mail: args.mail.as_deref(),
        ksef: args.ksef.as_deref(),
        saldeo: args.saldeo.as_deref(),
        db_path: Some(db_path),
        review_score: args.review_score,
        confirm: args.confirm,
        upload_url: None,
    })?;
    if !args.confirm {
        // Dry run: nic nie zostało wysłane, więc błąd może przerwać narzędzie.
        if args.approve {
            plan.ksef_approve = Some(saldeo_approve_pending_ksef(
                db_path,
                year,
                args.review_score,
                false,
            )?);
        }
        return Ok(McpToolOutput::ok(serde_json::to_value(plan)?));
    }

    ensure_saldeo_session()?;
    let mut followup_errors: Vec<(&str, String)> = Vec::new();
    if let Err(err) = saldeo_upload_plan(
        &mut plan,
        &default_saldeo_storage_state_path(),
        DEFAULT_SALDEO_UPLOAD_URL,
        "file",
    ) {
        let attempted = plan
            .items
            .iter()
            .any(|item| item.upload_status == "uploaded" || item.upload_status == "failed");
        if !attempted {
            return Err(err);
        }
        followup_errors.push(("upload", format!("{err:#}")));
    }
    if (plan.summary.uploaded_count > 0 || args.approve)
        && let Err(err) = saldeo_fetch_with_progress(
            year,
            &default_saldeo_storage_state_path(),
            &default_saldeo_out_path(year),
            Some(db_path),
            None,
        )
    {
        eprintln!("  [Saldeo] refresh po uploadzie nie powiódł się: {err}");
        followup_errors.push(("saldeo_refresh", format!("{err:#}")));
    }
    if args.approve {
        match saldeo_approve_pending_ksef(db_path, year, args.review_score, true) {
            Ok(approve_plan) => plan.ksef_approve = Some(approve_plan),
            Err(err) => followup_errors.push(("approve", format!("{err:#}"))),
        }
    }
    Ok(compose_upload_output(
        serde_json::to_value(&plan)?,
        &followup_errors,
    ))
}

/// Składa wynik uploadu: plan + (opcjonalnie) błędy kroków po wysyłce.
pub(crate) fn compose_upload_output(
    mut plan: Value,
    followup_errors: &[(&str, String)],
) -> McpToolOutput {
    if followup_errors.is_empty() {
        return McpToolOutput::ok(plan);
    }
    let details = followup_errors
        .iter()
        .map(|(step, error)| format!("{step}: {error}"))
        .collect::<Vec<_>>()
        .join("; ");
    if let Some(object) = plan.as_object_mut() {
        object.insert(
            "followup_errors".to_string(),
            Value::Array(
                followup_errors
                    .iter()
                    .map(|(step, error)| serde_json::json!({"step": step, "error": error}))
                    .collect(),
            ),
        );
    }
    McpToolOutput {
        value: plan,
        error: Some(format!(
            "Upload to Saldeo was attempted, but a later step failed ({details}). \
             The plan below records which files were uploaded."
        )),
    }
}

/// Wartość argumentu; `null` traktujemy jak brak argumentu.
fn json_arg<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|value| !value.is_null())
}

pub(crate) fn json_string_arg(
    args: &Value,
    key: &str,
) -> std::result::Result<Option<String>, McpError> {
    match json_arg(args, key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(other) => Err(McpError::invalid_argument(key, "a string", other)),
    }
}

pub(crate) fn json_path_arg(
    args: &Value,
    key: &str,
) -> std::result::Result<Option<PathBuf>, McpError> {
    Ok(json_string_arg(args, key)?.map(PathBuf::from))
}

pub(crate) fn json_bool(
    args: &Value,
    key: &str,
    default: bool,
) -> std::result::Result<bool, McpError> {
    match json_arg(args, key) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(other) => Err(McpError::invalid_argument(key, "a boolean", other)),
    }
}

pub(crate) fn json_i32(
    args: &Value,
    key: &str,
    default: i32,
) -> std::result::Result<i32, McpError> {
    match json_arg(args, key) {
        None => Ok(default),
        Some(value) => value
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| McpError::invalid_argument(key, "a 32-bit integer", value)),
    }
}

pub(crate) fn json_usize(
    args: &Value,
    key: &str,
    default: usize,
) -> std::result::Result<usize, McpError> {
    match json_arg(args, key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| McpError::invalid_argument(key, "a non-negative integer", value)),
    }
}

pub(crate) fn json_u8(args: &Value, key: &str, default: u8) -> std::result::Result<u8, McpError> {
    match json_arg(args, key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|v| u8::try_from(v).ok())
            .ok_or_else(|| McpError::invalid_argument(key, "an integer between 0 and 255", value)),
    }
}

/// `year`: domyślnie bieżący rok (wspólny helper z CLI).
pub(crate) fn json_year(args: &Value) -> std::result::Result<i32, McpError> {
    json_i32(args, "year", default_year())
}

/// `review_score`: domyślnie [`DEFAULT_REVIEW_SCORE`], zakres jak w CLI.
pub(crate) fn json_review_score(args: &Value) -> std::result::Result<u8, McpError> {
    let key = "review_score";
    let expected = format!("an integer between {MIN_REVIEW_SCORE} and {MAX_REVIEW_SCORE}");
    let score = json_u8(args, key, DEFAULT_REVIEW_SCORE).map_err(|_| {
        McpError::invalid_argument(key, &expected, json_arg(args, key).unwrap_or(&Value::Null))
    })?;
    if (MIN_REVIEW_SCORE..=MAX_REVIEW_SCORE).contains(&score) {
        Ok(score)
    } else {
        Err(McpError::invalid_argument(
            key,
            &expected,
            &Value::from(score),
        ))
    }
}
