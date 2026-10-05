use super::*;

fn mail_pdf(name: &str) -> InvoiceRecord {
    let mut record = empty_record(SourceKind::Mail);
    record.source_path = Some(format!("data/mail/{name}"));
    record.content_hash = name.to_string();
    record
}

#[test]
fn paid_llm_is_only_for_hard_invoice_candidates() {
    let mut easy = mail_pdf("notes.pdf");
    easy.invoice_number = Some("FV/1/2026".into());
    easy.issue_date = NaiveDate::from_ymd_opt(2026, 1, 2);
    easy.gross_amount_minor = Some(12300);
    easy.currency = None;
    easy.seller_name = Some("Nabywca".into());
    easy.buyer_name = Some("productmesh".into());
    assert!(record_missing_core_fields(&easy));
    assert!(!record_needs_paid_llm(&easy));

    let mut hard = mail_pdf("Invoice-M73SJH5X-0005.pdf");
    hard.buyer_name = Some("productmesh".into());
    assert!(record_needs_paid_llm(&hard));

    let mut weak = mail_pdf("19b971d93dabb196_1_TwojeDokumenty.pdf");
    weak.buyer_name = Some("productmesh".into());
    assert!(!record_needs_paid_llm(&weak));

    let mut heading = mail_pdf("scan.pdf");
    heading.invoice_number = Some("FAKTURY".into());
    heading.buyer_tax_id = Some("5242920020".into());
    assert!(record_needs_paid_llm(&heading));
}

#[test]
fn foreign_vat_numbers_do_not_replace_polish_buyer_nip() {
    let text = "Invoice number E3C69EBD-0001\nEleven Labs Inc.                 Bill to\n169 Madison Ave                 productmesh\nEU OSS VAT EU372062016           PL VAT PL5242920020\nGB VAT GB457922166\nZA VAT 4300323179\nTax ID 3312323741\n";
    let record = parse_text_invoice(SourceKind::Mail, text);
    assert_eq!(record.seller_tax_id, None);
    assert_eq!(record.buyer_tax_id.as_deref(), Some("5242920020"));
    assert_eq!(record.seller_name.as_deref(), Some("Eleven Labs Inc."));
}

#[test]
fn rejects_truncated_json_even_if_it_parses() {
    let response = serde_json::json!({"choices":[{"finish_reason":"length","message":{"content":"{\"seller_name\":\"Firma\"}"}}]});
    assert!(ppmlx_response_json(&response).is_err());
}

#[test]
fn accepts_complete_response_but_not_missing_content() {
    let response = serde_json::json!({"choices":[{"finish_reason":"stop","message":{"content":"{\"seller_name\":\"Firma\"}"}}]});
    assert_eq!(
        ppmlx_response_json(&response).unwrap()["seller_name"],
        "Firma"
    );
    assert!(ppmlx_response_json(&serde_json::json!({"choices":[]})).is_err());
    assert!(
        ppmlx_response_json(&serde_json::json!({"choices":[{"finish_reason":"stop"}]})).is_err()
    );
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "lab-llm-ledger-{}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn incomplete_pdf(dir: &TempDir, name: &str) -> InvoiceRecord {
    let path = dir.0.join(name);
    fs::write(&path, name.as_bytes()).unwrap();
    let mut record = empty_record(SourceKind::Mail);
    record.source_path = Some(path.display().to_string());
    record.content_hash = format!("hash-{name}");
    record.buyer_name = Some("productmesh".into());
    assert!(record_missing_core_fields(&record));
    record
}

fn queue(records: &[InvoiceRecord], request: LlmRequest, model: &str) -> Vec<usize> {
    let mut ledgers = LlmAttemptLedgers::default();
    llm_queue(
        records,
        &HashSet::new(),
        false,
        request,
        model,
        &mut ledgers,
    )
    .unwrap()
}

fn run_once(records: &mut [InvoiceRecord], result: fn() -> Result<bool>) -> usize {
    let mut ledgers = LlmAttemptLedgers::default();
    let queued = llm_queue(
        records,
        &HashSet::new(),
        false,
        LlmRequest::Automatic,
        "model-a",
        &mut ledgers,
    )
    .unwrap();
    let mut calls = 0;
    run_llm_queue(
        records,
        &queued,
        None,
        false,
        "model-a",
        &mut ledgers,
        |_, _| {
            calls += 1;
            result()
        },
        |_, _| Ok(()),
    )
    .unwrap();
    calls
}

