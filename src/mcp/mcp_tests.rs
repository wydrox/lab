use super::*;
use crate::cli::{Cli, Commands};
use std::sync::atomic::{AtomicUsize, Ordering};

// Żaden test nie wywołuje narzędzia, które sięga do sieci lub Saldeo:
// tools/call jest testowane tylko na błędach walidacji (przed wykonaniem)
// i na `reconcile_status` z pustą bazą w katalogu tymczasowym.

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "lab-mcp-{}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Ścieżka bazy, której testy nie powinny nigdy otworzyć.
fn unused_db() -> PathBuf {
    std::env::temp_dir().join("lab-mcp-tests-must-not-exist.sqlite")
}

fn serve(input: &[u8]) -> String {
    let mut reader = io::Cursor::new(input.to_vec());
    let mut out = Vec::new();
    serve_mcp(&unused_db(), &mut reader, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn serve_lines(input: &str) -> Vec<Value> {
    let out = serve(input.as_bytes());
    assert!(out.is_empty() || out.ends_with('\n'), "output: {out:?}");
    out.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn framed(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

fn request(db: &Path, body: Value) -> Value {
    handle_mcp_message(db, &body.to_string()).expect("request must get a response")
}

fn call_tool(db: &Path, name: &str, arguments: Value) -> Value {
    request(
        db,
        serde_json::json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}}),
    )
}

fn error_code(response: &Value) -> i64 {
    response["error"]["code"].as_i64().unwrap_or_else(|| {
        panic!("expected JSON-RPC error, got {response}");
    })
}

// --- framing ---

#[test]
fn newline_request_gets_newline_response() {
    let out = serve(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
    assert!(!out.starts_with("Content-Length"), "output: {out:?}");
    assert!(out.ends_with('\n'));
    assert_eq!(out.matches('\n').count(), 1, "one message per line");
    let response: Value = serde_json::from_str(out.trim_end()).unwrap();
    assert_eq!(
        response,
        serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}})
    );
}

#[test]
fn newline_response_has_no_embedded_newlines() {
    // tools/list zawiera długie opisy; kompaktowy JSON nie może mieć '\n'.
    let responses = serve_lines("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n");
    assert_eq!(responses.len(), 1);
    assert!(responses[0]["result"]["tools"].as_array().unwrap().len() >= 8);
}

#[test]
fn content_length_request_gets_content_length_response() {
    let body = r#"{"jsonrpc":"2.0","id":"a","method":"ping"}"#;
    let out = serve(framed(body).as_bytes());
    let (header, payload) = out.split_once("\r\n\r\n").expect("header block");
    let len: usize = header
        .strip_prefix("Content-Length: ")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(len, payload.len());
    let response: Value = serde_json::from_str(payload).unwrap();
    assert_eq!(response["id"], "a");
    assert_eq!(response["result"], serde_json::json!({}));
}

#[test]
fn content_length_header_with_extra_headers_and_multiple_messages() {
    let first = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let second = r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#;
    let input = format!(
        "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: {}\r\n\r\n{first}{}",
        first.len(),
        framed(second)
    );
    let out = serve(input.as_bytes());
    assert_eq!(
        out.matches("Content-Length: ").count(),
        2,
        "output: {out:?}"
    );
}

#[test]
fn framing_follows_each_request() {
    let input = format!(
        "{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        framed(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#),
        r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
    );
    let out = serve(input.as_bytes());
    // Odpowiedzi czytamy tym samym czytnikiem: każda ma ramkowanie żądania.
    let mut reader = io::Cursor::new(out.into_bytes());
    let mut seen = Vec::new();
    while let Some(message) = read_mcp_message(&mut reader).unwrap() {
        let response: Value = serde_json::from_str(&message.payload.unwrap()).unwrap();
        seen.push((response["id"].as_i64().unwrap(), message.framing));
    }
    assert_eq!(
        seen,
        vec![
            (1, McpFraming::Newline),
            (2, McpFraming::ContentLength),
            (3, McpFraming::Newline),
        ]
    );
}

