use super::*;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

const TODAY: (i32, u32, u32) = (2026, 10, 5);

fn today() -> NaiveDate {
    date(TODAY.0, TODAY.1, TODAY.2)
}

/// A real file, because the upload item is offered only for an existing local PDF.
struct TempPdf(PathBuf);

impl TempPdf {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("lab-tui-year-{name}-{}.pdf", std::process::id()));
        std::fs::write(&path, b"%PDF-1.4\n").unwrap();
        Self(path)
    }

    fn path(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for TempPdf {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn mail(hash: &str, number: &str, issue_date: Option<NaiveDate>) -> InvoiceRecord {
    let mut record = empty_record(SourceKind::Mail);
    record.content_hash = hash.into();
    record.invoice_number = Some(number.into());
    record.issue_date = issue_date;
    record.seller_tax_id = Some("5250001009".into());
    record.seller_name = Some("Dostawca Sp. z o.o.".into());
    record.gross_amount_minor = Some(12300);
    record.currency = Some("PLN".into());
    record
}

fn saldeo(id: i64, number: &str, issue_date: Option<NaiveDate>) -> InvoiceRecord {
    let mut record = empty_record(SourceKind::Saldeo);
    record.content_hash = format!("saldeo:{id}");
    record.invoice_number = Some(number.into());
    record.issue_date = issue_date;
    record.seller_tax_id = Some("5250001009".into());
    record.seller_name = Some("Dostawca Sp. z o.o.".into());
    record.gross_amount_minor = Some(12300);
    record.currency = Some("PLN".into());
    record
}

/// The pure part of `build_invoice_table_rows_with_progress` (no KSeF statuses).
fn year_table(
    mail: Vec<InvoiceRecord>,
    ksef: Vec<InvoiceRecord>,
    saldeo: Vec<InvoiceRecord>,
    year: i32,
) -> Vec<InvoiceTableRow> {
    let report = tri_reconcile(
        filter_invoice_records_for_year_table(mail, year),
        filter_invoice_records_for_year_table(ksef, year),
        filter_invoice_records_for_year_table(saldeo, year),
        70,
    );
    invoice_table_rows_from_report(&report, year, today(), None, None)
}

#[test]
fn undated_gmail_only_record_is_listed_and_uploadable_only_in_the_current_year() {
    let pdf = TempPdf::new("undated");
    let mut record = mail("mail:undated", "FV/77/2026", None);
    record.source_path = Some(pdf.path());

    let current = year_table(vec![record.clone()], Vec::new(), Vec::new(), TODAY.0);
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].sources, "G/-/-");
    assert_eq!(current[0].record.issue_date, None, "shown without a date");
    assert!(current[0].upload_item.is_some());
    assert!(invoice_table_row_accepts(
        &current[0],
        InvoiceTableAction::Upload
    ));

    // Same rule as the CLI plan: an undated file goes to the current period only.
    let past = year_table(vec![record], Vec::new(), Vec::new(), TODAY.0 - 1);
    assert_eq!(
        past.len(),
        1,
        "still listed, so it can be re-read with the LLM"
    );
    assert!(past[0].upload_item.is_none());
    assert!(!invoice_table_row_accepts(
        &past[0],
        InvoiceTableAction::Upload
    ));
    assert!(saldeo_upload_target_period(None, TODAY.0 - 1, today()).is_err());

    // A file dated in a past year is still uploadable in that year's table.
    let mut dated = mail("mail:dated", "FV/78/2025", Some(date(2025, 6, 1)));
    dated.source_path = Some(pdf.path());
    let past = year_table(vec![dated], Vec::new(), Vec::new(), 2025);
    assert!(past[0].upload_item.is_some());
}

#[test]
fn undated_saldeo_document_matches_its_gmail_counterpart() {
    let pdf = TempPdf::new("saldeo-match");
    let mut gmail = mail("mail:matched", "FV/12/2026", Some(date(2026, 3, 10)));
    gmail.source_path = Some(pdf.path());
    let document = saldeo(4012, "FV/12/2026", None);
    // The old year filter dropped this document.
    assert!(!invoice_record_matches_year(&document, 2026));

    let rows = year_table(vec![gmail], Vec::new(), vec![document], 2026);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].sources, "G/-/S");
    assert!(
        rows[0].upload_item.is_none(),
        "present in Saldeo: no upload"
    );
    assert_eq!(rows[0].record.issue_date, Some(date(2026, 3, 10)));
}

