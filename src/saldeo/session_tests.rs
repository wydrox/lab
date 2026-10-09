use super::*;
use std::cell::{Cell, RefCell};

const NOW: f64 = 1_800_000_000.0;

fn storage(cookies: Value) -> String {
    serde_json::json!({ "cookies": cookies, "origins": [] }).to_string()
}

fn cookie(name: &str, value: &str, domain: &str, path: &str, secure: bool) -> Value {
    serde_json::json!({
        "name": name,
        "value": value,
        "domain": domain,
        "path": path,
        "expires": -1,
        "httpOnly": true,
        "secure": secure,
        "sameSite": "Lax",
    })
}

fn sample_session() -> SaldeoSession {
    saldeo_session_from_storage_text(&storage(serde_json::json!([
        cookie("JSESSIONID", "sess", "saldeo.brainshare.pl", "/", true),
        cookie(
            "X-SALDEO-XSRF-C-TOKEN",
            "xsrf",
            "saldeo.brainshare.pl",
            "/",
            false
        ),
        cookie("shared", "parent", ".brainshare.pl", "/", false),
        cookie("tracker", "t", ".google.com", "/", true),
        cookie("cdn", "c", "storage.example-cdn.com", "/", true),
    ])))
    .expect("session")
}

fn names(cookies: &[&SaldeoCookie]) -> Vec<String> {
    cookies.iter().map(|c| c.name.clone()).collect()
}

#[test]
fn saldeo_host_gets_only_its_cookies_and_xsrf() {
    let session = sample_session();
    assert_eq!(session.xsrf, "xsrf");
    assert_eq!(
        session.cookie_header,
        "JSESSIONID=sess; X-SALDEO-XSRF-C-TOKEN=xsrf; shared=parent"
    );
    assert_eq!(
        session
            .cookie_header_for(SALDEO_DOCUMENT_SEARCH_URL)
            .as_deref(),
        Some("JSESSIONID=sess; X-SALDEO-XSRF-C-TOKEN=xsrf; shared=parent")
    );
    assert_eq!(session.xsrf_for(SALDEO_DOCUMENT_SEARCH_URL), Some("xsrf"));
}

#[test]
fn foreign_host_gets_no_saldeo_cookies_and_no_xsrf() {
    let session = sample_session();
    let url = "https://files.example.net/doc/123.pdf?sig=abc";
    assert_eq!(session.cookie_header_for(url), None);
    assert_eq!(session.xsrf_for(url), None);
    // Host o podobnej nazwie nie jest subdomeną brainshare.pl.
    assert_eq!(
        session.cookie_header_for("https://evilbrainshare.pl/x"),
        None
    );
    // A storage host gets only the cookie stored for it, never Saldeo's session.
    assert_eq!(
        session
            .cookie_header_for("https://storage.example-cdn.com/f.pdf")
            .as_deref(),
        Some("cdn=c")
    );
    assert_eq!(
        session.xsrf_for("https://storage.example-cdn.com/f.pdf"),
        None
    );
}

#[test]
fn relative_download_url_resolves_to_saldeo_host_and_gets_its_cookies() {
    let session = sample_session();
    for (raw, resolved) in [
        (
            "/rest/client/document/download/123",
            "https://saldeo.brainshare.pl/rest/client/document/download/123",
        ),
        (
            "rest/client/document/download/123?x=1",
            "https://saldeo.brainshare.pl/rest/client/document/download/123?x=1",
        ),
        (
            " https://saldeo.brainshare.pl/f/1.pdf ",
            "https://saldeo.brainshare.pl/f/1.pdf",
        ),
    ] {
        let url = saldeo_download_url(raw).unwrap();
        assert_eq!(url, resolved, "{raw}");
        assert_eq!(
            session.cookie_header_for(&url).as_deref(),
            Some("JSESSIONID=sess; X-SALDEO-XSRF-C-TOKEN=xsrf; shared=parent"),
            "{raw}"
        );
    }
}