#[test]
fn blank_lines_between_messages_are_skipped() {
    let input = "\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\n   \n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n\n";
    let responses = serve_lines(input);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[1]["id"], 2);
}

#[test]
fn crlf_terminated_json_line_is_accepted() {
    let responses = serve_lines("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\r\n");
    assert_eq!(responses[0]["result"], serde_json::json!({}));
}

#[test]
fn garbage_line_gets_parse_error_and_server_continues() {
    let responses = serve_lines("not json\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n");
    assert_eq!(responses.len(), 2);
    assert_eq!(error_code(&responses[0]), -32700);
    assert_eq!(responses[0]["id"], Value::Null);
    assert_eq!(responses[1]["id"], 2);
}

#[test]
fn header_block_without_content_length_is_parse_error_not_fatal() {
    let input = "Content-Type: application/json\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n";
    let out = serve(input.as_bytes());
    assert!(out.contains("-32700"), "output: {out:?}");
    assert!(out.contains("\"id\":2"), "server must keep going: {out:?}");
}

#[test]
fn eof_ends_server_cleanly() {
    assert_eq!(serve(b""), "");
    assert_eq!(serve(b"\n\n"), "");
    // Ucięta wiadomość nagłówkowa: koniec strumienia, bez błędu.
    assert_eq!(serve(b"Content-Length: 50\r\n\r\n{\"jsonrpc\""), "");
}

#[test]
fn read_mcp_message_reports_framing() {
    let mut reader =
        io::Cursor::new(format!("\n{}{{\"a\":1}}\n", framed("{\"b\":2}")).into_bytes());
    let first = read_mcp_message(&mut reader).unwrap().unwrap();
    assert_eq!(first.framing, McpFraming::ContentLength);
    assert_eq!(first.payload.as_deref(), Ok("{\"b\":2}"));
    let second = read_mcp_message(&mut reader).unwrap().unwrap();
    assert_eq!(second.framing, McpFraming::Newline);
    assert_eq!(second.payload.as_deref(), Ok("{\"a\":1}"));
    assert!(read_mcp_message(&mut reader).unwrap().is_none());
}

// --- JSON-RPC semantics ---

#[test]
fn notifications_get_no_response() {
    let db = unused_db();
    for body in [
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}),
        // Bez `id` nawet nieznana metoda i tools/call to notyfikacje.
        serde_json::json!({"jsonrpc":"2.0","method":"some/unknown"}),
        serde_json::json!({"jsonrpc":"2.0","method":"ping"}),
        serde_json::json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"upload","arguments":{"confirm":true}}}),
        // Odpowiedź klienta na żądanie serwera.
        serde_json::json!({"jsonrpc":"2.0","id":5,"result":{}}),
    ] {
        assert_eq!(handle_mcp_message(&db, &body.to_string()), None, "{body}");
    }
    let out = serve(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n");
    assert_eq!(out, "");
}

#[test]
fn ping_returns_empty_object() {
    let response = request(
        &unused_db(),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
    );
    assert_eq!(
        response,
        serde_json::json!({"jsonrpc":"2.0","id":3,"result":{}})
    );
}

#[test]
fn unknown_method_returns_method_not_found() {
    let response = request(
        &unused_db(),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"resources/list"}),
    );
    assert_eq!(error_code(&response), -32601);
    assert_eq!(response["id"], 4);
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("resources/list")
    );
}

#[test]
fn request_without_method_is_invalid_request() {
    let response = request(&unused_db(), serde_json::json!({"jsonrpc":"2.0","id":9}));
    assert_eq!(error_code(&response), -32600);
    let response = handle_mcp_message(&unused_db(), "[1,2]").unwrap();
    assert_eq!(error_code(&response), -32600);
}

#[test]
fn unknown_tool_and_bad_arguments_object_are_invalid_params() {
    let db = unused_db();
    let response = call_tool(&db, "no_such_tool", serde_json::json!({}));
    assert_eq!(error_code(&response), -32602);
    let response = request(
        &db,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"approve","arguments":[1]}}),
    );
    assert_eq!(error_code(&response), -32602);
    let response = request(
        &db,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}),
    );
    assert_eq!(error_code(&response), -32602);
}