#[test]
fn records_dated_in_another_year_stay_excluded() {
    let pdf = TempPdf::new("other-year");
    let mut gmail = mail("mail:2025", "FV/1/2025", Some(date(2025, 12, 30)));
    gmail.source_path = Some(pdf.path());
    let mut ksef = empty_record(SourceKind::Ksef);
    ksef.content_hash = "ksef:2027".into();
    ksef.invoice_number = Some("FV/3/2027".into());
    ksef.issue_date = Some(date(2027, 1, 2));
    let mut sale_only = saldeo(9, "FV/9/2025", None);
    sale_only.sale_date = Some(date(2025, 11, 1));

    let rows = year_table(vec![gmail], vec![ksef], vec![sale_only], 2026);
    assert!(rows.is_empty(), "{rows:#?}");

    let mut undated = empty_record(SourceKind::Ksef);
    undated.content_hash = "ksef:undated".into();
    assert!(invoice_record_in_year_table(&undated, 2026));
    undated.issue_date = Some(date(2026, 1, 1));
    assert!(invoice_record_in_year_table(&undated, 2026));
    assert!(!invoice_record_in_year_table(&undated, 2025));
}

fn listed_row(
    hash: &str,
    issue_date: Option<NaiveDate>,
    sources: &str,
    accounting: Option<bool>,
) -> InvoiceTableRow {
    let mut record = empty_record(SourceKind::Mail);
    record.content_hash = hash.into();
    record.invoice_number = Some(format!("FV/{hash}"));
    record.issue_date = issue_date;
    InvoiceTableRow {
        selected: false,
        sources: sources.into(),
        mail_record: Some(record.clone()),
        ksef_record: None,
        record,
        saldeo_record: None,
        upload_item: None,
        ksef_document_id: None,
        ksef_accounting: accounting,
        action: InvoiceTableAction::None,
        updated: false,
    }
}

#[test]
fn undated_rows_sort_last_by_date_and_follow_the_approved_filter() {
    let rows = vec![
        listed_row("feb", Some(date(2026, 2, 1)), "G/K/S", None),
        listed_row("undated-open", None, "G/-/-", None),
        listed_row("jan", Some(date(2026, 1, 1)), "G/K/S", Some(true)),
        listed_row("undated-approved", None, "G/K/S", Some(true)),
    ];

    assert_eq!(
        invoice_table_visible_indices(&rows, false, 0, false),
        vec![0, 1, 2, 3]
    );
    let ascending = invoice_table_visible_indices(&rows, false, 1, false);
    assert_eq!(&ascending[..2], &[2, 0]);
    assert_eq!(ascending.len(), 4);
    assert!(
        ascending[2..]
            .iter()
            .all(|&idx| rows[idx].record.issue_date.is_none())
    );
    let descending = invoice_table_visible_indices(&rows, false, 1, true);
    assert_eq!(&descending[..2], &[0, 2]);
    assert!(
        descending[2..]
            .iter()
            .all(|&idx| rows[idx].record.issue_date.is_none())
    );

    // `f` hides approved rows present everywhere, dated or not; undated open rows stay.
    assert_eq!(
        invoice_table_visible_indices(&rows, true, 0, false),
        vec![0, 1]
    );
    assert_eq!(
        invoice_table_visible_indices(&rows, true, 1, false),
        vec![0, 1]
    );
}

