use crate::*;

const SORT_LABELS: [&str; 7] = [
    "domyślne",
    "data",
    "faktura",
    "kontrahent",
    "brutto",
    "waluta",
    "źródła",
];

fn compare_sort_values<T: Ord>(a: Option<T>, b: Option<T>, descending: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Some(a), Some(b)) => {
            if descending {
                b.cmp(&a)
            } else {
                a.cmp(&b)
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_invoice_sort(
    a: &InvoiceRecord,
    b: &InvoiceRecord,
    column: usize,
    descending: bool,
) -> std::cmp::Ordering {
    match column {
        1 => compare_sort_values(a.issue_date, b.issue_date, descending),
        2 => compare_sort_values(
            a.invoice_number.as_ref().map(|s| s.to_lowercase()),
            b.invoice_number.as_ref().map(|s| s.to_lowercase()),
            descending,
        ),
        3 => compare_sort_values(
            Some(counterparty_name(Some(a)).to_lowercase()),
            Some(counterparty_name(Some(b)).to_lowercase()),
            descending,
        ),
        4 => compare_sort_values(a.gross_amount_minor, b.gross_amount_minor, descending),
        5 => compare_sort_values(
            a.currency.as_ref().map(|s| s.to_lowercase()),
            b.currency.as_ref().map(|s| s.to_lowercase()),
            descending,
        ),
        _ => std::cmp::Ordering::Equal,
    }
}

#[cfg(test)]
mod sorting_tests;
#[cfg(test)]
mod tui_action_tests;
#[cfg(test)]
mod tui_keys_tests;
#[cfg(test)]
mod tui_ksef_read_tests;
#[cfg(test)]
mod tui_status_tests;
#[cfg(test)]
mod tui_stderr_tests;
#[cfg(test)]
mod tui_theme_tests;
#[cfg(test)]
mod tui_warnings_tests;
#[cfg(test)]
mod tui_year_llm_tests;

pub(crate) fn interactive_tui(db_path: &Path) -> Result<()> {
    interactive_reconcile_actions(db_path)
}

pub(crate) fn interactive_reconcile_actions(db_path: &Path) -> Result<()> {
    let mut year: i32 = cli::default_year();
    let mut review_score: u8 = cli::DEFAULT_REVIEW_SCORE;
    if !saldeo_session_valid(&default_saldeo_storage_state_path()) {
        eprintln!(
            "  [Saldeo] sesja nieważna — otwieram tabelę. Menu → Saldeo, żeby zalogować Helium."
        );
    }
    let mut rows = build_invoice_table_rows(year, review_score, db_path)?;

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
                if edit_saldeo_record_override(&record, db_path)? {
                    rows = build_invoice_table_rows(year, review_score, db_path)?;
                }
            }
        }
    }
}

pub(crate) enum TuiResult {
    Cancel,
    Doctor,
    Onboard,
    Correct(Box<InvoiceRecord>),
}

#[derive(Debug, Clone, Copy)]
struct TuiTheme {
    light: bool,
}

/// Zmienne środowiska, z których wynika motyw TUI.
#[derive(Debug, Clone, Default)]
struct ThemeInputs {
    /// `LAB_THEME=light|dark`; inna wartość jest pomijana.
    lab_theme: Option<String>,
    lab_tui_theme: Option<String>,
    term_background: Option<String>,
    colorfgbg: Option<String>,
    term_program: Option<String>,
}

/// Czy motyw jasny. Kolejność: `LAB_THEME` (light/dark), `LAB_TUI_THEME`, `TERM_BACKGROUND`,
/// `COLORFGBG` (tło 7 albo ≥15 = jasne); potem, tylko w Terminal.app (`TERM_PROGRAM=Apple_Terminal`),
/// wygląd systemu z `system_is_dark` (`None` = nieznany); w pozostałych przypadkach ciemny.
/// `system_is_dark` jest wołane tylko wtedy, gdy nic wcześniej nie rozstrzygnęło.
fn theme_is_light(inputs: &ThemeInputs, system_is_dark: impl FnOnce() -> Option<bool>) -> bool {
    match inputs
        .lab_theme
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("light") => return true,
        Some("dark") => return false,
        _ => {}
    }
    if let Some(value) = &inputs.lab_tui_theme {
        return matches!(value.to_ascii_lowercase().as_str(), "light" | "jasny");
    }
    if let Some(value) = &inputs.term_background {
        return value.eq_ignore_ascii_case("light");
    }
    if let Some(bg) = inputs.colorfgbg.as_deref().and_then(|value| {
        value
            .split(';')
            .next_back()
            .and_then(|bg| bg.parse::<u8>().ok())
    }) {
        return bg == 7 || bg >= 15;
    }
    if inputs.term_program.as_deref() == Some("Apple_Terminal") {
        return system_is_dark() == Some(false);
    }
    false
}

/// Wygląd systemu macOS: `defaults read -g AppleInterfaceStyle` wypisuje `Dark` w trybie
/// ciemnym, a w jasnym kończy się błędem. `None`, gdy nie udało się tego ustalić w 1 s.
fn macos_system_appearance_is_dark() -> Option<bool> {
    let mut command = Command::new("/usr/bin/defaults");
    apply_isolated_env(&mut command);
    let mut child = command
        .args(["read", "-g", "AppleInterfaceStyle"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < Duration::from_secs(1) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return Some(false);
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out.trim().eq_ignore_ascii_case("dark"))
}

impl TuiTheme {
    fn detect() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        let inputs = ThemeInputs {
            lab_theme: var("LAB_THEME"),
            lab_tui_theme: var("LAB_TUI_THEME"),
            term_background: var("TERM_BACKGROUND"),
            colorfgbg: var("COLORFGBG"),
            term_program: var("TERM_PROGRAM"),
        };
        Self {
            light: theme_is_light(&inputs, macos_system_appearance_is_dark),
        }
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

    fn warning(self) -> Style {
        Style::default().fg(if self.light {
            Color::Magenta
        } else {
            Color::Yellow
        })
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
    pub(crate) selected: bool,
    pub(crate) sources: String,
    /// Display record: always `tri_row_display_record` over the parts below.
    pub(crate) record: InvoiceRecord,
    /// Source parts of the reconciled row; an LLM result replaces only `mail_record`.
    pub(crate) mail_record: Option<InvoiceRecord>,
    pub(crate) ksef_record: Option<InvoiceRecord>,
    pub(crate) saldeo_record: Option<InvoiceRecord>,
    pub(crate) upload_item: Option<SaldeoSyncItem>,
    pub(crate) ksef_document_id: Option<i64>,
    pub(crate) ksef_accounting: Option<bool>,
    pub(crate) action: InvoiceTableAction,
    pub(crate) updated: bool,
}

impl InvoiceTableRow {
    fn is_actionable(&self) -> bool {
        self.upload_item.is_some() || self.ksef_document_id.is_some()
    }

    fn is_approved_in_ksef_and_saldeo(&self) -> bool {
        let [_, ksef, saldeo] = invoice_table_source_parts(&self.sources);
        ksef == "K" && saldeo == "S" && self.ksef_accounting == Some(true)
    }

    fn needs_attention(&self) -> bool {
        if self.is_approved_in_ksef_and_saldeo() {
            return false;
        }
        self.is_actionable()
            || self.sources.contains('-')
            || self.updated
            || self.sources.ends_with('*')
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
    db_path: &Path,
) -> Result<Vec<InvoiceTableRow>> {
    build_invoice_table_rows_with_progress(year, review_score, db_path, None)
}

pub(crate) fn build_invoice_table_rows_with_progress(
    year: i32,
    review_score: u8,
    db_path: &Path,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<Vec<InvoiceTableRow>> {
    if let Some(progress) = &progress {
        set_progress(progress, "Tabela: wczytywanie źródeł...");
    }
    let ksef = configured_ksef_out_path(year);
    let saldeo = default_saldeo_records_path(year);
    let mail_records =
        filter_invoice_records_for_year_table(load_default_mail_candidates(year)?, year);
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!("Tabela: Gmail {} rekordów...", mail_records.len()),
        );
    }
    let ksef_records =
        filter_invoice_records_for_year_table(load_records(SourceKind::Ksef, &ksef)?, year);
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!("Tabela: KSeF {} rekordów...", ksef_records.len()),
        );
    }
    let saldeo_records =
        filter_invoice_records_for_year_table(load_saldeo_records(&saldeo, Some(db_path))?, year);
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
    let ksef_statuses = resolve_ksef_accounting_statuses(year, &ksef_ids, progress.clone());

    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!("Tabela: render {} wierszy...", report.rows.len()),
        );
    }
    Ok(invoice_table_rows_from_report(
        &report,
        year,
        Utc::now().date_naive(),
        Some(&ksef_statuses.display),
        Some(&ksef_statuses.fresh),
    ))
}

/// Table rows of the year from a reconcile report built over
/// `filter_invoice_records_for_year_table` records.
pub(crate) fn invoice_table_rows_from_report(
    report: &TriReconcileReport,
    year: i32,
    today: NaiveDate,
    display_statuses: Option<&HashMap<i64, Option<bool>>>,
    fresh_statuses: Option<&HashMap<i64, Option<bool>>>,
) -> Vec<InvoiceTableRow> {
    report
        .rows
        .iter()
        .filter_map(|row| invoice_table_row_from_status_maps(row, display_statuses, fresh_statuses))
        .filter(|row| invoice_table_row_matches_year(row, year))
        .map(|mut row| {
            restrict_invoice_table_upload_to_period(&mut row, year, today);
            row
        })
        .collect()
}