#[test]
fn tool_execution_failure_is_error_result_not_jsonrpc_error() {
    let dir = TempDir::new();
    let db = dir.join("lab.sqlite");
    // Pusta baza lokalna: brak przebiegu tri-reconcile → błąd wykonania.
    let response = call_tool(&db, "reconcile_status", serde_json::json!({"year": 1999}));
    assert!(response.get("error").is_none(), "{response}");
    let result = &response["result"];
    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("1999"), "{text}");
}

#[test]
fn upload_followup_failure_keeps_plan_and_reports_error() {
    let plan = serde_json::json!({
        "year": 2026,
        "confirm": true,
        "summary": {"uploaded_count": 2},
        "items": [{"upload_status":"uploaded","source_path":"a.pdf"}]
    });
    let ok = compose_upload_output(plan.clone(), &[]);
    assert_eq!(ok.error, None);
    let ok_result = ok.into_tool_result();
    assert!(ok_result.get("isError").is_none());

    let failed = compose_upload_output(
        plan,
        &[
            ("saldeo_refresh", "timeout".to_string()),
            ("approve", "403 Forbidden".to_string()),
        ],
    );
    let result = failed.into_tool_result();
    assert_eq!(result["isError"], true);
    let content = result["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    let message = content[0]["text"].as_str().unwrap();
    assert!(message.contains("approve: 403 Forbidden"), "{message}");
    assert!(message.contains("saldeo_refresh: timeout"), "{message}");
    let plan_back: Value = serde_json::from_str(content[1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(plan_back["summary"]["uploaded_count"], 2);
    assert_eq!(plan_back["items"][0]["source_path"], "a.pdf");
    assert_eq!(plan_back["followup_errors"][1]["step"], "approve");
}

// --- argument validation ---

#[test]
fn json_bool_rejects_wrong_type() {
    let args = serde_json::json!({"confirm":"false","flag":true,"zero":0});
    let err = json_bool(&args, "confirm", false).unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("`confirm`"), "{}", err.message);
    assert!(json_bool(&args, "zero", false).is_err());
    assert_eq!(json_bool(&args, "flag", false), Ok(true));
    assert_eq!(json_bool(&args, "absent", true), Ok(true));
    assert_eq!(
        json_bool(&serde_json::json!({"x":null}), "x", false),
        Ok(false)
    );
}

#[test]
fn json_i32_rejects_wrong_type_and_overflow() {
    let args = serde_json::json!({"year":"2025","float":2025.5,"big":5_000_000_000_i64,"ok":2025});
    let err = json_i32(&args, "year", 2000).unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("`year`"), "{}", err.message);
    assert!(json_i32(&args, "float", 0).is_err());
    assert!(json_i32(&args, "big", 0).is_err());
    assert_eq!(json_i32(&args, "ok", 0), Ok(2025));
    assert_eq!(json_i32(&args, "absent", 7), Ok(7));
}

#[test]
fn json_u8_rejects_wrong_type_and_overflow() {
    let args = serde_json::json!({"score":300,"neg":-1,"text":"70","ok":70});
    let err = json_u8(&args, "score", 1).unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("`score`"), "{}", err.message);
    assert!(json_u8(&args, "neg", 1).is_err());
    assert!(json_u8(&args, "text", 1).is_err());
    assert_eq!(json_u8(&args, "ok", 1), Ok(70));
    assert_eq!(json_u8(&args, "absent", 1), Ok(1));
}

#[test]
fn json_usize_and_string_args_reject_wrong_type() {
    let args = serde_json::json!({"limit":"5","neg":-3,"path":5,"name":"x"});
    assert!(
        json_usize(&args, "limit", 20)
            .unwrap_err()
            .message
            .contains("`limit`")
    );
    assert!(json_usize(&args, "neg", 20).is_err());
    assert_eq!(json_usize(&args, "absent", 20), Ok(20));
    assert!(
        json_path_arg(&args, "path")
            .unwrap_err()
            .message
            .contains("`path`")
    );
    assert_eq!(json_string_arg(&args, "name"), Ok(Some("x".to_string())));
    assert_eq!(json_path_arg(&args, "absent"), Ok(None));
}

