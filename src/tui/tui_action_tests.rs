use super::*;

fn upload_item(path: &str) -> SaldeoSyncItem {
    SaldeoSyncItem {
        own_nip_check: own_nip::OwnNipCheck::NotChecked,
        status: "gmail_missing_saldeo".into(),
        source: "mail".into(),
        related_sources: vec!["mail".into()],
        invoice_number: Some(path.into()),
        issue_date: None,
        gross_amount_minor: None,
        currency: None,
        contractor: None,
        source_path: Some(format!("/tmp/{path}.pdf")),
        can_upload: true,
        upload_status: "planned".into(),
        saldeo_response_status: None,
        saldeo_response_body: None,
        error: None,
        file_sha256: None,
        saldeo_year: None,
        saldeo_month: None,
        saldeo_doc_upload_id: None,
        skip_reason: None,
    }
}

fn table_row(key: &str, upload: bool, ksef_id: Option<i64>) -> InvoiceTableRow {
    let mut record = empty_record(SourceKind::Mail);
    record.content_hash = key.into();
    record.invoice_number = Some(format!("FV/{key}"));
    InvoiceTableRow {
        selected: false,
        sources: "G/K/S".into(),
        mail_record: Some(record.clone()),
        ksef_record: None,
        record,
        saldeo_record: None,
        upload_item: upload.then(|| upload_item(key)),
        ksef_document_id: ksef_id,
        ksef_accounting: None,
        action: InvoiceTableAction::None,
        updated: false,
    }
}

fn ksef_rows() -> Vec<InvoiceTableRow> {
    vec![
        table_row("A", false, Some(1)),
        table_row("B", false, Some(2)),
        table_row("C", false, Some(3)),
    ]
}

#[test]
fn action_applies_to_highlighted_row_not_to_previously_marked_rows() {
    let mut rows = ksef_rows();
    let visible = vec![0, 1, 2];

    // Highlight A, Zatwierdź.
    assert_eq!(
        mark_invoice_table_rows(&mut rows, &visible, 0, InvoiceTableAction::ApproveKsef),
        1
    );
    assert_eq!(rows[0].action, InvoiceTableAction::ApproveKsef);
    assert!(!rows[0].selected, "marking must not select the row");

    // Move to B, Odrzuć: B is rejected, A stays approved.
    assert_eq!(
        mark_invoice_table_rows(&mut rows, &visible, 1, InvoiceTableAction::RejectKsef),
        1
    );
    assert_eq!(rows[0].action, InvoiceTableAction::ApproveKsef);
    assert_eq!(rows[1].action, InvoiceTableAction::RejectKsef);
    assert_eq!(rows[2].action, InvoiceTableAction::None);

    let (_, approve, reject) = collect_invoice_table_actions(&rows);
    assert_eq!(approve, vec![1]);
    assert_eq!(reject, vec![2]);
}

#[test]
fn explicit_space_selection_is_the_target_when_present() {
    let mut rows = ksef_rows();
    // Visible order differs from storage order (sorted table).
    let visible = vec![2, 0, 1];
    toggle_invoice_table_row_selection(&mut rows[0]);
    toggle_invoice_table_row_selection(&mut rows[2]);
    // Highlight is on B (visible index 2), but the selection wins.
    let changed = mark_invoice_table_rows(&mut rows, &visible, 2, InvoiceTableAction::ApproveKsef);
    assert_eq!(changed, 2);
    assert_eq!(rows[0].action, InvoiceTableAction::ApproveKsef);
    assert_eq!(rows[1].action, InvoiceTableAction::None);
    assert_eq!(rows[2].action, InvoiceTableAction::ApproveKsef);
}

#[test]
fn hidden_selected_rows_are_not_targets() {
    let mut rows = ksef_rows();
    rows[0].selected = true;
    // Row 0 is filtered out; the highlighted visible row is the target.
    let visible = vec![1, 2];
    assert_eq!(invoice_table_target_indices(&rows, &visible, 1), vec![2]);
}

