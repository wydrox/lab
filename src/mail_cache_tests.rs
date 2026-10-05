use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "lab-mail-cache-{}-{}-{}",
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

fn save(path: &Path, records: &[InvoiceRecord]) {
    write_records(records, OutputFormat::Jsonl, Some(path)).unwrap();
}

fn versioned(mut record: InvoiceRecord) -> InvoiceRecord {
    record.warnings.push(MAIL_PARSER_VERSION.to_string());
    record
}

fn complete_record(hash: &str) -> InvoiceRecord {
    let mut record = empty_record(SourceKind::Mail);
    record.content_hash = hash.to_string();
    record.invoice_number = Some("TEST/2026/1".to_string());
    record.issue_date = NaiveDate::from_ymd_opt(2026, 1, 2);
    record.gross_amount_minor = Some(12300);
    record.currency = Some("PLN".to_string());
    record.seller_name = Some("Example Sp. z o.o.".to_string());
    assert!(!record_missing_core_fields(&record));
    record
}

fn fixture_parse(path: &Path) -> Result<InvoiceRecord> {
    let mut record = complete_record(&hex::encode(Sha256::digest(fs::read(path)?)));
    record.source_path = Some(path.display().to_string());
    Ok(record)
}

#[test]
fn failed_reparse_preserves_original_and_retries_until_success() {
    for old_version in [None, Some("lab-mail-parser:v1"), Some(MAIL_PARSER_VERSION)] {
        for warning in [
            "nie udało się wyciągnąć tekstu PDF: test",
            "PDF bez tekstu; test",
            "OCR niedostępny; test",
            "błąd odczytu: test",
        ] {
            let dir = TempDir::new();
            let pdf = dir.join("attachment.PDF");
            fs::write(&pdf, b"synthetic fixture").unwrap();
            let mut stale = fixture_parse(&pdf).unwrap();
            stale.email_message_id = Some("message-123".into());
            stale.email_subject = Some("Faktura".into());
            stale.email_from = Some("sender@example.test".into());
            stale.warnings = vec![warning.into(), "custom import warning".into()];
            if let Some(version) = old_version {
                stale.warnings.push(version.into());
            }
            let cache = dir.join("records.jsonl");
            save(&cache, &[stale.clone()]);
            let candidates = dir.join("candidates.jsonl");
            save(&candidates, &[stale.clone()]);
            let untouched = fs::read(&candidates).unwrap();
            let before = fs::read(&cache).unwrap();
            for _ in 0..2 {
                let mut calls = 0;
                let (records, count) = sync_mail_records_with_parser(&dir.0, &[], |_| {
                    calls += 1;
                    let mut failed = empty_record(SourceKind::Mail);
                    failed.invoice_number = Some("filename-guess".into());
                    failed.warnings.push(warning.into());
                    Ok(failed)
                })
                .unwrap();
                assert_eq!(calls, 1);
                assert_eq!(count, 0);
                assert_eq!(
                    serde_json::to_value(records).unwrap(),
                    serde_json::to_value(vec![stale.clone()]).unwrap()
                );
                assert_eq!(fs::read(&cache).unwrap(), before);
            }
            let (_, count) =
                sync_mail_records_with_parser(&dir.0, &[], |_| Err(anyhow!("read failed")))
                    .unwrap();
            assert_eq!(count, 0);
            assert_eq!(fs::read(&cache).unwrap(), before);
            let (records, count) =
                sync_mail_records_with_parser(&dir.0, &[], fixture_parse).unwrap();
            assert_eq!(count, 1);
            assert_eq!(records[0].invoice_number, stale.invoice_number);
            assert_eq!(records[0].email_message_id, stale.email_message_id);
            assert_eq!(records[0].email_subject, stale.email_subject);
            assert_eq!(records[0].email_from, stale.email_from);
            assert_eq!(
                records[0].warnings,
                vec!["custom import warning", MAIL_PARSER_VERSION]
            );
            assert_eq!(
                sync_mail_records_with_parser(&dir.0, &[], |_| panic!("no retry after success"))
                    .unwrap()
                    .1,
                0
            );
            assert_eq!(fs::read(&pdf).unwrap(), b"synthetic fixture");
            assert_eq!(fs::read(&candidates).unwrap(), untouched);
        }
    }
}

