use super::*;
use clap::CommandFactory;
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

// Testy hamulców zatwierdzania, statusu tri-reconcile, reguły zapisu sync,
// otwierania bazy tylko do odczytu i wczytywania JSON. Bez sieci i bez Saldeo.

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "lab-cli-policy-{tag}-{}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
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

// --- zatwierdzanie KSeF: limit i require_mail ---

fn approve_row(document_id: i64, with_mail: bool) -> InvoiceTableRow {
    let mut record = empty_record(SourceKind::Ksef);
    record.invoice_number = Some(format!("FV/{document_id}/2026"));
    InvoiceTableRow {
        selected: false,
        sources: if with_mail { "G/K/S" } else { "-/K/S" }.to_string(),
        record,
        mail_record: None,
        ksef_record: None,
        saldeo_record: None,
        upload_item: None,
        ksef_document_id: Some(document_id),
        ksef_accounting: None,
        action: InvoiceTableAction::None,
        updated: false,
    }
}

fn rows(count: i64) -> Vec<InvoiceTableRow> {
    (1..=count).map(|id| approve_row(id, false)).collect()
}

fn policy(max_approve: usize, require_mail: bool) -> ApprovePolicy {
    ApprovePolicy {
        max_approve,
        require_mail,
    }
}

#[test]
fn approve_cap_exceeded_approves_nothing_and_fails_when_confirmed() {
    let called = Cell::new(false);
    let plan = approve_ksef_rows_with(&rows(3), 2026, true, policy(2, false), |_| {
        called.set(true);
        Ok(vec![1, 2, 3])
    })
    .unwrap();
    assert!(!called.get(), "cap exceeded: Saldeo must not be touched");
    assert!(plan.summary.cap_exceeded);
    assert_eq!(plan.summary.pending_count, 3);
    assert_eq!(plan.summary.approved_count, 0);
    assert!(plan.approved_document_ids.is_empty());
    assert_eq!(plan.document_ids, vec![1, 2, 3]);
    assert_eq!(plan.max_approve, 2);
    let message = plan.blocking_error().expect("confirmed run must fail");
    assert!(
        message.contains('3') && message.contains("limit 2"),
        "{message}"
    );
    assert!(message.contains("--max-approve"), "{message}");

    let json = serde_json::to_value(&plan).unwrap();
    assert_eq!(json["summary"]["cap_exceeded"], true);
    assert_eq!(json["max_approve"], 2);
    assert!(json["blocked_reason"].is_string());
}

#[test]
fn approve_cap_exceeded_dry_run_shows_the_block_without_failing() {
    let plan = approve_ksef_rows_with(&rows(3), 2026, false, policy(2, false), |_| {
        panic!("dry run must not mark documents")
    })
    .unwrap();
    assert!(plan.summary.cap_exceeded);
    assert!(plan.blocked_reason.is_some());
    assert_eq!(plan.blocking_error(), None);
    assert_eq!(plan.document_ids, vec![1, 2, 3]);
}

#[test]
fn approve_cap_zero_means_no_limit() {
    let plan = approve_ksef_rows_with(&rows(120), 2026, true, policy(0, false), |ids| {
        Ok(ids.to_vec())
    })
    .unwrap();
    assert!(!plan.summary.cap_exceeded);
    assert_eq!(plan.summary.approved_count, 120);
    assert_eq!(plan.blocking_error(), None);
}

#[test]
fn approve_at_or_under_cap_marks_pending_documents() {
    for count in [2, 1] {
        let marked = Cell::new(0usize);
        let plan = approve_ksef_rows_with(&rows(count), 2026, true, policy(2, false), |ids| {
            marked.set(ids.len());
            // Saldeo może odrzucić część (np. już oznaczone w międzyczasie).
            Ok(ids[..1].to_vec())
        })
        .unwrap();
        assert_eq!(marked.get(), count as usize);
        assert!(!plan.summary.cap_exceeded);
        assert_eq!(plan.summary.pending_count, count as usize);
        assert_eq!(plan.approved_document_ids, vec![1]);
        assert_eq!(plan.summary.approved_count, 1);
        assert_eq!(plan.blocking_error(), None);
    }
}