#[test]
fn ledger_blocks_resend_after_success_and_after_rejection() {
    let outcomes: [fn() -> Result<bool>; 3] = [
        || Ok(true),
        || Ok(false),
        || {
            Err(anyhow!(
                "LLM: kwoty są niespójne; netto + VAT musi być równe brutto"
            ))
        },
    ];
    for result in outcomes {
        let dir = TempDir::new();
        let mut records = vec![incomplete_pdf(&dir, "a.pdf")];
        assert_eq!(run_once(&mut records, result), 1);
        let ledger = dir.0.join(LLM_ATTEMPTS_FILE);
        assert!(ledger.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&ledger).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Still incomplete, but the automatic sync does not pay for it again.
        let mut fresh = vec![incomplete_pdf(&dir, "a.pdf")];
        assert_eq!(run_once(&mut fresh, || panic!("resent")), 0);
        // Explicit requests and another model still send.
        assert_eq!(queue(&fresh, LlmRequest::Explicit, "model-a"), vec![0]);
        assert_eq!(queue(&fresh, LlmRequest::Forced, "model-a"), vec![0]);
        assert_eq!(queue(&fresh, LlmRequest::Automatic, "model-b"), vec![0]);
    }
}

#[test]
fn ledger_allows_resend_after_transient_failure() {
    let transient: [fn() -> Result<bool>; 2] = [
        || {
            Err(transient_llm_error(
                "OpenRouter HTTP 503 Service Unavailable",
            ))
        },
        || {
            Err(openrouter_response_json(
                &serde_json::json!({"error":{"code":429,"message":"rate limited"}}),
            )
            .unwrap_err())
        },
    ];
    for result in transient {
        let dir = TempDir::new();
        let mut records = vec![incomplete_pdf(&dir, "a.pdf")];
        assert_eq!(run_once(&mut records, result), 1);
        let text = fs::read_to_string(dir.0.join(LLM_ATTEMPTS_FILE)).unwrap();
        assert!(text.contains("\"outcome\":\"transient\""), "{text}");
        assert_eq!(run_once(&mut records, || Ok(true)), 1);
        assert_eq!(run_once(&mut records, || panic!("resent")), 0);
    }
}

#[test]
fn ledger_entries_are_keyed_by_hash_model_and_version() {
    let dir = TempDir::new();
    let mut records = vec![incomplete_pdf(&dir, "a.pdf"), incomplete_pdf(&dir, "b.pdf")];
    let mut ledgers = LlmAttemptLedgers::default();
    run_llm_queue(
        &mut records,
        &[0],
        None,
        false,
        "model-a",
        &mut ledgers,
        |_, _| Ok(true),
        |_, _| Ok(()),
    )
    .unwrap();
    assert_eq!(queue(&records, LlmRequest::Automatic, "model-a"), vec![1]);
    let line = fs::read_to_string(dir.0.join(LLM_ATTEMPTS_FILE)).unwrap();
    let attempt: LlmAttempt = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(attempt.content_hash, "hash-a.pdf");
    assert_eq!(attempt.model, "model-a");
    assert_eq!(attempt.version, LLM_EXTRACTION_VERSION);
    assert_eq!(attempt.outcome, LlmAttemptOutcome::Applied);
}

#[test]
fn openrouter_errors_are_classified() {
    let transient =
        |value: Value| llm_error_is_transient(&openrouter_response_json(&value).unwrap_err());
    assert!(transient(
        serde_json::json!({"error":{"code":502,"message":"upstream"}})
    ));
    assert!(transient(
        serde_json::json!({"error":{"code":"503","message":"busy"}})
    ));
    assert!(!transient(
        serde_json::json!({"error":{"code":402,"message":"insufficient credits"}})
    ));
    assert!(!transient(
        serde_json::json!({"choices":[{"finish_reason":"length","message":{"content":"{}"}}]})
    ));
    assert!(!llm_error_is_transient(&anyhow!(
        "LLM: wymagany obiekt JSON"
    )));
}

fn llm_payload() -> Value {
    serde_json::json!({
        "invoice_number":"FV/1/2026", "issue_date":"2026-03-27",
        "sale_date":"2026-03-26", "due_date":null, "gross_amount":"22.83",
        "net_amount":"18.56", "vat_amount":"4.27", "currency":"PLN",
        "seller_tax_id":"6762531182", "buyer_tax_id":"5242920020",
        "seller_name":"Elocity sp. z o.o.", "buyer_name":"Productmesh"
    })
}