/// Upload is offered only where the CLI plan would send the file (`saldeo_upload_target_period`):
/// an undated file goes to the current period, so only in the current year's table.
pub(crate) fn restrict_invoice_table_upload_to_period(
    row: &mut InvoiceTableRow,
    year: i32,
    today: NaiveDate,
) {
    if row
        .upload_item
        .as_ref()
        .is_some_and(|item| saldeo_upload_target_period(item.issue_date, year, today).is_err())
    {
        row.upload_item = None;
        if row.action == InvoiceTableAction::Upload {
            row.action = InvoiceTableAction::None;
        }
    }
}

pub(crate) fn invoice_table_source_parts(sources: &str) -> [&str; 3] {
    let cleaned = sources.trim_end_matches('*');
    let mut parts = cleaned.split('/');
    [
        parts.next().unwrap_or("-"),
        parts.next().unwrap_or("-"),
        parts.next().unwrap_or("-"),
    ]
}

pub(crate) fn invoice_table_row_matches_year(row: &InvoiceTableRow, year: i32) -> bool {
    invoice_record_in_year_table(&row.record, year)
        || row
            .saldeo_record
            .as_ref()
            .is_some_and(|record| invoice_record_matches_year(record, year))
}

#[cfg(test)]
pub(crate) fn invoice_table_row_from_reconcile_row(
    row: &TriRow,
    ksef_statuses: Option<&HashMap<i64, Option<bool>>>,
) -> Option<InvoiceTableRow> {
    invoice_table_row_from_status_maps(row, ksef_statuses, ksef_statuses)
}

pub(crate) fn invoice_table_row_from_status_maps(
    row: &TriRow,
    display_statuses: Option<&HashMap<i64, Option<bool>>>,
    fresh_statuses: Option<&HashMap<i64, Option<bool>>>,
) -> Option<InvoiceTableRow> {
    let record = tri_row_display_record(row)?;
    let saldeo_record = row.saldeo.clone();
    let mut sources = row_source_mask(row);
    if saldeo_record
        .as_ref()
        .is_some_and(saldeo_record_has_override)
    {
        sources.push('*');
    }
    let upload_item = invoice_table_upload_item(row);
    let (ksef_document_id, ksef_accounting) = if let Some(ksef) = row.ksef.as_ref() {
        row.saldeo
            .as_ref()
            .filter(|record| ksef_references_match(ksef, record))
            .and_then(saldeo_document_id)
            .map(|document_id| {
                let accounting = display_statuses
                    .and_then(|statuses| statuses.get(&document_id))
                    .copied()
                    .flatten();
                let actionable_id = fresh_statuses
                    .is_some_and(|statuses| statuses.get(&document_id) == Some(&None))
                    .then_some(document_id);
                (actionable_id, accounting)
            })
            .unwrap_or((None, None))
    } else {
        (None, None)
    };
    Some(InvoiceTableRow {
        selected: false,
        sources,
        record,
        mail_record: row.mail.clone(),
        ksef_record: row.ksef.clone(),
        saldeo_record,
        upload_item,
        ksef_document_id,
        ksef_accounting,
        action: InvoiceTableAction::None,
        updated: false,
    })
}

/// Upload item for a Gmail file missing in Saldeo, built like the CLI plan builds it.
fn invoice_table_upload_item(row: &TriRow) -> Option<SaldeoSyncItem> {
    let mail = row.mail.as_ref()?;
    if row.saldeo.is_some() {
        return None;
    }
    let related_sources = [("mail", row.mail.as_ref()), ("ksef", row.ksef.as_ref())]
        .into_iter()
        .filter_map(|(name, record)| record.map(|_| name.to_string()))
        .collect::<Vec<_>>();
    let item = saldeo_sync_item_from_record(&row.status, mail, related_sources);
    item.can_upload.then_some(item)
}