#[test]
fn deselecting_with_space_clears_the_action() {
    let mut rows = ksef_rows();
    let visible = vec![0, 1, 2];
    toggle_invoice_table_row_selection(&mut rows[1]);
    mark_invoice_table_rows(&mut rows, &visible, 0, InvoiceTableAction::RejectKsef);
    assert_eq!(rows[1].action, InvoiceTableAction::RejectKsef);
    toggle_invoice_table_row_selection(&mut rows[1]);
    assert!(!rows[1].selected);
    assert_eq!(rows[1].action, InvoiceTableAction::None);
    let (_, approve, reject) = collect_invoice_table_actions(&rows);
    assert!(approve.is_empty() && reject.is_empty());
}

#[test]
fn clear_removes_action_of_highlighted_row_and_of_selection() {
    let mut rows = ksef_rows();
    let visible = vec![0, 1, 2];
    mark_invoice_table_rows(&mut rows, &visible, 0, InvoiceTableAction::ApproveKsef);
    mark_invoice_table_rows(&mut rows, &visible, 1, InvoiceTableAction::RejectKsef);

    // Nothing selected: Wyczyść acts on the highlighted row only.
    assert_eq!(clear_invoice_table_rows(&mut rows, &visible, 0), 1);
    assert_eq!(rows[0].action, InvoiceTableAction::None);
    assert_eq!(rows[1].action, InvoiceTableAction::RejectKsef);

    // With a selection: Wyczyść clears action and selection of the selected rows.
    toggle_invoice_table_row_selection(&mut rows[1]);
    toggle_invoice_table_row_selection(&mut rows[2]);
    mark_invoice_table_rows(&mut rows, &visible, 0, InvoiceTableAction::ApproveKsef);
    assert_eq!(clear_invoice_table_rows(&mut rows, &visible, 0), 2);
    assert!(
        rows.iter()
            .all(|row| row.action == InvoiceTableAction::None)
    );
    assert!(rows.iter().all(|row| !row.selected));
}

#[test]
fn marks_skip_rows_that_cannot_take_the_action() {
    let mut rows = vec![table_row("U", true, None), table_row("K", false, Some(9))];
    let visible = vec![0, 1];
    assert_eq!(
        mark_invoice_table_rows(&mut rows, &visible, 1, InvoiceTableAction::Upload),
        0
    );
    assert_eq!(rows[1].action, InvoiceTableAction::None);
    assert_eq!(
        mark_invoice_table_rows(&mut rows, &visible, 0, InvoiceTableAction::Upload),
        1
    );
}

#[test]
fn commit_snapshot_executes_only_visible_rows() {
    let mut rows = ksef_rows();
    for row in &mut rows {
        row.action = InvoiceTableAction::ApproveKsef;
    }
    let snapshot = invoice_table_commit_rows(&rows, &[0, 2]);
    let (_, approve, _) = collect_invoice_table_actions(&snapshot);
    assert_eq!(approve, vec![1, 3]);
}

fn progress() -> Arc<Mutex<String>> {
    Arc::new(Mutex::new(String::new()))
}