#[test]
fn approve_default_policy_cap_is_fifty() {
    assert_eq!(ApprovePolicy::default(), policy(50, false));
    let plan = approve_ksef_rows_with(&rows(51), 2026, true, ApprovePolicy::default(), |_| {
        panic!("51 > 50 must block")
    })
    .unwrap();
    assert!(plan.blocking_error().is_some());
}

#[test]
fn approve_require_mail_skips_ksef_only_documents_with_reason() {
    let rows = vec![
        approve_row(1, true),
        approve_row(2, false),
        approve_row(3, true),
    ];
    let plan = approve_ksef_rows_with(&rows, 2026, true, policy(50, true), |ids| {
        assert_eq!(ids, &[1, 3]);
        Ok(ids.to_vec())
    })
    .unwrap();
    assert!(plan.require_mail);
    assert_eq!(plan.document_ids, vec![1, 3]);
    assert_eq!(plan.approved_document_ids, vec![1, 3]);
    assert_eq!(plan.summary.skipped_count, 1);
    let skipped = &plan.skipped[0];
    assert_eq!(skipped.document_id, 2);
    assert_eq!(skipped.invoice_number.as_deref(), Some("FV/2/2026"));
    assert_eq!(skipped.sources, "-/K/S");
    assert!(skipped.reason.contains("Gmail"), "{}", skipped.reason);

    // Bez filtra KSeF-only dokumenty są zatwierdzane normalnie.
    let plan = approve_ksef_rows_with(&rows, 2026, true, policy(50, false), |ids| Ok(ids.to_vec()))
        .unwrap();
    assert_eq!(plan.approved_document_ids, vec![1, 2, 3]);
    assert!(plan.skipped.is_empty());
}

#[test]
fn approve_cap_counts_only_documents_left_after_require_mail() {
    let mut rows = rows(5);
    rows.push(approve_row(6, true));
    let plan =
        approve_ksef_rows_with(&rows, 2026, true, policy(1, true), |ids| Ok(ids.to_vec())).unwrap();
    assert!(!plan.summary.cap_exceeded);
    assert_eq!(plan.approved_document_ids, vec![6]);
    assert_eq!(plan.summary.skipped_count, 5);
}

#[test]
fn approve_require_mail_env_values() {
    for on in ["1", "true", "TRUE", " yes ", "on"] {
        assert!(cli::env_flag_enabled(Some(on)), "{on}");
    }
    for off in ["0", "", "false", "no", "2"] {
        assert!(!cli::env_flag_enabled(Some(off)), "{off}");
    }
    assert!(!cli::env_flag_enabled(None));
}

fn parse_cli(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("lab-cli").chain(args.iter().copied())).unwrap()
}

#[test]
fn cli_approve_and_upload_parse_cap_and_require_mail() {
    match parse_cli(&["approve"]).command.unwrap() {
        Commands::Approve {
            max_approve,
            require_mail,
            ..
        } => assert_eq!((max_approve, require_mail), (50, false)),
        other => panic!("unexpected {other:?}"),
    }
    match parse_cli(&["approve", "--max-approve", "0", "--require-mail"])
        .command
        .unwrap()
    {
        Commands::Approve {
            max_approve,
            require_mail,
            ..
        } => assert_eq!((max_approve, require_mail), (0, true)),
        other => panic!("unexpected {other:?}"),
    }
    match parse_cli(&["upload", "--approve", "--max-approve", "7"])
        .command
        .unwrap()
    {
        Commands::Upload {
            approve,
            max_approve,
            require_mail,
            ..
        } => assert_eq!((approve, max_approve, require_mail), (true, 7, false)),
        other => panic!("unexpected {other:?}"),
    }
    assert!(
        Cli::try_parse_from(["lab-cli", "approve", "--max-approve", "-1"]).is_err(),
        "negative cap must be rejected"
    );
}

