use crate::*;
use dialoguer::{Confirm, Input};
use std::collections::{HashMap, HashSet};

#[cfg(test)]
mod upload_tests;

pub(crate) struct SaldeoSyncPlanConfig<'a> {
    pub(crate) year: i32,
    pub(crate) tri_report: Option<&'a Path>,
    pub(crate) mail: Option<&'a Path>,
    pub(crate) ksef: Option<&'a Path>,
    pub(crate) saldeo: Option<&'a Path>,
    pub(crate) db_path: Option<&'a Path>,
    pub(crate) review_score: u8,
    pub(crate) confirm: bool,
    pub(crate) upload_url: Option<String>,
}

pub(crate) fn saldeo_sync_plan(config: SaldeoSyncPlanConfig<'_>) -> Result<SaldeoSyncPlan> {
    saldeo_sync_plan_at(config, Utc::now().date_naive())
}

pub(crate) fn saldeo_sync_plan_at(
    config: SaldeoSyncPlanConfig<'_>,
    today: NaiveDate,
) -> Result<SaldeoSyncPlan> {
    let report = if let Some(path) = config.tri_report {
        read_tri_report(path)?
    } else {
        let mail = config
            .mail
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_mail_candidates_path(config.year));
        let ksef = config
            .ksef
            .map(Path::to_path_buf)
            .unwrap_or_else(|| configured_ksef_out_path(config.year));
        let saldeo = config
            .saldeo
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_saldeo_records_path(config.year));
        tri_reconcile(
            load_records(SourceKind::Mail, &mail)?,
            load_records(SourceKind::Ksef, &ksef)?,
            load_saldeo_records(&saldeo, config.db_path)?,
            config.review_score,
        )
    };
    let saldeo_record_count = report
        .rows
        .iter()
        .filter(|row| row.saldeo.is_some())
        .count();
    let ledger = config.db_path.map(open_db).transpose()?;
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for row in &report.rows {
        if row.saldeo.is_some() {
            continue;
        }
        let Some(record) = row.mail.as_ref() else {
            continue;
        };
        let related_sources = [("mail", row.mail.as_ref()), ("ksef", row.ksef.as_ref())]
            .into_iter()
            .filter_map(|(name, record)| record.map(|_| name.to_string()))
            .collect::<Vec<_>>();
        let key = record
            .source_path
            .clone()
            .or_else(|| record.invoice_number.clone())
            .unwrap_or_else(|| tri_row_key(row));
        if !seen.insert(key) {
            continue;
        }
        let mut item = saldeo_sync_item_from_record(&row.status, record, related_sources);
        saldeo_apply_plan_guards(&mut item, config.year, today, ledger.as_ref())?;
        items.push(item);
    }
    let summary = saldeo_sync_summary(&items);
    let mut warnings = Vec::new();
    if let Err(err) = saldeo_empty_saldeo_guard(
        Some(saldeo_record_count),
        summary.uploadable_count,
        saldeo_allow_empty_from_env(),
    ) {
        warnings.push(err.to_string());
    }
    Ok(SaldeoSyncPlan {
        generated_at: Utc::now(),
        year: config.year,
        confirm: config.confirm,
        upload_url: config
            .upload_url
            .or_else(|| Some(DEFAULT_SALDEO_UPLOAD_URL.to_string())),
        summary,
        items,
        ksef_approve: None,
        warnings,
        saldeo_record_count: Some(saldeo_record_count),
        ledger_db_path: config.db_path.map(Path::to_path_buf),
    })
}

pub(crate) fn saldeo_sync_item_from_record(
    status: &str,
    record: &InvoiceRecord,
    related_sources: Vec<String>,
) -> SaldeoSyncItem {
    let source_path = record.source_path.clone();
    let can_upload = source_path
        .as_deref()
        .map(|path| Path::new(path).is_file())
        .unwrap_or(false);
    SaldeoSyncItem {
        status: status.to_string(),
        source: source_as_str(record.source).to_string(),
        related_sources,
        invoice_number: record.invoice_number.clone(),
        issue_date: record.issue_date,
        gross_amount_minor: record.gross_amount_minor,
        currency: record.currency.clone(),
        contractor: record
            .seller_name
            .clone()
            .or_else(|| record.buyer_name.clone()),
        source_path,
        can_upload,
        upload_status: if can_upload {
            "planned"
        } else {
            "missing_local_file"
        }
        .to_string(),
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

/// Pozycja wysłana w tym przebiegu bez sukcesu: błąd uploadu albo brak potwierdzenia.
/// `unconfirmed` z rejestru (poprzedni przebieg) ma `can_upload == false` i nie jest liczone.
pub(crate) fn saldeo_sync_item_failed(item: &SaldeoSyncItem) -> bool {
    item.upload_status == "failed" || (item.upload_status == "unconfirmed" && item.can_upload)
}

pub(crate) fn saldeo_sync_summary(items: &[SaldeoSyncItem]) -> SaldeoSyncSummary {
    let count = |status: &str| items.iter().filter(|i| i.upload_status == status).count();
    SaldeoSyncSummary {
        total_missing_saldeo: items.len(),
        uploadable_count: items.iter().filter(|i| i.can_upload).count(),
        missing_file_count: count("missing_local_file"),
        already_uploaded_count: count("already_uploaded"),
        unconfirmed_count: count("unconfirmed"),
        other_year_count: count("other_year"),
        uploaded_count: count("uploaded"),
        failed_count: items.iter().filter(|i| saldeo_sync_item_failed(i)).count(),
    }
}

pub(crate) const DEFAULT_SALDEO_UPLOAD_URL: &str =
    "https://saldeo.brainshare.pl/rest/client/document/generate-urls-for-upload";

pub(crate) const SALDEO_LEDGER_UPLOADED: &str = "uploaded";
pub(crate) const SALDEO_LEDGER_UNCONFIRMED: &str = "unconfirmed";

/// Rejestr plików wysłanych do Saldeo. Klucz: sha256 bajtów pliku.
pub(crate) fn ensure_saldeo_upload_ledger_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS saldeo_upload_ledger (
            file_sha256 TEXT PRIMARY KEY,
            source_path TEXT,
            invoice_number TEXT,
            saldeo_year INTEGER NOT NULL,
            saldeo_month INTEGER NOT NULL,
            doc_upload_id INTEGER,
            response_status INTEGER,
            state TEXT NOT NULL CHECK (state IN ('uploaded', 'unconfirmed')),
            error TEXT,
            uploaded_at TEXT NOT NULL
        );
        "#,
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SaldeoUploadLedgerEntry {
    pub file_sha256: String,
    pub source_path: Option<String>,
    pub invoice_number: Option<String>,
    pub saldeo_year: i32,
    pub saldeo_month: u32,
    pub doc_upload_id: Option<i64>,
    pub response_status: Option<u16>,
    pub state: String,
    pub error: Option<String>,
    pub uploaded_at: DateTime<Utc>,
}

pub(crate) fn saldeo_file_sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(crate) fn saldeo_upload_ledger_get(
    conn: &Connection,
    file_sha256: &str,
) -> Result<Option<SaldeoUploadLedgerEntry>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT file_sha256, source_path, invoice_number, saldeo_year, saldeo_month,
               doc_upload_id, response_status, state, error, uploaded_at
        FROM saldeo_upload_ledger WHERE file_sha256 = ?1
        "#,
    )?;
    let mut rows = stmt.query(params![file_sha256])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let uploaded_at: String = row.get(9)?;
    Ok(Some(SaldeoUploadLedgerEntry {
        file_sha256: row.get(0)?,
        source_path: row.get(1)?,
        invoice_number: row.get(2)?,
        saldeo_year: row.get(3)?,
        saldeo_month: row.get(4)?,
        doc_upload_id: row.get(5)?,
        response_status: row.get(6)?,
        state: row.get(7)?,
        error: row.get(8)?,
        uploaded_at: DateTime::parse_from_rfc3339(&uploaded_at)
            .with_context(|| format!("niepoprawna data w saldeo_upload_ledger: {uploaded_at}"))?
            .with_timezone(&Utc),
    }))
}