fn parsed_record() -> InvoiceRecord {
    let mut record = empty_record(SourceKind::Mail);
    record.invoice_number = Some("FV/1/2026".into());
    record.issue_date = NaiveDate::from_ymd_opt(2026, 3, 28);
    record.currency = Some("PLN".into());
    record.seller_name = Some("Elocity".into());
    record.buyer_tax_id = Some("1234567890".into());
    record
}

#[test]
fn consistent_llm_amounts_replace_inconsistent_ones() {
    let mut record = parsed_record();
    record.net_amount_minor = Some(956700);
    record.vat_amount_minor = Some(427);
    record.gross_amount_minor = Some(2283);
    assert!(record_missing_core_fields(&record));
    assert!(invoice_validation::apply_validated_invoice_json(&mut record, &llm_payload()).unwrap());
    assert_eq!(record.net_amount_minor, Some(1856));
    assert_eq!(record.vat_amount_minor, Some(427));
    assert_eq!(record.gross_amount_minor, Some(2283));
    assert!(
        record
            .warnings
            .iter()
            .any(|w| w.contains("zastąpiono niespójne kwoty") && w.contains("9567.00"))
    );
    assert!(!record_missing_core_fields(&record));

    // Partial or inconsistent LLM amounts cannot repair them; nothing changes.
    for (key, value) in [("net_amount", Value::Null), ("net_amount", "18.00".into())] {
        let mut record = parsed_record();
        record.net_amount_minor = Some(956700);
        record.gross_amount_minor = Some(2283);
        let before = serde_json::to_string(&record).unwrap();
        let mut payload = llm_payload();
        payload[key] = value;
        assert!(invoice_validation::apply_validated_invoice_json(&mut record, &payload).is_err());
        assert_eq!(serde_json::to_string(&record).unwrap(), before);
    }
}

#[test]
fn consistent_amounts_and_other_fields_stay_fill_only() {
    let mut record = parsed_record();
    record.net_amount_minor = Some(1000);
    record.vat_amount_minor = Some(230);
    record.gross_amount_minor = Some(1230);
    let mut payload = llm_payload();
    payload["invoice_number"] = "FV/OTHER/2026".into();
    assert!(invoice_validation::apply_validated_invoice_json(&mut record, &payload).unwrap());
    assert_eq!(record.gross_amount_minor, Some(1230));
    assert_eq!(record.net_amount_minor, Some(1000));
    assert_eq!(record.invoice_number.as_deref(), Some("FV/1/2026"));
    assert_eq!(record.issue_date, NaiveDate::from_ymd_opt(2026, 3, 28));
    assert_eq!(record.buyer_tax_id.as_deref(), Some("1234567890"));
    assert_eq!(record.seller_name.as_deref(), Some("Elocity"));
    // Gaps are still filled.
    assert_eq!(record.seller_tax_id.as_deref(), Some("6762531182"));
    assert_eq!(record.sale_date, NaiveDate::from_ymd_opt(2026, 3, 26));
    assert!(record.warnings.is_empty());
}

#[test]
fn filename_guessed_number_is_replaced_by_llm_number() {
    let mut record = parsed_record();
    record.invoice_number = Some("scan_0001".into());
    record
        .warnings
        .push("numer faktury odczytany z nazwy pliku".into());
    record.gross_amount_minor = Some(2283);
    assert!(invoice_validation::apply_validated_invoice_json(&mut record, &llm_payload()).unwrap());
    assert_eq!(record.invoice_number.as_deref(), Some("FV/1/2026"));
    assert!(!record_number_guessed_from_filename(&record));
    assert!(
        record
            .warnings
            .iter()
            .any(|w| w.contains("zastąpiono numer faktury") && w.contains("scan_0001"))
    );
    // A null number from the LLM keeps the guess and its warning.
    let mut record = parsed_record();
    record.invoice_number = Some("scan_0001".into());
    record
        .warnings
        .push("numer faktury odczytany z nazwy pliku".into());
    record.gross_amount_minor = Some(2283);
    let mut payload = llm_payload();
    payload["invoice_number"] = Value::Null;
    invoice_validation::apply_validated_invoice_json(&mut record, &payload).unwrap();
    assert_eq!(record.invoice_number.as_deref(), Some("scan_0001"));
    assert!(record_number_guessed_from_filename(&record));
}