/// The reconcile row the table row was built from (scores are not kept; the merge and the
/// upload item do not use them).
fn invoice_table_row_parts(row: &InvoiceTableRow) -> TriRow {
    TriRow {
        status: row
            .upload_item
            .as_ref()
            .map(|item| item.status.clone())
            .unwrap_or_default(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: None,
        ksef_score_to_saldeo: None,
        mail: row.mail_record.clone(),
        ksef: row.ksef_record.clone(),
        saldeo: row.saldeo_record.clone(),
    }
}

/// A KSeF approve/reject targets the Saldeo document, so it is only offered when that
/// document points at exactly the KSeF invoice shown in the row.
pub(crate) fn ksef_references_match(ksef: &InvoiceRecord, saldeo: &InvoiceRecord) -> bool {
    let reference = |record: &InvoiceRecord| {
        record
            .ksef_reference
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    match (reference(ksef), reference(saldeo)) {
        (Some(ksef), Some(saldeo)) => ksef == saldeo,
        _ => false,
    }
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

/// Live update from an LLM run: `record` (a Gmail record) replaces only the Gmail part of
/// the rows holding it; the display record and the upload item are then rebuilt from the
/// parts exactly as the table rebuild builds them. Returns whether any row changed.
pub(crate) fn apply_invoice_record_update_to_rows(
    rows: &mut [InvoiceTableRow],
    record: &InvoiceRecord,
    year: i32,
    today: NaiveDate,
) -> bool {
    let mut changed = false;
    for row in rows.iter_mut() {
        let Some(mail) = row
            .mail_record
            .as_ref()
            .filter(|mail| mail.content_hash == record.content_hash)
        else {
            continue;
        };
        let mail_changed = serde_json::to_value(mail).ok() != serde_json::to_value(record).ok();
        let before = invoice_table_row_signature(row);
        row.mail_record = Some(record.clone());
        refresh_invoice_table_row_from_parts(row, year, today);
        if mail_changed || invoice_table_row_signature(row) != before {
            row.updated = true;
            changed = true;
        }
    }
    changed
}

/// Recomputes the display record and the upload item from the row's source parts.
fn refresh_invoice_table_row_from_parts(row: &mut InvoiceTableRow, year: i32, today: NaiveDate) {
    let parts = invoice_table_row_parts(row);
    if let Some(record) = tri_row_display_record(&parts) {
        row.record = record;
    }
    if row.upload_item.is_some() {
        row.upload_item = invoice_table_upload_item(&parts);
        if row.upload_item.is_none() && row.action == InvoiceTableAction::Upload {
            row.action = InvoiceTableAction::None;
        }
        restrict_invoice_table_upload_to_period(row, year, today);
    }
}

/// Gmail content hashes the LLM sends for the selected rows (the Gmail part, not the
/// display record, which is the Saldeo record for corrected rows).
pub(crate) fn invoice_table_llm_selected_hashes(rows: &[InvoiceTableRow]) -> Vec<String> {
    rows.iter()
        .filter(|row| row.selected)
        .filter_map(|row| row.mail_record.as_ref())
        .map(|mail| mail.content_hash.clone())
        .collect()
}

/// After the rebuild that ends an operation: rows whose Gmail part was updated live by the
/// LLM and rows whose action succeeded stay marked as updated. Only the marker is carried;
/// the display comes from the rebuild's merge. Returns how many rows it newly marked.
pub(crate) fn carry_invoice_table_updated_markers(
    new_rows: &mut [InvoiceTableRow],
    live_updated_mail_hashes: &HashSet<String>,
    accepted_action_keys: &HashSet<String>,
) -> usize {
    let mut newly_marked = 0usize;
    for row in new_rows {
        let live_updated = row
            .mail_record
            .as_ref()
            .is_some_and(|mail| live_updated_mail_hashes.contains(&mail.content_hash));
        if (live_updated || accepted_action_keys.contains(&invoice_table_row_key(row)))
            && !row.updated
        {
            row.updated = true;
            newly_marked += 1;
        }
    }
    newly_marked
}

/// Indices of the rows shown in the table: the `f` filter, then the sort column
/// (0 keeps the reconcile order; missing values sort last in both directions).
pub(crate) fn invoice_table_visible_indices(
    rows: &[InvoiceTableRow],
    hide_approved_ksef_saldeo: bool,
    sort_column: usize,
    sort_descending: bool,
) -> Vec<usize> {
    let mut visible = rows
        .iter()
        .enumerate()
        .filter_map(|(idx, row)| {
            invoice_table_row_passes_filter(row, hide_approved_ksef_saldeo).then_some(idx)
        })
        .collect::<Vec<_>>();
    if sort_column != 0 {
        visible.sort_by(|&a, &b| {
            if sort_column == 6 {
                compare_sort_values(
                    Some(&rows[a].sources),
                    Some(&rows[b].sources),
                    sort_descending,
                )
            } else {
                compare_invoice_sort(
                    &rows[a].record,
                    &rows[b].record,
                    sort_column,
                    sort_descending,
                )
            }
        });
    }
    visible
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

/// Rows an action key / menu entry applies to: the visible rows the user explicitly
/// multi-selected with Space, otherwise only the highlighted row.
pub(crate) fn invoice_table_target_indices(
    rows: &[InvoiceTableRow],
    visible: &[usize],
    table_sel: usize,
) -> Vec<usize> {
    let selected = visible
        .iter()
        .copied()
        .filter(|&idx| rows.get(idx).is_some_and(|row| row.selected))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        visible.get(table_sel).copied().into_iter().collect()
    } else {
        selected
    }
}

pub(crate) fn invoice_table_row_accepts(row: &InvoiceTableRow, action: InvoiceTableAction) -> bool {
    match action {
        InvoiceTableAction::None => true,
        InvoiceTableAction::Upload => row.can_upload(),
        InvoiceTableAction::ApproveKsef | InvoiceTableAction::RejectKsef => row.can_mark_ksef(),
    }
}

/// Sets `action` on the target rows that support it. Marking never changes the selection.
pub(crate) fn mark_invoice_table_rows(
    rows: &mut [InvoiceTableRow],
    visible: &[usize],
    table_sel: usize,
    action: InvoiceTableAction,
) -> usize {
    let mut changed = 0usize;
    for idx in invoice_table_target_indices(rows, visible, table_sel) {
        let row = &mut rows[idx];
        if invoice_table_row_accepts(row, action) {
            row.action = action;
            changed += 1;
        }
    }
    changed
}

/// "Wyczyść": drops both the action and the selection of the target rows.
pub(crate) fn clear_invoice_table_rows(
    rows: &mut [InvoiceTableRow],
    visible: &[usize],
    table_sel: usize,
) -> usize {
    let mut cleared = 0usize;
    for idx in invoice_table_target_indices(rows, visible, table_sel) {
        let row = &mut rows[idx];
        if row.action != InvoiceTableAction::None || row.selected {
            cleared += 1;
        }
        row.action = InvoiceTableAction::None;
        row.selected = false;
    }
    cleared
}

/// Space / paint mode toggle. Deselecting a row also drops its action.
pub(crate) fn toggle_invoice_table_row_selection(row: &mut InvoiceTableRow) {
    row.selected = !row.selected;
    if !row.selected {
        row.action = InvoiceTableAction::None;
    }
}

/// Snapshot executed by "Akceptuj": only rows currently on screen (so their action label is
/// visible) keep their action.
pub(crate) fn invoice_table_commit_rows(
    rows: &[InvoiceTableRow],
    visible: &[usize],
) -> Vec<InvoiceTableRow> {
    let visible = visible.iter().copied().collect::<HashSet<_>>();
    rows.iter()
        .enumerate()
        .map(|(idx, row)| {
            let mut row = row.clone();
            if !visible.contains(&idx) {
                row.action = InvoiceTableAction::None;
            }
            row
        })
        .collect()
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
    receiver: std::sync::mpsc::Receiver<PendingResult>,
    description: String,
    new_year: Option<i32>,
    new_review_score: Option<u8>,
    progress: Arc<Mutex<String>>,
    record_updates: Option<std::sync::mpsc::Receiver<InvoiceRecord>>,
    /// Gmail content hashes whose rows a live record update changed during this action.
    live_updated_mail_hashes: HashSet<String>,
}

pub(crate) enum PendingActionStart {
    Started(PendingAction),
    Noop(String),
}

/// What a background operation hands back to the TUI: the rebuilt table (or the error that
/// prevented it) and, for "Akceptuj", the per-row outcome of the executed actions.
pub(crate) struct PendingResult {
    pub(crate) rows: Result<Vec<InvoiceTableRow>>,
    pub(crate) commit: Option<InvoiceTableCommitReport>,
}

impl From<Result<Vec<InvoiceTableRow>>> for PendingResult {
    fn from(rows: Result<Vec<InvoiceTableRow>>) -> Self {
        Self { rows, commit: None }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InvoiceTableCommitEntry {
    pub(crate) row_key: String,
    pub(crate) action: InvoiceTableAction,
    pub(crate) ksef_document_id: Option<i64>,
    pub(crate) label: String,
    pub(crate) error: Option<String>,
}

impl InvoiceTableCommitEntry {
    fn matches(&self, row: &InvoiceTableRow) -> bool {
        invoice_table_row_key(row) == self.row_key
            || (self.ksef_document_id.is_some() && row.ksef_document_id == self.ksef_document_id)
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct InvoiceTableCommitReport {
    pub(crate) succeeded: Vec<InvoiceTableCommitEntry>,
    pub(crate) failed: Vec<InvoiceTableCommitEntry>,
}

/// After "Akceptuj": rows whose own operation succeeded lose their action, rows that failed
/// keep it (when the row can still take it), so a retry only re-sends what failed.
pub(crate) fn apply_commit_report_to_rows(
    rows: &mut [InvoiceTableRow],
    report: &InvoiceTableCommitReport,
) {
    for row in rows.iter_mut() {
        if report.succeeded.iter().any(|entry| entry.matches(row)) {
            row.action = InvoiceTableAction::None;
            row.selected = false;
        }
        if let Some(entry) = report.failed.iter().find(|entry| entry.matches(row)) {
            row.action = if invoice_table_row_accepts(row, entry.action) {
                entry.action
            } else {
                InvoiceTableAction::None
            };
        }
    }
}

pub(crate) fn invoice_table_commit_status(
    description: &str,
    report: &InvoiceTableCommitReport,
    refresh_error: Option<&str>,
    rows_total: usize,
    updated: usize,
) -> String {
    let ok = report.succeeded.len();
    let failed = report.failed.len();
    if failed == 0 && refresh_error.is_none() {
        return format!(
            "✓ {description} zakończone: udane {ok}, nieudane 0; {rows_total} faktur, {updated} nowych/zmienionych"
        );
    }
    let mut parts = vec![format!("✗ {description}: udane {ok}, nieudane {failed}")];
    if failed > 0 {
        let details = report
            .failed
            .iter()
            .map(|entry| {
                format!(
                    "{}: {}",
                    entry.label,
                    entry.error.as_deref().unwrap_or("błąd")
                )
            })
            .collect::<Vec<_>>()
            .join(" | ");
        parts.push(format!(
            "nieudane zostają oznaczone do ponowienia — {details}"
        ));
    }
    if let Some(error) = refresh_error {
        parts.push(format!("odświeżenie Saldeo nie powiodło się: {error}"));
    }
    parts.join("; ")
}

pub(crate) fn set_progress(progress: &Arc<Mutex<String>>, message: impl Into<String>) {
    *progress.lock().unwrap() = message.into();
}

/// Runs `job` on a worker thread. stderr is already redirected to the log for the whole TUI
/// session (see `TuiTerminalSession`), so workers do not touch file descriptors.
fn spawn_pending_action(
    description: String,
    initial_progress: String,
    new_year: Option<i32>,
    new_review_score: Option<u8>,
    record_updates: Option<std::sync::mpsc::Receiver<InvoiceRecord>>,
    job: impl FnOnce(Arc<Mutex<String>>) -> PendingResult + Send + 'static,
) -> PendingAction {
    let progress = Arc::new(Mutex::new(initial_progress));
    let progress_clone = progress.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(job(progress_clone));
    });
    PendingAction {
        receiver: rx,
        description,
        new_year,
        new_review_score,
        progress,
        record_updates,
        live_updated_mail_hashes: HashSet::new(),
    }
}

pub(crate) fn begin_invoice_table_commit(
    rows: &[InvoiceTableRow],
    year: i32,
    review_score: u8,
    db_path: PathBuf,
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
    PendingActionStart::Started(spawn_pending_action(
        description.clone(),
        format!("{}: przygotowanie operacji...", description),
        Some(year),
        Some(review_score),
        None,
        move |progress| {
            execute_invoice_table_actions(year, review_score, rows_snapshot, progress, db_path)
        },
    ))
}

pub(crate) fn begin_invoice_table_refresh(
    year: i32,
    review_score: u8,
    db_path: PathBuf,
    description: String,
) -> PendingAction {
    let description_clone = description.clone();
    spawn_pending_action(
        description.clone(),
        format!("{}: przygotowanie pełnego sync...", description),
        Some(year),
        Some(review_score),
        None,
        move |progress| {
            (|| -> Result<Vec<InvoiceTableRow>> {
                set_progress(
                    &progress,
                    format!(
                        "{}: start dla roku {year} (KSeF + Gmail/PDF + Saldeo)...",
                        description_clone
                    ),
                );
                let conn = open_db(&db_path)?;
                run_sync_sources_with_progress(
                    year,
                    false,
                    false,
                    false,
                    false,
                    None,
                    None,
                    None,
                    DEFAULT_PRODUCTMESH_NIP,
                    Some(&db_path),
                    Some(&conn),
                    Some(progress.clone()),
                )?;
                set_progress(
                    &progress,
                    format!("{}: budowanie tabeli...", description_clone),
                );
                build_invoice_table_rows_with_progress(
                    year,
                    review_score,
                    &db_path,
                    Some(progress.clone()),
                )
            })()
            .into()
        },
    )
}

fn begin_invoice_table_rebuild(year: i32, review_score: u8, db_path: PathBuf) -> PendingAction {
    spawn_pending_action(
        "Rebuild".to_string(),
        format!("Przebudowa (zgodność 3way {review_score})..."),
        Some(year),
        Some(review_score),
        None,
        move |progress| {
            build_invoice_table_rows_with_progress(year, review_score, &db_path, Some(progress))
                .into()
        },
    )
}

fn begin_invoice_table_saldeo_refresh(
    year: i32,
    review_score: u8,
    db_path: PathBuf,
) -> PendingAction {
    spawn_pending_action(
        "Saldeo".to_string(),
        "Saldeo: sprawdzam zapisaną sesję...".to_string(),
        Some(year),
        Some(review_score),
        None,
        move |progress| {
            (|| -> Result<Vec<InvoiceTableRow>> {
                ensure_saldeo_session_or_auth(Some(progress.clone()))?;
                set_progress(&progress, "Saldeo: odświeżanie danych...");
                sync_reconcile_metadata_with_progress(
                    year,
                    false,
                    true,
                    &db_path,
                    Some(progress.clone()),
                )?;
                set_progress(&progress, "Saldeo: budowanie tabeli...");
                build_invoice_table_rows_with_progress(
                    year,
                    review_score,
                    &db_path,
                    Some(progress.clone()),
                )
            })()
            .into()
        },
    )
}

fn begin_invoice_table_reconcile(year: i32, review_score: u8, db_path: PathBuf) -> PendingAction {
    spawn_pending_action(
        "Reconcile".to_string(),
        format!("Reconcile: metadane KSeF/Saldeo dla roku {year}..."),
        Some(year),
        Some(review_score),
        None,
        move |progress| {
            (|| -> Result<Vec<InvoiceTableRow>> {
                set_progress(
                    &progress,
                    "Reconcile: odświeżanie metadanych KSeF/Saldeo...",
                );
                sync_reconcile_metadata_with_progress(
                    year,
                    true,
                    true,
                    &db_path,
                    Some(progress.clone()),
                )?;
                set_progress(&progress, "Reconcile: budowanie tabeli...");
                build_invoice_table_rows_with_progress(
                    year,
                    review_score,
                    &db_path,
                    Some(progress.clone()),
                )
            })()
            .into()
        },
    )
}

fn begin_invoice_table_llm(
    rows: &[InvoiceTableRow],
    year: i32,
    review_score: u8,
    db_path: PathBuf,
) -> PendingAction {
    let y = year;
    let score = review_score;
    let db = db_path;
    let selected_hashes = invoice_table_llm_selected_hashes(rows);
    let has_selection = !selected_hashes.is_empty();
    let selected_hashes_clone = selected_hashes.clone();
    let (record_tx, record_rx) = std::sync::mpsc::channel();
    let initial_progress = if has_selection {
        format!("LLM: przygotowanie {} faktur...", selected_hashes.len())
    } else {
        format!("LLM: wczytywanie faktur dla roku {y}...")
    };
    let description = if has_selection {
        "LLM (wybrane)".to_string()
    } else {
        "LLM".to_string()
    };
    spawn_pending_action(
        description,
        initial_progress,
        Some(y),
        None,
        Some(record_rx),
        move |progress_clone| {
            (|| -> Result<Vec<InvoiceTableRow>> {
                for mail_path in [
                    default_mail_candidates_path(y),
                    default_amazon_mail_candidates_path(y),
                ] {
                    if !mail_path.exists() {
                        continue;
                    }
                    *progress_clone.lock().unwrap() = "LLM: wczytywanie faktur...".to_string();
                    let mut candidates = load_records(SourceKind::Mail, &mail_path)?;
                    let conn = open_db(&db)?;
                    if has_selection {
                        let mut to_enrich: Vec<InvoiceRecord> = candidates
                            .iter()
                            .filter(|c| selected_hashes_clone.contains(&c.content_hash))
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
                            *progress_clone.lock().unwrap() =
                                format!("LLM: parsowanie {} faktur...", to_enrich.len());
                            let empty_skip = std::collections::HashSet::new();
                            enrich_candidates_with_gemma_with_hook(
                                &mut to_enrich,
                                &empty_skip,
                                Some(progress_clone.clone()),
                                true,
                                |enriched_records, idx| {
                                    let enriched = enriched_records[idx].clone();
                                    if let Some(pos) = candidates
                                        .iter()
                                        .position(|c| c.content_hash == enriched.content_hash)
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
                                    store_records(&conn, std::slice::from_ref(&enriched))?;
                                    let _ = record_tx.send(enriched);
                                    Ok(())
                                },
                            )?;
                            set_progress(&progress_clone, "LLM: zapis per dokument zakończony");
                        }
                    } else {
                        let cached = apply_cached_mail_candidates(&mail_path, &mut candidates)?;
                        *progress_clone.lock().unwrap() = "LLM: parsowanie faktur...".to_string();
                        enrich_candidates_with_gemma_with_hook(
                            &mut candidates,
                            &cached,
                            Some(progress_clone.clone()),
                            false,
                            |all_records, idx| {
                                let enriched = all_records[idx].clone();
                                write_records(all_records, OutputFormat::Jsonl, Some(&mail_path))?;
                                store_records(&conn, std::slice::from_ref(&enriched))?;
                                let _ = record_tx.send(enriched);
                                Ok(())
                            },
                        )?;
                        set_progress(&progress_clone, "LLM: zapis per dokument zakończony");
                    }
                }
                set_progress(&progress_clone, "LLM: budowanie tabeli...");
                build_invoice_table_rows_with_progress(y, score, &db, Some(progress_clone.clone()))
            })()
            .into()
        },
    )
}

pub(crate) fn execute_invoice_table_actions(
    year: i32,
    review_score: u8,
    rows: Vec<InvoiceTableRow>,
    progress: Arc<Mutex<String>>,
    db_path: PathBuf,
) -> PendingResult {
    let storage_state = default_saldeo_storage_state_path();
    let mut session: Option<std::result::Result<SaldeoSession, String>> = None;
    execute_invoice_table_actions_with(
        year,
        &rows,
        &progress,
        |plan| {
            plan.ledger_db_path = Some(db_path.clone());
            saldeo_upload_plan_with_progress(
                plan,
                &storage_state,
                DEFAULT_SALDEO_UPLOAD_URL,
                "file",
                Some(progress.clone()),
            )
        },
        |ids, mark_is_accounting| {
            let session = session.get_or_insert_with(|| {
                ensure_saldeo_session_or_auth(Some(progress.clone()))
                    .and_then(|_| read_saldeo_session(&storage_state))
                    .map_err(|err| err.to_string())
            });
            match session {
                Ok(session) => saldeo_mark_ksef_documents(session, ids, mark_is_accounting),
                Err(err) => Err(anyhow!("{err}")),
            }
        },
        || {
            set_progress(&progress, "Akceptuj: odświeżam Saldeo po zmianach...");
            saldeo_fetch_with_progress(
                year,
                &storage_state,
                &default_saldeo_out_path(year),
                Some(&db_path),
                Some(progress.clone()),
            )?;
            set_progress(&progress, "Akceptuj: przebudowuję tabelę...");
            build_invoice_table_rows_with_progress(
                year,
                review_score,
                &db_path,
                Some(progress.clone()),
            )
        },
    )
}

pub(crate) fn saldeo_upload_item_succeeded(item: &SaldeoSyncItem) -> bool {
    matches!(item.upload_status.as_str(), "uploaded" | "already_uploaded")
}

fn saldeo_upload_item_label(item: &SaldeoSyncItem) -> String {
    item.source_path
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .or(item.invoice_number.as_deref())
        .unwrap_or("plik")
        .to_string()
}

/// Executes the marked actions independently (a failed upload does not stop KSeF marks),
/// records the outcome per row and always attempts the Saldeo refresh afterwards.
pub(crate) fn execute_invoice_table_actions_with(
    year: i32,
    rows: &[InvoiceTableRow],
    progress: &Arc<Mutex<String>>,
    mut upload: impl FnMut(&mut SaldeoSyncPlan) -> Result<()>,
    mut mark: impl FnMut(&[i64], bool) -> Result<Vec<i64>>,
    refresh: impl FnOnce() -> Result<Vec<InvoiceTableRow>>,
) -> PendingResult {
    let mut report = InvoiceTableCommitReport::default();

    let upload_targets = rows
        .iter()
        .filter(|row| row.action == InvoiceTableAction::Upload)
        .filter_map(|row| {
            row.upload_item
                .clone()
                .map(|item| (invoice_table_row_key(row), item))
        })
        .collect::<Vec<_>>();
    if !upload_targets.is_empty() {
        set_progress(
            progress,
            format!(
                "Akceptuj: upload do Saldeo ({} plików)...",
                upload_targets.len()
            ),
        );
        let selected_upload_items = upload_targets
            .iter()
            .map(|(_, item)| item.clone())
            .collect::<Vec<_>>();
        let mut upload_plan = SaldeoSyncPlan {
            generated_at: Utc::now(),
            year,
            confirm: true,
            upload_url: Some(DEFAULT_SALDEO_UPLOAD_URL.to_string()),
            summary: saldeo_sync_summary(&selected_upload_items),
            items: selected_upload_items,
            ksef_approve: None,
            ..Default::default()
        };
        let upload_error = upload(&mut upload_plan).err().map(|err| err.to_string());
        let same_shape = upload_plan.items.len() == upload_targets.len();
        for (idx, (row_key, original)) in upload_targets.iter().enumerate() {
            let item = if same_shape {
                upload_plan.items.get(idx)
            } else {
                upload_plan.items.iter().find(|item| {
                    original.source_path.is_some() && item.source_path == original.source_path
                })
            };
            let error = match item {
                Some(item) if saldeo_upload_item_succeeded(item) => None,
                Some(item) => Some(
                    item.error
                        .clone()
                        .or_else(|| upload_error.clone())
                        .unwrap_or_else(|| format!("status uploadu: {}", item.upload_status)),
                ),
                None => Some(
                    upload_error
                        .clone()
                        .unwrap_or_else(|| "brak wyniku uploadu".to_string()),
                ),
            };
            let entry = InvoiceTableCommitEntry {
                row_key: row_key.clone(),
                action: InvoiceTableAction::Upload,
                ksef_document_id: None,
                label: saldeo_upload_item_label(original),
                error,
            };
            if entry.error.is_none() {
                report.succeeded.push(entry);
            } else {
                report.failed.push(entry);
            }
        }
    }

    for (action, mark_is_accounting, verb) in [
        (InvoiceTableAction::ApproveKsef, true, "zatwierdzam"),
        (InvoiceTableAction::RejectKsef, false, "odrzucam"),
    ] {
        let targets = rows
            .iter()
            .filter(|row| row.action == action)
            .filter_map(|row| {
                let id = row.ksef_document_id?;
                let label = row
                    .record
                    .invoice_number
                    .clone()
                    .unwrap_or_else(|| format!("dokument {id}"));
                Some((invoice_table_row_key(row), id, label))
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            continue;
        }
        set_progress(
            progress,
            format!(
                "Akceptuj: {verb} KSeF w Saldeo ({} dokumentów)...",
                targets.len()
            ),
        );
        let ids = targets.iter().map(|(_, id, _)| *id).collect::<Vec<_>>();
        let outcome = mark(&ids, mark_is_accounting).map_err(|err| err.to_string());
        for (row_key, id, label) in targets {
            let error = match &outcome {
                Ok(marked) if marked.contains(&id) => None,
                Ok(_) => Some(
                    "pominięto — Saldeo nie pokazuje dokumentu jako nieoznaczonego".to_string(),
                ),
                Err(err) => Some(err.clone()),
            };
            let entry = InvoiceTableCommitEntry {
                row_key,
                action,
                ksef_document_id: Some(id),
                label,
                error,
            };
            if entry.error.is_none() {
                report.succeeded.push(entry);
            } else {
                report.failed.push(entry);
            }
        }
    }

    PendingResult {
        rows: refresh(),
        commit: Some(report),
    }
}

fn finish_pending_action(
    rows: &mut Vec<InvoiceTableRow>,
    pending: &PendingAction,
    result: PendingResult,
    year: &mut i32,
    review_score: &mut u8,
) -> String {
    let PendingResult {
        rows: rows_result,
        commit,
    } = result;
    let mut new_rows = match rows_result {
        Ok(new_rows) => new_rows,
        Err(err) => {
            return match &commit {
                Some(report) => {
                    apply_commit_report_to_rows(rows, report);
                    invoice_table_commit_status(
                        &pending.description,
                        report,
                        Some(&err.to_string()),
                        rows.len(),
                        0,
                    )
                }
                None => format!("✗ Błąd {}: {err}", pending.description),
            };
        }
    };
    if let Some(y) = pending.new_year {
        *year = y;
    }
    if let Some(t) = pending.new_review_score {
        *review_score = t;
    }
    let accepted_action_keys = commit
        .as_ref()
        .map(|report| {
            report
                .succeeded
                .iter()
                .map(|entry| entry.row_key.clone())
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let mut updated_count = mark_updated_invoice_rows(rows, &mut new_rows);
    updated_count += carry_invoice_table_updated_markers(
        &mut new_rows,
        &pending.live_updated_mail_hashes,
        &accepted_action_keys,
    );
    if let Some(report) = &commit {
        apply_commit_report_to_rows(&mut new_rows, report);
    }
    *rows = new_rows;
    match &commit {
        Some(report) => invoice_table_commit_status(
            &pending.description,
            report,
            None,
            rows.len(),
            updated_count,
        ),
        None => format!(
            "✓ {} zakończone: {} faktur, {} nowych/zmienionych",
            pending.description,
            rows.len(),
            updated_count
        ),
    }
}

// ---------------------------------------------------------------------------
// stderr → log redirection

/// A descriptor redirected onto a file, remembering a CLOEXEC duplicate of the original so
/// it can be put back. Dropping it restores the original descriptor.
pub(crate) struct FdRedirect {
    target: std::os::unix::io::RawFd,
    saved: Option<std::os::unix::io::RawFd>,
}

impl FdRedirect {
    pub(crate) fn redirect(
        target: std::os::unix::io::RawFd,
        file: &std::fs::File,
    ) -> std::io::Result<Self> {
        use std::os::unix::io::AsRawFd;
        // SAFETY: plain descriptor syscalls; every return value is checked and the
        // duplicate is closed on failure.
        let saved = unsafe { libc::fcntl(target, libc::F_DUPFD_CLOEXEC, 0) };
        if saved < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::dup2(file.as_raw_fd(), target) } < 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(saved) };
            return Err(err);
        }
        Ok(Self {
            target,
            saved: Some(saved),
        })
    }

    pub(crate) fn restore(&mut self) {
        if let Some(saved) = self.saved.take() {
            // SAFETY: `saved` is a descriptor we own; it is closed exactly once.
            unsafe {
                libc::dup2(saved, self.target);
                libc::close(saved);
            }
        }
    }
}

impl Drop for FdRedirect {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Holds at most one active redirect of a descriptor.
pub(crate) struct StderrRedirectSlot {
    active: Option<FdRedirect>,
}

impl StderrRedirectSlot {
    pub(crate) const fn new() -> Self {
        Self { active: None }
    }

    #[cfg(test)]
    pub(crate) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Redirects `target` to the file from `open` unless a redirect is already active
    /// (then `open` is not called). Returns whether a redirect is active afterwards.
    pub(crate) fn ensure(
        &mut self,
        target: std::os::unix::io::RawFd,
        open: impl FnOnce() -> std::io::Result<std::fs::File>,
    ) -> bool {
        if self.active.is_none()
            && let Ok(redirect) = open().and_then(|file| FdRedirect::redirect(target, &file))
        {
            self.active = Some(redirect);
        }
        self.active.is_some()
    }

    pub(crate) fn restore(&mut self) {
        if let Some(mut redirect) = self.active.take() {
            redirect.restore();
        }
    }
}

static STDERR_REDIRECT: Mutex<StderrRedirectSlot> = Mutex::new(StderrRedirectSlot::new());

/// Default log location: `~/Library/Logs/lab/lab.log` (`LAB_LOG` overrides it).
pub(crate) fn default_lab_log_path(home: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let home = home.filter(|home| !home.is_empty())?;
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("Logs")
            .join("lab")
            .join("lab.log"),
    )
}

/// Opens a log for appending. New files are created with mode 0600; with `private` the
/// parent directory is created with mode 0700 and an existing file is tightened to 0600.
pub(crate) fn open_lab_log_file(path: &Path, private: bool) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    if private && let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    if private {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn open_lab_log() -> std::io::Result<std::fs::File> {
    if let Some(path) = std::env::var_os("LAB_LOG").filter(|value| !value.is_empty()) {
        return open_lab_log_file(Path::new(&path), false);
    }
    let path = default_lab_log_path(std::env::var_os("HOME").as_deref()).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "brak HOME dla logu LAB")
    })?;
    open_lab_log_file(&path, true)
}

/// Redirects the process-wide stderr (fd 2) to the LAB log so background work cannot draw
/// over the TUI. Idempotent; undone by `restore_stderr`.
pub(crate) fn redirect_stderr_to_log() -> bool {
    STDERR_REDIRECT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ensure(libc::STDERR_FILENO, open_lab_log)
}

/// Puts the original stderr back (no-op when it is not redirected).
pub(crate) fn restore_stderr() {
    STDERR_REDIRECT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .restore();
}

fn install_stderr_restoring_panic_hook() {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // try_lock: never block inside a panic hook.
            if let Ok(mut slot) = STDERR_REDIRECT.try_lock() {
                slot.restore();
            }
            previous(info);
        }));
    });
}