#[test]
fn absolute_foreign_download_url_gets_no_cookies_and_non_https_is_rejected() {
    let session = sample_session();
    for raw in [
        "https://files.example.net/doc/123.pdf?sig=abc",
        // Protocol-relative: an absolute URL on another host, not a Saldeo path.
        "//files.example.net/doc/123.pdf",
    ] {
        let url = saldeo_download_url(raw).unwrap();
        assert!(url.starts_with("https://files.example.net/"), "{url}");
        assert_eq!(session.cookie_header_for(&url), None, "{raw}");
    }
    for raw in [
        "http://saldeo.brainshare.pl/rest/x",
        "ftp://saldeo.brainshare.pl/x",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:application/pdf;base64,AAAA",
    ] {
        let err = saldeo_download_url(raw).unwrap_err().to_string();
        assert!(err.contains("https"), "{raw}: {err}");
    }
}

#[test]
fn parent_domain_cookie_matches_subdomain_but_host_only_does_not() {
    let session = sample_session();
    let url = "https://cdn.brainshare.pl/files/1.pdf";
    assert_eq!(
        session.cookie_header_for(url).as_deref(),
        Some("shared=parent")
    );
    // Same host name but not HTTPS-Saldeo: no XSRF even though a cookie matches.
    assert_eq!(session.xsrf_for(url), None);
    assert_eq!(session.xsrf_for("http://saldeo.brainshare.pl/rest/"), None);
}

#[test]
fn path_secure_and_expiry_rules() {
    let mut old = cookie("old", "1", "saldeo.brainshare.pl", "/", false);
    old["expires"] = serde_json::json!(NOW - 10.0);
    let mut fresh = cookie("fresh", "1", "saldeo.brainshare.pl", "/", false);
    fresh["expires"] = serde_json::json!(NOW + 10.0);
    let storage_value = serde_json::json!({ "cookies": [
        cookie("rest", "1", "saldeo.brainshare.pl", "/rest", false),
        cookie("restslash", "1", "saldeo.brainshare.pl", "/rest/", false),
        cookie("other", "1", "saldeo.brainshare.pl", "/other", false),
        cookie("secure", "1", "saldeo.brainshare.pl", "/", true),
        old,
        fresh,
    ] });
    let cookies = saldeo_cookies_from_storage(&storage_value).unwrap();
    assert_eq!(
        names(&saldeo_cookies_for_url(
            &cookies,
            "https://saldeo.brainshare.pl/rest/client/document/list/search",
            NOW
        )),
        vec!["rest", "restslash", "secure", "fresh"]
    );
    assert_eq!(
        names(&saldeo_cookies_for_url(
            &cookies,
            "https://saldeo.brainshare.pl/restore",
            NOW
        )),
        vec!["secure", "fresh"]
    );
    assert_eq!(
        names(&saldeo_cookies_for_url(
            &cookies,
            "http://saldeo.brainshare.pl/rest",
            NOW
        )),
        vec!["rest", "fresh"]
    );
}

#[test]
fn xsrf_cookie_for_another_host_is_not_used() {
    let text = storage(serde_json::json!([
        cookie("JSESSIONID", "sess", "saldeo.brainshare.pl", "/", true),
        cookie(
            "X-SALDEO-XSRF-C-TOKEN",
            "xsrf",
            "other.example.com",
            "/",
            false
        ),
    ]));
    assert!(saldeo_session_from_storage_text(&text).is_err());
}

#[test]
fn session_response_classification() {
    let ok = r#"{"status":"SUCCESS","data":{"resultCollection":[],"totalCount":0}}"#;
    assert!(saldeo_session_response_valid(200, ok));
    // No status field: accepted as long as the list shape is there.
    assert!(saldeo_session_response_valid(
        200,
        r#"{"data":{"resultCollection":[{"documentId":1}]}}"#
    ));
    assert!(!saldeo_session_response_valid(
        200,
        r#"{"status":"NOT_LOGGED_IN","data":{"resultCollection":[]}}"#
    ));
    assert!(!saldeo_session_response_valid(
        200,
        r#"{"status":"ERROR","message":"Session expired"}"#
    ));
    assert!(!saldeo_session_response_valid(
        200,
        "<!DOCTYPE html><html><body><form id=\"login\"></form></body></html>"
    ));
    assert!(!saldeo_session_response_valid(
        200,
        r#"{"status":"SUCCESS"}"#
    ));
    assert!(!saldeo_session_response_valid(200, ""));
    assert!(!saldeo_session_response_valid(401, ok));
}

