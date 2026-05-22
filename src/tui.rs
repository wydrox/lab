use crate::*;

pub(crate) fn interactive_tui(db_path: &Path) -> Result<()> {
    interactive_reconcile_actions(db_path)
}

pub(crate) fn interactive_reconcile_actions(db_path: &Path) -> Result<()> {
    let mut year: i32 = 2026;
    let mut review_score: u8 = 70;
    let mut rows = build_invoice_table_rows(year, review_score)?;

    loop {
        match run_invoice_table_tui(&mut rows, &mut year, &mut review_score, db_path)? {
            TuiResult::Cancel => return Ok(()),
            TuiResult::Doctor => {
                eprintln!("  [LAB] diagnostyka...");
                doctor(db_path, "GMAIL_ACCESS_TOKEN")?;
            }
            TuiResult::Onboard => {
                eprintln!("  [LAB] konfiguracja...");
                onboard(db_path, false, None)?;
            }
            TuiResult::Correct(record) => {
                if edit_saldeo_record_override(&record)? {
                    rows = build_invoice_table_rows(year, review_score)?;
                }
            }
        }
    }
}

pub(crate) enum TuiResult {
    Cancel,
    Doctor,
    Onboard,
    Correct(InvoiceRecord),
}

#[derive(Debug, Clone, Copy)]
struct TuiTheme {
    light: bool,
}

impl TuiTheme {
    fn detect() -> Self {
        if let Ok(value) = std::env::var("LAB_TUI_THEME") {
            return Self {
                light: matches!(value.to_ascii_lowercase().as_str(), "light" | "jasny"),
            };
        }
        if let Ok(value) = std::env::var("TERM_BACKGROUND") {
            return Self {
                light: value.eq_ignore_ascii_case("light"),
            };
        }
        let light = std::env::var("COLORFGBG")
            .ok()
            .and_then(|value| {
                value
                    .split(';')
                    .next_back()
                    .and_then(|bg| bg.parse::<u8>().ok())
            })
            .is_some_and(|bg| bg == 7 || bg >= 15);
        Self { light }
    }

    fn neutral(self) -> Style {
        Style::default().fg(Color::Reset)
    }

    fn muted(self) -> Style {
        Style::default().fg(if self.light {
            Color::DarkGray
        } else {
            Color::Gray
        })
    }

    fn very_muted(self) -> Style {
        Style::default().fg(Color::DarkGray)
    }

    fn header(self) -> Style {
        Style::default()
            .fg(if self.light {
                Color::Blue
            } else {
                Color::Yellow
            })
            .add_modifier(Modifier::BOLD)
    }

    fn updated(self) -> Style {
        Style::default()
            .fg(if self.light {
                Color::Green
            } else {
                Color::LightGreen
            })
            .add_modifier(Modifier::ITALIC)
    }

    fn upload(self) -> Style {
        Style::default().fg(if self.light { Color::Blue } else { Color::Cyan })
    }

    fn approve(self) -> Style {
        Style::default().fg(Color::Green)
    }

    fn reject(self) -> Style {
        Style::default().fg(Color::Red)
    }

    fn selected(self) -> Style {
        Style::default()
            .fg(Color::Black)
            .bg(if self.light {
                Color::Yellow
            } else {
                Color::White
            })
            .add_modifier(Modifier::BOLD)
    }

    fn table_highlight(self) -> Style {
        Style::default()
            .fg(Color::Black)
            .bg(if self.light {
                Color::Yellow
            } else {
                Color::White
            })
            .add_modifier(Modifier::BOLD)
    }

    fn inactive_button(self) -> Style {
        let style = self.very_muted();
        if self.light {
            style
        } else {
            style.add_modifier(Modifier::DIM)
        }
    }

    fn status_pending(self) -> Color {
        if self.light {
            Color::Blue
        } else {
            Color::Yellow
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvoiceTableAction {
    None,
    Upload,
    ApproveKsef,
    RejectKsef,
}

#[derive(Debug, Clone)]
pub(crate) struct InvoiceTableRow {
    selected: bool,
    sources: String,
    record: InvoiceRecord,
    saldeo_record: Option<InvoiceRecord>,
    upload_item: Option<SaldeoSyncItem>,
    ksef_document_id: Option<i64>,
    ksef_accounting: Option<bool>,
    action: InvoiceTableAction,
    updated: bool,
}

impl InvoiceTableRow {
    fn is_actionable(&self) -> bool {
        self.upload_item.is_some() || self.ksef_document_id.is_some()
    }

    fn needs_attention(&self) -> bool {
        self.is_actionable() || self.sources.contains('-') || self.updated || self.sources.ends_with('*')
    }

    fn can_upload(&self) -> bool {
        self.upload_item.is_some()
    }

    fn can_mark_ksef(&self) -> bool {
        self.ksef_document_id.is_some()
    }
}

pub(crate) fn build_invoice_table_rows(
    year: i32,
    review_score: u8,
) -> Result<Vec<InvoiceTableRow>> {
    build_invoice_table_rows_with_progress(year, review_score, None)
}

pub(crate) fn build_invoice_table_rows_with_progress(
    year: i32,
    review_score: u8,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<Vec<InvoiceTableRow>> {
    if let Some(progress) = &progress {
        set_progress(progress, "Tabela: wczytywanie źródeł...");
    }
    let mail = default_mail_candidates_path(year);
    let ksef = configured_ksef_out_path(year);
    let saldeo = default_saldeo_records_path(year);
    let mail_records = load_records(SourceKind::Mail, &mail)?;
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!("Tabela: Gmail {} rekordów...", mail_records.len()),
        );
    }
    let ksef_records = load_records(SourceKind::Ksef, &ksef)?;
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!("Tabela: KSeF {} rekordów...", ksef_records.len()),
        );
    }
    let saldeo_records = load_saldeo_records(&saldeo)?;
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!(
                "Tabela: Saldeo {} rekordów, porównuję...",
                saldeo_records.len()
            ),
        );
    }
    let report = tri_reconcile(
        mail_records,
        ksef_records,
        saldeo_records.clone(),
        review_score,
    );

    let ksef_ids = saldeo_ksef_accounting_candidates(&saldeo_records)
        .into_iter()
        .map(|candidate| candidate.document_id)
        .collect::<Vec<_>>();
    let ksef_statuses = if ksef_ids.is_empty() {
        Some(HashMap::new())
    } else {
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Tabela: status KSeF w Saldeo ({} dokumentów)...",
                    ksef_ids.len()
                ),
            );
        }
        match read_saldeo_session(&default_saldeo_storage_state_path())
            .and_then(|session| saldeo_fetch_ksef_accounting_statuses(&session, &ksef_ids))
        {
            Ok(statuses) => Some(statuses),
            Err(err) => {
                eprintln!(
                    "  [Saldeo] pomijam statusy KSeF w tabeli (sesja/API niedostępne): {err}"
                );
                if let Some(progress) = &progress {
                    set_progress(
                        progress,
                        "Tabela: status KSeF w Saldeo niedostępny — użyj Menu → Saldeo, aby odświeżyć sesję",
                    );
                }
                None
            }
        }
    };

    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!("Tabela: render {} wierszy...", report.rows.len()),
        );
    }
    let rows = report
        .rows
        .iter()
        .filter_map(|row| invoice_table_row_from_reconcile_row(row, ksef_statuses.as_ref()))
        .collect::<Vec<_>>();
    Ok(rows)
}