/// Owns the terminal for one TUI session: raw mode + alternate screen + stderr redirect.
/// Dropping it (normal return, `?` early return or unwinding) restores all of them.
struct TuiTerminalSession {
    terminal: ratatui::DefaultTerminal,
}

impl TuiTerminalSession {
    fn start() -> Self {
        let terminal = ratatui::init();
        install_stderr_restoring_panic_hook();
        redirect_stderr_to_log();
        Self { terminal }
    }
}

impl Drop for TuiTerminalSession {
    fn drop(&mut self) {
        ratatui::restore();
        restore_stderr();
    }
}

// ---------------------------------------------------------------------------
// Keys, menu and status line

pub(crate) const MI_SYNC: usize = 0;
pub(crate) const MI_RECONCILE: usize = 1;
pub(crate) const MI_LLM: usize = 2;
pub(crate) const MI_UPLOAD: usize = 3;
pub(crate) const MI_APPROVE: usize = 4;
pub(crate) const MI_REJECT: usize = 5;
pub(crate) const MI_CLEAR: usize = 6;
pub(crate) const MI_COMMIT: usize = 7;
pub(crate) const MI_EDIT: usize = 8;
pub(crate) const MI_MENU: usize = 9;
pub(crate) const MAIN_COUNT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TuiCommand {
    Quit,
    CloseMenu,
    MoveDown,
    MoveUp,
    Sync,
    Reconcile,
    Llm,
    MarkUpload,
    MarkApprove,
    MarkReject,
    ClearMarks,
    Commit,
    Edit,
    OpenMenu,
}