pub(crate) fn saldeo_upload_ledger_put(
    conn: &Connection,
    entry: &SaldeoUploadLedgerEntry,
) -> Result<()> {
    conn.execute(
        r#"
        INSERT OR REPLACE INTO saldeo_upload_ledger (
            file_sha256, source_path, invoice_number, saldeo_year, saldeo_month,
            doc_upload_id, response_status, state, error, uploaded_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        "#,
        params![
            entry.file_sha256,
            entry.source_path,
            entry.invoice_number,
            entry.saldeo_year,
            entry.saldeo_month,
            entry.doc_upload_id,
            entry.response_status,
            entry.state,
            entry.error,
            entry.uploaded_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// Okres Saldeo dla pliku w planie roku `plan_year`. Plik z datą wystawienia spoza roku
/// planu nie jest wysyłany (folder Gmail roku Y zawiera też faktury z grudnia Y-1).
/// Plik bez daty idzie do bieżącego okresu i tylko w planie bieżącego roku.
pub(crate) fn saldeo_upload_target_period(
    issue_date: Option<NaiveDate>,
    plan_year: i32,
    today: NaiveDate,
) -> std::result::Result<(i32, u32), String> {
    match issue_date {
        Some(date) if date.year() == plan_year => Ok((date.year(), date.month())),
        Some(date) => Err(format!(
            "data wystawienia {date} jest poza rokiem planu {plan_year}; wyślij z planu roku {}",
            date.year()
        )),
        None if plan_year == today.year() => Ok((today.year(), today.month())),
        None => Err(format!(
            "brak daty wystawienia; plik bez daty trafia do bieżącego okresu i jest wysyłany tylko w planie roku {}",
            today.year()
        )),
    }
}

fn saldeo_apply_period_rule(
    item: &mut SaldeoSyncItem,
    plan_year: i32,
    today: NaiveDate,
) -> Option<(i32, u32)> {
    match saldeo_upload_target_period(item.issue_date, plan_year, today) {
        Ok((year, month)) => {
            item.saldeo_year = Some(year);
            item.saldeo_month = Some(month);
            Some((year, month))
        }
        Err(reason) => {
            item.can_upload = false;
            item.upload_status = "other_year".to_string();
            item.skip_reason = Some(reason);
            None
        }
    }
}

fn saldeo_mark_ledger_hit(item: &mut SaldeoSyncItem, entry: &SaldeoUploadLedgerEntry) {
    item.can_upload = false;
    item.saldeo_year = Some(entry.saldeo_year);
    item.saldeo_month = Some(entry.saldeo_month);
    item.saldeo_doc_upload_id = entry.doc_upload_id;
    let when = entry.uploaded_at.format("%Y-%m-%d %H:%M UTC");
    let period = format!("{}-{:02}", entry.saldeo_year, entry.saldeo_month);
    if entry.state == SALDEO_LEDGER_UNCONFIRMED {
        item.upload_status = "unconfirmed".to_string();
        item.skip_reason = Some(format!(
            "plik wysłany {when} do okresu {period} bez potwierdzenia Saldeo; sprawdź ręcznie — LAB nie ponawia takiego uploadu"
        ));
    } else {
        item.upload_status = "already_uploaded".to_string();
        item.skip_reason = Some(format!(
            "ten sam plik (sha256) wysłano do Saldeo {when} do okresu {period}"
        ));
    }
}

fn saldeo_apply_plan_guards(
    item: &mut SaldeoSyncItem,
    plan_year: i32,
    today: NaiveDate,
    ledger: Option<&Connection>,
) -> Result<()> {
    if !item.can_upload {
        return Ok(());
    }
    let Some(source_path) = item.source_path.clone() else {
        return Ok(());
    };
    let bytes = match fs::read(&source_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            item.can_upload = false;
            item.upload_status = "missing_local_file".to_string();
            item.error = Some(format!("odczyt {source_path}: {err}"));
            return Ok(());
        }
    };
    let file_sha256 = saldeo_file_sha256(&bytes);
    item.file_sha256 = Some(file_sha256.clone());
    if let Some(conn) = ledger
        && let Some(entry) = saldeo_upload_ledger_get(conn, &file_sha256)?
    {
        saldeo_mark_ledger_hit(item, &entry);
        return Ok(());
    }
    saldeo_apply_period_rule(item, plan_year, today);
    Ok(())
}

/// Pusty zbiór Saldeo przy pozycjach do wysłania zwykle oznacza nieudany odczyt Saldeo,
/// a nie pustą księgowość — wtedy upload wysłałby wszystko jeszcze raz.
pub(crate) fn saldeo_empty_saldeo_guard(
    saldeo_record_count: Option<usize>,
    uploadable_count: usize,
    allow_empty: bool,
) -> Result<()> {
    if saldeo_record_count == Some(0) && uploadable_count > 0 && !allow_empty {
        return Err(anyhow!(
            "brak rekordów Saldeo dla roku przy {uploadable_count} plikach do wysłania — LAB nie odróżni pustego Saldeo od nieudanego odczytu; odśwież Saldeo albo ustaw LAB_ALLOW_EMPTY_SALDEO=1"
        ));
    }
    Ok(())
}

pub(crate) fn saldeo_allow_empty_from_env() -> bool {
    std::env::var("LAB_ALLOW_EMPTY_SALDEO").as_deref() == Ok("1")
}

/// Wyłączny lock zapisu do Saldeo (flock na pliku w ~/.config/lab). Zwalniany przy drop.
pub(crate) struct SaldeoWriteLock {
    file: fs::File,
}

impl Drop for SaldeoWriteLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub(crate) fn default_saldeo_write_lock_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("lab").join("saldeo-write.lock")
}

pub(crate) fn saldeo_write_lock() -> Result<SaldeoWriteLock> {
    saldeo_write_lock_at(&default_saldeo_write_lock_path())
}

pub(crate) fn saldeo_write_lock_at(path: &Path) -> Result<SaldeoWriteLock> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("otwarcie locka Saldeo {}", path.display()))?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            let holder = fs::read_to_string(path)
                .ok()
                .map(|pid| pid.trim().to_string())
                .filter(|pid| !pid.is_empty())
                .map(|pid| format!(" (PID {pid})"))
                .unwrap_or_default();
            return Err(anyhow!(
                "inny proces LAB{holder} zapisuje teraz do Saldeo (TUI, CLI, MCP albo zadanie launchd); lock {} jest zajęty — spróbuj po jego zakończeniu",
                path.display()
            ));
        }
        return Err(err).with_context(|| format!("flock {}", path.display()));
    }
    let _ = file.set_len(0);
    let _ = writeln!(file, "{}", std::process::id());
    Ok(SaldeoWriteLock { file })
}

pub(crate) fn saldeo_upload_plan(
    plan: &mut SaldeoSyncPlan,
    storage_state: &Path,
    upload_url: &str,
    file_field: &str,
) -> Result<()> {
    saldeo_upload_plan_with_progress(plan, storage_state, upload_url, file_field, None)
}