// --- reconcile --status: zapisany czas, próg i błędy dekodowania ---

fn stored_report(score: u8) -> TriReconcileReport {
    let mut mail = empty_record(SourceKind::Mail);
    mail.content_hash = "mail:status".into();
    mail.invoice_number = Some("FV/1/2024".into());
    mail.issue_date = NaiveDate::from_ymd_opt(2024, 3, 1);
    let mut report = tri_reconcile(vec![mail], Vec::new(), Vec::new(), score);
    report.generated_at = DateTime::parse_from_rfc3339("2024-03-02T10:15:30.123456Z")
        .unwrap()
        .with_timezone(&Utc);
    report
}

#[test]
fn reconcile_status_returns_stored_timestamp_and_score() {
    let dir = TempDir::new("status");
    let conn = open_db(&dir.join("lab.sqlite")).unwrap();
    let report = stored_report(85);
    store_tri_reconcile_report(&conn, 2024, &report).unwrap();

    let loaded = load_last_tri_report(&conn, 2024).unwrap();
    assert_eq!(loaded.generated_at, report.generated_at);
    assert_eq!(loaded.review_score, 85);
    assert_eq!(loaded.rows.len(), report.rows.len());
    assert_eq!(
        loaded.rows[0].mail.as_ref().unwrap().invoice_number,
        Some("FV/1/2024".into())
    );

    // Najnowszy przebieg wygrywa, z własnym czasem i progiem.
    let mut newer = stored_report(60);
    newer.generated_at += chrono::Duration::days(1);
    store_tri_reconcile_report(&conn, 2024, &newer).unwrap();
    let loaded = load_last_tri_report(&conn, 2024).unwrap();
    assert_eq!(loaded.generated_at, newer.generated_at);
    assert_eq!(loaded.review_score, 60);
}

#[test]
fn reconcile_status_reads_runs_written_by_an_older_schema() {
    // Kolumny generated_at i review_score są w tri_reconcile_runs od jej powstania;
    // przebieg wstawiony bezpośrednio SQL-em (jak starsza wersja) czyta się bez migracji.
    let dir = TempDir::new("status-old");
    let conn = open_db(&dir.join("lab.sqlite")).unwrap();
    let row = &stored_report(70).rows[0];
    conn.execute(
        "INSERT INTO tri_reconcile_runs (generated_at, year, review_score, mail_count, ksef_count,
            saldeo_count, summary_json, report_hash, previous_run_id, added_count, removed_count,
            changed_count)
         VALUES ('2023-11-05T08:00:00+00:00', 2023, 75, 1, 0, 0, '{}', 'h', NULL, 1, 0, 0)",
        [],
    )
    .unwrap();
    let run_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO tri_reconcile_rows (run_id, row_key, row_hash, status, row_json)
         VALUES (?1, 'k', 'h', ?2, ?3)",
        params![run_id, row.status, serde_json::to_string(row).unwrap()],
    )
    .unwrap();
    let loaded = load_last_tri_report(&conn, 2023).unwrap();
    assert_eq!(
        loaded.generated_at.to_rfc3339(),
        "2023-11-05T08:00:00+00:00"
    );
    assert_eq!(loaded.review_score, 75);
    assert_eq!(loaded.rows.len(), 1);
}

#[test]
fn reconcile_status_row_decode_error_is_an_error() {
    let dir = TempDir::new("status-bad");
    let conn = open_db(&dir.join("lab.sqlite")).unwrap();
    let diff = store_tri_reconcile_report(&conn, 2024, &stored_report(70)).unwrap();
    conn.execute(
        "UPDATE tri_reconcile_rows SET row_json = '{\"status\": 1' WHERE run_id = ?1",
        params![diff.run_id],
    )
    .unwrap();
    let err = load_last_tri_report(&conn, 2024).unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("row_json"), "{message}");
    assert!(message.contains(&diff.run_id.to_string()), "{message}");
}