impl TuiCommand {
    /// Commands that start work or change marks are refused while an operation runs.
    pub(crate) fn requires_idle(self) -> bool {
        !matches!(
            self,
            TuiCommand::Quit | TuiCommand::CloseMenu | TuiCommand::MoveDown | TuiCommand::MoveUp
        )
    }
}

/// Command behind a main-menu button (Enter).
pub(crate) fn main_menu_command(menu_sel: usize) -> Option<TuiCommand> {
    Some(match menu_sel {
        MI_SYNC => TuiCommand::Sync,
        MI_RECONCILE => TuiCommand::Reconcile,
        MI_LLM => TuiCommand::Llm,
        MI_UPLOAD => TuiCommand::MarkUpload,
        MI_APPROVE => TuiCommand::MarkApprove,
        MI_REJECT => TuiCommand::MarkReject,
        MI_CLEAR => TuiCommand::ClearMarks,
        MI_COMMIT => TuiCommand::Commit,
        MI_EDIT => TuiCommand::Edit,
        MI_MENU => TuiCommand::OpenMenu,
        _ => return None,
    })
}

/// Keyboard shortcuts of the table view (text input is handled before this is consulted).
/// Action letters are inactive while the submenu is open. Ctrl+C quits, it never executes.
pub(crate) fn invoice_table_key_command(
    code: KeyCode,
    modifiers: crossterm::event::KeyModifiers,
    menu_open: bool,
) -> Option<TuiCommand> {
    use crossterm::event::KeyModifiers as M;
    let command_key = modifiers.intersects(M::SUPER | M::META);
    let plain = !modifiers.intersects(M::CONTROL | M::ALT | M::SUPER | M::META);
    let command = match code {
        KeyCode::Char('c') if modifiers.contains(M::CONTROL) => TuiCommand::Quit,
        KeyCode::Char('q') if plain => TuiCommand::Quit,
        KeyCode::Esc if menu_open => TuiCommand::CloseMenu,
        KeyCode::Esc => TuiCommand::Quit,
        KeyCode::Down => TuiCommand::MoveDown,
        KeyCode::Up => TuiCommand::MoveUp,
        KeyCode::Char('j') if plain => TuiCommand::MoveDown,
        KeyCode::Char('k') if plain => TuiCommand::MoveUp,
        _ if menu_open => return None,
        KeyCode::Char('c') | KeyCode::Enter if command_key => TuiCommand::Commit,
        KeyCode::Char('u') if plain => TuiCommand::MarkUpload,
        KeyCode::Char('a') if plain => TuiCommand::MarkApprove,
        KeyCode::Char('r') if plain => TuiCommand::MarkReject,
        KeyCode::Char('n') if plain => TuiCommand::ClearMarks,
        KeyCode::Char('c') if plain => TuiCommand::Commit,
        KeyCode::Char('e') if plain => TuiCommand::Edit,
        _ => return None,
    };
    Some(command)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuitRequest {
    Exit,
    Warn,
}

/// The first q/Esc during a running operation only warns; the next one exits (aborting it).
pub(crate) fn invoice_table_quit_request(operation_running: bool, warned: bool) -> QuitRequest {
    if operation_running && !warned {
        QuitRequest::Warn
    } else {
        QuitRequest::Exit
    }
}

pub(crate) const QUIT_WHILE_RUNNING_WARNING: &str =
    "Trwa operacja — ponowne q/Esc przerwie ją i zamknie LAB";
pub(crate) const BUSY_NOTICE: &str = "Trwa operacja — poczekaj na zakończenie";

pub(crate) fn invoice_table_help_text(actionable_only: bool) -> &'static str {
    if actionable_only {
        "j/k=ruch spc=zaznacz u=upload a=zatwierdź r=odrzuć n=wyczyść c=wykonaj e=popraw f=pokaż zatw. q=wyjdź"
    } else {
        "j/k=ruch spc=zaznacz u=upload a=zatwierdź r=odrzuć n=wyczyść c=wykonaj e=popraw f=ukryj zatw. q=wyjdź"
    }
}

