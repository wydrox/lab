use crate::*;
use dialoguer::{Confirm, Input};

pub(crate) struct SaldeoSyncPlanConfig<'a> {
    pub(crate) year: i32,
    pub(crate) tri_report: Option<&'a Path>,
    pub(crate) mail: Option<&'a Path>,
    pub(crate) ksef: Option<&'a Path>,
    pub(crate) saldeo: Option<&'a Path>,
    pub(crate) review_score: u8,
    pub(crate) confirm: bool,
    pub(crate) upload_url: Option<String>,
}

pub(crate) fn saldeo_sync_plan(config: SaldeoSyncPlanConfig<'_>) -> Result<SaldeoSyncPlan> {
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
            load_saldeo_records(&saldeo)?,
            config.review_score,
        )
    };
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
        items.push(saldeo_sync_item_from_record(
            &row.status,
            record,
            related_sources,
        ));
    }
    let summary = saldeo_sync_summary(&items);
    Ok(SaldeoSyncPlan {
        generated_at: Utc::now(),
        year: config.year,
        confirm: config.confirm,
        upload_url: config
            .upload_url
            .or_else(|| Some(DEFAULT_SALDEO_UPLOAD_URL.to_string())),
        summary,
        items,
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
    }
}

pub(crate) fn saldeo_sync_summary(items: &[SaldeoSyncItem]) -> SaldeoSyncSummary {
    SaldeoSyncSummary {
        total_missing_saldeo: items.len(),
        uploadable_count: items.iter().filter(|i| i.can_upload).count(),
        missing_file_count: items.iter().filter(|i| !i.can_upload).count(),
        uploaded_count: items
            .iter()
            .filter(|i| i.upload_status == "uploaded")
            .count(),
        failed_count: items.iter().filter(|i| i.upload_status == "failed").count(),
    }
}

pub(crate) const DEFAULT_SALDEO_UPLOAD_URL: &str =
    "https://saldeo.brainshare.pl/rest/client/document/generate-urls-for-upload";

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
    let session = read_saldeo_session(storage_state)?;
    let client = Client::builder().build()?;
    let uploadable_count = plan.items.iter().filter(|item| item.can_upload).count();
    let mut upload_index = 0usize;
    for item in &mut plan.items {
        if !item.can_upload {
            continue;
        }
        let Some(source_path) = &item.source_path else {
            continue;
        };
        upload_index += 1;
        if let Some(progress) = &progress {
            let label = Path::new(source_path)
                .file_name()
                .and_then(|name| name.to_str())
                .or(item.invoice_number.as_deref())
                .unwrap_or("plik");
            set_progress(
                progress,
                format!("Upload Saldeo {upload_index}/{uploadable_count}: {label}"),
            );
        }
        let upload_year = item.issue_date.map(|d| d.year()).unwrap_or(plan.year);
        let upload_month = item
            .issue_date
            .map(|d| d.month())
            .unwrap_or_else(|| Utc::now().month());
        match saldeo_upload_file(
            &client,
            &session,
            upload_url,
            Path::new(source_path),
            upload_year,
            upload_month,
        ) {
            Ok((status, body)) => {
                item.upload_status = "uploaded".to_string();
                item.saldeo_response_status = Some(status);
                item.saldeo_response_body = Some(body);
            }
            Err(err) => {
                item.upload_status = "failed".to_string();
                item.error = Some(err.to_string());
            }
        }
    }
    plan.summary = saldeo_sync_summary(&plan.items);
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

