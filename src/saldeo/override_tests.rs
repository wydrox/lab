use super::*;

/// Temporary database. The sentinel row keeps the table non-empty, so the legacy
/// `~/.config/lab/saldeo-overrides.json` import never runs against the real HOME.
struct TempDb {
    root: PathBuf,
    path: PathBuf,
}

impl TempDb {
    fn new(tag: &str) -> Self {
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(format!("lab-saldeo-override-{tag}-{nonce}"));
        let path = root.join("lab.sqlite");
        let conn = open_db(&path).unwrap();
        conn.execute(
            "INSERT INTO saldeo_overrides (content_hash, created_at, updated_at) VALUES ('saldeo:sentinel', 'x', 'x')",
            [],
        )
        .unwrap();
        Self { root, path }
    }

    fn overrides(&self) -> HashMap<String, SaldeoRecordOverride> {
        let mut overrides = load_saldeo_record_overrides(Some(&self.path)).unwrap();
        overrides.remove("saldeo:sentinel");
        overrides
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn saldeo_record(hash: &str) -> InvoiceRecord {
    let mut record = empty_record(SourceKind::Saldeo);
    record.content_hash = hash.into();
    record
}

fn report_with(row: TriRow) -> TriReconcileReport {
    TriReconcileReport {
        generated_at: Utc::now(),
        review_score: 70,
        summary: TriSummary {
            mail_count: 0,
            ksef_count: 1,
            saldeo_count: 1,
            in_all_three: 0,
            gmail_ksef_missing_saldeo: 0,
            gmail_saldeo_missing_ksef: 0,
            gmail_only: 0,
            ksef_saldeo_missing_gmail: 1,
            ksef_only: 0,
            saldeo_only: 0,
        },
        rows: vec![row],
    }
}

fn ksef_saldeo_row(ksef: InvoiceRecord, saldeo: InvoiceRecord) -> TriRow {
    TriRow {
        status: "ksef_saldeo_missing_gmail".into(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: None,
        ksef_score_to_saldeo: Some(75),
        mail: None,
        ksef: Some(ksef),
        saldeo: Some(saldeo),
    }
}

fn invoice_number_override(hash: &str) -> SaldeoRecordOverride {
    let mut override_row = SaldeoRecordOverride::empty(hash);
    override_row.invoice_number = Some("FV/1/2026".into());
    override_row.baseline.as_mut().unwrap().invoice_number =
        Some(SaldeoBaselineValue { saldeo: None });
    override_row
}

#[test]
fn field_override_applies_only_while_saldeo_matches_baseline() {
    let override_row = invoice_number_override("saldeo:1");

    let mut unchanged = saldeo_record("saldeo:1");
    unchanged.gross_amount_minor = Some(12300);
    assert!(apply_saldeo_record_override(&mut unchanged, &override_row));
    assert_eq!(unchanged.invoice_number.as_deref(), Some("FV/1/2026"));
    assert_eq!(unchanged.gross_amount_minor, Some(12300));
    assert!(saldeo_record_has_override(&unchanged));
    assert_eq!(
        saldeo_override_applied_fields(&unchanged),
        vec!["invoice_number".to_string()]
    );
    // Already applied: a second pass is a no-op.
    assert!(!apply_saldeo_record_override(&mut unchanged, &override_row));
    assert_eq!(unchanged.invoice_number.as_deref(), Some("FV/1/2026"));

    let mut saldeo_changed = saldeo_record("saldeo:1");
    saldeo_changed.invoice_number = Some("FV/1/2026/KOR".into());
    assert!(!apply_saldeo_record_override(
        &mut saldeo_changed,
        &override_row
    ));
    assert_eq!(
        saldeo_changed.invoice_number.as_deref(),
        Some("FV/1/2026/KOR")
    );
    assert!(!saldeo_record_has_override(&saldeo_changed));
}

#[test]
fn stale_field_override_is_dropped_from_storage() {
    let db = TempDb::new("stale");
    let mut override_row = invoice_number_override("saldeo:2");
    override_row.seller_name = Some("Sprzedawca Sp. z o.o.".into());
    override_row.baseline.as_mut().unwrap().seller_name = Some(SaldeoBaselineValue {
        saldeo: Some("nabywca".into()),
    });
    save_saldeo_record_overrides(&db.path, std::slice::from_ref(&override_row)).unwrap();

    let mut raw = saldeo_record("saldeo:2");
    raw.seller_name = Some("nabywca".into());
    let mut records = vec![raw.clone()];
    assert_eq!(
        apply_saldeo_record_overrides(&mut records, Some(&db.path)).unwrap(),
        1
    );
    assert_eq!(records[0].invoice_number.as_deref(), Some("FV/1/2026"));
    assert_eq!(
        records[0].seller_name.as_deref(),
        Some("Sprzedawca Sp. z o.o.")
    );
    assert_eq!(db.overrides().get("saldeo:2"), Some(&override_row));

    // The accountant fixes the number in Saldeo: Saldeo wins, the seller fix stays.
    let mut fixed_in_saldeo = raw.clone();
    fixed_in_saldeo.invoice_number = Some("FV/9/2026".into());
    let mut records = vec![fixed_in_saldeo];
    apply_saldeo_record_overrides(&mut records, Some(&db.path)).unwrap();
    assert_eq!(records[0].invoice_number.as_deref(), Some("FV/9/2026"));
    assert_eq!(
        records[0].seller_name.as_deref(),
        Some("Sprzedawca Sp. z o.o.")
    );
    let stored = db.overrides().remove("saldeo:2").unwrap();
    assert_eq!(
        stored.baseline.as_ref().unwrap().field_names(),
        vec!["seller_name"]
    );
    assert_eq!(stored.invoice_number, None);

    // Dropped for good, even if Saldeo goes back to the old value.
    let mut records = vec![raw.clone()];
    apply_saldeo_record_overrides(&mut records, Some(&db.path)).unwrap();
    assert_eq!(records[0].invoice_number, None);

    // Once every field is stale the override row disappears.
    let mut renamed = raw;
    renamed.seller_name = Some("Inna Firma S.A.".into());
    let mut records = vec![renamed];
    assert_eq!(
        apply_saldeo_record_overrides(&mut records, Some(&db.path)).unwrap(),
        0
    );
    assert!(!saldeo_record_has_override(&records[0]));
    assert!(db.overrides().is_empty());
}

#[test]
fn repair_stores_only_filled_fields_with_baseline() {
    let mut saldeo = saldeo_record("saldeo:repair");
    saldeo.issue_date = NaiveDate::from_ymd_opt(2026, 1, 2);
    saldeo.currency = Some("PLN".into());
    let mut ksef = empty_record(SourceKind::Ksef);
    ksef.content_hash = "ksef:repair".into();
    ksef.invoice_number = Some("FV/1/2026".into());
    ksef.issue_date = NaiveDate::from_ymd_opt(2026, 1, 3);
    ksef.currency = Some("EUR".into());

    let items =
        repair_saldeo_items_from_report(&report_with(ksef_saldeo_row(ksef, saldeo.clone())));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].changed_fields, vec!["invoice_number".to_string()]);
    let override_row = &items[0].override_row;
    assert_eq!(override_row, &invoice_number_override("saldeo:repair"));

    // Later Saldeo's OCR sets the gross amount: the repair must not clear it.
    let mut later = saldeo;
    later.gross_amount_minor = Some(12300);
    assert!(apply_saldeo_record_override(&mut later, override_row));
    assert_eq!(later.gross_amount_minor, Some(12300));
    assert_eq!(later.invoice_number.as_deref(), Some("FV/1/2026"));
    assert_eq!(later.issue_date, NaiveDate::from_ymd_opt(2026, 1, 2));
}

#[test]
fn repair_never_writes_filename_numbers_or_touches_overridden_fields() {
    let mut ksef = empty_record(SourceKind::Ksef);
    ksef.content_hash = "ksef:file".into();
    ksef.invoice_number = Some("skan_faktury.pdf".into());
    ksef.gross_amount_minor = Some(500);
    let items = repair_saldeo_items_from_report(&report_with(ksef_saldeo_row(
        ksef.clone(),
        saldeo_record("saldeo:file"),
    )));
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].changed_fields,
        vec!["gross_amount_minor".to_string()]
    );
    assert_eq!(items[0].override_row.invoice_number, None);

    // A field the user already corrected (here: cleared gross) is not refilled.
    let mut corrected = saldeo_record("saldeo:file");
    corrected.warnings.push(format!(
        "{SALDEO_OVERRIDE_WARNING_PREFIX}: gross_amount_minor"
    ));
    let items = repair_saldeo_items_from_report(&report_with(ksef_saldeo_row(ksef, corrected)));
    assert!(items.is_empty());
}