pub(crate) fn saldeo_upload_plan_with_progress(
    plan: &mut SaldeoSyncPlan,
    storage_state: &Path,
    upload_url: &str,
    _file_field: &str,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<()> {
    let _lock = saldeo_write_lock()?;
    let db_path = plan.ledger_db_path.clone().ok_or_else(|| {
        anyhow!(
            "upload Saldeo wymaga bazy LAB z rejestrem wysłanych plików; plan nie wskazuje bazy"
        )
    })?;
    let uploadable_count = plan.items.iter().filter(|item| item.can_upload).count();
    saldeo_empty_saldeo_guard(
        plan.saldeo_record_count,
        uploadable_count,
        saldeo_allow_empty_from_env(),
    )?;
    if uploadable_count == 0 {
        plan.summary = saldeo_sync_summary(&plan.items);
        return Ok(());
    }
    let conn = open_db(&db_path)?;
    ensure_saldeo_session_or_auth(progress.clone())?;
    let session = read_saldeo_session(storage_state)?;
    let client = Client::builder().build()?;
    let today = Utc::now().date_naive();
    let result = saldeo_upload_items_with(
        plan,
        &conn,
        today,
        progress.as_ref(),
        |path, bytes, year, month| {
            saldeo_upload_bytes(
                &client, &session, upload_url, path, bytes, year, month, today,
            )
        },
    );
    plan.summary = saldeo_sync_summary(&plan.items);
    result
}

/// Wynik wysłania jednego pliku.
#[derive(Debug)]
pub(crate) enum SaldeoUploadOutcome {
    /// PUT i confirm zakończone sukcesem.
    Uploaded {
        year: i32,
        month: u32,
        doc_upload_id: i64,
        response_status: u16,
        body: String,
    },
    /// PUT się udał, confirm nie (błąd, timeout, nieoczekiwana odpowiedź) — Saldeo może mieć plik.
    Unconfirmed {
        year: i32,
        month: u32,
        doc_upload_id: i64,
        response_status: Option<u16>,
        error: String,
    },
    /// Plik nie trafił do Saldeo (generate albo PUT nie powiodły się).
    Failed { error: String },
}

/// Pętla uploadu z wstrzykiwanym transportem. Rejestr jest sprawdzany przed każdym plikiem
/// i zapisywany zaraz po nim; błąd zapisu rejestru przerywa resztę przebiegu.
pub(crate) fn saldeo_upload_items_with(
    plan: &mut SaldeoSyncPlan,
    conn: &Connection,
    today: NaiveDate,
    progress: Option<&Arc<Mutex<String>>>,
    mut upload: impl FnMut(&Path, Vec<u8>, i32, u32) -> SaldeoUploadOutcome,
) -> Result<()> {
    let plan_year = plan.year;
    let uploadable_count = plan.items.iter().filter(|item| item.can_upload).count();
    let mut upload_index = 0usize;
    for item in &mut plan.items {
        if !item.can_upload {
            continue;
        }
        let Some(source_path) = item.source_path.clone() else {
            continue;
        };
        upload_index += 1;
        if let Some(progress) = progress {
            let label = Path::new(&source_path)
                .file_name()
                .and_then(|name| name.to_str())
                .or(item.invoice_number.as_deref())
                .unwrap_or("plik");
            set_progress(
                progress,
                format!("Upload Saldeo {upload_index}/{uploadable_count}: {label}"),
            );
        }
        let bytes = match fs::read(&source_path) {
            Ok(bytes) => bytes,
            Err(err) => {
                item.upload_status = "failed".to_string();
                item.error = Some(format!("odczyt {source_path}: {err}"));
                continue;
            }
        };
        let file_sha256 = saldeo_file_sha256(&bytes);
        item.file_sha256 = Some(file_sha256.clone());
        if let Some(entry) = saldeo_upload_ledger_get(conn, &file_sha256)? {
            saldeo_mark_ledger_hit(item, &entry);
            continue;
        }
        let Some((year, month)) = saldeo_apply_period_rule(item, plan_year, today) else {
            continue;
        };
        let entry = match upload(Path::new(&source_path), bytes, year, month) {
            SaldeoUploadOutcome::Uploaded {
                year,
                month,
                doc_upload_id,
                response_status,
                body,
            } => {
                item.upload_status = "uploaded".to_string();
                item.saldeo_response_status = Some(response_status);
                item.saldeo_response_body = Some(body);
                item.saldeo_year = Some(year);
                item.saldeo_month = Some(month);
                item.saldeo_doc_upload_id = Some(doc_upload_id);
                SaldeoUploadLedgerEntry {
                    file_sha256,
                    source_path: Some(source_path.clone()),
                    invoice_number: item.invoice_number.clone(),
                    saldeo_year: year,
                    saldeo_month: month,
                    doc_upload_id: Some(doc_upload_id),
                    response_status: Some(response_status),
                    state: SALDEO_LEDGER_UPLOADED.to_string(),
                    error: None,
                    uploaded_at: Utc::now(),
                }
            }
            SaldeoUploadOutcome::Unconfirmed {
                year,
                month,
                doc_upload_id,
                response_status,
                error,
            } => {
                item.upload_status = "unconfirmed".to_string();
                item.saldeo_response_status = response_status;
                item.saldeo_year = Some(year);
                item.saldeo_month = Some(month);
                item.saldeo_doc_upload_id = Some(doc_upload_id);
                item.error = Some(error.clone());
                SaldeoUploadLedgerEntry {
                    file_sha256,
                    source_path: Some(source_path.clone()),
                    invoice_number: item.invoice_number.clone(),
                    saldeo_year: year,
                    saldeo_month: month,
                    doc_upload_id: Some(doc_upload_id),
                    response_status,
                    state: SALDEO_LEDGER_UNCONFIRMED.to_string(),
                    error: Some(error),
                    uploaded_at: Utc::now(),
                }
            }
            SaldeoUploadOutcome::Failed { error } => {
                item.upload_status = "failed".to_string();
                item.error = Some(error);
                continue;
            }
        };
        if let Err(err) = saldeo_upload_ledger_put(conn, &entry) {
            let message =
                format!("plik jest w Saldeo, ale zapis rejestru uploadów nie powiódł się: {err:#}");
            item.error = Some(match item.error.take() {
                Some(previous) => format!("{previous}; {message}"),
                None => message,
            });
            return Err(err.context(format!(
                "zapis rejestru uploadów Saldeo dla {source_path}; przerywam, żeby nie wysłać pliku ponownie"
            )));
        }
    }
    Ok(())
}

pub(crate) struct SaldeoSession {
    pub(crate) cookie_header: String,
    pub(crate) xsrf: String,
}

pub(crate) fn read_saldeo_session(storage_state: &Path) -> Result<SaldeoSession> {
    let storage: Value = serde_json::from_str(&read_saldeo_storage_state(storage_state)?)?;
    let cookies = storage
        .get("cookies")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("storage-state Saldeo nie zawiera cookies"))?;
    let cookie_header = cookies
        .iter()
        .filter_map(|cookie| {
            let name = cookie.get("name")?.as_str()?;
            let value = cookie.get("value")?.as_str()?;
            Some(format!("{name}={value}"))
        })
        .collect::<Vec<_>>()
        .join("; ");
    let xsrf = cookies
        .iter()
        .find(|cookie| cookie.get("name").and_then(|v| v.as_str()) == Some("X-SALDEO-XSRF-C-TOKEN"))
        .and_then(|cookie| cookie.get("value"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("brak X-SALDEO-XSRF-C-TOKEN w storage-state; odśwież sesję Saldeo"))?
        .to_string();
    Ok(SaldeoSession {
        cookie_header,
        xsrf,
    })
}

pub(crate) fn saldeo_period_is_closed(response: &Value) -> bool {
    if response.get("status").and_then(Value::as_str) != Some("VALIDATION_ERROR") {
        return false;
    }
    response
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|error| {
            let field = error.get("field").and_then(Value::as_str).unwrap_or("");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            field.eq_ignore_ascii_case("month")
                && (message.contains("zamknięty") || message.contains("closed"))
        })
}

pub(crate) fn saldeo_fallback_upload_period(
    closed_year: i32,
    closed_month: u32,
    now: NaiveDate,
) -> (i32, u32) {
    let (next_year, next_month) = if closed_month >= 12 {
        (closed_year + 1, 1)
    } else {
        (closed_year, closed_month.max(1) + 1)
    };
    let now_year = now.year();
    let now_month = now.month();
    if (next_year, next_month) >= (now_year, now_month) {
        (next_year, next_month)
    } else {
        (now_year, now_month)
    }
}

#[allow(clippy::too_many_arguments)]
fn saldeo_generate_upload_urls(
    client: &Client,
    session: &SaldeoSession,
    upload_url: &str,
    file_name: &str,
    content_type: &str,
    size: usize,
    year: i32,
    month: u32,
) -> Result<Value> {
    let body = serde_json::json!({
        "year": year,
        "month": month,
        "documentTypeId": -1,
        "files": [{
            "filename": file_name,
            "contentType": content_type,
            "size": size,
        }],
        "clientId": null,
    });
    client
        .post(upload_url)
        .header("Cookie", &session.cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", &session.xsrf)
        .header("saldeoApp", "angularApp")
        .header("timeout", "60000")
        .json(&body)
        .send()
        .with_context(|| format!("Saldeo generate upload URL {file_name}"))?
        .error_for_status()?
        .json()
        .map_err(Into::into)
}

/// Prosi Saldeo o URL uploadu, przechodząc z zamkniętego miesiąca do następnego otwartego.
/// Zwraca okres faktycznie użyty i odpowiedź `generate` (status `SUCCESS`).
pub(crate) fn saldeo_resolve_upload_period(
    year: i32,
    month: u32,
    today: NaiveDate,
    mut generate: impl FnMut(i32, u32) -> Result<Value>,
) -> Result<(i32, u32, Value)> {
    let mut year = year;
    let mut month = month;
    for _ in 0..14 {
        let generated = generate(year, month)?;
        if saldeo_period_is_closed(&generated) {
            let (next_year, next_month) = saldeo_fallback_upload_period(year, month, today);
            if (next_year, next_month) == (year, month) {
                return Err(anyhow!("Saldeo generate upload URL failed: {generated}"));
            }
            year = next_year;
            month = next_month;
            continue;
        }
        if generated.get("status").and_then(Value::as_str) != Some("SUCCESS") {
            return Err(anyhow!("Saldeo generate upload URL failed: {generated}"));
        }
        return Ok((year, month, generated));
    }
    Err(anyhow!(
        "Saldeo generate upload URL failed: brak otwartego miesiąca do zapisu"
    ))
}

/// Ocena odpowiedzi `doc-upload/{id}/confirm`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SaldeoConfirmCheck {
    Confirmed,
    /// Saldeo jednoznacznie odrzuciło potwierdzenie (HTTP != 2xx albo status != SUCCESS).
    Rejected(String),
    /// Odpowiedź bez rozpoznawalnego statusu — nie wiadomo, czy dokument powstał.
    Unknown(String),
}