fn gks_parts() -> TriRow {
    let mut gmail = mail("mail:gks", "FV/OCR/1", Some(date(2026, 5, 1)));
    gmail.gross_amount_minor = Some(9900);
    let mut ksef = empty_record(SourceKind::Ksef);
    ksef.content_hash = "ksef:gks".into();
    ksef.invoice_number = Some("FV/1/2026".into());
    ksef.issue_date = Some(date(2026, 5, 1));
    ksef.seller_tax_id = Some("5250001009".into());
    ksef.seller_name = Some("Dostawca KSeF".into());
    ksef.gross_amount_minor = Some(12300);
    ksef.currency = Some("PLN".into());
    ksef.ksef_reference = Some("5250001009-20260501-ABC".into());
    let mut document = saldeo(55, "FV/1/2026", Some(date(2026, 5, 1)));
    document.ksef_reference = ksef.ksef_reference.clone();
    TriRow {
        status: "in_all_three".into(),
        mail_score_to_ksef: Some(80),
        mail_score_to_saldeo: Some(80),
        ksef_score_to_saldeo: Some(100),
        mail: Some(gmail),
        ksef: Some(ksef),
        saldeo: Some(document),
    }
}

fn llm_result(base: &InvoiceRecord, number: &str, gross: i64) -> InvoiceRecord {
    let mut record = base.clone();
    record.invoice_number = Some(number.into());
    record.gross_amount_minor = Some(gross);
    record.seller_name = Some("LLM Seller".into());
    record
}

#[test]
fn llm_update_on_a_gks_row_changes_only_the_gmail_part() {
    let parts = gks_parts();
    let mut rows = vec![invoice_table_row_from_reconcile_row(&parts, None).unwrap()];
    assert_eq!(rows[0].record.invoice_number.as_deref(), Some("FV/1/2026"));
    assert_eq!(rows[0].record.gross_amount_minor, Some(12300));
    let key = invoice_table_row_key(&rows[0]);

    let llm = llm_result(parts.mail.as_ref().unwrap(), "FV/LLM/1", 5000);
    assert!(apply_invoice_record_update_to_rows(
        &mut rows,
        &llm,
        2026,
        today()
    ));

    let row = &rows[0];
    assert!(row.updated);
    assert_eq!(row.record.invoice_number.as_deref(), Some("FV/1/2026"));
    assert_eq!(row.record.gross_amount_minor, Some(12300));
    assert_eq!(row.record.seller_name.as_deref(), Some("Dostawca KSeF"));
    let gmail = row.mail_record.as_ref().unwrap();
    assert_eq!(gmail.invoice_number.as_deref(), Some("FV/LLM/1"));
    assert_eq!(gmail.gross_amount_minor, Some(5000));
    assert_eq!(invoice_table_row_key(row), key, "row identity is unchanged");
    let mut expected = parts.clone();
    expected.mail = Some(llm);
    assert_eq!(
        serde_json::to_value(&row.record).unwrap(),
        serde_json::to_value(tri_row_display_record(&expected).unwrap()).unwrap(),
        "display is the rebuild's merge"
    );
}

#[test]
fn llm_update_on_a_gmail_only_row_shows_the_new_values() {
    let pdf = TempPdf::new("gmail-only");
    let mut gmail = mail("mail:only", "FV/OCR/2", None);
    gmail.gross_amount_minor = None;
    gmail.source_path = Some(pdf.path());
    let mut rows = year_table(vec![gmail.clone()], Vec::new(), Vec::new(), TODAY.0);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].upload_item.is_some());
    assert_eq!(rows[0].record.gross_amount_minor, None);

    let mut llm = llm_result(&gmail, "FV/2/2026", 4567);
    llm.issue_date = Some(date(2026, 4, 2));
    assert!(apply_invoice_record_update_to_rows(
        &mut rows,
        &llm,
        TODAY.0,
        today()
    ));
    let row = &rows[0];
    assert!(row.updated);
    assert_eq!(row.record.invoice_number.as_deref(), Some("FV/2/2026"));
    assert_eq!(row.record.gross_amount_minor, Some(4567));
    assert_eq!(row.record.issue_date, Some(date(2026, 4, 2)));
    assert_eq!(row.record.seller_name.as_deref(), Some("LLM Seller"));
    // The upload item carries the new date, so it goes to the right Saldeo period.
    let item = row.upload_item.as_ref().unwrap();
    assert_eq!(item.issue_date, Some(date(2026, 4, 2)));
    assert_eq!(item.invoice_number.as_deref(), Some("FV/2/2026"));

    // A date outside the table's year withdraws the upload, as in the CLI plan.
    rows[0].action = InvoiceTableAction::Upload;
    let mut moved = llm.clone();
    moved.issue_date = Some(date(2025, 12, 30));
    assert!(apply_invoice_record_update_to_rows(
        &mut rows,
        &moved,
        TODAY.0,
        today()
    ));
    assert!(rows[0].upload_item.is_none());
    assert_eq!(rows[0].action, InvoiceTableAction::None);
}