pub(crate) fn invoice_table_row_from_reconcile_row(
    row: &TriRow,
    ksef_statuses: Option<&HashMap<i64, Option<bool>>>,
) -> Option<InvoiceTableRow> {
    let record = tri_row_display_record(row)?;
    let saldeo_record = row.saldeo.clone();
    let mut sources = row_source_mask(row);
    if saldeo_record.as_ref().is_some_and(saldeo_record_has_override) {
        sources.push('*');
    }
    let upload_item = row.mail.as_ref().and_then(|mail| {
        if row.saldeo.is_some() {
            return None;
        }
        let related_sources = [("mail", row.mail.as_ref()), ("ksef", row.ksef.as_ref())]
            .into_iter()
            .filter_map(|(name, record)| record.map(|_| name.to_string()))
            .collect::<Vec<_>>();
        let item = saldeo_sync_item_from_record(&row.status, mail, related_sources);
        item.can_upload.then_some(item)
    });
    let (ksef_document_id, ksef_accounting) = row
        .saldeo
        .as_ref()
        .filter(|record| {
            record
                .ksef_reference
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        })
        .and_then(saldeo_document_id)
        .map(|document_id| {
            let Some(ksef_statuses) = ksef_statuses else {
                return (None, None);
            };
            let accounting = ksef_statuses.get(&document_id).copied().flatten();
            let actionable_id = if accounting.is_none() {
                Some(document_id)
            } else {
                None
            };
            (actionable_id, accounting)
        })
        .unwrap_or((None, None));
    Some(InvoiceTableRow {
        selected: false,
        sources,
        record,
        saldeo_record,
        upload_item,
        ksef_document_id,
        ksef_accounting,
        action: InvoiceTableAction::None,
        updated: false,
    })
}

pub(crate) fn mark_updated_invoice_rows(
    previous_rows: &[InvoiceTableRow],
    new_rows: &mut [InvoiceTableRow],
) -> usize {
    let previous = previous_rows
        .iter()
        .map(|row| (invoice_table_row_key(row), invoice_table_row_signature(row)))
        .collect::<HashMap<_, _>>();
    let mut updated_count = 0usize;
    for row in new_rows {
        let key = invoice_table_row_key(row);
        let signature = invoice_table_row_signature(row);
        row.updated = previous.get(&key) != Some(&signature);
        if row.updated {
            updated_count += 1;
        }
    }
    updated_count
}

pub(crate) fn invoice_table_updated_count(rows: &[InvoiceTableRow]) -> usize {
    rows.iter().filter(|row| row.updated).count()
}

pub(crate) fn apply_invoice_record_update_to_rows(
    rows: &mut [InvoiceTableRow],
    record: &InvoiceRecord,
) -> bool {
    let mut changed = false;
    for row in rows.iter_mut() {
        if row.record.content_hash != record.content_hash {
            continue;
        }
        let before = invoice_table_row_signature(row);
        apply_invoice_record_update(&mut row.record, record);
        if invoice_table_row_signature(row) != before {
            row.updated = true;
            changed = true;
        }
    }
    changed
}

fn apply_invoice_record_update(target: &mut InvoiceRecord, source: &InvoiceRecord) {
    target.source_path = source.source_path.clone();
    target.invoice_number = source.invoice_number.clone();
    target.seller_tax_id = source.seller_tax_id.clone();
    target.buyer_tax_id = source.buyer_tax_id.clone();
    target.seller_name = source.seller_name.clone();
    target.buyer_name = source.buyer_name.clone();
    target.issue_date = source.issue_date;
    target.sale_date = source.sale_date;
    target.due_date = source.due_date;
    target.gross_amount_minor = source.gross_amount_minor;
    target.net_amount_minor = source.net_amount_minor;
    target.vat_amount_minor = source.vat_amount_minor;
    target.currency = source.currency.clone();
    target.ksef_reference = source.ksef_reference.clone();
    target.email_message_id = source.email_message_id.clone();
    target.email_subject = source.email_subject.clone();
    target.email_from = source.email_from.clone();
    target.warnings = source.warnings.clone();
}

fn invoice_table_row_key(row: &InvoiceTableRow) -> String {
    row.record.content_hash.clone()
}

fn invoice_table_row_signature(row: &InvoiceTableRow) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        row.sources,
        row.record.invoice_number.as_deref().unwrap_or(""),
        counterparty_name(Some(&row.record)),
        row.record
            .issue_date
            .map(|date| date.to_string())
            .unwrap_or_default(),
        row.record.gross_amount_minor.unwrap_or_default(),
        row.record.currency.as_deref().unwrap_or(""),
        invoice_table_ksef_status(row),
        row.upload_item.is_some(),
        row.ksef_document_id.is_some()
    )
}

pub(crate) fn invoice_table_counts(rows: &[InvoiceTableRow]) -> (usize, usize, usize, usize) {
    let mut u = 0;
    let mut a = 0;
    let mut r = 0;
    let mut s = 0;
    for row in rows {
        if row.selected {
            s += 1;
        }
        match row.action {
            InvoiceTableAction::Upload => u += 1,
            InvoiceTableAction::ApproveKsef => a += 1,
            InvoiceTableAction::RejectKsef => r += 1,
            InvoiceTableAction::None => {}
        }
    }
    (u, a, r, s)
}

pub(crate) fn invoice_table_target_indices(
    rows: &[InvoiceTableRow],
    visible: &[usize],
    table_sel: usize,
) -> Vec<usize> {
    let selected = rows
        .iter()
        .enumerate()
        .filter_map(|(idx, row)| row.selected.then_some(idx))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        visible.get(table_sel).copied().into_iter().collect()
    } else {
        selected
    }
}

pub(crate) fn collect_invoice_table_actions(
    rows: &[InvoiceTableRow],
) -> (Vec<SaldeoSyncItem>, Vec<i64>, Vec<i64>) {
    let selected_upload_items = rows
        .iter()
        .filter(|row| row.action == InvoiceTableAction::Upload)
        .filter_map(|row| row.upload_item.clone())
        .collect::<Vec<_>>();
    let selected_approve_ids = rows
        .iter()
        .filter(|row| row.action == InvoiceTableAction::ApproveKsef)
        .filter_map(|row| row.ksef_document_id)
        .collect::<Vec<_>>();
    let selected_reject_ids = rows
        .iter()
        .filter(|row| row.action == InvoiceTableAction::RejectKsef)
        .filter_map(|row| row.ksef_document_id)
        .collect::<Vec<_>>();
    (
        selected_upload_items,
        selected_approve_ids,
        selected_reject_ids,
    )
}