pub(crate) fn saldeo_check_confirm_response(http_status: u16, body: &str) -> SaldeoConfirmCheck {
    let snippet = body.chars().take(500).collect::<String>();
    if !(200..300).contains(&http_status) {
        return SaldeoConfirmCheck::Rejected(format!(
            "Saldeo confirm failed HTTP {http_status}: {snippet}"
        ));
    }
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return SaldeoConfirmCheck::Unknown(format!(
            "Saldeo confirm HTTP {http_status} bez odpowiedzi JSON: {snippet:?}"
        ));
    };
    match value.get("status").and_then(Value::as_str) {
        Some("SUCCESS") => SaldeoConfirmCheck::Confirmed,
        Some(status) => {
            SaldeoConfirmCheck::Rejected(format!("Saldeo confirm status {status}: {snippet}"))
        }
        None => SaldeoConfirmCheck::Unknown(format!(
            "Saldeo confirm HTTP {http_status} bez pola status: {snippet}"
        )),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn saldeo_upload_bytes(
    client: &Client,
    session: &SaldeoSession,
    upload_url: &str,
    path: &Path,
    bytes: Vec<u8>,
    year: i32,
    month: u32,
    today: NaiveDate,
) -> SaldeoUploadOutcome {
    let failed = |error: String| SaldeoUploadOutcome::Failed { error };
    let Some(file_name) = path.file_name().and_then(|v| v.to_str()) else {
        return failed(format!("brak nazwy pliku: {}", path.display()));
    };
    let content_type = content_type_for_path(path);
    let (year, month, response) =
        match saldeo_resolve_upload_period(year, month, today, |year, month| {
            saldeo_generate_upload_urls(
                client,
                session,
                upload_url,
                file_name,
                content_type,
                bytes.len(),
                year,
                month,
            )
        }) {
            Ok(resolved) => resolved,
            Err(err) => return failed(format!("{err:#}")),
        };
    let Some(upload) = response.get("data").and_then(|v| v.get(file_name)) else {
        return failed(format!(
            "Saldeo response missing file entry for {file_name}: {response}"
        ));
    };
    let Some(doc_upload_id) = upload.get("docUploadId").and_then(|v| v.as_i64()) else {
        return failed(format!("Saldeo response missing docUploadId: {upload}"));
    };
    let Some(signed_url) = upload.get("url").and_then(|v| v.as_str()) else {
        return failed(format!("Saldeo response missing upload url: {upload}"));
    };
    let download_filename = upload
        .get("downloadFilename")
        .and_then(|v| v.as_str())
        .unwrap_or(file_name);
    let local_storage = upload
        .get("localStorage")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let upload_result = if local_storage {
        let part =
            reqwest::blocking::multipart::Part::bytes(bytes).file_name(file_name.to_string());
        let form = reqwest::blocking::multipart::Form::new().part("file", part);
        client.put(signed_url).multipart(form).send()
    } else {
        client
            .put(signed_url)
            .header("Content-Type", content_type)
            .header(
                "Content-Disposition",
                format!("attachment; filename=\"{download_filename}\""),
            )
            .body(bytes)
            .send()
    };

    if let Err(err) = upload_result.and_then(|r| r.error_for_status()) {
        let reject = saldeo_reject_upload(client, session, doc_upload_id, &err.to_string());
        let reject_note = match reject {
            Ok(()) => String::new(),
            Err(reject_err) => format!(" (reject docUploadId {doc_upload_id}: {reject_err})"),
        };
        return failed(format!("Saldeo signed upload failed: {err}{reject_note}"));
    }

    let confirm_url =
        format!("https://saldeo.brainshare.pl/rest/doc-upload/{doc_upload_id}/confirm");
    let unconfirmed =
        |response_status: Option<u16>, error: String| SaldeoUploadOutcome::Unconfirmed {
            year,
            month,
            doc_upload_id,
            response_status,
            error,
        };
    let confirm = match client
        .post(&confirm_url)
        .header("Cookie", &session.cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", &session.xsrf)
        .header("saldeoApp", "angularApp")
        .header("timeout", "60000")
        .json(&serde_json::json!({}))
        .send()
    {
        Ok(confirm) => confirm,
        Err(err) => {
            return unconfirmed(
                None,
                format!("Saldeo confirm upload {doc_upload_id}: {err}"),
            );
        }
    };
    let status = confirm.status().as_u16();
    let text = match confirm.text() {
        Ok(text) => text,
        Err(err) => {
            return unconfirmed(
                Some(status),
                format!("Saldeo confirm upload {doc_upload_id}: odczyt odpowiedzi: {err}"),
            );
        }
    };
    match saldeo_check_confirm_response(status, &text) {
        SaldeoConfirmCheck::Confirmed => SaldeoUploadOutcome::Uploaded {
            year,
            month,
            doc_upload_id,
            response_status: status,
            body: text.chars().take(2000).collect(),
        },
        SaldeoConfirmCheck::Rejected(error) => {
            let reject = saldeo_reject_upload(client, session, doc_upload_id, &text);
            let reject_note = match reject {
                Ok(()) => format!(" (wysłano reject docUploadId {doc_upload_id})"),
                Err(reject_err) => {
                    format!(" (reject docUploadId {doc_upload_id} nieudany: {reject_err})")
                }
            };
            unconfirmed(Some(status), format!("{error}{reject_note}"))
        }
        SaldeoConfirmCheck::Unknown(error) => unconfirmed(Some(status), error),
    }
}

pub(crate) fn saldeo_reject_upload(
    client: &Client,
    session: &SaldeoSession,
    doc_upload_id: i64,
    reason: &str,
) -> Result<()> {
    let url = format!("https://saldeo.brainshare.pl/rest/doc-upload/{doc_upload_id}/reject");
    client
        .post(url)
        .header("Cookie", &session.cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", &session.xsrf)
        .header("saldeoApp", "angularApp")
        .header("timeout", "60000")
        .body(reason.to_string())
        .send()?
        .error_for_status()?;
    Ok(())
}

pub(crate) fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("xml") => "application/xml",
        Some("json") => "application/json",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    }
}

pub(crate) fn write_saldeo_sync_csv(plan: &SaldeoSyncPlan, path: &Path) -> Result<()> {
    let mut writer =
        csv::Writer::from_path(path).with_context(|| format!("zapis CSV {}", path.display()))?;
    writer.write_record([
        "upload_status",
        "status",
        "source",
        "related_sources",
        "invoice_number",
        "issue_date",
        "gross_amount_minor",
        "currency",
        "contractor",
        "source_path",
        "can_upload",
        "saldeo_response_status",
        "error",
        "saldeo_period",
        "file_sha256",
        "skip_reason",
    ])?;
    for item in &plan.items {
        writer.write_record([
            item.upload_status.clone(),
            item.status.clone(),
            item.source.clone(),
            item.related_sources.join("+"),
            item.invoice_number.clone().unwrap_or_default(),
            item.issue_date.map(|d| d.to_string()).unwrap_or_default(),
            item.gross_amount_minor
                .map(|v| v.to_string())
                .unwrap_or_default(),
            item.currency.clone().unwrap_or_default(),
            item.contractor.clone().unwrap_or_default(),
            item.source_path.clone().unwrap_or_default(),
            item.can_upload.to_string(),
            item.saldeo_response_status
                .map(|v| v.to_string())
                .unwrap_or_default(),
            item.error.clone().unwrap_or_default(),
            match (item.saldeo_year, item.saldeo_month) {
                (Some(year), Some(month)) => format!("{year}-{month:02}"),
                _ => String::new(),
            },
            item.file_sha256.clone().unwrap_or_default(),
            item.skip_reason.clone().unwrap_or_default(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

pub(crate) fn default_saldeo_storage_state_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let lab_path = home
        .join(".config")
        .join("lab")
        .join("saldeo-storage-state.json");
    if lab_path.exists() {
        return lab_path;
    }
    home.join(".config")
        .join("ksef-mail-reconcile")
        .join("saldeo-storage-state.json")
}

pub(crate) const SALDEO_OVERRIDE_WARNING_PREFIX: &str = "lab override applied";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SaldeoRecordOverride {
    pub content_hash: String,
    pub invoice_number: Option<String>,
    pub seller_tax_id: Option<String>,
    pub buyer_tax_id: Option<String>,
    pub seller_name: Option<String>,
    pub buyer_name: Option<String>,
    pub issue_date: Option<NaiveDate>,
    pub gross_amount_minor: Option<i64>,
    pub currency: Option<String>,
}

pub(crate) fn default_saldeo_record_overrides_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config")
        .join("lab")
        .join("saldeo-overrides.json")
}

fn saldeo_record_override_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<SaldeoRecordOverride> {
    let issue_date_text: Option<String> = row.get(6)?;
    Ok(SaldeoRecordOverride {
        content_hash: row.get(0)?,
        invoice_number: row.get(1)?,
        seller_tax_id: row.get(2)?,
        buyer_tax_id: row.get(3)?,
        seller_name: row.get(4)?,
        buyer_name: row.get(5)?,
        issue_date: issue_date_text.as_deref().and_then(parse_date),
        gross_amount_minor: row.get(7)?,
        currency: row.get(8)?,
    })
}

fn import_legacy_saldeo_record_overrides(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM saldeo_overrides", [], |row| {
        row.get(0)
    })?;
    if count > 0 {
        return Ok(0);
    }
    let legacy = load_saldeo_record_overrides_from_file()?;
    if legacy.is_empty() {
        return Ok(0);
    }
    for override_row in legacy.values() {
        upsert_saldeo_record_override(conn, override_row)?;
    }
    Ok(legacy.len())
}

fn load_saldeo_record_overrides_from_file() -> Result<HashMap<String, SaldeoRecordOverride>> {
    let path = default_saldeo_record_overrides_path();
    if !path.is_file() {
        return Ok(HashMap::new());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("odczyt {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(HashMap::new());
    }
    let overrides = serde_json::from_str::<Vec<SaldeoRecordOverride>>(&text)
        .with_context(|| format!("niepoprawny override Saldeo {}", path.display()))?;
    Ok(overrides
        .into_iter()
        .map(|override_row| (override_row.content_hash.clone(), override_row))
        .collect())
}

fn load_saldeo_record_overrides_from_db(
    conn: &Connection,
) -> Result<HashMap<String, SaldeoRecordOverride>> {
    let _ = import_legacy_saldeo_record_overrides(conn)?;
    let mut stmt = conn.prepare(
        r#"
        SELECT content_hash, invoice_number, seller_tax_id, buyer_tax_id, seller_name, buyer_name,
               issue_date, gross_amount_minor, currency
        FROM saldeo_overrides ORDER BY updated_at DESC, content_hash ASC
        "#,
    )?;
    let rows = stmt.query_map([], saldeo_record_override_from_row)?;
    let mut overrides = HashMap::new();
    for row in rows {
        let override_row = row?;
        overrides.insert(override_row.content_hash.clone(), override_row);
    }
    Ok(overrides)
}

pub(crate) fn load_saldeo_record_overrides(
    db_path: Option<&Path>,
) -> Result<HashMap<String, SaldeoRecordOverride>> {
    match db_path {
        Some(path) => {
            let conn = open_db(path)?;
            load_saldeo_record_overrides_from_db(&conn)
        }
        None => load_saldeo_record_overrides_from_file(),
    }
}

fn upsert_saldeo_record_override(
    conn: &Connection,
    override_row: &SaldeoRecordOverride,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        r#"
        INSERT INTO saldeo_overrides (
            content_hash, invoice_number, seller_tax_id, buyer_tax_id, seller_name,
            buyer_name, issue_date, gross_amount_minor, currency, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
        ON CONFLICT(content_hash) DO UPDATE SET
            invoice_number = excluded.invoice_number,
            seller_tax_id = excluded.seller_tax_id,
            buyer_tax_id = excluded.buyer_tax_id,
            seller_name = excluded.seller_name,
            buyer_name = excluded.buyer_name,
            issue_date = excluded.issue_date,
            gross_amount_minor = excluded.gross_amount_minor,
            currency = excluded.currency,
            updated_at = excluded.updated_at
        "#,
        params![
            override_row.content_hash,
            override_row.invoice_number,
            override_row.seller_tax_id,
            override_row.buyer_tax_id,
            override_row.seller_name,
            override_row.buyer_name,
            override_row.issue_date.map(|d| d.to_string()),
            override_row.gross_amount_minor,
            override_row.currency,
            now,
        ],
    )?;
    Ok(())
}

pub(crate) fn save_saldeo_record_override(
    db_path: &Path,
    override_row: &SaldeoRecordOverride,
) -> Result<()> {
    let conn = open_db(db_path)?;
    let _ = import_legacy_saldeo_record_overrides(&conn)?;
    upsert_saldeo_record_override(&conn, override_row)
}

pub(crate) struct SaldeoRepairCandidate {
    pub(crate) override_row: SaldeoRecordOverride,
    pub(crate) invoice_number: Option<String>,
    pub(crate) changed_fields: Vec<String>,
}

pub(crate) fn saldeo_override_from_record(record: &InvoiceRecord) -> SaldeoRecordOverride {
    SaldeoRecordOverride {
        content_hash: record.content_hash.clone(),
        invoice_number: record.invoice_number.clone(),
        seller_tax_id: record.seller_tax_id.clone(),
        buyer_tax_id: record.buyer_tax_id.clone(),
        seller_name: record.seller_name.clone(),
        buyer_name: record.buyer_name.clone(),
        issue_date: record.issue_date,
        gross_amount_minor: record.gross_amount_minor,
        currency: record.currency.clone(),
    }
}

pub(crate) fn saldeo_override_changed_fields(
    before: &InvoiceRecord,
    after: &InvoiceRecord,
) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if before.invoice_number != after.invoice_number {
        changed.push("invoice_number");
    }
    if before.seller_tax_id != after.seller_tax_id {
        changed.push("seller_tax_id");
    }
    if before.buyer_tax_id != after.buyer_tax_id {
        changed.push("buyer_tax_id");
    }
    if before.seller_name != after.seller_name {
        changed.push("seller_name");
    }
    if before.buyer_name != after.buyer_name {
        changed.push("buyer_name");
    }
    if before.issue_date != after.issue_date {
        changed.push("issue_date");
    }
    if before.gross_amount_minor != after.gross_amount_minor {
        changed.push("gross_amount_minor");
    }
    if before.currency != after.currency {
        changed.push("currency");
    }
    changed
}

pub(crate) fn repair_saldeo_items_from_report(
    report: &TriReconcileReport,
) -> Vec<SaldeoRepairCandidate> {
    let mut items = Vec::new();
    for row in &report.rows {
        let Some(mut saldeo) = row.saldeo.clone() else {
            continue;
        };
        if saldeo.source != SourceKind::Saldeo {
            continue;
        }
        let original = saldeo.clone();
        if let Some(ksef) = &row.ksef {
            merge_missing_invoice_metadata(&mut saldeo, ksef);
        }
        if let Some(mail) = &row.mail {
            merge_missing_invoice_metadata(&mut saldeo, mail);
        }
        let changed_fields = saldeo_override_changed_fields(&original, &saldeo);
        if changed_fields.is_empty() {
            continue;
        }
        items.push(SaldeoRepairCandidate {
            invoice_number: saldeo.invoice_number.clone(),
            override_row: saldeo_override_from_record(&saldeo),
            changed_fields: changed_fields.into_iter().map(str::to_string).collect(),
        });
    }
    items
}

pub(crate) fn saldeo_duplicate_groups(records: &[InvoiceRecord]) -> Vec<SaldeoDuplicateGroup> {
    let mut by_key = HashMap::<String, Vec<&InvoiceRecord>>::new();
    for record in records {
        if record.source != SourceKind::Saldeo {
            continue;
        }
        let Some(key) = reconcile_dedupe_key(record) else {
            continue;
        };
        by_key.entry(key).or_default().push(record);
    }
    let mut groups = by_key
        .into_iter()
        .filter(|(_, records)| records.len() > 1)
        .map(|(key, records)| {
            let kept = records
                .iter()
                .max_by_key(|record| record_completeness_score(record))
                .expect("duplicate group is not empty");
            SaldeoDuplicateGroup {
                key,
                invoice_number: records
                    .iter()
                    .find_map(|record| record.invoice_number.clone()),
                content_hashes: records
                    .iter()
                    .map(|record| record.content_hash.clone())
                    .collect(),
                kept_content_hash: kept.content_hash.clone(),
            }
        })
        .collect::<Vec<_>>();
    groups.sort_by(|a, b| a.key.cmp(&b.key));
    groups
}

pub(crate) fn saldeo_record_has_override(record: &InvoiceRecord) -> bool {
    record.source == SourceKind::Saldeo
        && record
            .warnings
            .iter()
            .any(|warning| warning.starts_with(SALDEO_OVERRIDE_WARNING_PREFIX))
}

pub(crate) fn apply_saldeo_record_overrides(
    records: &mut [InvoiceRecord],
    db_path: Option<&Path>,
) -> Result<usize> {
    let overrides = load_saldeo_record_overrides(db_path)?;
    let mut applied = 0usize;
    for record in records.iter_mut() {
        if record.source != SourceKind::Saldeo {
            continue;
        }
        if let Some(override_row) = overrides.get(&record.content_hash)
            && apply_saldeo_record_override(record, override_row)
        {
            applied += 1;
        }
    }
    Ok(applied)
}

pub(crate) fn apply_saldeo_record_override(
    record: &mut InvoiceRecord,
    override_row: &SaldeoRecordOverride,
) -> bool {
    if saldeo_record_has_override(record)
        && record.invoice_number == override_row.invoice_number
        && record.seller_tax_id == override_row.seller_tax_id
        && record.buyer_tax_id == override_row.buyer_tax_id
        && record.seller_name == override_row.seller_name
        && record.buyer_name == override_row.buyer_name
        && record.issue_date == override_row.issue_date
        && record.gross_amount_minor == override_row.gross_amount_minor
        && record.currency == override_row.currency
    {
        return false;
    }

    let mut changed_fields = Vec::new();
    macro_rules! set_field {
        ($field:ident, $name:literal) => {
            if record.$field != override_row.$field {
                changed_fields.push($name);
                record.$field = override_row.$field.clone();
            }
        };
    }
    set_field!(invoice_number, "invoice_number");
    set_field!(seller_tax_id, "seller_tax_id");
    set_field!(buyer_tax_id, "buyer_tax_id");
    set_field!(seller_name, "seller_name");
    set_field!(buyer_name, "buyer_name");
    set_field!(issue_date, "issue_date");
    set_field!(gross_amount_minor, "gross_amount_minor");
    set_field!(currency, "currency");

    record
        .warnings
        .retain(|warning| !warning.starts_with(SALDEO_OVERRIDE_WARNING_PREFIX));
    if changed_fields.is_empty() {
        record
            .warnings
            .push(SALDEO_OVERRIDE_WARNING_PREFIX.to_string());
    } else {
        record.warnings.push(format!(
            "{}: {}",
            SALDEO_OVERRIDE_WARNING_PREFIX,
            changed_fields.join(",")
        ));
    }
    true
}

pub(crate) fn edit_saldeo_record_override(record: &InvoiceRecord, db_path: &Path) -> Result<bool> {
    if record.source != SourceKind::Saldeo {
        return Err(anyhow!("poprawa działa tylko dla rekordów Saldeo"));
    }

    eprintln!(
        "\nPoprawiam rekord Saldeo: {} ({})",
        record.invoice_number.as_deref().unwrap_or("bez numeru"),
        record.content_hash
    );
    eprintln!(
        "  kontrahent: {}",
        record
            .seller_name
            .as_deref()
            .or(record.buyer_name.as_deref())
            .unwrap_or("-")
    );

    let invoice_number =
        prompt_string_value("Numer faktury", record.invoice_number.as_deref(), |value| {
            Some(clean_invoice_number(value))
        })?;
    let seller_name = prompt_string_value(
        "Sprzedawca / kontrahent",
        record.seller_name.as_deref(),
        |value| Some(clean_name(value).unwrap_or_else(|| value.trim().to_string())),
    )?;
    let buyer_name = prompt_string_value("Nabywca", record.buyer_name.as_deref(), |value| {
        Some(clean_name(value).unwrap_or_else(|| value.trim().to_string()))
    })?;
    let seller_tax_id = prompt_string_value(
        "NIP sprzedawcy",
        record.seller_tax_id.as_deref(),
        normalize_tax_id,
    )?;
    let buyer_tax_id = prompt_string_value(
        "NIP nabywcy",
        record.buyer_tax_id.as_deref(),
        normalize_tax_id,
    )?;
    let issue_date = prompt_value(
        "Data wystawienia",
        record.issue_date.as_ref(),
        |date| date.to_string(),
        parse_date,
    )?;
    let gross_amount_minor = prompt_value(
        "Kwota brutto",
        record.gross_amount_minor.as_ref(),
        |amount| format_minor_money(*amount),
        parse_money_minor,
    )?;
    let currency = prompt_string_value("Waluta", record.currency.as_deref(), normalize_currency)?;

    let override_row = SaldeoRecordOverride {
        content_hash: record.content_hash.clone(),
        invoice_number,
        seller_tax_id,
        buyer_tax_id,
        seller_name,
        buyer_name,
        issue_date,
        gross_amount_minor,
        currency,
    };

    let overrides = load_saldeo_record_overrides(Some(db_path))?;
    if overrides.get(&override_row.content_hash) == Some(&override_row) {
        eprintln!("⏭ Bez zmian.\n");
        return Ok(false);
    }
    if overrides.get(&override_row.content_hash).is_none()
        && override_row.invoice_number == record.invoice_number
        && override_row.seller_tax_id == record.seller_tax_id
        && override_row.buyer_tax_id == record.buyer_tax_id
        && override_row.seller_name == record.seller_name
        && override_row.buyer_name == record.buyer_name
        && override_row.issue_date == record.issue_date
        && override_row.gross_amount_minor == record.gross_amount_minor
        && override_row.currency == record.currency
    {
        eprintln!("⏭ Bez zmian.\n");
        return Ok(false);
    }

    let confirm = Confirm::new()
        .with_prompt(format!(
            "Zapisać poprawki do SQLite: {}?",
            db_path.display()
        ))
        .default(true)
        .interact()?;
    if !confirm {
        eprintln!("⏭ Anulowano.\n");
        return Ok(false);
    }

    save_saldeo_record_override(db_path, &override_row)?;
    eprintln!(
        "✓ Zapisano poprawki Saldeo w SQLite: {}\n",
        db_path.display()
    );
    Ok(true)
}

fn prompt_string_value(
    label: &str,
    current: Option<&str>,
    parser: fn(&str) -> Option<String>,
) -> Result<Option<String>> {
    let current_text = current.unwrap_or("-");
    loop {
        let input = Input::<String>::new()
            .with_prompt(format!(
                "{} [{}] (Enter=bez zmian, '-'=wyczyść)",
                label, current_text
            ))
            .allow_empty(true)
            .interact_text()?;
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Ok(current.map(|value| value.to_string()));
        }
        if trimmed == "-" {
            return Ok(None);
        }
        if let Some(value) = parser(trimmed) {
            return Ok(Some(value));
        }
        eprintln!("✗ Niepoprawna wartość dla {label}: {trimmed}");
    }
}

fn prompt_value<T, F, G>(
    label: &str,
    current: Option<&T>,
    format_current: G,
    parser: F,
) -> Result<Option<T>>
where
    T: Clone,
    F: Fn(&str) -> Option<T>,
    G: Fn(&T) -> String,
{
    let current_text = current
        .map(|value| format_current(value))
        .unwrap_or_else(|| "-".to_string());
    loop {
        let input = Input::<String>::new()
            .with_prompt(format!(
                "{} [{}] (Enter=bez zmian, '-'=wyczyść)",
                label, current_text
            ))
            .allow_empty(true)
            .interact_text()?;
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Ok(current.cloned());
        }
        if trimmed == "-" {
            return Ok(None);
        }
        if let Some(value) = parser(trimmed) {
            return Ok(Some(value));
        }
        eprintln!("✗ Niepoprawna wartość dla {label}: {trimmed}");
    }
}