#[test]
fn partial_upload_failure_clears_only_succeeded_rows_and_still_refreshes() {
    let mut rows = vec![
        table_row("ok", true, None),
        table_row("bad", true, None),
        table_row("approve", false, Some(7)),
        table_row("idle", true, None),
    ];
    rows[0].action = InvoiceTableAction::Upload;
    rows[1].action = InvoiceTableAction::Upload;
    rows[2].action = InvoiceTableAction::ApproveKsef;

    let mut refreshed = false;
    let mut marked = Vec::new();
    let result = execute_invoice_table_actions_with(
        2026,
        &rows,
        &progress(),
        |plan| {
            assert_eq!(plan.items.len(), 2);
            plan.items[0].upload_status = "uploaded".into();
            plan.items[1].upload_status = "failed".into();
            plan.items[1].error = Some("HTTP 500".into());
            Ok(())
        },
        |ids, approve| {
            marked.push((ids.to_vec(), approve));
            Ok(ids.to_vec())
        },
        || {
            refreshed = true;
            // Fresh table: the uploaded row is now matched with Saldeo, the others remain.
            let mut fresh = vec![
                table_row("ok", false, None),
                table_row("bad", true, None),
                table_row("approve", false, None),
                table_row("idle", true, None),
            ];
            fresh[0].sources = "G/-/S".into();
            Ok(fresh)
        },
    );
    assert!(refreshed, "Saldeo refresh must run after a partial failure");
    assert_eq!(marked, vec![(vec![7], true)]);

    let report = result.commit.expect("commit report");
    assert_eq!(report.succeeded.len(), 2);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].row_key, "bad");
    assert_eq!(report.failed[0].error.as_deref(), Some("HTTP 500"));

    let mut new_rows = result.rows.expect("rows");
    apply_commit_report_to_rows(&mut new_rows, &report);
    assert_eq!(new_rows[0].action, InvoiceTableAction::None);
    assert_eq!(new_rows[1].action, InvoiceTableAction::Upload);
    assert_eq!(new_rows[2].action, InvoiceTableAction::None);
    assert_eq!(new_rows[3].action, InvoiceTableAction::None);

    // A retry only re-sends the failed file.
    let (uploads, approve, reject) = collect_invoice_table_actions(&new_rows);
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].invoice_number.as_deref(), Some("bad"));
    assert!(approve.is_empty() && reject.is_empty());

    let status = invoice_table_commit_status("Akceptuj", &report, None, 4, 1);
    assert!(status.starts_with('✗'), "{status}");
    assert!(status.contains("udane 2, nieudane 1"), "{status}");
    assert!(status.contains("bad.pdf: HTTP 500"), "{status}");
}

#[test]
fn refresh_failure_keeps_failed_actions_on_current_rows() {
    let mut rows = vec![
        table_row("ok", true, None),
        table_row("approve", false, Some(7)),
        table_row("reject", false, Some(8)),
    ];
    rows[0].action = InvoiceTableAction::Upload;
    rows[1].action = InvoiceTableAction::ApproveKsef;
    rows[2].action = InvoiceTableAction::RejectKsef;

    let result = execute_invoice_table_actions_with(
        2026,
        &rows,
        &progress(),
        |plan| {
            plan.items[0].upload_status = "uploaded".into();
            Ok(())
        },
        |ids, approve| {
            if approve {
                Err(anyhow!("markAccounting failed"))
            } else {
                Ok(ids.to_vec())
            }
        },
        || Err(anyhow!("Saldeo offline")),
    );
    let report = result.commit.unwrap();
    assert!(result.rows.is_err());
    apply_commit_report_to_rows(&mut rows, &report);
    assert_eq!(rows[0].action, InvoiceTableAction::None);
    assert_eq!(rows[1].action, InvoiceTableAction::ApproveKsef);
    assert_eq!(rows[2].action, InvoiceTableAction::None);

    let status = invoice_table_commit_status("Akceptuj", &report, Some("Saldeo offline"), 3, 0);
    assert!(status.contains("udane 2, nieudane 1"), "{status}");
    assert!(status.contains("markAccounting failed"), "{status}");
    assert!(status.contains("Saldeo offline"), "{status}");
}

#[test]
fn upload_error_before_any_item_marks_all_uploads_failed() {
    let mut rows = vec![table_row("a", true, None), table_row("b", true, None)];
    for row in &mut rows {
        row.action = InvoiceTableAction::Upload;
    }
    let result = execute_invoice_table_actions_with(
        2026,
        &rows,
        &progress(),
        |_| Err(anyhow!("sesja Saldeo nieważna")),
        |_, _| panic!("no KSeF marks requested"),
        || Ok(Vec::new()),
    );
    let report = result.commit.unwrap();
    assert!(report.succeeded.is_empty());
    assert_eq!(report.failed.len(), 2);
    assert!(
        report
            .failed
            .iter()
            .all(|entry| entry.error.as_deref() == Some("sesja Saldeo nieważna"))
    );
}