pub(crate) struct PendingAction {
    receiver: std::sync::mpsc::Receiver<Result<Vec<InvoiceTableRow>>>,
    description: String,
    new_year: Option<i32>,
    new_review_score: Option<u8>,
    progress: Arc<Mutex<String>>,
    record_updates: Option<std::sync::mpsc::Receiver<InvoiceRecord>>,
}

pub(crate) enum PendingActionStart {
    Started(PendingAction),
    Noop(String),
}

pub(crate) fn set_progress(progress: &Arc<Mutex<String>>, message: impl Into<String>) {
    *progress.lock().unwrap() = message.into();
}

pub(crate) fn begin_invoice_table_commit(
    rows: &[InvoiceTableRow],
    year: i32,
    review_score: u8,
) -> PendingActionStart {
    let (upload_items, approve_ids, reject_ids) = collect_invoice_table_actions(rows);
    if upload_items.is_empty() && approve_ids.is_empty() && reject_ids.is_empty() {
        return PendingActionStart::Noop(
            "Akceptuj: nic do wykonania (najpierw wybierz Upload/Zatwierdź/Odrzuć)".to_string(),
        );
    }

    let description = format!(
        "Akceptuj (upload {}, zatw. {}, odrz. {})",
        upload_items.len(),
        approve_ids.len(),
        reject_ids.len()
    );
    let rows_snapshot = rows.to_vec();
    let progress = Arc::new(Mutex::new(format!(
        "{}: przygotowanie operacji...",
        description
    )));
    let progress_clone = progress.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        redirect_stderr_to_log();
        let result =
            execute_invoice_table_actions(year, review_score, rows_snapshot, progress_clone);
        let _ = tx.send(result);
    });

    PendingActionStart::Started(PendingAction {
        receiver: rx,
        description,
        new_year: Some(year),
        new_review_score: Some(review_score),
        progress,
        record_updates: None,
    })
}

pub(crate) fn execute_invoice_table_actions(
    year: i32,
    review_score: u8,
    rows: Vec<InvoiceTableRow>,
    progress: Arc<Mutex<String>>,
) -> Result<Vec<InvoiceTableRow>> {
    let (selected_upload_items, selected_approve_ids, selected_reject_ids) =
        collect_invoice_table_actions(&rows);
    let storage_state = default_saldeo_storage_state_path();

    if !selected_upload_items.is_empty() {
        set_progress(
            &progress,
            format!(
                "Akceptuj: upload do Saldeo ({} plików)...",
                selected_upload_items.len()
            ),
        );
        let mut upload_plan = SaldeoSyncPlan {
            generated_at: Utc::now(),
            year,
            confirm: true,
            upload_url: Some(DEFAULT_SALDEO_UPLOAD_URL.to_string()),
            summary: saldeo_sync_summary(&selected_upload_items),
            items: selected_upload_items,
        };
        saldeo_upload_plan_with_progress(
            &mut upload_plan,
            &storage_state,
            DEFAULT_SALDEO_UPLOAD_URL,
            "file",
            Some(progress.clone()),
        )?;
        if upload_plan.summary.failed_count > 0 {
            let errors = upload_plan
                .items
                .iter()
                .filter(|item| item.upload_status == "failed")
                .filter_map(|item| {
                    let name = item
                        .source_path
                        .as_deref()
                        .and_then(|path| Path::new(path).file_name())
                        .and_then(|name| name.to_str())
                        .or(item.invoice_number.as_deref())
                        .unwrap_or("plik");
                    item.error.as_ref().map(|error| format!("{name}: {error}"))
                })
                .collect::<Vec<_>>()
                .join(" | ");
            return Err(anyhow!(
                "upload Saldeo nie powiódł się ({}/{} błędów): {}",
                upload_plan.summary.failed_count,
                upload_plan.summary.uploadable_count,
                errors
            ));
        }
    }

    if !selected_approve_ids.is_empty() || !selected_reject_ids.is_empty() {
        let session = read_saldeo_session(&storage_state)?;
        if !selected_approve_ids.is_empty() {
            set_progress(
                &progress,
                format!(
                    "Akceptuj: zatwierdzam KSeF w Saldeo ({} dokumentów)...",
                    selected_approve_ids.len()
                ),
            );
            saldeo_mark_ksef_documents(&session, &selected_approve_ids, true)?;
        }
        if !selected_reject_ids.is_empty() {
            set_progress(
                &progress,
                format!(
                    "Akceptuj: odrzucam KSeF w Saldeo ({} dokumentów)...",
                    selected_reject_ids.len()
                ),
            );
            saldeo_mark_ksef_documents(&session, &selected_reject_ids, false)?;
        }
    }

    set_progress(&progress, "Akceptuj: odświeżam Saldeo po zmianach...");
    saldeo_fetch_with_progress(
        year,
        &storage_state,
        &default_saldeo_out_path(year),
        Some(progress.clone()),
    )?;
    set_progress(&progress, "Akceptuj: przebudowuję tabelę...");
    build_invoice_table_rows_with_progress(year, review_score, Some(progress.clone()))
}

/// Redirect stderr to a log file for the current thread.
pub(crate) fn redirect_stderr_to_log() {
    let log_path = std::env::var("LAB_LOG").unwrap_or_else(|_| "/tmp/lab.log".to_string());
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use std::os::unix::io::IntoRawFd;
        let fd = file.into_raw_fd();
        unsafe {
            libc::dup2(fd, libc::STDERR_FILENO);
            libc::close(fd);
        }
    }
}