#[test]
fn repair_save_merges_with_existing_field_overrides() {
    let db = TempDb::new("merge");
    let mut manual = SaldeoRecordOverride::empty("saldeo:3");
    manual.seller_name = Some("Ręcznie Poprawiony".into());
    manual.baseline.as_mut().unwrap().seller_name = Some(SaldeoBaselineValue { saldeo: None });
    save_saldeo_record_overrides(&db.path, std::slice::from_ref(&manual)).unwrap();
    save_saldeo_record_overrides(&db.path, &[invoice_number_override("saldeo:3")]).unwrap();

    let stored = db.overrides().remove("saldeo:3").unwrap();
    assert_eq!(
        stored.baseline.as_ref().unwrap().field_names(),
        vec!["invoice_number", "seller_name"]
    );
    assert_eq!(stored.seller_name.as_deref(), Some("Ręcznie Poprawiony"));
    assert_eq!(stored.invoice_number.as_deref(), Some("FV/1/2026"));
}

#[test]
fn manual_edit_stores_only_changed_fields() {
    let existing = invoice_number_override("saldeo:4");
    let mut shown = saldeo_record("saldeo:4");
    shown.seller_name = Some("Saldeo Seller".into());
    shown.gross_amount_minor = Some(12300);
    assert!(apply_saldeo_record_override(&mut shown, &existing));

    // User changes the gross amount and leaves everything else as shown.
    let mut requested = SaldeoRecordOverride::empty("saldeo:4");
    requested.baseline = None;
    requested.invoice_number = shown.invoice_number.clone();
    requested.seller_name = shown.seller_name.clone();
    requested.gross_amount_minor = Some(45600);
    let edited = saldeo_override_from_edit(&shown, Some(&existing), &requested);
    let baseline = edited.baseline.as_ref().unwrap();
    assert_eq!(
        baseline.field_names(),
        vec!["invoice_number", "gross_amount_minor"]
    );
    assert_eq!(
        baseline.invoice_number,
        Some(SaldeoBaselineValue { saldeo: None })
    );
    assert_eq!(
        baseline.gross_amount_minor,
        Some(SaldeoBaselineValue {
            saldeo: Some(12300)
        })
    );
    assert_eq!(edited.gross_amount_minor, Some(45600));
    assert_eq!(edited.seller_name, None);

    // Clearing the number back to Saldeo's own (empty) value removes that override.
    requested.invoice_number = None;
    requested.gross_amount_minor = Some(12300);
    let reverted = saldeo_override_from_edit(&shown, Some(&existing), &requested);
    assert!(reverted.baseline.as_ref().unwrap().is_empty());
}