#[test]
fn llm_selection_uses_the_gmail_part_hash() {
    let mut parts = gks_parts();
    // A corrected Saldeo record becomes the display record, so its hash is not the PDF's.
    parts.saldeo.as_mut().unwrap().content_hash = "saldeo:55".into();
    let mut row = invoice_table_row_from_reconcile_row(&parts, None).unwrap();
    row.record.content_hash = "saldeo:55".into();
    row.selected = true;
    let mut ksef_only = listed_row("k", None, "-/K/-", None);
    ksef_only.mail_record = None;
    ksef_only.selected = true;
    assert_eq!(
        invoice_table_llm_selected_hashes(&[row, ksef_only]),
        vec!["mail:gks".to_string()]
    );
}

fn llm_pending(live: &[&str]) -> PendingAction {
    let (_result_tx, receiver) = std::sync::mpsc::channel();
    let (_record_tx, record_updates) = std::sync::mpsc::channel();
    PendingAction {
        receiver,
        description: "LLM".into(),
        new_year: Some(2026),
        new_review_score: None,
        progress: Arc::new(Mutex::new(String::new())),
        record_updates: Some(record_updates),
        live_updated_mail_hashes: live.iter().map(|hash| hash.to_string()).collect(),
    }
}

#[test]
fn rebuild_after_llm_keeps_the_marker_and_the_merged_display() {
    let parts = gks_parts();
    let other = listed_row("other", Some(date(2026, 2, 1)), "G/-/-", None);
    let mut rows = vec![
        invoice_table_row_from_reconcile_row(&parts, None).unwrap(),
        other.clone(),
    ];
    // Left over from an earlier refresh; not touched by this LLM run.
    rows[1].updated = true;

    let llm = llm_result(parts.mail.as_ref().unwrap(), "FV/LLM/1", 5000);
    assert!(apply_invoice_record_update_to_rows(
        &mut rows,
        &llm,
        2026,
        today()
    ));
    // Simulate the stale display the old code produced, to show it is not carried over.
    rows[0].record.invoice_number = Some("FV/LLM/1".into());
    rows[0].record.gross_amount_minor = Some(5000);

    // The rebuild reads the LLM result back from disk.
    let mut rebuilt_parts = parts.clone();
    rebuilt_parts.mail = Some(llm.clone());
    let rebuilt = vec![
        invoice_table_row_from_reconcile_row(&rebuilt_parts, None).unwrap(),
        other,
    ];
    let mut year = 2026;
    let mut score = 70;
    let pending = llm_pending(&["mail:gks"]);
    finish_pending_action(
        &mut rows,
        &pending,
        PendingResult {
            rows: Ok(rebuilt),
            commit: None,
        },
        &mut year,
        &mut score,
    );

    let row = &rows[0];
    assert!(row.updated, "LLM-updated row stays marked");
    assert_eq!(row.record.invoice_number.as_deref(), Some("FV/1/2026"));
    assert_eq!(row.record.gross_amount_minor, Some(12300));
    assert_eq!(
        row.mail_record.as_ref().unwrap().invoice_number.as_deref(),
        Some("FV/LLM/1")
    );
    assert!(!rows[1].updated, "unchanged row not touched by the run");

    // The marker alone is carried, also when the rebuild produced a row with the same
    // signature as before.
    let mut again = vec![invoice_table_row_from_reconcile_row(&rebuilt_parts, None).unwrap()];
    let live = HashSet::from(["mail:gks".to_string()]);
    assert_eq!(
        carry_invoice_table_updated_markers(&mut again, &live, &HashSet::new()),
        1
    );
    assert!(again[0].updated);
    assert_eq!(again[0].record.invoice_number.as_deref(), Some("FV/1/2026"));
}