#[test]
fn reconcile_status_bad_stored_timestamp_is_an_error() {
    let dir = TempDir::new("status-ts");
    let conn = open_db(&dir.join("lab.sqlite")).unwrap();
    store_tri_reconcile_report(&conn, 2024, &stored_report(70)).unwrap();
    conn.execute("UPDATE tri_reconcile_runs SET generated_at = 'wczoraj'", [])
        .unwrap();
    let message = format!("{:#}", load_last_tri_report(&conn, 2024).unwrap_err());
    assert!(message.contains("generated_at"), "{message}");
}

// --- reconcile: reguła roku wystawienia dla domyślnego źródła Gmail ---

fn year_mail(hash: &str, number: &str, issue_date: Option<NaiveDate>) -> InvoiceRecord {
    let mut record = empty_record(SourceKind::Mail);
    record.content_hash = hash.into();
    record.invoice_number = Some(number.into());
    record.issue_date = issue_date;
    record
}

/// Domyślny plik kandydatów 2026 z rekordami datowanymi na 2025, 2026, 2027 i bez daty
/// oraz puste jawne pliki KSeF i Saldeo (bez nich reconcile pobiera dane online).
fn reconcile_year_fixture(dir: &TempDir) -> (PathBuf, PathBuf, Vec<InvoiceRecord>) {
    let date = |y, m, d| NaiveDate::from_ymd_opt(y, m, d);
    let mail = vec![
        year_mail("mail:dec", "FV/12/2025", date(2025, 12, 30)),
        year_mail("mail:in-year", "FV/3/2026", date(2026, 3, 1)),
        year_mail("mail:jan", "FV/1/2027", date(2027, 1, 4)),
        year_mail("mail:undated", "FV/X", None),
    ];
    let candidates = default_mail_candidates_path(2026);
    fs::create_dir_all(candidates.parent().unwrap()).unwrap();
    write_records(&mail, OutputFormat::Jsonl, Some(&candidates)).unwrap();
    let ksef = dir.join("ksef.jsonl");
    let saldeo = dir.join("saldeo.jsonl");
    fs::write(&ksef, "").unwrap();
    fs::write(&saldeo, "").unwrap();
    (ksef, saldeo, mail)
}

fn report_mail_hashes(report: &TriReconcileReport) -> Vec<String> {
    let mut hashes = report
        .rows
        .iter()
        .filter_map(|row| row.mail.as_ref())
        .map(|record| record.content_hash.clone())
        .collect::<Vec<_>>();
    hashes.sort();
    hashes
}

#[test]
fn reconcile_default_mail_source_keeps_only_issue_year_and_undated_records() {
    let dir = TempDir::new("reconcile-year");
    let _root = set_test_lab_root(&dir.0);
    let (ksef, saldeo, _) = reconcile_year_fixture(&dir);
    let db = dir.join("lab.sqlite");
    let out = dir.join("report.json");
    handle_reconcile_command(
        &db,
        false,
        None,
        Some(ksef),
        Some(saldeo),
        70,
        Some(out.clone()),
        false,
        None,
        true,
        2026,
    )
    .unwrap();
    let report: TriReconcileReport =
        serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report.summary.mail_count, 2);
    assert_eq!(report.summary.gmail_only, 2);
    assert_eq!(
        report_mail_hashes(&report),
        ["mail:in-year", "mail:undated"]
    );
    // `reconcile --status` czyta przebieg zapisany przez --store (job launchd).
    let stored = load_last_tri_report(&open_existing_db(&db).unwrap(), 2026).unwrap();
    assert_eq!(stored.summary.gmail_only, 2);
    assert_eq!(
        report_mail_hashes(&stored),
        ["mail:in-year", "mail:undated"]
    );
}

