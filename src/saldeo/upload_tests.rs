use super::*;
use std::cell::Cell;

fn temp_root(tag: &str) -> PathBuf {
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(format!("lab-upload-{tag}-{nonce}"));
    fs::create_dir_all(&root).unwrap();
    root
}

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

fn mail_record(path: &Path, issue_date: Option<NaiveDate>) -> InvoiceRecord {
    let mut record = empty_record(SourceKind::Mail);
    record.source_path = Some(path.display().to_string());
    record.content_hash = format!("mail:{}", path.display());
    record.issue_date = issue_date;
    record
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

fn saldeo_only_row() -> TriRow {
    let mut saldeo = empty_record(SourceKind::Saldeo);
    saldeo.content_hash = "saldeo:1".into();
    saldeo.invoice_number = Some("FV/1/2026".into());
    TriRow {
        status: "saldeo_only".into(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: None,
        ksef_score_to_saldeo: None,
        mail: None,
        ksef: None,
        saldeo: Some(saldeo),
    }
}

fn write_report(root: &Path, rows: Vec<TriRow>) -> PathBuf {
    let report = TriReconcileReport {
        generated_at: Utc::now(),
        review_score: 70,
        summary: TriSummary {
            mail_count: 0,
            ksef_count: 0,
            saldeo_count: 0,
            in_all_three: 0,
            gmail_ksef_missing_saldeo: 0,
            gmail_saldeo_missing_ksef: 0,
            gmail_only: 0,
            ksef_saldeo_missing_gmail: 0,
            ksef_only: 0,
            saldeo_only: 0,
        },
        rows,
    };
    let path = root.join("tri.json");
    fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
    path
}

fn plan_for(report: &Path, db: &Path, year: i32, today: NaiveDate) -> SaldeoSyncPlan {
    saldeo_sync_plan_at(
        SaldeoSyncPlanConfig {
            year,
            tri_report: Some(report),
            mail: None,
            ksef: None,
            saldeo: None,
            db_path: Some(db),
            review_score: 70,
            confirm: true,
            upload_url: None,
        },
        today,
    )
    .unwrap()
}

fn uploaded(year: i32, month: u32) -> SaldeoUploadOutcome {
    SaldeoUploadOutcome::Uploaded {
        year,
        month,
        doc_upload_id: 77,
        response_status: 200,
        body: r#"{"status":"SUCCESS"}"#.into(),
    }
}

#[test]
fn ledger_skips_same_file_on_next_plan_and_keeps_used_period() {
    let root = temp_root("ledger");
    let pdf = root.join("faktura.pdf");
    fs::write(&pdf, b"%PDF-1.4 bez numeru faktury").unwrap();
    let db = root.join("lab.sqlite");
    let today = date(2026, 10, 5);
    // Brak numeru faktury: fuzzy-match z kopią w Saldeo nie przekroczy progu.
    let report = write_report(
        &root,
        vec![
            mail_row(mail_record(&pdf, Some(date(2026, 7, 3)))),
            saldeo_only_row(),
        ],
    );

    let mut plan = plan_for(&report, &db, 2026, today);
    assert_eq!(plan.summary.uploadable_count, 1);
    assert_eq!(plan.items[0].upload_status, "planned");
    assert_eq!(
        (plan.items[0].saldeo_year, plan.items[0].saldeo_month),
        (Some(2026), Some(7))
    );
    let expected_sha = saldeo_file_sha256(b"%PDF-1.4 bez numeru faktury");
    assert_eq!(
        plan.items[0].file_sha256.as_deref(),
        Some(expected_sha.as_str())
    );

    // Lipiec zamknięty: transport zapisał plik w październiku.
    let conn = open_db(&db).unwrap();
    let calls = Cell::new(0);
    saldeo_upload_items_with(&mut plan, &conn, today, None, |_, _, year, month| {
        calls.set(calls.get() + 1);
        assert_eq!((year, month), (2026, 7));
        uploaded(2026, 10)
    })
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(plan.items[0].upload_status, "uploaded");
    assert_eq!(
        (plan.items[0].saldeo_year, plan.items[0].saldeo_month),
        (Some(2026), Some(10))
    );
    let entry = saldeo_upload_ledger_get(&conn, &expected_sha)
        .unwrap()
        .unwrap();
    assert_eq!(entry.state, SALDEO_LEDGER_UPLOADED);
    assert_eq!((entry.saldeo_year, entry.saldeo_month), (2026, 10));
    assert_eq!(entry.doc_upload_id, Some(77));
    assert_eq!(entry.response_status, Some(200));
    assert_eq!(entry.source_path, Some(pdf.display().to_string()));

    // Drugi plan: ten sam plik (także pod inną ścieżką) nie jest już do wysłania.
    let copy = root.join("kopia-z-innego-maila.pdf");
    fs::copy(&pdf, &copy).unwrap();
    let report = write_report(
        &root,
        vec![
            mail_row(mail_record(&pdf, Some(date(2026, 7, 3)))),
            mail_row(mail_record(&copy, Some(date(2026, 7, 3)))),
            saldeo_only_row(),
        ],
    );
    let mut plan = plan_for(&report, &db, 2026, today);
    assert_eq!(plan.summary.uploadable_count, 0);
    assert_eq!(plan.summary.already_uploaded_count, 2);
    for item in &plan.items {
        assert!(!item.can_upload);
        assert_eq!(item.upload_status, "already_uploaded");
        assert_eq!(
            (item.saldeo_year, item.saldeo_month),
            (Some(2026), Some(10))
        );
        assert!(item.skip_reason.as_deref().unwrap().contains("2026-10"));
    }
    saldeo_upload_items_with(&mut plan, &conn, today, None, |_, _, _, _| {
        panic!("plik z rejestru nie może być wysłany ponownie")
    })
    .unwrap();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unconfirmed_upload_is_ledgered_and_never_retried() {
    let root = temp_root("unconfirmed");
    let pdf = root.join("faktura.pdf");
    fs::write(&pdf, b"%PDF unconfirmed").unwrap();
    let db = root.join("lab.sqlite");
    let today = date(2026, 10, 5);
    let report = write_report(
        &root,
        vec![
            mail_row(mail_record(&pdf, Some(date(2026, 9, 1)))),
            saldeo_only_row(),
        ],
    );
    let conn = open_db(&db).unwrap();

    let mut plan = plan_for(&report, &db, 2026, today);
    saldeo_upload_items_with(&mut plan, &conn, today, None, |_, _, year, month| {
        SaldeoUploadOutcome::Unconfirmed {
            year,
            month,
            doc_upload_id: 5,
            response_status: None,
            error: "Saldeo confirm upload 5: timeout".into(),
        }
    })
    .unwrap();
    plan.summary = saldeo_sync_summary(&plan.items);
    assert_eq!(plan.items[0].upload_status, "unconfirmed");
    assert_eq!(
        plan.summary.failed_count, 1,
        "brak potwierdzenia w tym przebiegu to błąd"
    );
    let entry = saldeo_upload_ledger_get(&conn, plan.items[0].file_sha256.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(entry.state, SALDEO_LEDGER_UNCONFIRMED);
    assert_eq!((entry.saldeo_year, entry.saldeo_month), (2026, 9));
    assert!(entry.error.unwrap().contains("timeout"));

    let mut plan = plan_for(&report, &db, 2026, today);
    assert!(!plan.items[0].can_upload);
    assert_eq!(plan.items[0].upload_status, "unconfirmed");
    assert_eq!(plan.summary.unconfirmed_count, 1);
    assert_eq!(plan.summary.failed_count, 0);
    assert_eq!(plan.summary.uploadable_count, 0);
    saldeo_upload_items_with(&mut plan, &conn, today, None, |_, _, _, _| {
        panic!("unconfirmed nie jest ponawiany automatycznie")
    })
    .unwrap();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn ledger_check_at_upload_time_covers_plans_built_without_it() {
    // TUI buduje pozycje bez rejestru; leaf i tak nie wyśle dwa razy tych samych bajtów.
    let root = temp_root("batch");
    let a = root.join("a.pdf");
    let b = root.join("b.pdf");
    fs::write(&a, b"%PDF same").unwrap();
    fs::write(&b, b"%PDF same").unwrap();
    let conn = open_db(&root.join("lab.sqlite")).unwrap();
    let items = [&a, &b]
        .into_iter()
        .map(|path| {
            saldeo_sync_item_from_record(
                "gmail_only",
                &mail_record(path, Some(date(2026, 5, 5))),
                vec!["mail".into()],
            )
        })
        .collect::<Vec<_>>();
    let mut plan = SaldeoSyncPlan {
        year: 2026,
        summary: saldeo_sync_summary(&items),
        items,
        ..Default::default()
    };
    let calls = Cell::new(0);
    saldeo_upload_items_with(&mut plan, &conn, date(2026, 10, 5), None, |_, _, y, m| {
        calls.set(calls.get() + 1);
        uploaded(y, m)
    })
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(plan.items[0].upload_status, "uploaded");
    assert_eq!(plan.items[1].upload_status, "already_uploaded");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn failed_upload_is_not_ledgered_and_stays_retryable() {
    let root = temp_root("failed");
    let pdf = root.join("f.pdf");
    fs::write(&pdf, b"%PDF failed").unwrap();
    let db = root.join("lab.sqlite");
    let today = date(2026, 10, 5);
    let report = write_report(
        &root,
        vec![
            mail_row(mail_record(&pdf, Some(date(2026, 2, 2)))),
            saldeo_only_row(),
        ],
    );
    let conn = open_db(&db).unwrap();
    let mut plan = plan_for(&report, &db, 2026, today);
    saldeo_upload_items_with(&mut plan, &conn, today, None, |_, _, _, _| {
        SaldeoUploadOutcome::Failed {
            error: "Saldeo signed upload failed".into(),
        }
    })
    .unwrap();
    plan.summary = saldeo_sync_summary(&plan.items);
    assert_eq!(plan.summary.failed_count, 1);
    let plan = plan_for(&report, &db, 2026, today);
    assert!(plan.items[0].can_upload);
    assert_eq!(plan.items[0].upload_status, "planned");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn ledger_write_failure_aborts_remaining_uploads() {
    let root = temp_root("ledger-fail");
    let a = root.join("a.pdf");
    let b = root.join("b.pdf");
    fs::write(&a, b"%PDF a").unwrap();
    fs::write(&b, b"%PDF b").unwrap();
    let conn = open_db(&root.join("lab.sqlite")).unwrap();
    let items = [&a, &b]
        .into_iter()
        .map(|path| {
            saldeo_sync_item_from_record(
                "gmail_only",
                &mail_record(path, Some(date(2026, 5, 5))),
                vec!["mail".into()],
            )
        })
        .collect::<Vec<_>>();
    let mut plan = SaldeoSyncPlan {
        year: 2026,
        items,
        ..Default::default()
    };
    let calls = Cell::new(0);
    let result =
        saldeo_upload_items_with(&mut plan, &conn, date(2026, 10, 5), None, |_, _, y, m| {
            calls.set(calls.get() + 1);
            conn.execute("DROP TABLE saldeo_upload_ledger", []).unwrap();
            uploaded(y, m)
        });
    assert!(result.is_err());
    assert_eq!(calls.get(), 1);
    assert_eq!(plan.items[0].upload_status, "uploaded");
    assert!(plan.items[0].error.as_deref().unwrap().contains("rejestru"));
    assert_eq!(plan.items[1].upload_status, "planned");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn upload_period_follows_plan_year_and_undated_rule() {
    let today = date(2026, 1, 14);
    assert_eq!(
        saldeo_upload_target_period(Some(date(2026, 1, 2)), 2026, today),
        Ok((2026, 1))
    );
    // Faktura z 30 grudnia wysłana mailem w styczniu: folder 2026, ale nie plan 2026.
    assert!(saldeo_upload_target_period(Some(date(2025, 12, 30)), 2026, today).is_err());
    assert_eq!(
        saldeo_upload_target_period(Some(date(2025, 12, 30)), 2025, today),
        Ok((2025, 12))
    );
    // Bez daty: bieżący okres (dzisiejszy rok i miesiąc), tylko w planie bieżącego roku.
    assert_eq!(
        saldeo_upload_target_period(None, 2026, today),
        Ok((2026, 1))
    );
    assert!(saldeo_upload_target_period(None, 2025, today).is_err());

    let root = temp_root("year");
    let december = root.join("grudzien.pdf");
    let undated = root.join("bez-daty.pdf");
    let january = root.join("styczen.pdf");
    fs::write(&december, b"%PDF dec").unwrap();
    fs::write(&undated, b"%PDF none").unwrap();
    fs::write(&january, b"%PDF jan").unwrap();
    let db = root.join("lab.sqlite");
    let report = write_report(
        &root,
        vec![
            mail_row(mail_record(&december, Some(date(2025, 12, 30)))),
            mail_row(mail_record(&undated, None)),
            mail_row(mail_record(&january, Some(date(2026, 1, 3)))),
            saldeo_only_row(),
        ],
    );
    let plan = plan_for(&report, &db, 2026, today);
    let by_path = |path: &Path| {
        plan.items
            .iter()
            .find(|item| item.source_path.as_deref() == Some(path.to_str().unwrap()))
            .unwrap()
    };
    let item = by_path(&december);
    assert!(!item.can_upload);
    assert_eq!(item.upload_status, "other_year");
    assert!(item.skip_reason.as_deref().unwrap().contains("2025-12-30"));
    let item = by_path(&undated);
    assert!(item.can_upload);
    assert_eq!((item.saldeo_year, item.saldeo_month), (Some(2026), Some(1)));
    let item = by_path(&january);
    assert!(item.can_upload);
    assert_eq!(plan.summary.other_year_count, 1);
    assert_eq!(plan.summary.uploadable_count, 2);

    // Plan 2025 oglądany w 2026: grudniowa faktura tak, plik bez daty nie.
    let plan = plan_for(&report, &db, 2025, today);
    let find = |path: &Path| {
        plan.items
            .iter()
            .find(|item| item.source_path.as_deref() == Some(path.to_str().unwrap()))
            .unwrap()
    };
    assert!(find(&december).can_upload);
    assert_eq!(find(&undated).upload_status, "other_year");
    assert_eq!(find(&january).upload_status, "other_year");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn document_list_response_is_validated() {
    let ok = serde_json::json!({
        "status": "SUCCESS",
        "data": {"resultCollection": [{"id": 1}, {"id": 2}], "totalCount": 2}
    });
    assert_eq!(saldeo_document_list_items(&ok, 2026, 3).unwrap().len(), 2);
    let empty = serde_json::json!({"status": "SUCCESS", "data": {"resultCollection": []}});
    assert!(
        saldeo_document_list_items(&empty, 2026, 3)
            .unwrap()
            .is_empty()
    );
    let no_status = serde_json::json!({"data": {"resultCollection": [{"id": 1}]}});
    assert_eq!(
        saldeo_document_list_items(&no_status, 2026, 3)
            .unwrap()
            .len(),
        1
    );

    let error_status = serde_json::json!({
        "status": "ERROR",
        "data": {"resultCollection": []}
    });
    let err = saldeo_document_list_items(&error_status, 2026, 3).unwrap_err();
    assert!(err.to_string().contains("2026-03"));
    assert!(err.to_string().contains("ERROR"));

    let missing = serde_json::json!({"status": "SUCCESS", "data": {}});
    assert!(saldeo_document_list_items(&missing, 2026, 3).is_err());
    let not_array = serde_json::json!({"status": "SUCCESS", "data": {"resultCollection": null}});
    assert!(saldeo_document_list_items(&not_array, 2026, 3).is_err());
    assert!(saldeo_document_list_items(&serde_json::json!({}), 2026, 3).is_err());

    let mismatch = serde_json::json!({
        "status": "SUCCESS",
        "data": {"resultCollection": [{"id": 1}], "totalCount": 3}
    });
    assert!(saldeo_document_list_items(&mismatch, 2026, 3).is_err());
    let nested_mismatch = serde_json::json!({
        "status": "SUCCESS",
        "data": {"resultCollection": [{"id": 1}], "pagination": {"totalCount": 2}}
    });
    assert!(saldeo_document_list_items(&nested_mismatch, 2026, 3).is_err());
}

#[test]
fn confirm_response_requires_success_status() {
    assert_eq!(
        saldeo_check_confirm_response(200, r#"{"status":"SUCCESS","data":null}"#),
        SaldeoConfirmCheck::Confirmed
    );
    assert!(matches!(
        saldeo_check_confirm_response(200, r#"{"status":"ERROR"}"#),
        SaldeoConfirmCheck::Rejected(_)
    ));
    assert!(matches!(
        saldeo_check_confirm_response(500, r#"{"status":"SUCCESS"}"#),
        SaldeoConfirmCheck::Rejected(_)
    ));
    assert!(matches!(
        saldeo_check_confirm_response(200, ""),
        SaldeoConfirmCheck::Unknown(_)
    ));
    assert!(matches!(
        saldeo_check_confirm_response(200, r#"{"data":{}}"#),
        SaldeoConfirmCheck::Unknown(_)
    ));
}

#[test]
fn closed_month_fallback_returns_period_actually_used() {
    let today = date(2026, 10, 5);
    let mut requested = Vec::new();
    let (year, month, response) = saldeo_resolve_upload_period(2026, 7, today, |y, m| {
        requested.push((y, m));
        Ok(if (y, m) == (2026, 7) {
            serde_json::json!({
                "status": "VALIDATION_ERROR",
                "data": [{"field": "month", "message": "Miesiąc jest zamknięty"}]
            })
        } else {
            serde_json::json!({"status": "SUCCESS", "data": {}})
        })
    })
    .unwrap();
    assert_eq!((year, month), (2026, 10));
    assert_eq!(requested, vec![(2026, 7), (2026, 10)]);
    assert_eq!(response["status"], "SUCCESS");

    let err = saldeo_resolve_upload_period(2026, 7, today, |_, _| {
        Ok(serde_json::json!({"status": "ERROR"}))
    })
    .unwrap_err();
    assert!(err.to_string().contains("generate upload URL failed"));
}

#[test]
fn empty_saldeo_blocks_upload_unless_allowed() {
    assert!(saldeo_empty_saldeo_guard(Some(0), 3, false).is_err());
    assert!(saldeo_empty_saldeo_guard(Some(0), 3, true).is_ok());
    assert!(saldeo_empty_saldeo_guard(Some(0), 0, false).is_ok());
    assert!(saldeo_empty_saldeo_guard(Some(12), 3, false).is_ok());
    assert!(saldeo_empty_saldeo_guard(None, 3, false).is_ok());

    let root = temp_root("empty");
    let pdf = root.join("f.pdf");
    fs::write(&pdf, b"%PDF empty").unwrap();
    let report = write_report(
        &root,
        vec![mail_row(mail_record(&pdf, Some(date(2026, 4, 1))))],
    );
    let plan = plan_for(&report, &root.join("lab.sqlite"), 2026, date(2026, 10, 5));
    assert_eq!(plan.saldeo_record_count, Some(0));
    assert_eq!(plan.summary.uploadable_count, 1);
    if !saldeo_allow_empty_from_env() {
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("LAB_ALLOW_EMPTY_SALDEO"))
        );
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn write_lock_is_exclusive_and_released_on_drop() {
    let root = temp_root("lock");
    let path = root.join("nested").join("saldeo-write.lock");
    let first = saldeo_write_lock_at(&path).unwrap();
    let err = saldeo_write_lock_at(&path)
        .err()
        .expect("drugi lock musi się nie udać");
    assert!(err.to_string().contains("zajęty"));
    assert!(
        err.to_string()
            .contains(&format!("PID {}", std::process::id()))
    );
    drop(first);
    let again = saldeo_write_lock_at(&path).unwrap();
    drop(again);
    let _ = fs::remove_dir_all(root);
}