#[allow(dead_code)]
pub(crate) fn saldeo_fetch(
    year: i32,
    storage_state: &Path,
    out_dir: &Path,
    db_path: Option<&Path>,
) -> Result<SaldeoFetchResult> {
    saldeo_fetch_with_progress(year, storage_state, out_dir, db_path, None)
}

pub(crate) fn saldeo_fetch_with_progress(
    year: i32,
    storage_state: &Path,
    out_dir: &Path,
    _db_path: Option<&Path>,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<SaldeoFetchResult> {
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!("Saldeo: przygotowanie katalogu ({year})..."),
        );
    }
    fs::create_dir_all(out_dir).with_context(|| format!("mkdir {}", out_dir.display()))?;
    if let Some(progress) = &progress {
        set_progress(progress, "Saldeo: odczyt zapisanej sesji...");
    }
    ensure_saldeo_session_or_auth(progress.clone())?;
    let storage: Value = serde_json::from_str(&read_saldeo_storage_state(storage_state)?)?;
    let cookies = storage
        .get("cookies")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("storage-state Saldeo nie zawiera cookies"))?;
    let cookie_header = cookies
        .iter()
        .filter_map(|cookie| {
            let name = cookie.get("name")?.as_str()?;
            let value = cookie.get("value")?.as_str()?;
            Some(format!("{name}={value}"))
        })
        .collect::<Vec<_>>()
        .join("; ");
    let xsrf = cookies
        .iter()
        .find(|cookie| cookie.get("name").and_then(|v| v.as_str()) == Some("X-SALDEO-XSRF-C-TOKEN"))
        .and_then(|cookie| cookie.get("value"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow!("brak X-SALDEO-XSRF-C-TOKEN w storage-state; odśwież sesję Saldeo")
        })?;

    let client = Client::builder().build()?;
    let mut documents = Vec::new();
    for month in 1..=12 {
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Saldeo: pobieram dokumenty {year}-{month:02} (razem {})...",
                    documents.len()
                ),
            );
        }
        let body = serde_json::json!({
            "pagination": {
                "pageNumber": 0,
                "pageSize": -1,
                "totalCount": 0,
                "columnSorted": { "sortColumn": "DOCUMENT_CREATE_DATE", "sortDirection": "ASC" }
            },
            "filter": {
                "period": { "partOfYear": month, "year": year, "selectionType": "selectedMonth" },
                "duplicatesEnable": false,
                "duplicates": false,
                "splitPayment": false,
                "types": [],
                "contractors": [],
                "stages": [],
                "categories": [],
                "registers": [],
                "tags": [],
                "assignUsers": [],
                "addedBy": [],
                "added": [],
                "paymentStatuses": [],
                "accountingPaymentTypes": [],
                "searchQuery": "",
                "selectKsefDocumentsYesCheckbox": false,
                "selectKsefDocumentsNoCheckbox": false,
                "ksefNumber": "",
                "ksefMiniWorkflowStatus": null,
                "ksefBoId": null,
                "dimensionReportDocumentIds": [],
                "dimensions": null
            }
        });
        let value: Value = client
            .post("https://saldeo.brainshare.pl/rest/client/document/list/search")
            .header("Cookie", &cookie_header)
            .header("X-SALDEO-XSRF-H-TOKEN", xsrf)
            .header("saldeoApp", "angularApp")
            .header("timeout", "60000")
            .json(&body)
            .send()?
            .error_for_status()
            .map_err(|e| {
                if e.status() == Some(reqwest::StatusCode::UNAUTHORIZED) {
                    anyhow!(
                        "Sesja Saldeo wygasła (401). Odśwież Playwright storage state:\n  {}",
                        default_saldeo_storage_state_path().display()
                    )
                } else {
                    anyhow!("Saldeo document/list/search month={month}: {e}")
                }
            })?
            .json()
            .with_context(|| {
                format!("Saldeo document/list/search {year}-{month:02}: odpowiedź nie jest JSON")
            })?;
        let items = saldeo_document_list_items(&value, year, month)?;
        let month_count = items.len();
        for mut item in items {
            if let Value::Object(ref mut map) = item {
                map.insert("saldeoMonth".to_string(), serde_json::json!(month));
            }
            documents.push(item);
        }
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Saldeo: {year}-{month:02}: +{month_count} (razem {})",
                    documents.len()
                ),
            );
        }
    }

    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!(
                "Saldeo: konwersja {} dokumentów do rekordów...",
                documents.len()
            ),
        );
    }
    let mut records = saldeo_documents_to_records(&documents);
    if let Some(progress) = &progress {
        set_progress(progress, "Saldeo: uzupełnianie z lokalnych danych KSeF...");
    }
    saldeo_enrich_records_from_ksef(&mut records, year)?;
    saldeo_enrich_records_from_downloads_with_progress(
        &client,
        &cookie_header,
        out_dir,
        &documents,
        &mut records,
        progress.clone(),
    )?;
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!("Saldeo: zapis {} rekordów...", records.len()),
        );
    }
    // Pliki podmieniamy dopiero po pobraniu wszystkich 12 miesięcy (każdy błąd wyżej kończy
    // funkcję przed zapisem), atomowo: tmp + rename. Bazę zapisują wywołujący po Ok.
    let raw_output = out_dir.join("documents.json");
    let records_output = out_dir.join("records.jsonl");
    let mut jsonl = Vec::new();
    for record in &records {
        serde_json::to_writer(&mut jsonl, record)?;
        jsonl.push(b'\n');
    }
    write_private_file(&raw_output, &serde_json::to_vec_pretty(&documents)?)?;
    write_private_file(&records_output, &jsonl)?;

    Ok(SaldeoFetchResult {
        summary: SaldeoFetchSummary {
            year,
            documents_count: documents.len(),
            records_count: records.len(),
            raw_output: raw_output.display().to_string(),
            records_output: records_output.display().to_string(),
        },
        records,
    })
}