#[test]
fn incomplete_reparse_keeps_good_fields_and_repairs_placeholders() {
    let dir = TempDir::new();
    let pdf = dir.join("attachment.pdf");
    fs::write(&pdf, b"synthetic fixture").unwrap();
    let mut stale = fixture_parse(&pdf).unwrap();
    stale.seller_name = Some("Sprzedawca".into());
    stale.buyer_name = Some("Existing Buyer".into());
    stale.seller_tax_id = Some("1234567890".into());
    stale.buyer_tax_id = Some("9876543210".into());
    stale.sale_date = stale.issue_date;
    stale.due_date = stale.issue_date;
    stale.net_amount_minor = Some(10000);
    stale.vat_amount_minor = Some(2300);
    stale.ksef_reference = Some("reference".into());
    save(&dir.join("records.jsonl"), &[stale.clone()]);
    let (records, count) = sync_mail_records_with_parser(&dir.0, &[], |_| {
        let mut partial = empty_record(SourceKind::Mail);
        partial.content_hash = stale.content_hash.clone();
        partial.invoice_number = Some("filename-guess".into());
        partial.seller_name = Some("Correct Seller".into());
        partial.buyer_name = Some("Nabywca".into());
        Ok(partial)
    })
    .unwrap();
    assert_eq!(count, 1);
    stale.seller_name = Some("Correct Seller".into());
    stale.warnings.push(MAIL_PARSER_VERSION.into());
    assert_eq!(
        serde_json::to_value(records).unwrap(),
        serde_json::to_value(vec![stale]).unwrap()
    );
}

#[test]
fn current_missing_and_non_pdf_records_are_not_reparsed() {
    let dir = TempDir::new();
    let pdf = dir.join("current.pdf");
    let text = dir.join("old.txt");
    fs::write(&pdf, b"PDF fixture").unwrap();
    fs::write(&text, b"text fixture").unwrap();
    let mut current = versioned(complete_record("current"));
    current.source_path = Some(pdf.display().to_string());
    let mut missing = complete_record("missing");
    missing.source_path = Some(dir.join("missing.pdf").display().to_string());
    let mut non_pdf = complete_record("text");
    non_pdf.source_path = Some(text.display().to_string());
    let no_path = complete_record("no-path");
    let original = vec![current, missing, non_pdf, no_path];
    let cache = dir.join("records.jsonl");
    save(&cache, &original);
    let before = fs::read(&cache).unwrap();
    let (records, count) = sync_mail_records(&dir.0, &[]).unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        serde_json::to_value(records).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    assert_eq!(fs::read(cache).unwrap(), before);
}

#[test]
fn fresh_scan_and_incremental_records_get_current_version_and_deduplicate() {
    let dir = TempDir::new();
    fs::write(dir.join("first.txt"), b"first invoice fixture").unwrap();
    let (records, count) = sync_mail_records_with_parser(&dir.0, &[], fixture_parse).unwrap();
    assert_eq!(count, 1);
    assert!(records[0].warnings.iter().any(|w| w == MAIL_PARSER_VERSION));
    let second = dir.join("second.pdf");
    fs::write(&second, b"second PDF fixture").unwrap();
    let files = vec![
        second.display().to_string(),
        second.display().to_string(),
        dir.join("absent.pdf").display().to_string(),
    ];
    let (records, count) = sync_mail_records_with_parser(&dir.0, &files, fixture_parse).unwrap();
    assert_eq!(count, 1);
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|r| r.warnings.iter().any(|w| w == MAIL_PARSER_VERSION))
    );
    assert_eq!(
        sync_mail_records_with_parser(&dir.0, &files, fixture_parse)
            .unwrap()
            .1,
        0
    );
}

#[test]
fn stale_candidates_cannot_replace_current_parser_results() {
    let dir = TempDir::new();
    let cache = dir.join("candidates.jsonl");
    for marker in [
        None,
        Some("lab-mail-parser:v1"),
        Some("lab-mail-parser:v20"),
    ] {
        let mut stale = complete_record("same-hash");
        if let Some(marker) = marker {
            stale.warnings.push(marker.to_string());
        }
        save(&cache, &[stale]);
        let fresh = versioned(empty_record(SourceKind::Mail));
        let mut candidates = vec![fresh];
        candidates[0].content_hash = "same-hash".to_string();
        let before = serde_json::to_value(&candidates).unwrap();
        let bytes = fs::read(&cache).unwrap();
        assert!(
            apply_cached_mail_candidates(&cache, &mut candidates)
                .unwrap()
                .is_empty()
        );
        assert_eq!(serde_json::to_value(candidates).unwrap(), before);
        assert_eq!(fs::read(&cache).unwrap(), bytes);
    }
}