#[test]
fn legacy_snapshot_rows_migrate_with_current_saldeo_as_baseline() {
    let db = TempDb::new("legacy");
    // Row written by an older version: full snapshot, schema without baseline_json.
    let conn = open_db(&db.path).unwrap();
    conn.execute(
        r#"
        INSERT INTO saldeo_overrides (
            content_hash, invoice_number, seller_tax_id, buyer_tax_id, seller_name,
            buyer_name, issue_date, gross_amount_minor, currency, created_at, updated_at
        ) VALUES ('saldeo:legacy', 'FV/legacy/2026', NULL, NULL, 'Legacy Seller', NULL,
                  '2026-05-02', 12345, 'PLN', 'x', 'x')
        "#,
        [],
    )
    .unwrap();
    drop(conn);

    let mut raw = saldeo_record("saldeo:legacy");
    raw.invoice_number = Some("skan.pdf".into());
    raw.seller_name = Some("Legacy Seller".into());
    raw.issue_date = NaiveDate::from_ymd_opt(2026, 5, 2);
    raw.currency = Some("PLN".into());

    // First load: display is the same as with the old full-snapshot semantics.
    let mut records = vec![raw.clone()];
    assert_eq!(
        apply_saldeo_record_overrides(&mut records, Some(&db.path)).unwrap(),
        1
    );
    assert_eq!(records[0].invoice_number.as_deref(), Some("FV/legacy/2026"));
    assert_eq!(records[0].gross_amount_minor, Some(12345));
    assert_eq!(records[0].seller_name.as_deref(), Some("Legacy Seller"));
    assert!(saldeo_record_has_override(&records[0]));

    let stored = db.overrides().remove("saldeo:legacy").unwrap();
    let baseline = stored.baseline.as_ref().expect("migrated to field-level");
    assert_eq!(
        baseline.field_names(),
        vec!["invoice_number", "gross_amount_minor"]
    );
    assert_eq!(
        baseline.invoice_number,
        Some(SaldeoBaselineValue {
            saldeo: Some("skan.pdf".into())
        })
    );
    assert_eq!(
        baseline.gross_amount_minor,
        Some(SaldeoBaselineValue { saldeo: None })
    );

    // Later Saldeo fills the amount itself: Saldeo wins, the number fix stays.
    let mut later = raw;
    later.gross_amount_minor = Some(99900);
    let mut records = vec![later];
    apply_saldeo_record_overrides(&mut records, Some(&db.path)).unwrap();
    assert_eq!(records[0].gross_amount_minor, Some(99900));
    assert_eq!(records[0].invoice_number.as_deref(), Some("FV/legacy/2026"));
}

#[test]
fn legacy_json_overrides_keep_their_format() {
    let json = r#"[{"content_hash":"saldeo:json","invoice_number":"FV/2/2026",
        "seller_tax_id":null,"buyer_tax_id":null,"seller_name":null,"buyer_name":null,
        "issue_date":null,"gross_amount_minor":null,"currency":null}]"#;
    let parsed = serde_json::from_str::<Vec<SaldeoRecordOverride>>(json).unwrap();
    assert!(parsed[0].is_legacy());
    assert!(
        !serde_json::to_string(&parsed[0])
            .unwrap()
            .contains("baseline")
    );

    let mut record = saldeo_record("saldeo:json");
    record.gross_amount_minor = Some(100);
    assert!(apply_saldeo_record_override(&mut record, &parsed[0]));
    assert_eq!(record.invoice_number.as_deref(), Some("FV/2/2026"));
    // In the legacy snapshot gross was empty; an empty snapshot field never blanks
    // a value Saldeo holds.
    assert_eq!(record.gross_amount_minor, Some(100));
}