#[test]
fn invalid_tool_arguments_return_invalid_params_naming_the_argument() {
    let db = unused_db();
    for (tool, args, key) in [
        ("reconcile", serde_json::json!({"year":"2025"}), "year"),
        ("upload", serde_json::json!({"confirm":"false"}), "confirm"),
        (
            "upload",
            serde_json::json!({"review_score":300}),
            "review_score",
        ),
        ("approve", serde_json::json!({"confirm":1}), "confirm"),
        ("repair", serde_json::json!({"llm":"yes"}), "llm"),
        (
            "sync",
            serde_json::json!({"ksef_input":["a"]}),
            "ksef_input",
        ),
        ("tri_runs", serde_json::json!({"limit":-1}), "limit"),
        (
            "reconcile_status",
            serde_json::json!({"year":2025.0}),
            "year",
        ),
    ] {
        let response = call_tool(&db, tool, args.clone());
        assert_eq!(error_code(&response), -32602, "{tool} {args}");
        let message = response["error"]["message"].as_str().unwrap();
        assert!(message.contains(&format!("`{key}`")), "{tool}: {message}");
    }
    assert!(
        !db.exists(),
        "validation must fail before touching the database"
    );
}

#[test]
fn sync_rejects_mail_with_amazon_mail() {
    let err = parse_mcp_tool_call("sync", &serde_json::json!({"mail":true,"amazon_mail":true}))
        .unwrap_err();
    assert_eq!(err.code, -32602);
}

// --- defaults ---