#[test]
fn compatible_complete_candidates_are_reused_and_skipped() {
    let dir = TempDir::new();
    let cache = dir.join("candidates.jsonl");
    let cached = versioned(complete_record("same-hash"));
    save(&cache, &[cached.clone()]);
    let mut candidate = versioned(empty_record(SourceKind::Mail));
    candidate.content_hash = "same-hash".to_string();
    let mut candidates = vec![candidate];
    let skip = apply_cached_mail_candidates(&cache, &mut candidates).unwrap();
    assert!(skip.contains("same-hash"));
    assert_eq!(
        serde_json::to_value(&candidates[0]).unwrap(),
        serde_json::to_value(cached).unwrap()
    );
}

#[test]
fn incomplete_cache_cannot_degrade_complete_fresh_candidate() {
    let dir = TempDir::new();
    let cache = dir.join("candidates.jsonl");
    for field in 0..6 {
        let mut cached = versioned(complete_record("same-hash"));
        match field {
            0 => cached.invoice_number = None,
            1 => cached.issue_date = None,
            2 => cached.gross_amount_minor = None,
            3 => cached.currency = None,
            4 => cached.seller_name = None,
            _ => cached.seller_name = Some("Sprzedawca".to_string()),
        }
        assert!(record_missing_core_fields(&cached));
        save(&cache, &[cached.clone()]);
        let mut candidates = vec![versioned(complete_record("same-hash"))];
        let before = serde_json::to_value(&candidates).unwrap();
        assert!(
            apply_cached_mail_candidates(&cache, &mut candidates)
                .unwrap()
                .is_empty()
        );
        assert!(!record_missing_core_fields(&candidates[0]));
        assert_eq!(serde_json::to_value(candidates).unwrap(), before);
    }
}

#[test]
fn absent_candidate_cache_leaves_records_unchanged() {
    let dir = TempDir::new();
    let mut candidates = vec![versioned(complete_record("hash"))];
    let before = serde_json::to_value(&candidates).unwrap();
    assert!(
        apply_cached_mail_candidates(&dir.join("candidates.jsonl"), &mut candidates)
            .unwrap()
            .is_empty()
    );
    assert_eq!(serde_json::to_value(candidates).unwrap(), before);
}

#[test]
fn password_pdf_warning_does_not_retry() {
    assert!(!mail_parse_needs_retry(&InvoiceRecord {
        warnings: vec![PDF_PASSWORD_WARNING.into()],
        ..empty_record(SourceKind::Mail)
    }));
    assert!(mail_warning_needs_retry(
        "nie udało się wyciągnąć tekstu PDF: test"
    ));
    assert!(!mail_warning_needs_retry(PDF_PASSWORD_WARNING));
}

#[test]
fn failed_new_parse_is_not_versioned_and_retries() {
    for incremental in [false, true] {
        let dir = TempDir::new();
        let pdf = dir.join("fixture.pdf");
        fs::write(&pdf, b"synthetic fixture").unwrap();
        if incremental {
            save(&dir.join("records.jsonl"), &[]);
        }
        let paths = vec![pdf.display().to_string()];
        let (records, count) = sync_mail_records_with_parser(&dir.0, &paths, |path| {
            let mut record = fixture_parse(path)?;
            record.warnings.push("OCR niedostępny; test".into());
            Ok(record)
        })
        .unwrap();
        assert_eq!(count, 1);
        assert!(!records[0].warnings.iter().any(|w| w == MAIL_PARSER_VERSION));
        let (records, count) = sync_mail_records_with_parser(&dir.0, &[], fixture_parse).unwrap();
        assert_eq!(count, 1);
        assert!(!mail_parse_needs_retry(&records[0]));
        assert!(records[0].warnings.iter().any(|w| w == MAIL_PARSER_VERSION));
    }
}

#[test]
fn failed_candidate_cache_does_not_block_retry_or_replace_fresh_data() {
    let dir = TempDir::new();
    let path = dir.join("candidates.jsonl");
    for warning in [
        "OCR niedostępny; test",
        "nie udało się wyciągnąć tekstu PDF: test",
        "PDF bez tekstu; test",
        "błąd odczytu: test",
    ] {
        let mut cached = versioned(complete_record("hash"));
        cached.warnings.push(warning.into());
        save(&path, &[cached]);
        let mut fresh = empty_record(SourceKind::Mail);
        fresh.content_hash = "hash".into();
        let mut candidates = vec![fresh];
        let before = serde_json::to_value(&candidates).unwrap();
        assert!(
            apply_cached_mail_candidates(&path, &mut candidates)
                .unwrap()
                .is_empty()
        );
        assert_eq!(serde_json::to_value(candidates).unwrap(), before);
    }
}