fn failure(kind: SaldeoReadFailure) -> SaldeoReadError {
    SaldeoReadError::new(kind, anyhow!("{kind:?}"))
}

/// Runs the retry helper over a scripted sequence of attempt results.
fn run_script(
    script: Vec<std::result::Result<u32, SaldeoReadFailure>>,
) -> (
    std::result::Result<u32, SaldeoReadFailure>,
    usize,
    Vec<Duration>,
) {
    let calls = Cell::new(0usize);
    let sleeps = RefCell::new(Vec::new());
    let result = saldeo_read_with_retry(
        "test",
        |d| sleeps.borrow_mut().push(d),
        || {
            let idx = calls.get();
            calls.set(idx + 1);
            script[idx].map_err(failure)
        },
    )
    .map_err(|err| err.kind);
    (result, calls.get(), sleeps.into_inner())
}

#[test]
fn retry_on_503_then_success() {
    let (result, calls, sleeps) = run_script(vec![
        Err(SaldeoReadFailure::Status(503)),
        Err(SaldeoReadFailure::Timeout),
        Ok(7),
    ]);
    assert_eq!(result, Ok(7));
    assert_eq!(calls, 3);
    assert_eq!(sleeps, vec![Duration::from_secs(1), Duration::from_secs(2)]);
}

#[test]
fn retry_stops_after_three_attempts() {
    let (result, calls, sleeps) = run_script(vec![
        Err(SaldeoReadFailure::Status(502)),
        Err(SaldeoReadFailure::Connection),
        Err(SaldeoReadFailure::Status(504)),
        Ok(1),
    ]);
    assert_eq!(result, Err(SaldeoReadFailure::Status(504)));
    assert_eq!(calls, 3);
    assert_eq!(sleeps.len(), 2);
}

#[test]
fn no_retry_on_auth_or_other_errors() {
    for kind in [
        SaldeoReadFailure::Status(401),
        SaldeoReadFailure::Status(403),
        SaldeoReadFailure::Status(500),
        SaldeoReadFailure::Status(404),
        SaldeoReadFailure::Other,
    ] {
        let (result, calls, sleeps) = run_script(vec![Err(kind), Ok(1)]);
        assert_eq!(result, Err(kind));
        assert_eq!(calls, 1, "{kind:?}");
        assert!(sleeps.is_empty());
    }
}

#[test]
fn numberless_document_is_kept_with_warning() {
    let doc = serde_json::json!({
        "documentId": 4242,
        "issueDate": "2026-03-05T00:00:00",
        "grossPrice": { "value": 123.45, "currency": "PLN" },
        "filename": "skan.pdf",
    });
    let record = saldeo_document_to_record(&doc).expect("number-less document kept");
    assert_eq!(record.invoice_number, None);
    assert_eq!(record.content_hash, "saldeo:4242");
    assert_eq!(record.gross_amount_minor, Some(12345));
    assert_eq!(record.issue_date, NaiveDate::from_ymd_opt(2026, 3, 5));
    assert!(
        record
            .warnings
            .iter()
            .any(|w| w == SALDEO_NO_NUMBER_WARNING)
    );

    // Nothing to identify it at all: still dropped.
    assert!(saldeo_document_to_record(&serde_json::json!({"issueDate": "2026-03-05"})).is_none());

    // A numbered document gets no such warning.
    let numbered = saldeo_document_to_record(&serde_json::json!({
        "documentId": 1, "number": "FV/1/2026"
    }))
    .unwrap();
    assert!(numbered.warnings.is_empty());
}

#[test]
fn numberless_documents_are_not_merged_by_dedupe() {
    let docs = (1..=3)
        .map(|id| {
            serde_json::json!({
                "documentId": id,
                "issueDate": "2026-03-05",
                "grossPrice": { "value": 100.0 },
            })
        })
        .collect::<Vec<_>>();
    let records = saldeo_documents_to_records(&docs);
    assert_eq!(records.len(), 3);
    assert!(records.iter().all(|r| reconcile_dedupe_key(r).is_none()));
    assert_eq!(dedupe_reconcile_records(records.clone()).len(), 3);
    assert!(saldeo_duplicate_groups(&records).is_empty());
}