/// Walidacja odpowiedzi `document/list/search` dla jednego miesiąca. HTTP 200 nie wystarcza:
/// treść z błędem albo bez `data.resultCollection` wyglądałaby jak pusty miesiąc, a wtedy
/// każda faktura z Gmaila trafiłaby do planu uploadu.
pub(crate) fn saldeo_document_list_items(
    value: &Value,
    year: i32,
    month: u32,
) -> Result<Vec<Value>> {
    let context = format!("Saldeo document/list/search {year}-{month:02}");
    let snippet = || value.to_string().chars().take(500).collect::<String>();
    if let Some(status) = value.get("status")
        && status.as_str() != Some("SUCCESS")
    {
        return Err(anyhow!("{context}: status {status}: {}", snippet()));
    }
    let data = value.get("data");
    let items = data
        .and_then(|d| d.get("resultCollection"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            anyhow!(
                "{context}: brak tablicy data.resultCollection: {}",
                snippet()
            )
        })?;
    let total = data.and_then(|d| {
        d.get("totalCount")
            .map(|v| ("data.totalCount", v))
            .or_else(|| {
                d.get("pagination")
                    .and_then(|p| p.get("totalCount"))
                    .map(|v| ("data.pagination.totalCount", v))
            })
    });
    if let Some((field, total)) = total
        && let Some(total) = total.as_u64()
        && total != items.len() as u64
    {
        return Err(anyhow!(
            "{context}: {field} = {total}, a odpowiedź zawiera {} dokumentów — niepełna lista",
            items.len()
        ));
    }
    Ok(items.clone())
}