#[test]
fn conflicting_complete_cache_does_not_replace_fresh_values_or_skip() {
    let dir = TempDir::new();
    let path = dir.join("candidates.jsonl");
    for field in 0..3 {
        let mut cached = versioned(complete_record("hash"));
        match field {
            0 => cached.gross_amount_minor = Some(1),
            1 => cached.invoice_number = Some("OTHER".into()),
            _ => cached.seller_name = Some("Other Seller".into()),
        }
        save(&path, &[cached]);
        let mut candidates = vec![versioned(complete_record("hash"))];
        let before = serde_json::to_value(&candidates).unwrap();
        assert!(
            apply_cached_mail_candidates(&path, &mut candidates)
                .unwrap()
                .is_empty()
        );
        assert_eq!(serde_json::to_value(candidates).unwrap(), before);
    }
}

#[test]
fn compatible_partial_cache_fills_gaps_without_skip_and_preserves_metadata() {
    let dir = TempDir::new();
    let path = dir.join("candidates.jsonl");
    let mut cached = versioned(complete_record("hash"));
    cached.gross_amount_minor = None;
    cached.source_path = Some("old-path.pdf".into());
    cached.email_subject = Some("old subject".into());
    save(&path, &[cached]);
    let mut fresh = versioned(empty_record(SourceKind::Mail));
    fresh.content_hash = "hash".into();
    fresh.source_path = Some("fresh-path.pdf".into());
    fresh.email_subject = Some("fresh subject".into());
    fresh.seller_name = Some("Sprzedawca".into());
    let mut candidates = vec![fresh];
    assert!(
        apply_cached_mail_candidates(&path, &mut candidates)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        candidates[0].seller_name.as_deref(),
        Some("Example Sp. z o.o.")
    );
    assert_eq!(candidates[0].invoice_number.as_deref(), Some("TEST/2026/1"));
    assert_eq!(candidates[0].source_path.as_deref(), Some("fresh-path.pdf"));
    assert_eq!(
        candidates[0].email_subject.as_deref(),
        Some("fresh subject")
    );
    assert!(record_missing_core_fields(&candidates[0]));
}

#[test]
fn cache_missing_fresh_optional_fields_is_not_reused() {
    let dir = TempDir::new();
    let path = dir.join("candidates.jsonl");
    save(&path, &[versioned(complete_record("hash"))]);
    let mut fresh = versioned(complete_record("hash"));
    fresh.net_amount_minor = Some(10000);
    fresh.buyer_tax_id = Some("9876543210".into());
    let mut candidates = vec![fresh];
    let before = serde_json::to_value(&candidates).unwrap();
    assert!(
        apply_cached_mail_candidates(&path, &mut candidates)
            .unwrap()
            .is_empty()
    );
    assert_eq!(serde_json::to_value(candidates).unwrap(), before);
}

#[test]
fn fresh_retry_warning_is_not_hidden_by_complete_cache() {
    let dir = TempDir::new();
    let path = dir.join("candidates.jsonl");
    save(&path, &[versioned(complete_record("hash"))]);
    let mut fresh = versioned(complete_record("hash"));
    fresh.warnings.push("OCR niedostępny; test".into());
    let mut candidates = vec![fresh];
    let before = serde_json::to_value(&candidates).unwrap();
    assert!(
        apply_cached_mail_candidates(&path, &mut candidates)
            .unwrap()
            .is_empty()
    );
    assert_eq!(serde_json::to_value(candidates).unwrap(), before);
}