#[test]
fn ksef_ids_not_actually_marked_count_as_failed() {
    let mut rows = vec![
        table_row("x", false, Some(1)),
        table_row("y", false, Some(2)),
    ];
    rows[0].action = InvoiceTableAction::ApproveKsef;
    rows[1].action = InvoiceTableAction::ApproveKsef;
    let result = execute_invoice_table_actions_with(
        2026,
        &rows,
        &progress(),
        |_| panic!("no uploads requested"),
        |_, _| Ok(vec![1]),
        || Ok(Vec::new()),
    );
    let report = result.commit.unwrap();
    assert_eq!(report.succeeded.len(), 1);
    assert_eq!(report.succeeded[0].ksef_document_id, Some(1));
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].ksef_document_id, Some(2));
}

#[test]
fn all_succeeded_status_is_green() {
    let report = InvoiceTableCommitReport {
        succeeded: vec![InvoiceTableCommitEntry {
            row_key: "a".into(),
            action: InvoiceTableAction::Upload,
            ksef_document_id: None,
            label: "a.pdf".into(),
            error: None,
        }],
        failed: Vec::new(),
    };
    let status = invoice_table_commit_status("Akceptuj", &report, None, 10, 1);
    assert!(status.starts_with('✓'), "{status}");
    assert!(status.contains("udane 1, nieudane 0"), "{status}");
}

fn approvable_tri_row(ksef_ref: Option<&str>, saldeo_ref: Option<&str>) -> TriRow {
    let mut ksef = empty_record(SourceKind::Ksef);
    ksef.content_hash = "ksef:1".into();
    ksef.invoice_number = Some("FV/1/2026".into());
    ksef.ksef_reference = ksef_ref.map(str::to_string);
    let mut saldeo = empty_record(SourceKind::Saldeo);
    saldeo.content_hash = "saldeo:42".into();
    saldeo.invoice_number = Some("FV/1/2026".into());
    saldeo.ksef_reference = saldeo_ref.map(str::to_string);
    TriRow {
        status: "ksef_saldeo_missing_gmail".into(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: None,
        ksef_score_to_saldeo: Some(80),
        mail: None,
        ksef: Some(ksef),
        saldeo: Some(saldeo),
    }
}

fn approvable_id(ksef_ref: Option<&str>, saldeo_ref: Option<&str>) -> Option<i64> {
    let statuses = HashMap::from([(42, None)]);
    invoice_table_row_from_reconcile_row(&approvable_tri_row(ksef_ref, saldeo_ref), Some(&statuses))
        .unwrap()
        .ksef_document_id
}

#[test]
fn approvable_only_when_saldeo_points_at_the_rows_ksef_document() {
    let reference = "5242920020-20260904-7B4DFEC00001-B8";
    assert_eq!(approvable_id(Some(reference), Some(reference)), Some(42));
    assert_eq!(
        approvable_id(Some(reference), Some(&format!("  {reference} "))),
        Some(42)
    );
    assert_eq!(
        approvable_id(Some(reference), Some("5242920020-20260904-000000000000-00")),
        None
    );
    assert_eq!(approvable_id(Some(reference), None), None);
    assert_eq!(approvable_id(None, Some(reference)), None);
    assert_eq!(approvable_id(Some("  "), Some("  ")), None);
    assert_eq!(approvable_id(None, None), None);
}

#[test]
fn mismatched_reference_cannot_be_marked_in_the_table() {
    let statuses = HashMap::from([(42, None)]);
    let row = invoice_table_row_from_reconcile_row(
        &approvable_tri_row(Some("REF-A"), Some("REF-B")),
        Some(&statuses),
    )
    .unwrap();
    let mut rows = vec![row];
    assert_eq!(
        mark_invoice_table_rows(&mut rows, &[0], 0, InvoiceTableAction::ApproveKsef),
        0
    );
    assert!(pending_ksef_approve_ids(&rows).is_empty());
}