#[test]
fn reconcile_explicit_mail_path_is_used_as_given() {
    let dir = TempDir::new("reconcile-explicit");
    let _root = set_test_lab_root(&dir.0);
    let (ksef, saldeo, mail) = reconcile_year_fixture(&dir);
    let explicit = dir.join("mail.jsonl");
    write_records(&mail, OutputFormat::Jsonl, Some(&explicit)).unwrap();
    let out = dir.join("report.json");
    handle_reconcile_command(
        &dir.join("lab.sqlite"),
        false,
        Some(explicit),
        Some(ksef),
        Some(saldeo),
        70,
        Some(out.clone()),
        false,
        None,
        false,
        2026,
    )
    .unwrap();
    let report: TriReconcileReport =
        serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report.summary.mail_count, 4);
    assert_eq!(
        report_mail_hashes(&report),
        ["mail:dec", "mail:in-year", "mail:jan", "mail:undated"]
    );
}

#[test]
fn reconcile_mail_loader_filters_only_the_default_source() {
    let dir = TempDir::new("reconcile-loader");
    let _root = set_test_lab_root(&dir.0);
    let (_, _, mail) = reconcile_year_fixture(&dir);
    let hashes = |records: Vec<InvoiceRecord>| {
        let mut hashes = records
            .into_iter()
            .map(|record| record.content_hash)
            .collect::<Vec<_>>();
        hashes.sort();
        hashes
    };
    assert_eq!(
        hashes(load_reconcile_mail_candidates(None, 2026).unwrap()),
        ["mail:in-year", "mail:undated"]
    );
    // Rok 2025 z pliku 2026: tylko grudniowa faktura i rekord bez daty.
    let candidates = default_mail_candidates_path(2025);
    fs::create_dir_all(candidates.parent().unwrap()).unwrap();
    write_records(&mail, OutputFormat::Jsonl, Some(&candidates)).unwrap();
    assert_eq!(
        hashes(load_reconcile_mail_candidates(None, 2025).unwrap()),
        ["mail:dec", "mail:undated"]
    );
    // Upload i doctor dalej dostają pełny plik (plan oznacza inne lata jako other_year).
    assert_eq!(
        load_mail_candidates_or_default(None, 2026).unwrap().len(),
        4
    );
    let explicit = dir.join("mail.jsonl");
    write_records(&mail, OutputFormat::Jsonl, Some(&explicit)).unwrap();
    assert_eq!(
        load_reconcile_mail_candidates(Some(&explicit), 2026)
            .unwrap()
            .len(),
        4
    );
}

// --- sync: wspólna reguła zapisu do SQLite ---

/// (store, ksef_input, ksef, mail, amazon_mail, saldeo, oczekiwany zapis)
type SyncStoreCase<'a> = (bool, Option<&'a Path>, bool, bool, bool, bool, bool);

#[test]
fn sync_store_rule_is_shared_and_respects_amazon_mail() {
    let input = Path::new("export");
    let cases: [SyncStoreCase; 9] = [
        (false, None, false, false, false, false, true), // wszystkie źródła, KSeF online
        (false, None, true, false, false, false, true),  // --ksef online
        (false, None, false, false, true, false, false), // --amazon-mail
        (false, None, false, true, false, false, false), // --mail
        (false, None, false, false, false, true, false), // --saldeo
        (false, Some(input), false, false, false, false, false), // KSeF z pliku
        (false, Some(input), true, false, false, false, false),
        (true, None, false, false, true, false, true), // --store zawsze zapisuje
        (true, Some(input), false, true, false, false, true),
    ];
    for (store, ksef_input, ksef, mail, amazon_mail, saldeo, expected) in cases {
        assert_eq!(
            sync_stores_to_db(store, ksef_input, ksef, mail, amazon_mail, saldeo),
            expected,
            "store={store} input={ksef_input:?} ksef={ksef} mail={mail} amazon={amazon_mail} saldeo={saldeo}"
        );
    }
}