pub(crate) fn load_saldeo_records(
    input: &Path,
    db_path: Option<&Path>,
) -> Result<Vec<InvoiceRecord>> {
    let mut records = if input.extension().and_then(|e| e.to_str()) == Some("jsonl") {
        load_records(SourceKind::Saldeo, input)?
    } else {
        let text = fs::read_to_string(input)
            .with_context(|| format!("odczyt Saldeo {}", input.display()))?;
        if let Ok(mut records) = serde_json::from_str::<Vec<InvoiceRecord>>(&text) {
            for record in &mut records {
                record.source = SourceKind::Saldeo;
            }
            records
        } else {
            let value: Value = serde_json::from_str(&text)?;
            let docs = value.as_array().ok_or_else(|| {
                anyhow!("Saldeo input musi być tablicą documents albo InvoiceRecord[]")
            })?;
            saldeo_documents_to_records(docs)
        }
    };
    let overridden_count = apply_saldeo_record_overrides(&mut records, db_path)?;
    if overridden_count > 0 {
        eprintln!("  [Saldeo] zastosowano lokalne poprawki: {overridden_count} rekordów");
    }
    Ok(records)
}

pub(crate) fn saldeo_documents_to_records(documents: &[Value]) -> Vec<InvoiceRecord> {
    documents
        .iter()
        .filter_map(saldeo_document_to_record)
        .collect()
}

pub(crate) fn saldeo_enrich_records_from_ksef(
    records: &mut [InvoiceRecord],
    year: i32,
) -> Result<()> {
    let ksef_path = configured_ksef_out_path(year);
    if !ksef_path.exists() {
        return Ok(());
    }
    let ksef_records = load_records(SourceKind::Ksef, &ksef_path)
        .with_context(|| format!("odczyt lokalnych metadanych KSeF {}", ksef_path.display()))?;
    let by_ksef = ksef_records
        .iter()
        .filter_map(|record| {
            record
                .ksef_reference
                .as_ref()
                .map(|ksef| (ksef.clone(), record))
        })
        .collect::<HashMap<_, _>>();
    let mut enriched = 0usize;
    for record in records.iter_mut() {
        let Some(ksef_reference) = record.ksef_reference.as_ref() else {
            continue;
        };
        let Some(ksef_record) = by_ksef.get(ksef_reference) else {
            continue;
        };
        if merge_missing_invoice_metadata(record, ksef_record) {
            enriched += 1;
            record
                .warnings
                .push("saldeo enriched from ksef metadata".to_string());
        }
    }
    if enriched > 0 {
        eprintln!("  [Saldeo] uzupełniono z KSeF: {enriched} rekordów");
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn saldeo_enrich_records_from_downloads(
    client: &Client,
    cookie_header: &str,
    out_dir: &Path,
    documents: &[Value],
    records: &mut [InvoiceRecord],
) -> Result<()> {
    saldeo_enrich_records_from_downloads_with_progress(
        client,
        cookie_header,
        out_dir,
        documents,
        records,
        None,
    )
}

pub(crate) fn saldeo_enrich_records_from_downloads_with_progress(
    client: &Client,
    cookie_header: &str,
    out_dir: &Path,
    documents: &[Value],
    records: &mut [InvoiceRecord],
    progress: Option<Arc<Mutex<String>>>,
) -> Result<()> {
    let mut by_hash = records
        .iter()
        .enumerate()
        .map(|(idx, record)| (record.content_hash.clone(), idx))
        .collect::<HashMap<_, _>>();
    let needs_download = documents
        .iter()
        .filter(|doc| {
            let Some(document_id) = saldeo_document_id_from_value(doc) else {
                return false;
            };
            let key = format!("saldeo:{document_id}");
            let Some(record_idx) = by_hash.get(&key).copied() else {
                return false;
            };
            saldeo_record_needs_document_fallback(&records[record_idx])
                && json_string(doc, "downloadUrl").is_some()
        })
        .count();
    let mut checked = 0usize;
    let mut enriched = 0usize;
    for doc in documents {
        let Some(document_id) = saldeo_document_id_from_value(doc) else {
            continue;
        };
        let key = format!("saldeo:{document_id}");
        let Some(record_idx) = by_hash.get(&key).copied() else {
            continue;
        };
        if !saldeo_record_needs_document_fallback(&records[record_idx]) {
            continue;
        }
        let Some(download_url) = json_string(doc, "downloadUrl") else {
            continue;
        };
        checked += 1;
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Saldeo: uzupełnianie z plików {checked}/{needs_download} (doc {document_id})..."
                ),
            );
        }
        let local_path = saldeo_cached_document_path(out_dir, doc, &document_id);
        if let Err(err) =
            saldeo_download_document(client, cookie_header, &download_url, &local_path)
        {
            records[record_idx]
                .warnings
                .push(format!("saldeo download fallback failed: {err}"));
            continue;
        }
        match parse_file(SourceKind::Saldeo, &local_path) {
            Ok(parsed) => {
                if merge_missing_invoice_metadata(&mut records[record_idx], &parsed) {
                    records[record_idx]
                        .warnings
                        .push("saldeo enriched from downloaded document".to_string());
                    enriched += 1;
                }
            }
            Err(err) => records[record_idx]
                .warnings
                .push(format!("saldeo parse fallback failed: {err}")),
        }
    }
    if enriched > 0 {
        eprintln!("  [Saldeo] uzupełniono z pobranych plików: {enriched} rekordów");
    }
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!("Saldeo: uzupełnianie z plików gotowe ({enriched} rekordów)"),
        );
    }
    by_hash.clear();
    Ok(())
}