/// Text for the status line: progress of the running operation (prefixed by a notice such
/// as the quit warning) or the last status message.
pub(crate) fn status_bar_message(
    progress: Option<&str>,
    notice: Option<&str>,
    status: &str,
) -> String {
    let base = match progress {
        Some(progress) if !progress.is_empty() => progress,
        _ => status,
    };
    match notice.filter(|notice| !notice.is_empty()) {
        Some(notice) if base.is_empty() => format!("⚠ {notice}"),
        Some(notice) => format!("⚠ {notice} · {base}"),
        None => base.to_string(),
    }
}

/// Status line clipped to `max_width` terminal columns without ever splitting a character.
pub(crate) fn status_bar_text(message: &str, prefix: &str, max_width: usize) -> String {
    let full = if prefix.is_empty() {
        format!(" {message}")
    } else {
        format!(" {prefix} {message}")
    };
    let full = full
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    fit_display_width(&full, max_width)
}

pub(crate) fn fit_display_width(text: &str, max_width: usize) -> String {
    let width_of = |value: &str| ratatui::text::Span::raw(value).width();
    if width_of(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let budget = max_width - 1; // room for '…'
    let mut out = String::new();
    let mut used = 0usize;
    let mut buf = [0u8; 4];
    for ch in text.chars() {
        let width = width_of(ch.encode_utf8(&mut buf));
        if used + width > budget {
            break;
        }
        used += width;
        out.push(ch);
    }
    out.push('…');
    out
}

/// Bookkeeping entries of `InvoiceRecord::warnings` that are not notes for the user: the
/// mail parser version, the applied-LLM marker, the Saldeo override marker (shown as `*`
/// in G/K/S) and provenance markers of KSeF online and enriched Saldeo records.
pub(crate) fn is_internal_record_warning(warning: &str) -> bool {
    warning.trim().is_empty()
        || own_nip::is_internal_marker(warning)
        || is_mail_parser_version_marker(warning)
        || is_llm_applied_marker(warning)
        || warning.starts_with(SALDEO_OVERRIDE_WARNING_PREFIX)
        || warning == KSEF_ONLINE_METADATA_MARKER
        || warning == SALDEO_ENRICHED_FROM_KSEF_MARKER
        || warning == SALDEO_ENRICHED_FROM_DOWNLOAD_MARKER
}

pub(crate) fn user_facing_record_warnings(record: &InvoiceRecord) -> Vec<&str> {
    record
        .warnings
        .iter()
        .map(String::as_str)
        .filter(|warning| !is_internal_record_warning(warning))
        .collect()
}

/// User-facing warnings of the row's source records (not of the merged display record),
/// each with its source letter in G/K/S order.
pub(crate) fn invoice_table_row_warnings(row: &InvoiceTableRow) -> Vec<(char, &str)> {
    [
        ('G', row.mail_record.as_ref()),
        ('K', row.ksef_record.as_ref()),
        ('S', row.saldeo_record.as_ref()),
    ]
    .into_iter()
    .filter_map(|(source, record)| record.map(|record| (source, record)))
    .flat_map(|(source, record)| {
        user_facing_record_warnings(record)
            .into_iter()
            .map(move |warning| (source, warning))
    })
    .collect()
}

/// Width of the G/K/S column: the mask and both markers (`*` correction, `!` warnings).
pub(crate) const INVOICE_TABLE_SOURCES_WIDTH: u16 = 7;

/// G/K/S cell: `sources` (with the `*` of a corrected Saldeo record) and `!` when a source
/// record carries a user-facing warning. `sources` itself stays unmarked for sorting and
/// filtering.
pub(crate) fn invoice_table_sources_cell(row: &InvoiceTableRow) -> String {
    if invoice_table_row_warnings(row).is_empty() {
        row.sources.clone()
    } else {
        format!("{}!", row.sources)
    }
}

/// Warnings of the highlighted row for the table's bottom border, fitted to `max_width`
/// terminal columns; empty when there is no row or no user-facing warning.
pub(crate) fn invoice_table_warning_detail(
    row: Option<&InvoiceTableRow>,
    max_width: usize,
) -> String {
    let warnings = row.map(invoice_table_row_warnings).unwrap_or_default();
    if warnings.is_empty() {
        return String::new();
    }
    let text = warnings
        .iter()
        .map(|(source, warning)| {
            let warning = warning
                .split(|ch: char| ch.is_whitespace() || ch.is_control())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            format!("{source}: {warning}")
        })
        .collect::<Vec<_>>()
        .join(" · ");
    fit_display_width(&format!(" ! {text} "), max_width)
}

/// Column widths of the invoice table. Fixed-content columns (selection, G/K/S with its
/// markers, currency) get their width; the others share the rest in the proportions they
/// have on an 80-column terminal, so none of them is narrower there than before the
/// G/K/S column fitted its markers.
pub(crate) fn invoice_table_column_constraints() -> [Constraint; 8] {
    [
        Constraint::Percentage(4),
        Constraint::Fill(6),
        Constraint::Length(INVOICE_TABLE_SOURCES_WIDTH),
        Constraint::Fill(16),
        Constraint::Fill(15),
        Constraint::Fill(8),
        Constraint::Fill(13),
        Constraint::Length(3),
    ]
}

pub(crate) const INVOICE_TABLE_HEADERS: [&str; 8] = [
    "sel",
    "akcja / KSeF",
    "G/K/S",
    "faktura",
    "kontrahent",
    "data",
    "brutto",
    "wal",
];

/// Cell texts of a table row after the selection mark (`[ ]`, `[*]` or a spinner).
pub(crate) fn invoice_table_row_cells(row: &InvoiceTableRow, sel_mark: String) -> [String; 8] {
    [
        sel_mark,
        invoice_table_action_ksef_label(row),
        invoice_table_sources_cell(row),
        truncate(row.record.invoice_number.as_deref().unwrap_or("-"), 24),
        truncate(&counterparty_name(Some(&row.record)), 26),
        row.record
            .issue_date
            .map(|date| date.to_string())
            .unwrap_or_else(|| "-".to_string()),
        row.record
            .gross_amount_minor
            .map(format_minor_money)
            .unwrap_or_else(|| "-".to_string()),
        row.record.currency.as_deref().unwrap_or("-").to_string(),
    ]
}

/// Validates the 3-way matching threshold typed in the menu.
pub(crate) fn parse_review_threshold(value: &str) -> std::result::Result<u8, String> {
    let value = value.trim();
    match value.parse::<u16>() {
        Ok(threshold) if (50..=100).contains(&threshold) => Ok(threshold as u8),
        _ => Err(format!(
            "zgodność 3way: podaj liczbę z zakresu 50–100 (wpisano „{value}”)"
        )),
    }
}

pub(crate) fn wrap_menu_selection(current: usize, delta: isize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let count_usize = count;
    let count = count_usize as isize;
    let current = (current % count_usize) as isize;
    (current + delta).rem_euclid(count) as usize
}

pub(crate) fn run_invoice_table_tui(
    rows: &mut Vec<InvoiceTableRow>,
    year: &mut i32,
    review_score: &mut u8,
    db_path: &Path,
) -> Result<TuiResult> {
    let mut session = TuiTerminalSession::start();
    let theme = TuiTheme::detect();
    let mut table_sel = 0usize;
    let mut menu_sel = 0usize;
    let mut actionable_only = false;
    let mut sort_column = 0usize;
    let mut sort_descending = false;
    let mut editing_year: Option<String> = None;
    let mut editing_threshold: Option<String> = None;
    let mut paint_mode: bool = false;
    let mut menu_open: bool = false;
    let mut spinner_frame: usize = 0;
    let mut status_message: String = String::new();
    let mut pending_action: Option<PendingAction> = None;
    // Shown in the status line while an operation runs (its progress hides status_message).
    let mut pending_notice: Option<String> = None;
    let mut quit_warned = false;
    let mut loop_result: Result<TuiResult> = Ok(TuiResult::Cancel);

    // Submenu (when menu_open)
    const SM_DOCTOR: usize = 0;
    const SM_ONBOARD: usize = 1;
    const SM_YEAR: usize = 2;
    const SM_THRESHOLD: usize = 3;
    const SM_SALDEO: usize = 4;
    const SM_BACK: usize = 5;
    const SUB_COUNT: usize = 6;

    let selected_saldeo_record =
        |rows: &Vec<InvoiceTableRow>, visible: &[usize], table_sel: usize| {
            let row_idx = *visible.get(table_sel)?;
            rows.get(row_idx)?.saldeo_record.clone()
        };

    let tick_rate = std::time::Duration::from_millis(100);
    let mut last_tick = std::time::Instant::now();

    loop {
        // Check pending async action
        if let Some(pending) = pending_action.as_mut() {
            if let Some(record_updates) = &pending.record_updates {
                let today = Utc::now().date_naive();
                loop {
                    match record_updates.try_recv() {
                        Ok(record) => {
                            if apply_invoice_record_update_to_rows(rows, &record, *year, today) {
                                pending
                                    .live_updated_mail_hashes
                                    .insert(record.content_hash.clone());
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
                Ok(result) => {
                    status_message =
                        finish_pending_action(rows, pending, result, year, review_score);
                    pending_action = None;
                    pending_notice = None;
                    quit_warned = false;
                    table_sel = 0;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Still running
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    status_message = format!("✗ Błąd {}: wątek przerwany", pending.description);
                    pending_action = None;
                    pending_notice = None;
                    quit_warned = false;
                    table_sel = 0;
                }
            }
        }

        // Tick spinner every 100ms
        if last_tick.elapsed() >= tick_rate {
            spinner_frame = spinner_frame.wrapping_add(1);
            last_tick = std::time::Instant::now();
        }

        let visible =
            invoice_table_visible_indices(rows, actionable_only, sort_column, sort_descending);
        if visible.is_empty() {
            table_sel = 0;
        } else if table_sel >= visible.len() {
            table_sel = visible.len().saturating_sub(1);
        }
        let menu_items = if menu_open { SUB_COUNT } else { MAIN_COUNT };
        if menu_sel >= menu_items {
            menu_sel = menu_items.saturating_sub(1);
        }
        let committing = pending_action
            .as_ref()
            .is_some_and(|pending| pending.description.starts_with("Akceptuj"));

        if let Err(err) = session.terminal.draw(|frame| {
            let area = frame.area();
            let chunks = Layout::vertical([
                Constraint::Min(5),
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

            // Table
            let header = Row::new(INVOICE_TABLE_HEADERS.map(Cell::from)).style(theme.header());
            let table_rows = visible.iter().map(|vidx| {
                let row = &rows[*vidx];
                let busy_row = pending_action.is_some()
                    && (row.selected || (committing && row.action != InvoiceTableAction::None));
                let sel_mark = if busy_row {
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
                        InvoiceTableAction::None => theme.muted(),
                    }
                };
                Row::new(invoice_table_row_cells(row, sel_mark).map(Cell::from)).style(style)
            });
            // Warnings of the highlighted row go on the table's bottom border, so they never
            // take the status line from a running operation or a status message.
            let warning_detail = invoice_table_warning_detail(
                visible.get(table_sel).and_then(|idx| rows.get(*idx)),
                chunks[0].width.saturating_sub(2) as usize,
            );
            let table = Table::new(table_rows, invoice_table_column_constraints())
            .header(header)
            .block(
                Block::default()
                    .title(format!(
                        " LAB faktury {year} · zgodność 3way {review_score} · s: sort {} {} · Shift+s: kierunek ",
                        SORT_LABELS[sort_column],
                        if sort_column == 0 { "" } else if sort_descending { "↓" } else { "↑" },
                    ))
                    .title_bottom(ratatui::text::Line::styled(warning_detail, theme.warning()))
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
                        &format!("zgodność 3way:{}", threshold_text),
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
            let progress_text = pending_action
                .as_ref()
                .map(|pending| pending.progress.lock().unwrap().clone());
            let active_msg = status_bar_message(
                progress_text.as_deref(),
                pending_notice.as_deref(),
                &status_message,
            );
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
                let display_text =
                    status_bar_text(&active_msg, &prefix, chunks[2].width as usize);
                let line = ratatui::text::Line::styled(
                    display_text,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                );
                frame.render_widget(Paragraph::new(line), chunks[2]);
            }

            // Stats bar — stats left, help right
            let upd = invoice_table_updated_count(rows);
            let stats_text = format!(
                "sel:{} | up:{} | zatw:{} | odrz:{} | zm:{} | {}/{}{}",
                sel,
                u,
                a,
                r,
                upd,
                visible.len(),
                rows.len(),
                if actionable_only {
                    " | ukryte zatw. K+S"
                } else {
                    ""
                }
            );
            let help = invoice_table_help_text(actionable_only).to_string();
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
                                pending_action = Some(begin_invoice_table_refresh(
                                    y,
                                    score,
                                    db_path.to_path_buf(),
                                    "Sync".to_string(),
                                ));
                            } else {
                                status_message = format!("Rok: niepoprawna wartość „{val}”");
                            }
                        } else if let Some(val) = editing_threshold.take() {
                            match parse_review_threshold(&val) {
                                Ok(t) => {
                                    pending_action = Some(begin_invoice_table_rebuild(
                                        *year,
                                        t,
                                        db_path.to_path_buf(),
                                    ));
                                }
                                Err(message) => {
                                    status_message = message;
                                    editing_threshold = Some(val);
                                }
                            }
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

            let command = if key.code == KeyCode::Enter && !cmd && !menu_open {
                main_menu_command(menu_sel)
            } else {
                invoice_table_key_command(key.code, key.modifiers, menu_open)
            };
            if command != Some(TuiCommand::Quit) {
                quit_warned = false;
                if pending_notice.as_deref() == Some(QUIT_WHILE_RUNNING_WARNING) {
                    pending_notice = None;
                }
            }

            if let Some(command) = command {
                if command.requires_idle() && pending_action.is_some() {
                    pending_notice = Some(BUSY_NOTICE.to_string());
                    continue;
                }
                match command {
                    TuiCommand::Quit => {
                        match invoice_table_quit_request(pending_action.is_some(), quit_warned) {
                            QuitRequest::Exit => break,
                            QuitRequest::Warn => {
                                quit_warned = true;
                                pending_notice = Some(QUIT_WHILE_RUNNING_WARNING.to_string());
                            }
                        }
                    }
                    TuiCommand::CloseMenu => menu_open = false,
                    TuiCommand::MoveDown => {
                        if table_sel + 1 < visible.len() {
                            table_sel += 1;
                            if (paint_mode || shift)
                                && let Some(row_idx) = visible.get(table_sel).copied()
                            {
                                toggle_invoice_table_row_selection(&mut rows[row_idx]);
                            }
                        }
                    }
                    TuiCommand::MoveUp => {
                        if table_sel > 0 {
                            table_sel -= 1;
                            if (paint_mode || shift)
                                && let Some(row_idx) = visible.get(table_sel).copied()
                            {
                                toggle_invoice_table_row_selection(&mut rows[row_idx]);
                            }
                        }
                    }
                    TuiCommand::Sync => {
                        pending_action = Some(begin_invoice_table_refresh(
                            *year,
                            *review_score,
                            db_path.to_path_buf(),
                            "Sync".to_string(),
                        ));
                    }
                    TuiCommand::Reconcile => {
                        pending_action = Some(begin_invoice_table_reconcile(
                            *year,
                            *review_score,
                            db_path.to_path_buf(),
                        ));
                    }
                    TuiCommand::Llm => {
                        pending_action = Some(begin_invoice_table_llm(
                            rows,
                            *year,
                            *review_score,
                            db_path.to_path_buf(),
                        ));
                    }
                    TuiCommand::MarkUpload => {
                        let changed = mark_invoice_table_rows(
                            rows,
                            &visible,
                            table_sel,
                            InvoiceTableAction::Upload,
                        );
                        status_message = if changed > 0 {
                            format!("Upload: oznaczono {changed}; użyj Akceptuj, żeby wysłać")
                        } else {
                            "Upload: brak wybranych/podświetlonych faktur do wysłania do Saldeo"
                                .to_string()
                        };
                    }
                    TuiCommand::MarkApprove => {
                        let changed = mark_invoice_table_rows(
                            rows,
                            &visible,
                            table_sel,
                            InvoiceTableAction::ApproveKsef,
                        );
                        status_message = if changed > 0 {
                            format!(
                                "Zatwierdź: oznaczono {changed}; użyj Akceptuj, żeby wysłać do Saldeo"
                            )
                        } else {
                            "Zatwierdź: brak dokumentów Saldeo/KSeF do oznaczenia (wybierz wiersz „nieozn.”)".to_string()
                        };
                    }
                    TuiCommand::MarkReject => {
                        let changed = mark_invoice_table_rows(
                            rows,
                            &visible,
                            table_sel,
                            InvoiceTableAction::RejectKsef,
                        );
                        status_message = if changed > 0 {
                            format!(
                                "Odrzuć: oznaczono {changed}; użyj Akceptuj, żeby wysłać do Saldeo"
                            )
                        } else {
                            "Odrzuć: brak dokumentów Saldeo/KSeF do oznaczenia (wybierz wiersz „nieozn.”)".to_string()
                        };
                    }
                    TuiCommand::ClearMarks => {
                        let cleared = clear_invoice_table_rows(rows, &visible, table_sel);
                        status_message = format!("Wyczyść: wyczyszczono {cleared}");
                    }
                    TuiCommand::Commit => {
                        match begin_invoice_table_commit(
                            &invoice_table_commit_rows(rows, &visible),
                            *year,
                            *review_score,
                            db_path.to_path_buf(),
                        ) {
                            PendingActionStart::Started(action) => pending_action = Some(action),
                            PendingActionStart::Noop(message) => status_message = message,
                        }
                    }
                    TuiCommand::Edit => {
                        if let Some(record) = selected_saldeo_record(rows, &visible, table_sel) {
                            loop_result = Ok(TuiResult::Correct(Box::new(record)));
                            break;
                        } else {
                            status_message =
                                "Popraw: wybrany wiersz nie ma rekordu Saldeo".to_string();
                        }
                    }
                    TuiCommand::OpenMenu => {
                        menu_open = true;
                        menu_sel = SM_BACK;
                    }
                }
                continue;
            }

            match key.code {
                KeyCode::Enter if menu_open && !cmd => {
                    if pending_action.is_some() {
                        pending_notice = Some(BUSY_NOTICE.to_string());
                        continue;
                    }
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
                            pending_action = Some(begin_invoice_table_saldeo_refresh(
                                *year,
                                *review_score,
                                db_path.to_path_buf(),
                            ));
                        }
                        SM_BACK => {
                            menu_open = false;
                            menu_sel = MI_MENU;
                        }
                        _ => {}
                    }
                }
                KeyCode::Right => {
                    let max = if menu_open { SUB_COUNT } else { MAIN_COUNT };
                    menu_sel = wrap_menu_selection(menu_sel, 1, max);
                }
                KeyCode::Left => {
                    let max = if menu_open { SUB_COUNT } else { MAIN_COUNT };
                    menu_sel = wrap_menu_selection(menu_sel, -1, max);
                }
                KeyCode::Home => {
                    table_sel = 0;
                    if paint_mode && let Some(row_idx) = visible.get(table_sel).copied() {
                        toggle_invoice_table_row_selection(&mut rows[row_idx]);
                    }
                }
                KeyCode::End => {
                    table_sel = visible.len().saturating_sub(1);
                    if paint_mode && let Some(row_idx) = visible.get(table_sel).copied() {
                        toggle_invoice_table_row_selection(&mut rows[row_idx]);
                    }
                }
                KeyCode::Char('s') if !cmd => {
                    sort_column = (sort_column + 1) % SORT_LABELS.len();
                    table_sel = 0;
                }
                KeyCode::Char('S') if !cmd => {
                    if sort_column == 0 {
                        sort_column = 1;
                    }
                    sort_descending = !sort_descending;
                    table_sel = 0;
                }
                KeyCode::Char('f') => {
                    actionable_only = !actionable_only;
                    table_sel = 0;
                    status_message = if actionable_only {
                        "Ukryto zatwierdzone KSeF+Saldeo. f pokazuje je z powrotem.".to_string()
                    } else {
                        "Pokazuję zatwierdzone KSeF+Saldeo.".to_string()
                    };
                }
                KeyCode::Char('v') => {
                    paint_mode = !paint_mode;
                }
                KeyCode::Char(' ') => {
                    if let Some(row_idx) = visible.get(table_sel).copied() {
                        toggle_invoice_table_row_selection(&mut rows[row_idx]);
                    }
                }
                _ => {}
            }
        }
    }

    drop(session);
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

pub(crate) fn invoice_table_row_passes_filter(
    row: &InvoiceTableRow,
    hide_approved_ksef_saldeo: bool,
) -> bool {
    !hide_approved_ksef_saldeo || !row.is_approved_in_ksef_and_saldeo()
}

pub(crate) fn invoice_table_ksef_status(row: &InvoiceTableRow) -> String {
    match (row.ksef_document_id, row.ksef_accounting) {
        (Some(_), None) => "nieoznaczone".to_string(),
        (_, Some(true)) => "zatwierdzone".to_string(),
        (_, Some(false)) => "odrzucone".to_string(),
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

pub(crate) struct KsefAccountingStatuses {
    pub(crate) display: HashMap<i64, Option<bool>>,
    pub(crate) fresh: HashMap<i64, Option<bool>>,
}

impl KsefAccountingStatuses {
    pub(crate) fn from_live(
        mut cached: HashMap<i64, Option<bool>>,
        live: Result<HashMap<i64, Option<bool>>>,
    ) -> Self {
        // Cached entries are display-only, even when a successful response is partial.
        let fresh = live.unwrap_or_default();
        cached.extend(fresh.iter().map(|(id, status)| (*id, *status)));
        Self {
            display: cached,
            fresh,
        }
    }
}

fn resolve_ksef_accounting_statuses(
    year: i32,
    document_ids: &[i64],
    progress: Option<Arc<Mutex<String>>>,
) -> KsefAccountingStatuses {
    if document_ids.is_empty() {
        return KsefAccountingStatuses::from_live(HashMap::new(), Ok(HashMap::new()));
    }
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!(
                "Tabela: status KSeF w Saldeo ({} dokumentów)...",
                document_ids.len()
            ),
        );
    }
    let cached = load_ksef_accounting_cache(year);
    if let Err(err) = ensure_saldeo_session_or_auth(progress.clone()) {
        eprintln!("  [Saldeo] automatyczne logowanie nie powiodło się: {err}");
    }
    match read_saldeo_session(&default_saldeo_storage_state_path())
        .and_then(|session| saldeo_fetch_ksef_accounting_statuses(&session, document_ids))
    {
        Ok(live) => {
            let statuses = KsefAccountingStatuses::from_live(cached, Ok(live));
            if let Err(err) = save_ksef_accounting_cache(year, &statuses.display) {
                eprintln!("  [Saldeo] nie zapisałem cache statusów KSeF: {err}");
            }
            statuses
        }
        Err(err) => {
            if cached.is_empty() {
                eprintln!(
                    "  [Saldeo] pomijam statusy KSeF w tabeli (sesja/API niedostępne): {err}"
                );
                if let Some(progress) = &progress {
                    set_progress(
                        progress,
                        "Tabela: status KSeF w Saldeo niedostępny — użyj Menu → Saldeo, aby odświeżyć sesję",
                    );
                }
            } else {
                eprintln!(
                    "  [Saldeo] statusy KSeF z cache ({} dokumentów); akcje wyłączone; live: {err}",
                    cached.len()
                );
                if let Some(progress) = &progress {
                    set_progress(
                        progress,
                        format!(
                            "Tabela: status KSeF z cache ({} dokumentów); akcje wyłączone",
                            cached.len()
                        ),
                    );
                }
            }
            KsefAccountingStatuses::from_live(cached, Err(err))
        }
    }
}

pub(crate) fn ksef_accounting_cache_path(year: i32) -> PathBuf {
    default_saldeo_out_path(year).join("ksef_accounting.json")
}

pub(crate) fn load_ksef_accounting_cache(year: i32) -> HashMap<i64, Option<bool>> {
    let path = ksef_accounting_cache_path(year);
    let Ok(text) = fs::read_to_string(&path) else {
        return HashMap::new();
    };
    serde_json::from_str::<HashMap<String, Option<bool>>>(&text)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, value)| key.parse().ok().map(|id| (id, value)))
        .collect()
}

pub(crate) fn save_ksef_accounting_cache(
    year: i32,
    statuses: &HashMap<i64, Option<bool>>,
) -> Result<()> {
    let path = ksef_accounting_cache_path(year);
    let json = statuses
        .iter()
        .map(|(id, value)| (id.to_string(), *value))
        .collect::<HashMap<_, _>>();
    write_private_file(&path, &serde_json::to_vec_pretty(&json)?)
}

pub(crate) fn pending_ksef_approve_ids(rows: &[InvoiceTableRow]) -> Vec<i64> {
    let mut ids = rows
        .iter()
        .filter_map(|row| row.ksef_document_id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
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
    const URL: &str = "https://saldeo.brainshare.pl/rest/client/document/ksef/accounting";
    let client = Client::builder().build()?;
    let body = serde_json::json!({"clientId": 0, "documentIds": document_ids});
    // Odczyt idempotentny: ponawiany przy błędach przejściowych (mark/approve nie są).
    let response: Value =
        saldeo_read_with_retry("document/ksef/accounting", std::thread::sleep, || {
            let request = session
                .authorize(client.post(URL), URL)
                .header("saldeoApp", "angularApp")
                .json(&body);
            saldeo_send_read(request)?
                .json::<Value>()
                .map_err(SaldeoReadError::from_reqwest)
        })
        .map_err(|err| err.error.context("Saldeo document/ksef/accounting"))?;
    parse_ksef_accounting_statuses(&response)
}

pub(crate) fn parse_ksef_accounting_statuses(
    response: &Value,
) -> Result<HashMap<i64, Option<bool>>> {
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
            let accounting = match item.get("accounting") {
                Some(Value::Null) => None,
                Some(Value::Bool(value)) => Some(*value),
                _ => continue,
            };
            out.insert(document_id, accounting);
        }
    }
    Ok(out)
}

pub(crate) fn mark_unmarked_ksef_documents_with(
    document_ids: &[i64],
    fetch: impl FnOnce(&[i64]) -> Result<HashMap<i64, Option<bool>>>,
    mark: impl FnOnce(&[i64]) -> Result<()>,
) -> Result<Vec<i64>> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }
    // Mutation never consults the display cache; lookup errors propagate before any write.
    let fresh = fetch(document_ids)?;
    let mut unmarked = document_ids
        .iter()
        .copied()
        .filter(|id| fresh.get(id) == Some(&None))
        .collect::<Vec<_>>();
    unmarked.sort_unstable();
    unmarked.dedup();
    if !unmarked.is_empty() {
        mark(&unmarked)?;
    }
    Ok(unmarked)
}

pub(crate) fn saldeo_mark_ksef_documents(
    session: &SaldeoSession,
    document_ids: &[i64],
    mark_is_accounting: bool,
) -> Result<Vec<i64>> {
    let _lock = saldeo_write_lock()?;
    mark_unmarked_ksef_documents_with(
        document_ids,
        |ids| saldeo_fetch_ksef_accounting_statuses(session, ids),
        |ids| saldeo_mark_ksef_documents_unchecked(session, ids, mark_is_accounting).map(|_| ()),
    )
}

fn saldeo_mark_ksef_documents_unchecked(
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