#[test]
fn mutating_tools_default_confirm_to_false() {
    let empty = serde_json::json!({});
    match parse_mcp_tool_call("upload", &empty).unwrap() {
        McpToolCall::Upload(args) => {
            assert!(!args.confirm);
            assert!(!args.approve);
        }
        other => panic!("unexpected {other:?}"),
    }
    match parse_mcp_tool_call("repair", &empty).unwrap() {
        McpToolCall::Repair { confirm, llm, .. } => {
            assert!(!confirm);
            assert!(!llm);
        }
        other => panic!("unexpected {other:?}"),
    }
    match parse_mcp_tool_call("approve", &empty).unwrap() {
        McpToolCall::Approve { confirm, .. } => assert!(!confirm),
        other => panic!("unexpected {other:?}"),
    }
    match parse_mcp_tool_call("upload", &serde_json::json!({"confirm":true})).unwrap() {
        McpToolCall::Upload(args) => assert!(args.confirm),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn tool_schemas_default_confirm_to_false_and_mention_dry_run() {
    let tools = mcp_tools();
    for tool in tools.as_array().unwrap() {
        if let Some(confirm) = tool["inputSchema"]["properties"].get("confirm") {
            assert_eq!(confirm["default"], false, "{}", tool["name"]);
            let description = confirm["description"].as_str().unwrap();
            assert!(description.contains("Default false"), "{description}");
        }
    }
    let upload = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "upload")
        .unwrap();
    assert!(
        upload["description"]
            .as_str()
            .unwrap()
            .contains("Dry run by default")
    );
}

#[test]
fn tool_schemas_share_review_score_and_do_not_hard_code_year() {
    let tools = mcp_tools();
    let mut review_score_count = 0;
    for tool in tools.as_array().unwrap() {
        let properties = &tool["inputSchema"]["properties"];
        if let Some(score) = properties.get("review_score") {
            review_score_count += 1;
            assert_eq!(score["default"], DEFAULT_REVIEW_SCORE, "{}", tool["name"]);
            assert_eq!(score["minimum"], MIN_REVIEW_SCORE);
            assert_eq!(score["maximum"], MAX_REVIEW_SCORE);
        }
        if let Some(year) = properties.get("year") {
            assert!(year.get("default").is_none(), "{}", tool["name"]);
            assert!(
                year["description"]
                    .as_str()
                    .unwrap()
                    .contains("current year")
            );
        }
    }
    assert_eq!(review_score_count, 4);
}

#[test]
fn shared_review_score_default_and_range() {
    assert_eq!(DEFAULT_REVIEW_SCORE, 70);
    assert_eq!((MIN_REVIEW_SCORE, MAX_REVIEW_SCORE), (50, 100));
    let empty = serde_json::json!({});
    assert_eq!(json_review_score(&empty), Ok(DEFAULT_REVIEW_SCORE));
    for tool in ["reconcile", "upload", "repair", "approve"] {
        let score = match parse_mcp_tool_call(tool, &empty).unwrap() {
            McpToolCall::Reconcile(args) => args.review_score,
            McpToolCall::Upload(args) => args.review_score,
            McpToolCall::Repair { review_score, .. }
            | McpToolCall::Approve { review_score, .. } => review_score,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(score, DEFAULT_REVIEW_SCORE, "{tool}");
    }
    for ok in [50, 70, 100] {
        assert_eq!(
            json_review_score(&serde_json::json!({ "review_score": ok })),
            Ok(ok)
        );
    }
    for bad in [
        serde_json::json!(49),
        serde_json::json!(45),
        serde_json::json!(101),
        serde_json::json!(300),
        serde_json::json!("70"),
    ] {
        let err = json_review_score(&serde_json::json!({ "review_score": bad })).unwrap_err();
        assert_eq!(err.code, -32602);
        assert!(
            err.message.contains("between 50 and 100"),
            "{}",
            err.message
        );
    }
}

fn parse_cli(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("lab-cli").chain(args.iter().copied()))
}

fn cli_year_and_score(args: &[&str]) -> (i32, Option<u8>) {
    match parse_cli(args).unwrap().command.unwrap() {
        Commands::Sync { year, .. } => (year, None),
        Commands::Reconcile {
            year, review_score, ..
        }
        | Commands::Upload {
            year, review_score, ..
        }
        | Commands::Repair {
            year, review_score, ..
        }
        | Commands::Approve {
            year, review_score, ..
        } => (year, Some(review_score)),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn cli_defaults_use_shared_review_score_and_current_year() {
    let current_year = chrono::Local::now().year();
    assert_eq!(default_year(), current_year);
    assert_eq!(cli_year_and_score(&["sync"]), (current_year, None));
    for command in ["reconcile", "upload", "repair", "approve"] {
        assert_eq!(
            cli_year_and_score(&[command]),
            (current_year, Some(DEFAULT_REVIEW_SCORE)),
            "{command}"
        );
    }
    assert_eq!(
        cli_year_and_score(&["upload", "--year", "2025", "--review-score", "50"]),
        (2025, Some(50))
    );
}

#[test]
fn cli_rejects_review_score_outside_range() {
    for command in ["reconcile", "upload", "repair", "approve"] {
        for bad in ["49", "101", "300"] {
            assert!(
                parse_cli(&[command, "--review-score", bad]).is_err(),
                "{command} {bad}"
            );
        }
        assert_eq!(
            cli_year_and_score(&[command, "--review-score", "100"]).1,
            Some(100)
        );
    }
}

#[test]
fn mcp_defaults_to_current_year() {
    let current_year = chrono::Local::now().year();
    let empty = serde_json::json!({});
    assert_eq!(json_year(&empty), Ok(current_year));
    for tool in [
        "sync",
        "reconcile",
        "reconcile_status",
        "upload",
        "repair",
        "approve",
    ] {
        let year = match parse_mcp_tool_call(tool, &empty).unwrap() {
            McpToolCall::Sync(args) => args.year,
            McpToolCall::Reconcile(args) => args.year,
            McpToolCall::Upload(args) => args.year,
            McpToolCall::ReconcileStatus { year }
            | McpToolCall::Repair { year, .. }
            | McpToolCall::Approve { year, .. } => year,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(year, current_year, "{tool}");
    }
    assert_eq!(json_year(&serde_json::json!({"year":2025})), Ok(2025));
}