fn gmail_message(id: &str, attachments: &[&str]) -> Value {
    let parts = attachments
        .iter()
        .enumerate()
        .map(|(idx, name)| {
            serde_json::json!({
                "filename": name,
                "mimeType": "application/pdf",
                "body": {"attachmentId": format!("att-{idx}")}
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "id": id,
        "payload": {
            "mimeType": "multipart/mixed",
            "headers": [{"name": "Subject", "value": "Faktury"}],
            "parts": parts
        }
    })
}

fn pdf_exts() -> HashSet<String> {
    HashSet::from(["pdf".to_string()])
}

fn encoded(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

#[test]
fn interrupted_fetch_is_not_cached_and_next_sync_parses_orphaned_file() {
    let dir = TempDir::new();
    // An earlier sync already wrote records.jsonl.
    save(&dir.join("records.jsonl"), &[]);
    let msg = gmail_message("msg1", &["a.pdf", "b.pdf"]);
    let metadata = dir.join("msg1_message.json");
    let err = gmail_store_message(&dir.0, "msg1", &msg, &pdf_exts(), |id| match id {
        "att-0" => Ok(Some(encoded(b"first invoice"))),
        _ => Err(anyhow!("HTTP 503")),
    })
    .unwrap_err();
    assert!(err.to_string().contains("503"));
    assert!(dir.join("msg1_1_a.pdf").is_file());
    assert!(!dir.join("msg1_2_b.pdf").exists());
    assert!(
        !metadata.exists(),
        "metadata must not mark a partial message"
    );
    assert!(!gmail_message_cached(&dir.0, "msg1", &pdf_exts()));

    // The next sync gets no saved files for the orphan but still parses it.
    let (records, count) = sync_mail_records_with_parser(&dir.0, &[], fixture_parse).unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        records[0].source_path.as_deref(),
        Some(dir.join("msg1_1_a.pdf").display().to_string().as_str())
    );

    // Refetch downloads only the missing attachment, then writes the metadata.
    let mut fetched = Vec::new();
    let saved = gmail_store_message(&dir.0, "msg1", &msg, &pdf_exts(), |id| {
        fetched.push(id.to_string());
        Ok(Some(encoded(b"second invoice")))
    })
    .unwrap();
    assert_eq!(fetched, vec!["att-1"]);
    assert_eq!(saved, vec![dir.join("msg1_2_b.pdf").display().to_string()]);
    assert!(gmail_message_cached(&dir.0, "msg1", &pdf_exts()));
    let (records, count) = sync_mail_records_with_parser(&dir.0, &[], fixture_parse).unwrap();
    assert_eq!(count, 1);
    assert_eq!(records.len(), 2);
    assert_eq!(
        sync_mail_records_with_parser(&dir.0, &[], |_| panic!("all files have records"))
            .unwrap()
            .1,
        0
    );
}

#[test]
fn orphan_parse_error_does_not_abort_sync() {
    let dir = TempDir::new();
    save(&dir.join("records.jsonl"), &[]);
    fs::write(dir.join("bad.pdf"), b"bad").unwrap();
    fs::write(dir.join("good.pdf"), b"good").unwrap();
    let (records, count) = sync_mail_records_with_parser(&dir.0, &[], |path| {
        if path.ends_with("bad.pdf") {
            Err(anyhow!("read failed"))
        } else {
            fixture_parse(path)
        }
    })
    .unwrap();
    assert_eq!(count, 1);
    assert_eq!(records.len(), 1);
    // Same bytes under another name are not parsed again.
    fs::write(dir.join("copy.pdf"), b"good").unwrap();
    let (_, count) = sync_mail_records_with_parser(&dir.0, &[], |path| {
        assert!(path.ends_with("bad.pdf"), "{}", path.display());
        Err(anyhow!("still failing"))
    })
    .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn gmail_metadata_is_written_last_and_files_are_private() {
    let dir = TempDir::new();
    let msg = gmail_message("msg2", &["x.pdf", "y.pdf", "notes.docx"]);
    let metadata = dir.join("msg2_message.json");
    let saved = gmail_store_message(&dir.0, "msg2", &msg, &pdf_exts(), |_| {
        assert!(!metadata.exists(), "metadata written before attachments");
        Ok(Some(encoded(b"pdf")))
    })
    .unwrap();
    assert_eq!(saved.len(), 2);
    assert!(metadata.is_file());
    let names = fs::read_dir(&dir.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<HashSet<_>>();
    assert_eq!(
        names,
        HashSet::from([
            "msg2_1_x.pdf".to_string(),
            "msg2_2_y.pdf".to_string(),
            "msg2_message.json".to_string()
        ]),
        "no temporary files left behind"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dir.join("msg2_1_x.pdf"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn legacy_metadata_with_missing_attachment_is_refetched() {
    let dir = TempDir::new();
    let msg = gmail_message("msg3", &["a.pdf", "b.pdf"]);
    // Old layout: metadata first, then the run died after one attachment.
    fs::write(
        dir.join("msg3_message.json"),
        serde_json::to_vec(&msg).unwrap(),
    )
    .unwrap();
    fs::write(dir.join("msg3_1_a.pdf"), b"first").unwrap();
    assert!(!gmail_message_cached(&dir.0, "msg3", &pdf_exts()));
    fs::write(dir.join("msg3_2_b.pdf"), b"second").unwrap();
    assert!(gmail_message_cached(&dir.0, "msg3", &pdf_exts()));
    fs::write(dir.join("msg3_message.json"), b"{truncated").unwrap();
    assert!(!gmail_message_cached(&dir.0, "msg3", &pdf_exts()));
}

#[test]
fn year_query_covers_january_of_next_year() {
    for query in [default_gmail_query(2025), amazon_gmail_query(2025)] {
        assert!(query.contains("after:2025/01/01"), "{query}");
        assert!(query.contains("before:2026/02/01"), "{query}");
        assert!(!query.contains("before:2026/01/01"), "{query}");
        assert!(query.contains("has:attachment filename:pdf"), "{query}");
    }
}

#[test]
fn other_year_candidates_are_dropped_and_undated_kept() {
    let dated = |hash: &str, date: Option<NaiveDate>| {
        let mut record = complete_record(hash);
        record.issue_date = date;
        record
    };
    let mut candidates = vec![
        dated("dec-prev", NaiveDate::from_ymd_opt(2024, 12, 30)),
        dated("this-year", NaiveDate::from_ymd_opt(2025, 12, 30)),
        dated("next-year", NaiveDate::from_ymd_opt(2026, 1, 3)),
        dated("undated", None),
    ];
    assert_eq!(retain_mail_candidates_for_year(&mut candidates, 2025), 2);
    let kept = candidates
        .iter()
        .map(|record| record.content_hash.as_str())
        .collect::<Vec<_>>();
    assert_eq!(kept, vec!["this-year", "undated"]);
}

#[test]
fn llm_result_is_reused_when_ocr_is_unavailable() {
    let dir = TempDir::new();
    let path = dir.join("candidates.jsonl");
    let ocr_warning = "OCR niedostępny; pozostawiono tekst Poppler: brak modelu";
    // The parse needed a retry, so the record never got the parser version marker.
    let mut cached = complete_record("hash");
    cached.warnings = vec![
        ocr_warning.into(),
        "LLM google/gemini-3.8-flash: zastosowano zweryfikowane uzupełnienie".into(),
    ];
    save(&path, &[cached]);
    let mut fresh = empty_record(SourceKind::Mail);
    fresh.content_hash = "hash".into();
    fresh.warnings.push(ocr_warning.into());
    let mut candidates = vec![fresh];
    let skip = apply_cached_mail_candidates(&path, &mut candidates).unwrap();
    assert!(skip.contains("hash"));
    assert_eq!(candidates[0].invoice_number.as_deref(), Some("TEST/2026/1"));
    assert!(!record_missing_core_fields(&candidates[0]));
}

#[test]
fn llm_corrections_in_cache_replace_inconsistent_amounts_and_filename_number() {
    let dir = TempDir::new();
    let path = dir.join("candidates.jsonl");
    let mut cached = versioned(complete_record("hash"));
    cached.invoice_number = Some("FV/7/2026".into());
    cached.net_amount_minor = Some(10000);
    cached.vat_amount_minor = Some(2300);
    cached
        .warnings
        .push("LLM model: zastosowano zweryfikowane uzupełnienie".into());
    save(&path, &[cached]);
    let mut fresh = versioned(complete_record("hash"));
    fresh.invoice_number = Some("scan_0001".into());
    fresh
        .warnings
        .push("numer faktury odczytany z nazwy pliku".into());
    fresh.net_amount_minor = Some(956700);
    fresh.vat_amount_minor = Some(2300);
    assert!(record_amounts_inconsistent(&fresh));
    let mut candidates = vec![fresh];
    let skip = apply_cached_mail_candidates(&path, &mut candidates).unwrap();
    assert!(skip.contains("hash"));
    let record = &candidates[0];
    assert_eq!(record.invoice_number.as_deref(), Some("FV/7/2026"));
    assert_eq!(record.net_amount_minor, Some(10000));
    assert_eq!(record.gross_amount_minor, Some(12300));
    assert!(!record_amounts_inconsistent(record));
    assert!(!record_number_guessed_from_filename(record));
}