pub(crate) fn saldeo_upload_file(
    client: &Client,
    session: &SaldeoSession,
    upload_url: &str,
    path: &Path,
    year: i32,
    month: u32,
) -> Result<(u16, String)> {
    let file_name = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| anyhow!("brak nazwy pliku: {}", path.display()))?;
    let bytes = fs::read(path).with_context(|| format!("odczyt {}", path.display()))?;
    let content_type = content_type_for_path(path);
    let body = serde_json::json!({
        "year": year,
        "month": month,
        "documentTypeId": -1,
        "files": [{
            "filename": file_name,
            "contentType": content_type,
            "size": bytes.len(),
        }],
        "clientId": null,
    });
    let response: Value = client
        .post(upload_url)
        .header("Cookie", &session.cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", &session.xsrf)
        .header("saldeoApp", "angularApp")
        .header("timeout", "60000")
        .json(&body)
        .send()
        .with_context(|| format!("Saldeo generate upload URL {}", path.display()))?
        .error_for_status()?
        .json()?;
    if response.get("status").and_then(|v| v.as_str()) != Some("SUCCESS") {
        return Err(anyhow!("Saldeo generate upload URL failed: {}", response));
    }
    let upload = response
        .get("data")
        .and_then(|v| v.get(file_name))
        .ok_or_else(|| anyhow!("Saldeo response missing file entry for {file_name}: {response}"))?;
    let doc_upload_id = upload
        .get("docUploadId")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("Saldeo response missing docUploadId: {upload}"))?;
    let signed_url = upload
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Saldeo response missing upload url: {upload}"))?;
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
        let _ = saldeo_reject_upload(client, session, doc_upload_id, &err.to_string());
        return Err(anyhow!("Saldeo signed upload failed: {err}"));
    }

    let confirm_url =
        format!("https://saldeo.brainshare.pl/rest/doc-upload/{doc_upload_id}/confirm");
    let confirm = client
        .post(&confirm_url)
        .header("Cookie", &session.cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", &session.xsrf)
        .header("saldeoApp", "angularApp")
        .header("timeout", "60000")
        .json(&serde_json::json!({}))
        .send()
        .with_context(|| format!("Saldeo confirm upload {doc_upload_id}"))?;
    let status = confirm.status().as_u16();
    let text = confirm.text().unwrap_or_default();
    if !(200..300).contains(&status) {
        let _ = saldeo_reject_upload(client, session, doc_upload_id, &text);
        return Err(anyhow!("Saldeo confirm failed HTTP {status}: {text}"));
    }
    Ok((status, text.chars().take(2000).collect()))
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

fn load_saldeo_record_overrides() -> Result<HashMap<String, SaldeoRecordOverride>> {
    let path = default_saldeo_record_overrides_path();
    if !path.is_file() {
        return Ok(HashMap::new());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("odczyt {}", path.display()))?;
    let overrides = serde_json::from_str::<Vec<SaldeoRecordOverride>>(&text)
        .with_context(|| format!("niepoprawny override Saldeo {}", path.display()))?;
    Ok(overrides
        .into_iter()
        .map(|override_row| (override_row.content_hash.clone(), override_row))
        .collect())
}

fn save_saldeo_record_overrides(overrides: &HashMap<String, SaldeoRecordOverride>) -> Result<()> {
    let path = default_saldeo_record_overrides_path();
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let mut rows = overrides.values().cloned().collect::<Vec<_>>();
    rows.sort_by(|a, b| a.content_hash.cmp(&b.content_hash));
    fs::write(&path, serde_json::to_vec_pretty(&rows)?)
        .with_context(|| format!("zapis {}", path.display()))
}

pub(crate) fn saldeo_record_has_override(record: &InvoiceRecord) -> bool {
    record.source == SourceKind::Saldeo
        && record
            .warnings
            .iter()
            .any(|warning| warning.starts_with(SALDEO_OVERRIDE_WARNING_PREFIX))
}

pub(crate) fn apply_saldeo_record_overrides(records: &mut [InvoiceRecord]) -> Result<usize> {
    let overrides = load_saldeo_record_overrides()?;
    let mut applied = 0usize;
    for record in records.iter_mut() {
        if record.source != SourceKind::Saldeo {
            continue;
        }
        if let Some(override_row) = overrides.get(&record.content_hash) {
            apply_saldeo_record_override(record, override_row);
            applied += 1;
        }
    }
    Ok(applied)
}

fn apply_saldeo_record_override(
    record: &mut InvoiceRecord,
    override_row: &SaldeoRecordOverride,
) -> bool {
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

pub(crate) fn edit_saldeo_record_override(record: &InvoiceRecord) -> Result<bool> {
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

    let invoice_number = prompt_string_value(
        "Numer faktury",
        record.invoice_number.as_deref(),
        clean_invoice_number,
    )?;
    let seller_name = prompt_string_value(
        "Sprzedawca / kontrahent",
        record.seller_name.as_deref(),
        |value| Some(clean_name(value).unwrap_or_else(|| value.trim().to_string())),
    )?;
    let buyer_name = prompt_string_value(
        "Nabywca",
        record.buyer_name.as_deref(),
        |value| Some(clean_name(value).unwrap_or_else(|| value.trim().to_string())),
    )?;
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
    let currency = prompt_string_value(
        "Waluta",
        record.currency.as_deref(),
        normalize_currency,
    )?;

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

    let mut overrides = load_saldeo_record_overrides()?;
    if overrides.get(&override_row.content_hash) == Some(&override_row) {
        eprintln!("⏭ Bez zmian.\n");
        return Ok(false);
    }

    let confirm = Confirm::new()
        .with_prompt(format!(
            "Zapisać poprawki do {}?",
            default_saldeo_record_overrides_path().display()
        ))
        .default(true)
        .interact()?;
    if !confirm {
        eprintln!("⏭ Anulowano.\n");
        return Ok(false);
    }

    overrides.insert(override_row.content_hash.clone(), override_row);
    save_saldeo_record_overrides(&overrides)?;
    eprintln!(
        "✓ Zapisano poprawki Saldeo: {}\n",
        default_saldeo_record_overrides_path().display()
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
    let current_text = current.map(&format_current).unwrap_or_else(|| "-".to_string());
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
) -> Result<SaldeoFetchResult> {
    saldeo_fetch_with_progress(year, storage_state, out_dir, None)
}

pub(crate) fn saldeo_fetch_with_progress(
    year: i32,
    storage_state: &Path,
    out_dir: &Path,
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
            .json()?;
        let items = value
            .get("data")
            .and_then(|d| d.get("resultCollection"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
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
    let overridden_count = apply_saldeo_record_overrides(&mut records)?;
    if overridden_count > 0 {
        eprintln!("  [Saldeo] zastosowano lokalne poprawki: {overridden_count} rekordów");
    }
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!("Saldeo: zapis {} rekordów...", records.len()),
        );
    }
    let raw_output = out_dir.join("documents.json");
    let records_output = out_dir.join("records.jsonl");
    fs::write(&raw_output, serde_json::to_vec_pretty(&documents)?)?;
    let mut jsonl = Vec::new();
    for record in &records {
        serde_json::to_writer(&mut jsonl, record)?;
        jsonl.push(b'\n');
    }
    fs::write(&records_output, jsonl)?;

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

pub(crate) fn load_saldeo_records(input: &Path) -> Result<Vec<InvoiceRecord>> {
    let mut records = if input.extension().and_then(|e| e.to_str()) == Some("jsonl") {
        load_records(SourceKind::Saldeo, input)?
    } else {
        let text =
            fs::read_to_string(input).with_context(|| format!("odczyt Saldeo {}", input.display()))?;
        if let Ok(mut records) = serde_json::from_str::<Vec<InvoiceRecord>>(&text) {
            for record in &mut records {
                record.source = SourceKind::Saldeo;
            }
            records
        } else {
            let value: Value = serde_json::from_str(&text)?;
            let docs = value
                .as_array()
                .ok_or_else(|| anyhow!("Saldeo input musi być tablicą documents albo InvoiceRecord[]"))?;
            saldeo_documents_to_records(docs)
        }
    };
    let overridden_count = apply_saldeo_record_overrides(&mut records)?;
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
            !(record_has_counterparty(&records[record_idx])
                && records[record_idx].issue_date.is_some()
                && records[record_idx].gross_amount_minor.is_some())
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
        if record_has_counterparty(&records[record_idx])
            && records[record_idx].issue_date.is_some()
            && records[record_idx].gross_amount_minor.is_some()
        {
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
    record
        .seller_name
        .as_ref()
        .or(record.buyer_name.as_ref())
        .is_some_and(|name| !name.trim().is_empty())
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
    fill_clone!(seller_tax_id);
    fill_clone!(buyer_tax_id);
    fill_clone!(seller_name);
    fill_clone!(buyer_name);
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