// --- bazy tylko do odczytu ---

#[test]
fn open_existing_db_refuses_missing_file_and_does_not_create_it() {
    let dir = TempDir::new("ro");
    let missing = dir.join("typo").join("lab.sqlite");
    let message = format!("{:#}", open_existing_db(&missing).unwrap_err());
    assert!(message.contains("nie istnieje"), "{message}");
    assert!(message.contains("typo"), "{message}");
    assert!(!missing.exists());
    assert!(!dir.join("typo").exists());

    let existing = dir.join("lab.sqlite");
    drop(open_db(&existing).unwrap());
    let conn = open_existing_db(&existing).unwrap();
    assert_eq!(db_stats(&conn).unwrap()["tri_reconcile_runs"], 0);
}

#[test]
fn db_read_commands_fail_on_missing_database() {
    let dir = TempDir::new("ro-cmd");
    let missing = dir.join("missing.sqlite");
    for command in [
        DbCommands::Stats,
        DbCommands::List {
            source: None,
            limit: 1,
        },
        DbCommands::TriRuns { limit: 1 },
    ] {
        assert!(handle_db_command(&missing, command).is_err());
        assert!(!missing.exists());
    }
}

// --- load_records: niepoprawny JSON ---

#[test]
fn malformed_json_input_is_an_error_naming_the_file() {
    let dir = TempDir::new("json");
    let bad = dir.join("ksef-export.json");
    fs::write(&bad, "Faktura VAT FV/1/2026 NIP 5242920020 {").unwrap();
    let message = format!("{:#}", load_records(SourceKind::Ksef, &bad).unwrap_err());
    assert!(message.contains("ksef-export.json"), "{message}");
    assert!(message.contains("niepoprawny JSON"), "{message}");
    assert!(message.contains("line 1"), "{message}");

    let upper = dir.join("RECORDS.JSON");
    fs::write(&upper, "[{").unwrap();
    assert!(load_records(SourceKind::Mail, &upper).is_err());

    let wrong_shape = dir.join("records.json");
    fs::write(&wrong_shape, r#"[{"invoice_number": "FV/1"}]"#).unwrap();
    let message = format!(
        "{:#}",
        load_records(SourceKind::Mail, &wrong_shape).unwrap_err()
    );
    assert!(message.contains("records.json"), "{message}");
}

#[test]
fn json_records_and_single_invoice_object_still_load() {
    let dir = TempDir::new("json-ok");
    let mut record = empty_record(SourceKind::Mail);
    record.content_hash = "abc".into();
    record.invoice_number = Some("FV/7/2026".into());
    let list = dir.join("records.json");
    fs::write(&list, serde_json::to_vec(&vec![record]).unwrap()).unwrap();
    let loaded = load_records(SourceKind::Ksef, &list).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].source, SourceKind::Ksef);

    let single = dir.join("invoice.json");
    fs::write(
        &single,
        r#"{"invoiceNumber": "FV/8/2026", "issueDate": "2026-01-02"}"#,
    )
    .unwrap();
    let loaded = load_records(SourceKind::Ksef, &single).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].invoice_number.as_deref(), Some("FV/8/2026"));
    assert!(
        loaded[0]
            .warnings
            .iter()
            .all(|warning| !warning.contains("jako tekst")),
        "{:?}",
        loaded[0].warnings
    );
}

// --- pomoc CLI ---

#[test]
fn ksef_input_help_says_ksef_is_fetched_online_without_it() {
    let mut command = Cli::command();
    let sync = command.find_subcommand_mut("sync").unwrap();
    let arg = sync
        .get_arguments()
        .find(|arg| arg.get_id() == "ksef_input")
        .unwrap();
    let help = arg.get_help().unwrap().to_string();
    assert!(help.contains("online"), "{help}");
    assert!(!help.contains("data/ksef"), "{help}");
}
