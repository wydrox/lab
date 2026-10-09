/// Treść funkcji `name` z kodu `tui.rs` (do następnej funkcji na poziomie modułu).
fn fn_body(name: &str) -> &'static str {
    let source = include_str!("../tui.rs");
    let start = source
        .find(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("brak fn {name}"));
    let end = start + source[start..].find("\n}\n").expect("koniec funkcji");
    &source[start..end]
}

#[test]
fn ksef_status_read_uses_retry_and_session_scoping() {
    let body = fn_body("saldeo_fetch_ksef_accounting_statuses");
    assert!(body.contains("saldeo_read_with_retry("), "{body}");
    assert!(body.contains(".authorize("), "{body}");
    assert!(body.contains("saldeo_send_read("), "{body}");
    // Nagłówki wyłącznie przez `authorize`: bez ręcznego Cookie/XSRF.
    assert!(!body.contains("cookie_header"), "{body}");
    assert!(!body.contains("X-SALDEO-XSRF-H-TOKEN"), "{body}");
}

#[test]
fn ksef_mark_is_not_retried() {
    let body = fn_body("saldeo_mark_ksef_documents_unchecked");
    assert!(!body.contains("saldeo_read_with_retry"), "{body}");
}