pub(crate) fn run_invoice_table_tui(
    rows: &mut Vec<InvoiceTableRow>,
    year: &mut i32,
    review_score: &mut u8,
    db_path: &Path,
) -> Result<TuiResult> {
    let mut terminal = ratatui::init();
    let theme = TuiTheme::detect();
    let mut table_sel = 0usize;
    let mut menu_sel = 0usize;
    let mut actionable_only = false;
    let mut editing_year: Option<String> = None;
    let mut editing_threshold: Option<String> = None;
    let mut paint_mode: bool = false;
    let mut menu_open: bool = false;
    let mut spinner_frame: usize = 0;
    let mut status_message: String = String::new();
    let mut pending_action: Option<PendingAction> = None;
    let mut loop_result: Result<TuiResult> = Ok(TuiResult::Cancel);

    // Main menu
    const MI_SYNC: usize = 0;
    const MI_RECONCILE: usize = 1;
    const MI_LLM: usize = 2;
    const MI_UPLOAD: usize = 3;
    const MI_APPROVE: usize = 4;
    const MI_REJECT: usize = 5;
    const MI_CLEAR: usize = 6;
    const MI_COMMIT: usize = 7;
    const MI_EDIT: usize = 8;
    const MI_MENU: usize = 9;
    const MAIN_COUNT: usize = 10;
    // Submenu (when menu_open)
    const SM_DOCTOR: usize = 0;
    const SM_ONBOARD: usize = 1;
    const SM_YEAR: usize = 2;
    const SM_THRESHOLD: usize = 3;
    const SM_SALDEO: usize = 4;
    const SM_BACK: usize = 5;
    const SUB_COUNT: usize = 6;

    let tick_rate = std::time::Duration::from_millis(100);
    let mut last_tick = std::time::Instant::now();

    loop {
        // Check pending async action
        if let Some(ref pending) = pending_action {
            if let Some(record_updates) = &pending.record_updates {
                loop {
                    match record_updates.try_recv() {
                        Ok(record) => {
                            if apply_invoice_record_update_to_rows(rows, &record) {
                                status_message = format!(
                                    "↻ {}: odświeżono {}",
                                    pending.description,
                                    record.invoice_number.as_deref().unwrap_or("dokument")
                                );
                            }
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                    }
                }
            }
            match pending.receiver.try_recv() {
                Ok(Ok(mut new_rows)) => {
                    if let Some(y) = pending.new_year {
                        *year = y;
                    }
                    if let Some(t) = pending.new_review_score {
                        *review_score = t;
                    }
                    let live_updated_records = if pending.record_updates.is_some() {
                        rows.iter()
                            .filter(|row| row.updated)
                            .map(|row| (invoice_table_row_key(row), row.record.clone()))
                            .collect::<HashMap<_, _>>()
                    } else {
                        HashMap::new()
                    };
                    let accepted_action_keys = if pending.description.starts_with("Akceptuj") {
                        rows.iter()
                            .filter(|row| row.action != InvoiceTableAction::None)
                            .map(invoice_table_row_key)
                            .collect::<HashSet<_>>()
                    } else {
                        HashSet::new()
                    };
                    let mut updated_count = mark_updated_invoice_rows(rows, &mut new_rows);
                    if !live_updated_records.is_empty() || !accepted_action_keys.is_empty() {
                        for row in &mut new_rows {
                            let key = invoice_table_row_key(row);
                            let was_updated = row.updated;
                            if let Some(record) = live_updated_records.get(&key) {
                                apply_invoice_record_update(&mut row.record, record);
                                row.updated = true;
                            }
                            if accepted_action_keys.contains(&key) {
                                row.updated = true;
                            }
                            if row.updated && !was_updated {
                                updated_count += 1;
                            }
                        }
                    }
                    *rows = new_rows;
                    status_message = format!(
                        "✓ {} zakończone: {} faktur, {} nowych/zmienionych",
                        pending.description,
                        rows.len(),
                        updated_count
                    );
                    pending_action = None;
                    table_sel = 0;
                }
                Ok(Err(e)) => {
                    status_message = format!("✗ Błąd {}: {e}", pending.description);
                    pending_action = None;
                    table_sel = 0;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Still running
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    status_message = format!("✗ Błąd {}: wątek przerwany", pending.description);
                    pending_action = None;
                    table_sel = 0;
                }
            }
        }

        // Tick spinner every 100ms
        if last_tick.elapsed() >= tick_rate {
            spinner_frame = spinner_frame.wrapping_add(1);
            last_tick = std::time::Instant::now();
        }

        let visible = rows
            .iter()
            .enumerate()
            .filter_map(|(idx, row)| (!actionable_only || row.needs_attention()).then_some(idx))
            .collect::<Vec<_>>();
        if visible.is_empty() {
            table_sel = 0;
        } else if table_sel >= visible.len() {
            table_sel = visible.len().saturating_sub(1);
        }
        let menu_items = if menu_open { SUB_COUNT } else { MAIN_COUNT };
        if menu_sel >= menu_items {
            menu_sel = menu_items.saturating_sub(1);
        }

        if let Err(err) = terminal.draw(|frame| {
            let area = frame.area();
            let chunks = Layout::vertical([
                Constraint::Min(5),
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

            // Table
            let header = Row::new(vec![
                Cell::from("sel"),
                Cell::from("akcja / KSeF"),
                Cell::from("G/K/S"),
                Cell::from("faktura"),
                Cell::from("kontrahent"),
                Cell::from("data"),
                Cell::from("brutto"),
                Cell::from("wal"),
            ])
            .style(theme.header());
            let table_rows = visible.iter().map(|vidx| {
                let row = &rows[*vidx];
                let sel_mark = if pending_action.is_some() && row.selected {
                    let spinner_chars = ['◐', '◓', '◑', '◒'];
                    let spin = spinner_chars[spinner_frame % spinner_chars.len()];
                    format!("[{}]", spin)
                } else if row.selected {
                    "[*]".to_string()
                } else {
                    "[ ]".to_string()
                };
                let style = if row.updated {
                    theme.updated()
                } else {
                    match row.action {
                        InvoiceTableAction::Upload => theme.upload(),
                        InvoiceTableAction::ApproveKsef => theme.approve(),
                        InvoiceTableAction::RejectKsef => theme.reject(),
                        InvoiceTableAction::None if row.needs_attention() => theme.neutral(),
                        InvoiceTableAction::None => theme.very_muted(),
                    }
                };
                Row::new(vec![
                    Cell::from(sel_mark),
                    Cell::from(invoice_table_action_ksef_label(row)),
                    Cell::from(row.sources.clone()),
                    Cell::from(truncate(
                        row.record.invoice_number.as_deref().unwrap_or("-"),
                        24,
                    )),
                    Cell::from(truncate(&counterparty_name(Some(&row.record)), 26)),
                    Cell::from(
                        row.record
                            .issue_date
                            .map(|date| date.to_string())
                            .unwrap_or_else(|| "-".to_string()),
                    ),
                    Cell::from(
                        row.record
                            .gross_amount_minor
                            .map(format_minor_money)
                            .unwrap_or_else(|| "-".to_string()),
                    ),
                    Cell::from(row.record.currency.as_deref().unwrap_or("-")),
                ])
                .style(style)
            });
            let table = Table::new(
                table_rows,
                [
                    Constraint::Percentage(4),
                    Constraint::Percentage(14),
                    Constraint::Percentage(7),
                    Constraint::Percentage(20),
                    Constraint::Percentage(22),
                    Constraint::Percentage(10),
                    Constraint::Percentage(16),
                    Constraint::Percentage(7),
                ],
            )
            .header(header)
            .block(
                Block::default()
                    .title(format!(" LAB faktury {year} · próg {review_score} "))
                    .borders(Borders::ALL)
                    .border_style(theme.very_muted()),
            )
            .row_highlight_style(theme.table_highlight());
            let mut table_state = TableState::default();
            if !visible.is_empty() {
                table_state.select(Some(table_sel));
            }
            frame.render_stateful_widget(table, chunks[0], &mut table_state);

            // Menu bar
            let (u, a, r, sel) = invoice_table_counts(rows);
            let year_text = editing_year.clone().unwrap_or_else(|| year.to_string());
            let threshold_text = editing_threshold
                .clone()
                .unwrap_or_else(|| review_score.to_string());
            let mkbtn = |label: &str, idx: usize, editing: bool| {
                let text = if editing {
                    format!("[ {}_ ]", label)
                } else {
                    format!("[ {} ]", label)
                };
                let style = if idx == menu_sel || editing {
                    theme.selected()
                } else {
                    theme.inactive_button()
                };
                ratatui::text::Line::styled(text, style)
            };
            if menu_open {
                let items = [
                    mkbtn("Doctor", SM_DOCTOR, false),
                    mkbtn("Onboard", SM_ONBOARD, false),
                    mkbtn(
                        &format!("Rok:{}", year_text),
                        SM_YEAR,
                        editing_year.is_some(),
                    ),
                    mkbtn(
                        &format!("Próg:{}", threshold_text),
                        SM_THRESHOLD,
                        editing_threshold.is_some(),
                    ),
                    mkbtn("Saldeo", SM_SALDEO, false),
                    mkbtn("◀ Wróć", SM_BACK, false),
                ];
                let mut sub_constraints = vec![Constraint::Fill(1)];
                for (i, line) in items.clone().iter().enumerate() {
                    let width = line.width() as u16;
                    sub_constraints.push(Constraint::Length(width));
                    if i < items.len() - 1 {
                        sub_constraints.push(Constraint::Length(1));
                    }
                }
                let menu_area = Layout::horizontal(sub_constraints).split(chunks[1]);
                for (i, line) in items.into_iter().enumerate() {
                    frame.render_widget(Paragraph::new(line), menu_area[i * 2 + 1]);
                }
            } else {
                let items = [
                    mkbtn("Sync", MI_SYNC, false),
                    mkbtn("Reconcile", MI_RECONCILE, false),
                    mkbtn("LLM", MI_LLM, false),
                    mkbtn("Upload", MI_UPLOAD, false),
                    mkbtn("Zatwierdź", MI_APPROVE, false),
                    mkbtn("Odrzuć", MI_REJECT, false),
                    mkbtn("Wyczyść", MI_CLEAR, false),
                    mkbtn("Akceptuj", MI_COMMIT, false),
                    mkbtn("Popraw", MI_EDIT, false),
                    mkbtn("☰ Menu", MI_MENU, false),
                ];
                let mut main_constraints = vec![];
                for (i, line) in items.clone().iter().enumerate() {
                    let width = line.width() as u16;
                    main_constraints.push(Constraint::Length(width));
                    if i < items.len() - 2 {
                        main_constraints.push(Constraint::Length(1));
                    } else if i == items.len() - 2 {
                        // Gap before Menu is Fill to push it right
                        main_constraints.push(Constraint::Fill(1));
                    }
                }
                let menu_area = Layout::horizontal(main_constraints).split(chunks[1]);
                let item_count = items.len();
                for (i, line) in items.into_iter().enumerate() {
                    let area_idx = if i == item_count - 1 {
                        menu_area.len() - 1
                    } else {
                        i * 2
                    };
                    frame.render_widget(Paragraph::new(line), menu_area[area_idx]);
                }
            }
            // Status bar
            let active_msg = if let Some(ref pending) = pending_action {
                let progress = pending.progress.lock().unwrap();
                if progress.is_empty() {
                    status_message.clone()
                } else {
                    progress.clone()
                }
            } else if !status_message.is_empty() {
                status_message.clone()
            } else {
                String::new()
            };
            if !active_msg.is_empty() {
                let (prefix, color) = if active_msg.starts_with("✓") {
                    ("".to_string(), Color::Green)
                } else if active_msg.starts_with("✗") {
                    ("".to_string(), Color::Red)
                } else if pending_action.is_some() {
                    let spinner_chars = ['◐', '◓', '◑', '◒'];
                    let spin = spinner_chars[spinner_frame % spinner_chars.len()];
                    (spin.to_string(), theme.status_pending())
                } else {
                    ("".to_string(), theme.status_pending())
                };
                let full_text = if prefix.is_empty() {
                    format!(" {}", active_msg)
                } else {
                    format!(" {} {}", prefix, active_msg)
                };
                let max_width = chunks[2].width as usize;
                let display_text = if full_text.chars().count() > max_width {
                    format!("{}...", &full_text[..max_width.saturating_sub(3)])
                } else {
                    full_text
                };
                let line = ratatui::text::Line::styled(
                    display_text,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                );
                frame.render_widget(Paragraph::new(line), chunks[2]);
            }

            // Stats bar — stats left, help right
            let upd = invoice_table_updated_count(rows);
            let stats_text = format!(
                "sel:{} | up:{} | zatw:{} | odrz:{} | zm:{} | {}/{}",
                sel,
                u,
                a,
                r,
                upd,
                visible.len(),
                rows.len()
            );
            let help = "f=braki/zmiany  e=popraw  spc=toggle  ⏎=select  ⌘c=commit  q=wyjdź".to_string();
            let stats_span = ratatui::text::Span::styled(
                stats_text,
                theme.muted().add_modifier(Modifier::ITALIC),
            );
            let help_span = ratatui::text::Span::styled(help, theme.very_muted());
            let stats_area = Layout::horizontal([
                Constraint::Fill(1),
                Constraint::Length(help_span.width() as u16),
            ])
            .split(chunks[3]);
            frame.render_widget(
                Paragraph::new(ratatui::text::Line::from(stats_span)),
                stats_area[0],
            );
            frame.render_widget(
                Paragraph::new(ratatui::text::Line::from(help_span)),
                stats_area[1],
            );
        }) {
            loop_result = Err(anyhow!("terminal draw: {err}"));
            break;
        }

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| std::time::Duration::from_secs(0));
        if event::poll(timeout)? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let cmd = key
                .modifiers
                .intersects(crossterm::event::KeyModifiers::SUPER)
                || key
                    .modifiers
                    .intersects(crossterm::event::KeyModifiers::META);
            let shift = key
                .modifiers
                .intersects(crossterm::event::KeyModifiers::SHIFT);

            // Handle field editing
            let editing = editing_year.is_some() || editing_threshold.is_some();
            if editing {
                let field = if editing_year.is_some() {
                    editing_year.as_mut()
                } else {
                    editing_threshold.as_mut()
                };
                match key.code {
                    KeyCode::Esc => {
                        editing_year = None;
                        editing_threshold = None;
                    }
                    KeyCode::Enter => {
                        if let Some(val) = editing_year.take() {
                            if let Ok(y) = val.parse::<i32>() {
                                let score = *review_score;
                                let (tx, rx) = std::sync::mpsc::channel();
                                let progress =
                                    Arc::new(Mutex::new(format!("Przebudowa (rok {y})...")));
                                let progress_clone = progress.clone();
                                std::thread::spawn(move || {
                                    redirect_stderr_to_log();
                                    let _ = tx.send(build_invoice_table_rows_with_progress(
                                        y,
                                        score,
                                        Some(progress_clone),
                                    ));
                                });
                                pending_action = Some(PendingAction {
                                    receiver: rx,
                                    description: "Rebuild".to_string(),
                                    new_year: Some(y),
                                    new_review_score: Some(score),
                                    progress,
                                    record_updates: None,
                                });
                            }
                        } else if let Some(val) = editing_threshold.take()
                            && let Ok(t) = val.parse::<u8>()
                        {
                            let y = *year;
                            let (tx, rx) = std::sync::mpsc::channel();
                            let progress =
                                Arc::new(Mutex::new(format!("Przebudowa (próg {t})...")));
                            let progress_clone = progress.clone();
                            std::thread::spawn(move || {
                                redirect_stderr_to_log();
                                let _ = tx.send(build_invoice_table_rows_with_progress(
                                    y,
                                    t,
                                    Some(progress_clone),
                                ));
                            });
                            pending_action = Some(PendingAction {
                                receiver: rx,
                                description: "Rebuild".to_string(),
                                new_year: Some(y),
                                new_review_score: Some(t),
                                progress,
                                record_updates: None,
                            });
                        }
                    }
                    KeyCode::Backspace => {
                        if let Some(s) = field {
                            s.pop();
                        }
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() => {
                        if let Some(s) = field {
                            s.push(c);
                        }
                    }
                    _ => {}
                }
                continue;
            }

            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Esc => {
                    if menu_open {
                        menu_open = false;
                    } else {
                        break;
                    }
                }
                KeyCode::Char('c') if cmd => {
                    if pending_action.is_some() {
                        status_message = "Trwa operacja — poczekaj na zakończenie".to_string();
                    } else {
                        match begin_invoice_table_commit(rows, *year, *review_score) {
                            PendingActionStart::Started(action) => pending_action = Some(action),
                            PendingActionStart::Noop(message) => status_message = message,
                        }
                    }
                }
                KeyCode::Enter if cmd => {
                    if pending_action.is_some() {
                        status_message = "Trwa operacja — poczekaj na zakończenie".to_string();
                    } else {
                        match begin_invoice_table_commit(rows, *year, *review_score) {
                            PendingActionStart::Started(action) => pending_action = Some(action),
                            PendingActionStart::Noop(message) => status_message = message,
                        }
                    }
                }
                KeyCode::Enter => {
                    if pending_action.is_some() {
                        status_message = "Trwa operacja — poczekaj na zakończenie".to_string();
                        continue;
                    }
                    if menu_open {
                        match menu_sel {
                            SM_DOCTOR => {
                                loop_result = Ok(TuiResult::Doctor);
                                break;
                            }
                            SM_ONBOARD => {
                                loop_result = Ok(TuiResult::Onboard);
                                break;
                            }
                            SM_YEAR => editing_year = Some(year.to_string()),
                            SM_THRESHOLD => editing_threshold = Some(review_score.to_string()),
                            SM_SALDEO => {
                                let y = *year;
                                let score = *review_score;
                                let db = db_path.to_path_buf();
                                let (tx, rx) = std::sync::mpsc::channel();
                                let progress = Arc::new(Mutex::new(
                                    "Saldeo: sprawdzam zapisaną sesję...".to_string(),
                                ));
                                let progress_clone = progress.clone();
                                std::thread::spawn(move || {
                                    redirect_stderr_to_log();
                                    let result = (|| -> Result<Vec<InvoiceTableRow>> {
                                        ensure_saldeo_session_or_auth(Some(
                                            progress_clone.clone(),
                                        ))?;
                                        set_progress(
                                            &progress_clone,
                                            "Saldeo: odświeżanie danych...",
                                        );
                                        sync_reconcile_metadata_with_progress(
                                            y,
                                            false,
                                            true,
                                            &db,
                                            Some(progress_clone.clone()),
                                        )?;
                                        set_progress(
                                            &progress_clone,
                                            "Saldeo: budowanie tabeli...",
                                        );
                                        build_invoice_table_rows_with_progress(
                                            y,
                                            score,
                                            Some(progress_clone.clone()),
                                        )
                                    })();
                                    let _ = tx.send(result);
                                });
                                pending_action = Some(PendingAction {
                                    receiver: rx,
                                    description: "Saldeo".to_string(),
                                    new_year: Some(y),
                                    new_review_score: Some(score),
                                    progress,
                                    record_updates: None,
                                });
                            }
                            SM_BACK => {
                                menu_open = false;
                                menu_sel = MI_MENU;
                            }
                            _ => {}
                        }
                    } else {
                        match menu_sel {
                            MI_SYNC => {
                                let y = *year;
                                let score = *review_score;
                                let db = db_path.to_path_buf();
                                let (tx, rx) = std::sync::mpsc::channel();
                                let progress = Arc::new(Mutex::new(format!(
                                    "Pełny sync dla roku {y} (KSeF + Gmail/PDF + Saldeo)..."
                                )));
                                let progress_clone = progress.clone();
                                std::thread::spawn(move || {
                                    redirect_stderr_to_log();
                                    let result = (|| -> Result<Vec<InvoiceTableRow>> {
                                        set_progress(
                                            &progress_clone,
                                            format!(
                                                "Sync: start dla roku {y} (KSeF → Gmail/PDF → Saldeo)..."
                                            ),
                                        );
                                        let conn = open_db(&db)?;
                                        run_sync_sources_with_progress(
                                            y,
                                            false,
                                            false,
                                            false,
                                            None,
                                            None,
                                            None,
                                            DEFAULT_PRODUCTMESH_NIP,
                                            Some(&conn),
                                            Some(progress_clone.clone()),
                                        )?;
                                        set_progress(&progress_clone, "Sync: budowanie tabeli...");
                                        build_invoice_table_rows_with_progress(
                                            y,
                                            score,
                                            Some(progress_clone.clone()),
                                        )
                                    })();
                                    let _ = tx.send(result);
                                });
                                pending_action = Some(PendingAction {
                                    receiver: rx,
                                    description: "Sync".to_string(),
                                    new_year: Some(y),
                                    new_review_score: Some(score),
                                    progress,
                                    record_updates: None,
                                });
                            }
                            MI_RECONCILE => {
                                let y = *year;
                                let t = *review_score;
                                let db = db_path.to_path_buf();
                                let (tx, rx) = std::sync::mpsc::channel();
                                let progress = Arc::new(Mutex::new(format!(
                                    "Reconcile: metadane KSeF/Saldeo dla roku {y}..."
                                )));
                                let progress_clone = progress.clone();
                                std::thread::spawn(move || {
                                    redirect_stderr_to_log();
                                    let result = (|| -> Result<Vec<InvoiceTableRow>> {
                                        set_progress(
                                            &progress_clone,
                                            "Reconcile: odświeżanie metadanych KSeF/Saldeo...",
                                        );
                                        sync_reconcile_metadata_with_progress(
                                            y,
                                            true,
                                            true,
                                            &db,
                                            Some(progress_clone.clone()),
                                        )?;
                                        set_progress(
                                            &progress_clone,
                                            "Reconcile: budowanie tabeli...",
                                        );
                                        build_invoice_table_rows_with_progress(
                                            y,
                                            t,
                                            Some(progress_clone.clone()),
                                        )
                                    })();
                                    let _ = tx.send(result);
                                });
                                pending_action = Some(PendingAction {
                                    receiver: rx,
                                    description: "Reconcile".to_string(),
                                    new_year: Some(y),
                                    new_review_score: Some(t),
                                    progress,
                                    record_updates: None,
                                });
                            }
                            MI_LLM => {
                                let y = *year;
                                let score = *review_score;
                                let db = db_path.to_path_buf();
                                let selected_hashes: Vec<String> = rows
                                    .iter()
                                    .filter(|r| r.selected && r.sources.contains('G'))
                                    .map(|r| r.record.content_hash.clone())
                                    .collect();
                                let has_selection = !selected_hashes.is_empty();
                                let selected_hashes_clone = selected_hashes.clone();
                                let (tx, rx) = std::sync::mpsc::channel();
                                let (record_tx, record_rx) = std::sync::mpsc::channel();
                                let progress = Arc::new(Mutex::new(if has_selection {
                                    format!(
                                        "LLM: przygotowanie {} faktur...",
                                        selected_hashes.len()
                                    )
                                } else {
                                    format!("LLM: wczytywanie faktur dla roku {y}...")
                                }));
                                let progress_clone = progress.clone();
                                std::thread::spawn(move || {
                                    redirect_stderr_to_log();
                                    let result = (|| -> Result<Vec<InvoiceTableRow>> {
                                        let mail_path = default_mail_candidates_path(y);
                                        if mail_path.exists() {
                                            *progress_clone.lock().unwrap() =
                                                "LLM: wczytywanie faktur...".to_string();
                                            let mut candidates =
                                                load_records(SourceKind::Mail, &mail_path)?;
                                            let conn = open_db(&db)?;
                                            if has_selection {
                                                let mut to_enrich: Vec<InvoiceRecord> = candidates
                                                    .iter()
                                                    .filter(|c| {
                                                        selected_hashes_clone
                                                            .contains(&c.content_hash)
                                                    })
                                                    .cloned()
                                                    .collect();
                                                // Wyczyść pola aby wymusić ponowne parsowanie
                                                for r in &mut to_enrich {
                                                    r.issue_date = None;
                                                    r.gross_amount_minor = None;
                                                    r.net_amount_minor = None;
                                                    r.vat_amount_minor = None;
                                                    r.currency = None;
                                                    r.seller_name = None;
                                                    r.buyer_name = None;
                                                    r.seller_tax_id = None;
                                                    r.buyer_tax_id = None;
                                                    r.sale_date = None;
                                                    r.due_date = None;
                                                    r.warnings.clear();
                                                }
                                                if !to_enrich.is_empty() {
                                                    *progress_clone.lock().unwrap() = format!(
                                                        "LLM: parsowanie {} faktur...",
                                                        to_enrich.len()
                                                    );
                                                    let empty_skip =
                                                        std::collections::HashSet::new();
                                                    enrich_candidates_with_gemma_with_hook(
                                                        &mut to_enrich,
                                                        &empty_skip,
                                                        Some(progress_clone.clone()),
                                                        |enriched_records, idx| {
                                                            let enriched =
                                                                enriched_records[idx].clone();
                                                            if let Some(pos) =
                                                                candidates.iter().position(|c| {
                                                                    c.content_hash
                                                                        == enriched.content_hash
                                                                })
                                                            {
                                                                candidates[pos] = enriched.clone();
                                                            }
                                                            set_progress(
                                                                &progress_clone,
                                                                format!(
                                                                    "LLM: zapis {}/{} do pliku i DB...",
                                                                    idx + 1,
                                                                    enriched_records.len()
                                                                ),
                                                            );
                                                            write_records(
                                                                &candidates,
                                                                OutputFormat::Jsonl,
                                                                Some(&mail_path),
                                                            )?;
                                                            store_records(
                                                                &conn,
                                                                std::slice::from_ref(&enriched),
                                                            )?;
                                                            let _ = record_tx.send(enriched);
                                                            Ok(())
                                                        },
                                                    )?;
                                                    set_progress(
                                                        &progress_clone,
                                                        "LLM: zapis per dokument zakończony",
                                                    );
                                                }
                                            } else {
                                                let cached = apply_cached_mail_candidates(
                                                    y,
                                                    &mut candidates,
                                                )?;
                                                *progress_clone.lock().unwrap() =
                                                    "LLM: parsowanie faktur...".to_string();
                                                enrich_candidates_with_gemma_with_hook(
                                                    &mut candidates,
                                                    &cached,
                                                    Some(progress_clone.clone()),
                                                    |all_records, idx| {
                                                        let enriched = all_records[idx].clone();
                                                        set_progress(
                                                            &progress_clone,
                                                            format!(
                                                                "LLM: zapis {}/{} do pliku i DB...",
                                                                idx + 1,
                                                                all_records.len()
                                                            ),
                                                        );
                                                        write_records(
                                                            all_records,
                                                            OutputFormat::Jsonl,
                                                            Some(&mail_path),
                                                        )?;
                                                        store_records(
                                                            &conn,
                                                            std::slice::from_ref(&enriched),
                                                        )?;
                                                        let _ = record_tx.send(enriched);
                                                        Ok(())
                                                    },
                                                )?;
                                                set_progress(
                                                    &progress_clone,
                                                    "LLM: zapis per dokument zakończony",
                                                );
                                            }
                                        }
                                        set_progress(&progress_clone, "LLM: budowanie tabeli...");
                                        build_invoice_table_rows_with_progress(
                                            y,
                                            score,
                                            Some(progress_clone.clone()),
                                        )
                                    })();
                                    let _ = tx.send(result);
                                });
                                pending_action = Some(PendingAction {
                                    receiver: rx,
                                    description: if has_selection {
                                        "LLM (wybrane)".to_string()
                                    } else {
                                        "LLM".to_string()
                                    },
                                    new_year: Some(y),
                                    new_review_score: None,
                                    progress,
                                    record_updates: Some(record_rx),
                                });
                            }
                            MI_UPLOAD => {
                                let targets =
                                    invoice_table_target_indices(rows, &visible, table_sel);
                                let mut changed = 0usize;
                                for idx in targets {
                                    if rows[idx].can_upload() {
                                        rows[idx].action = InvoiceTableAction::Upload;
                                        rows[idx].selected = true;
                                        changed += 1;
                                    }
                                }
                                status_message = if changed > 0 {
                                    format!(
                                        "Upload: oznaczono {changed}; użyj Akceptuj, żeby wysłać"
                                    )
                                } else {
                                    "Upload: brak wybranych/podświetlonych faktur do wysłania do Saldeo".to_string()
                                };
                            }
                            MI_APPROVE => {
                                let targets =
                                    invoice_table_target_indices(rows, &visible, table_sel);
                                let mut changed = 0usize;
                                for idx in targets {
                                    if rows[idx].can_mark_ksef() {
                                        rows[idx].action = InvoiceTableAction::ApproveKsef;
                                        rows[idx].selected = true;
                                        changed += 1;
                                    }
                                }
                                status_message = if changed > 0 {
                                    format!(
                                        "Zatwierdź: oznaczono {changed}; użyj Akceptuj, żeby wysłać do Saldeo"
                                    )
                                } else {
                                    "Zatwierdź: brak dokumentów Saldeo/KSeF do oznaczenia (wybierz wiersz „nieozn.”)".to_string()
                                };
                            }
                            MI_REJECT => {
                                let targets =
                                    invoice_table_target_indices(rows, &visible, table_sel);
                                let mut changed = 0usize;
                                for idx in targets {
                                    if rows[idx].can_mark_ksef() {
                                        rows[idx].action = InvoiceTableAction::RejectKsef;
                                        rows[idx].selected = true;
                                        changed += 1;
                                    }
                                }
                                status_message = if changed > 0 {
                                    format!(
                                        "Odrzuć: oznaczono {changed}; użyj Akceptuj, żeby wysłać do Saldeo"
                                    )
                                } else {
                                    "Odrzuć: brak dokumentów Saldeo/KSeF do oznaczenia (wybierz wiersz „nieozn.”)".to_string()
                                };
                            }
                            MI_CLEAR => {
                                for row in rows.iter_mut().filter(|r| r.selected) {
                                    row.action = InvoiceTableAction::None;
                                    row.selected = false;
                                }
                            }
                            MI_COMMIT => {
                                match begin_invoice_table_commit(rows, *year, *review_score) {
                                    PendingActionStart::Started(action) => {
                                        pending_action = Some(action)
                                    }
                                    PendingActionStart::Noop(message) => status_message = message,
                                }
                            }
                            MI_MENU => {
                                menu_open = true;
                                menu_sel = SM_BACK;
                            }
                            _ => {}
                        }
                    }
                }
                KeyCode::Down => {
                    if table_sel + 1 < visible.len() {
                        table_sel += 1;
                        if (paint_mode || shift)
                            && let Some(row_idx) = visible.get(table_sel).copied()
                        {
                            rows[row_idx].selected = !rows[row_idx].selected;
                        }
                    }
                }
                KeyCode::Up => {
                    if table_sel > 0 {
                        table_sel -= 1;
                        if (paint_mode || shift)
                            && let Some(row_idx) = visible.get(table_sel).copied()
                        {
                            rows[row_idx].selected = !rows[row_idx].selected;
                        }
                    }
                }
                KeyCode::Right => {
                    let max = if menu_open { SUB_COUNT } else { MAIN_COUNT };
                    menu_sel = (menu_sel + 1).min(max.saturating_sub(1));
                }
                KeyCode::Left => {
                    menu_sel = menu_sel.saturating_sub(1);
                }
                KeyCode::Home => {
                    table_sel = 0;
                    if paint_mode && let Some(row_idx) = visible.get(table_sel).copied() {
                        rows[row_idx].selected = !rows[row_idx].selected;
                    }
                }
                KeyCode::End => {
                    table_sel = visible.len().saturating_sub(1);
                    if paint_mode && let Some(row_idx) = visible.get(table_sel).copied() {
                        rows[row_idx].selected = !rows[row_idx].selected;
                    }
                }
                KeyCode::Char('f') => {
                    actionable_only = !actionable_only;
                    table_sel = 0;
                }
                KeyCode::Char('v') => {
                    paint_mode = !paint_mode;
                }
                KeyCode::Char(' ') => {
                    if let Some(row_idx) = visible.get(table_sel).copied() {
                        rows[row_idx].selected = !rows[row_idx].selected;
                    }
                }
                _ => {}
            }
        }
    }

    ratatui::restore();
    loop_result
}

pub(crate) fn invoice_table_action_ksef_label(row: &InvoiceTableRow) -> String {
    match row.action {
        InvoiceTableAction::Upload => "UPLOAD".to_string(),
        InvoiceTableAction::ApproveKsef => "ZATWIERDŹ".to_string(),
        InvoiceTableAction::RejectKsef => "ODRZUĆ".to_string(),
        InvoiceTableAction::None => invoice_table_ksef_status(row),
    }
}

pub(crate) fn invoice_table_ksef_status(row: &InvoiceTableRow) -> String {
    match (row.ksef_document_id, row.ksef_accounting) {
        (Some(_), None) => "nieozn.".to_string(),
        (_, Some(true)) => "zatw.".to_string(),
        (_, Some(false)) => "odrz.".to_string(),
        _ => "—".to_string(),
    }
}

pub(crate) fn row_source_mask(row: &TriRow) -> String {
    format!(
        "{}/{}/{}",
        if row.mail.is_some() { "G" } else { "-" },
        if row.ksef.is_some() { "K" } else { "-" },
        if row.saldeo.is_some() { "S" } else { "-" }
    )
}

#[derive(Debug, Clone)]
pub(crate) struct SaldeoKsefAccountingCandidate {
    document_id: i64,
}

pub(crate) fn saldeo_ksef_accounting_candidates(
    records: &[InvoiceRecord],
) -> Vec<SaldeoKsefAccountingCandidate> {
    let mut seen = HashSet::new();
    records
        .iter()
        .filter(|record| record.ksef_reference.is_some())
        .filter_map(|record| {
            let document_id = saldeo_document_id(record)?;
            if !seen.insert(document_id) {
                return None;
            }
            Some(SaldeoKsefAccountingCandidate { document_id })
        })
        .collect()
}

pub(crate) fn saldeo_document_id(record: &InvoiceRecord) -> Option<i64> {
    record.content_hash.strip_prefix("saldeo:")?.parse().ok()
}

pub(crate) fn saldeo_fetch_ksef_accounting_statuses(
    session: &SaldeoSession,
    document_ids: &[i64],
) -> Result<HashMap<i64, Option<bool>>> {
    if document_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let client = Client::builder().build()?;
    let body = serde_json::json!({"clientId": 0, "documentIds": document_ids});
    let response: Value = client
        .post("https://saldeo.brainshare.pl/rest/client/document/ksef/accounting")
        .header("Cookie", &session.cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", &session.xsrf)
        .header("saldeoApp", "angularApp")
        .json(&body)
        .send()?
        .error_for_status()?
        .json()?;
    if response.get("status").and_then(|v| v.as_str()) != Some("SUCCESS") {
        return Err(anyhow!("Saldeo KSeF accounting status failed: {response}"));
    }
    let mut out = HashMap::new();
    for item in response
        .get("data")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        if let Some(document_id) = item.get("documentId").and_then(|v| v.as_i64()) {
            out.insert(
                document_id,
                item.get("accounting").and_then(|v| v.as_bool()),
            );
        }
    }
    Ok(out)
}

pub(crate) fn saldeo_mark_ksef_documents(
    session: &SaldeoSession,
    document_ids: &[i64],
    mark_is_accounting: bool,
) -> Result<Value> {
    let client = Client::builder().build()?;
    let body = serde_json::json!({
        "ids": document_ids,
        "markIsAccounting": mark_is_accounting,
        "skipPreviousSetup": true,
    });
    let response: Value = client
        .post("https://saldeo.brainshare.pl/rest/client/document/list/bulkupdate/markAccounting")
        .header("Cookie", &session.cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", &session.xsrf)
        .header("saldeoApp", "angularApp")
        .json(&body)
        .send()?
        .error_for_status()?
        .json()?;
    if response.get("status").and_then(|v| v.as_str()) != Some("SUCCESS") {
        return Err(anyhow!("Saldeo markAccounting failed: {response}"));
    }
    Ok(response)
}
