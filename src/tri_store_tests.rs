use super::*;

struct TempDb {
    root: PathBuf,
    conn: Connection,
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
        let root = std::env::temp_dir().join(format!("lab-tri-store-{tag}-{nonce}"));
        let conn = open_db(&root.join("lab.sqlite")).unwrap();
        Self { root, conn }
    }

    fn count(&self, sql: &str) -> i64 {
        self.conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn run_keys(&self, run_id: i64) -> Vec<String> {
        let mut keys = load_tri_row_hashes(&self.conn, run_id)
            .unwrap()
            .into_keys()
            .collect::<Vec<_>>();
        keys.sort();
        keys
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn mail_row(record: InvoiceRecord) -> TriRow {
    TriRow {
        status: "gmail_only".into(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: None,
        ksef_score_to_saldeo: None,
        mail: Some(record),
        ksef: None,
        saldeo: None,
    }
}

fn mail_record(hash: &str) -> InvoiceRecord {
    let mut record = empty_record(SourceKind::Mail);
    record.content_hash = hash.into();
    record
}

fn report_with(rows: Vec<TriRow>) -> TriReconcileReport {
    TriReconcileReport {
        generated_at: Utc::now(),
        review_score: 70,
        summary: TriSummary {
            mail_count: rows.len(),
            ksef_count: 0,
            saldeo_count: 0,
            in_all_three: 0,
            gmail_ksef_missing_saldeo: 0,
            gmail_saldeo_missing_ksef: 0,
            gmail_only: rows.len(),
            ksef_saldeo_missing_gmail: 0,
            ksef_only: 0,
            saldeo_only: 0,
        },
        rows,
    }
}

/// Rows whose old keys collided: Gmail-only candidates without number/date/amount,
/// the same invoice number/date/amount from two sellers, and one exact duplicate.
fn colliding_rows() -> Vec<TriRow> {
    let mut same_number = Vec::new();
    for (hash, nip) in [
        ("mail:seller-a", "5210000001"),
        ("mail:seller-b", "5210000002"),
    ] {
        let mut record = mail_record(hash);
        record.invoice_number = Some("1/2026".into());
        record.issue_date = NaiveDate::from_ymd_opt(2026, 3, 1);
        record.gross_amount_minor = Some(12300);
        record.currency = Some("PLN".into());
        record.seller_tax_id = Some(nip.into());
        same_number.push(mail_row(record));
    }
    let duplicate = same_number[0].clone();
    let mut rows = vec![
        mail_row(mail_record("mail:empty-1")),
        mail_row(mail_record("mail:empty-2")),
    ];
    rows.extend(same_number);
    rows.push(duplicate);
    rows
}

#[test]
fn colliding_rows_store_with_distinct_keys() {
    let db = TempDb::new("collide");
    let report = report_with(colliding_rows());

    let diff = store_tri_reconcile_report(&db.conn, 2026, &report).unwrap();

    let keys = db.run_keys(diff.run_id);
    assert_eq!(keys.len(), report.rows.len(), "{keys:?}");
    assert_eq!(
        db.count("SELECT COUNT(*) FROM tri_reconcile_rows"),
        report.rows.len() as i64
    );
    assert!(keys.iter().any(|key| key.contains("nip:5210000001")));
    assert!(keys.iter().any(|key| key.contains("nip:5210000002")));
    assert!(keys.iter().any(|key| key.contains("mail:empty-1")));
    assert!(keys.iter().any(|key| key.contains("mail:empty-2")));
}

#[test]
fn row_keys_are_stable_across_stores_and_row_order() {
    let db = TempDb::new("stable");
    let report = report_with(colliding_rows());
    let first = store_tri_reconcile_report(&db.conn, 2026, &report).unwrap();
    let second = store_tri_reconcile_report(&db.conn, 2026, &report).unwrap();

    assert_eq!(db.run_keys(first.run_id), db.run_keys(second.run_id));
    assert_eq!(second.previous_run_id, Some(first.run_id));
    assert_eq!(
        (
            second.added_count,
            second.removed_count,
            second.changed_count
        ),
        (0, 0, 0)
    );

    let mut reversed = colliding_rows();
    reversed.reverse();
    let reversed = report_with(reversed);
    let third = store_tri_reconcile_report(&db.conn, 2026, &reversed).unwrap();
    assert_eq!(db.run_keys(first.run_id), db.run_keys(third.run_id));
    assert_eq!(
        (third.added_count, third.removed_count, third.changed_count),
        (0, 0, 0)
    );
}

#[test]
fn row_key_includes_identity_only_without_invoice_number() {
    let mut numbered = mail_record("mail:numbered");
    numbered.invoice_number = Some("FV/7/2026".into());
    assert_eq!(
        tri_row_key(&mail_row(numbered)),
        "inv:FV/7/2026|date:|gross:|cur:"
    );
    assert!(tri_row_key(&mail_row(mail_record("mail:anon"))).contains("ids:mail:anon//"));
}

#[test]
fn failed_store_leaves_no_partial_run() {
    let db = TempDb::new("rollback");
    let report = report_with(colliding_rows());
    db.conn
        .execute_batch(
            r#"
            CREATE TEMP TRIGGER fail_second_row BEFORE INSERT ON main.tri_reconcile_rows
            WHEN (SELECT COUNT(*) FROM main.tri_reconcile_rows WHERE run_id = NEW.run_id) >= 1
            BEGIN SELECT RAISE(ABORT, 'forced failure'); END;
            "#,
        )
        .unwrap();

    let err = store_tri_reconcile_report(&db.conn, 2026, &report).unwrap_err();
    assert!(format!("{err:#}").contains("forced failure"), "{err:#}");
    assert_eq!(db.count("SELECT COUNT(*) FROM tri_reconcile_runs"), 0);
    assert_eq!(db.count("SELECT COUNT(*) FROM tri_reconcile_rows"), 0);
    assert!(db.conn.is_autocommit());

    db.conn
        .execute_batch("DROP TRIGGER temp.fail_second_row")
        .unwrap();
    let diff = store_tri_reconcile_report(&db.conn, 2026, &report).unwrap();
    assert_eq!(diff.previous_run_id, None);
    assert_eq!(db.count("SELECT COUNT(*) FROM tri_reconcile_runs"), 1);
}

#[test]
fn failed_store_records_leaves_no_partial_batch() {
    let db = TempDb::new("records");
    db.conn
        .execute_batch(
            r#"
            CREATE TEMP TRIGGER fail_second_invoice BEFORE INSERT ON main.invoices
            WHEN (SELECT COUNT(*) FROM main.invoices) >= 1
            BEGIN SELECT RAISE(ABORT, 'forced failure'); END;
            "#,
        )
        .unwrap();
    let records = vec![mail_record("mail:one"), mail_record("mail:two")];

    assert!(store_records(&db.conn, &records).is_err());
    assert_eq!(db.count("SELECT COUNT(*) FROM invoices"), 0);

    db.conn
        .execute_batch("DROP TRIGGER temp.fail_second_invoice")
        .unwrap();
    assert_eq!(store_records(&db.conn, &records).unwrap().len(), 2);
    assert_eq!(db.count("SELECT COUNT(*) FROM invoices"), 2);
}