pub(crate) fn saldeo_document_id_from_value(doc: &Value) -> Option<String> {
    json_scalar_string(doc, "documentId")
}

pub(crate) fn saldeo_cached_document_path(
    out_dir: &Path,
    doc: &Value,
    document_id: &str,
) -> PathBuf {
    let filename = json_string(doc, "filename")
        .or_else(|| json_string(doc, "name"))
        .unwrap_or_else(|| format!("{document_id}.bin"));
    out_dir
        .join("files")
        .join(format!("{}_{}", document_id, sanitize_filename(&filename)))
}

pub(crate) fn saldeo_download_document(
    client: &Client,
    cookie_header: &str,
    download_url: &str,
    local_path: &Path,
) -> Result<()> {
    if local_path.is_file() && local_path.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
        return Ok(());
    }
    if let Some(parent) = local_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let bytes = client
        .get(download_url)
        .header("Cookie", cookie_header)
        .send()
        .with_context(|| format!("Saldeo download {download_url}"))?
        .error_for_status()
        .with_context(|| format!("Saldeo download status {download_url}"))?
        .bytes()
        .with_context(|| format!("Saldeo download body {download_url}"))?;
    fs::write(local_path, &bytes).with_context(|| format!("zapis {}", local_path.display()))?;
    Ok(())
}

pub(crate) fn record_has_counterparty(record: &InvoiceRecord) -> bool {
    let seller = record.seller_name.as_deref();
    let buyer = record.buyer_name.as_deref();
    let seller_useful = seller.is_some_and(counterparty_name_is_useful);
    let buyer_useful = buyer.is_some_and(counterparty_name_is_useful);
    let seller_suspicious = seller.is_some_and(|value| !counterparty_name_is_useful(value));
    let buyer_suspicious = buyer.is_some_and(|value| !counterparty_name_is_useful(value));
    (seller_useful || buyer_useful) && !seller_suspicious && !buyer_suspicious
}

fn counterparty_name_is_useful(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() < 3
        || !trimmed.chars().any(|c| c.is_alphabetic())
    {
        return false;
    }
    let normalized = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>();
    !matches!(
        normalized.as_str(),
        "nabywca"
            | "buyer"
            | "sprzedawca"
            | "seller"
            | "kontrahent"
            | "contractor"
            | "customer"
            | "klient"
            | "odbiorca"
            | "supplier"
            | "vendor"
            | "wystawca"
            | "dostawca"
    )
}

fn replace_counterparty_name(target: &mut Option<String>, source: &Option<String>) -> bool {
    let should_replace = target
        .as_deref()
        .map(|value| !counterparty_name_is_useful(value))
        .unwrap_or(true);
    if !should_replace {
        return false;
    }
    let Some(value) = source
        .as_deref()
        .map(str::trim)
        .filter(|v| counterparty_name_is_useful(v))
    else {
        return false;
    };
    let value = value.to_string();
    if target.as_ref() != Some(&value) {
        *target = Some(value);
        return true;
    }
    false
}

fn replace_tax_id(target: &mut Option<String>, source: &Option<String>) -> bool {
    let should_replace = target
        .as_deref()
        .map(|value| normalize_tax_id(value).is_none())
        .unwrap_or(true);
    if !should_replace {
        return false;
    }
    let Some(value) = source.as_deref().and_then(normalize_tax_id) else {
        return false;
    };
    if target.as_ref() != Some(&value) {
        *target = Some(value);
        return true;
    }
    false
}

fn saldeo_record_needs_document_fallback(record: &InvoiceRecord) -> bool {
    record
        .invoice_number
        .as_deref()
        .is_none_or(looks_like_filename_invoice_number)
        || record.issue_date.is_none()
        || record.gross_amount_minor.is_none()
        || !record_has_counterparty(record)
}

pub(crate) fn merge_missing_invoice_metadata(
    target: &mut InvoiceRecord,
    source: &InvoiceRecord,
) -> bool {
    let mut changed = false;
    macro_rules! fill_clone {
        ($field:ident) => {
            if target.$field.is_none() {
                if let Some(value) = source.$field.clone() {
                    target.$field = Some(value);
                    changed = true;
                }
            }
        };
    }
    macro_rules! fill_copy {
        ($field:ident) => {
            if target.$field.is_none() {
                if let Some(value) = source.$field {
                    target.$field = Some(value);
                    changed = true;
                }
            }
        };
    }
    if target.invoice_number.is_none()
        || target
            .invoice_number
            .as_deref()
            .is_some_and(looks_like_filename_invoice_number)
    {
        if let Some(value) = source.invoice_number.clone() {
            if !looks_like_filename_invoice_number(&value) {
                target.invoice_number = Some(value);
                changed = true;
            }
        }
    }
    if replace_tax_id(&mut target.seller_tax_id, &source.seller_tax_id) {
        changed = true;
    }
    if replace_tax_id(&mut target.buyer_tax_id, &source.buyer_tax_id) {
        changed = true;
    }
    if replace_counterparty_name(&mut target.seller_name, &source.seller_name) {
        changed = true;
    }
    if replace_counterparty_name(&mut target.buyer_name, &source.buyer_name) {
        changed = true;
    }
    fill_copy!(issue_date);
    fill_copy!(sale_date);
    fill_copy!(due_date);
    fill_copy!(gross_amount_minor);
    fill_copy!(net_amount_minor);
    fill_copy!(vat_amount_minor);
    fill_clone!(currency);
    fill_clone!(ksef_reference);
    changed
}

pub(crate) fn saldeo_document_to_record(doc: &Value) -> Option<InvoiceRecord> {
    let invoice_number = json_string(doc, "number").or_else(|| json_string(doc, "name"));
    let ksef_reference = json_string(doc, "ksefNumber");
    if invoice_number.is_none() && ksef_reference.is_none() {
        return None;
    }
    let mut record = empty_record(SourceKind::Saldeo);
    record.invoice_number = invoice_number.map(|v| clean_invoice_number(&v));
    record.issue_date =
        json_string(doc, "issueDate").and_then(|v| parse_date(v.get(0..10).unwrap_or(&v)));
    record.sale_date =
        json_string(doc, "saleDate").and_then(|v| parse_date(v.get(0..10).unwrap_or(&v)));
    record.due_date =
        json_string(doc, "paymentDate").and_then(|v| parse_date(v.get(0..10).unwrap_or(&v)));
    record.gross_amount_minor = money_value_to_minor(doc.get("grossPrice"));
    record.net_amount_minor = money_value_to_minor(doc.get("netPrice"));
    record.vat_amount_minor = money_value_to_minor(doc.get("vatPrice"));
    record.currency = json_string(doc, "currency")
        .or_else(|| {
            doc.get("grossPrice")
                .and_then(|v| v.get("currency"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .and_then(|v| normalize_currency(&v));
    record.ksef_reference = ksef_reference;
    record.seller_name = json_string(doc, "contractorDescription")
        .or_else(|| json_string(doc, "contractorName"))
        .and_then(|v| clean_name(&v));
    record.source_path = json_string(doc, "downloadUrl").or_else(|| json_string(doc, "filename"));
    record.content_hash = saldeo_document_id_from_value(doc)
        .map(|id| format!("saldeo:{id}"))
        .or_else(|| record.ksef_reference.clone())
        .unwrap_or_else(|| {
            let raw = serde_json::to_string(doc).unwrap_or_default();
            hex::encode(Sha256::digest(raw.as_bytes()))
        });
    Some(record)
}

pub(crate) fn looks_like_filename_invoice_number(value: &str) -> bool {
    let upper = value.trim().to_ascii_uppercase();
    upper.ends_with(".PDF")
        || upper.ends_with(".XML")
        || upper.ends_with(".JPG")
        || upper.ends_with(".JPEG")
        || upper.ends_with(".PNG")
}

pub(crate) fn json_scalar_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| match v {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

pub(crate) fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub(crate) fn money_value_to_minor(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    let amount = value
        .get("value")
        .and_then(|v| v.as_f64())
        .or_else(|| value.as_f64())?;
    Some((amount * 100.0).round() as i64)
}
