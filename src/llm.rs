use crate::*;

const LLM_APPLIED_SUFFIX: &str = ": zastosowano zweryfikowane uzupełnienie";

/// Marker left by an applied, verified LLM answer (`LLM <model>: zastosowano …`).
pub(crate) fn is_llm_applied_marker(warning: &str) -> bool {
    warning.starts_with("LLM ") && warning.ends_with(LLM_APPLIED_SUFFIX)
}

pub(crate) fn record_has_llm_result(record: &InvoiceRecord) -> bool {
    record
        .warnings
        .iter()
        .any(|warning| is_llm_applied_marker(warning))
}

// The parser marks invoice numbers guessed from the file name with this warning
// (FILENAME_NUMBER_WARNING in parser/numbers.rs).
pub(crate) fn is_filename_number_warning(warning: &str) -> bool {
    warning == FILENAME_NUMBER_WARNING
}

pub(crate) fn record_number_guessed_from_filename(record: &InvoiceRecord) -> bool {
    record
        .warnings
        .iter()
        .any(|warning| is_filename_number_warning(warning))
}

const LLM_ATTEMPTS_FILE: &str = "llm_attempts.jsonl";

// Part of the attempt key: bump when the prompt, response schema or validation
// rules change, so earlier answers no longer block a new attempt.
const LLM_EXTRACTION_VERSION: &str = "lab-invoice-llm:v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum LlmAttemptOutcome {
    Applied,
    Unchanged,
    Rejected,
    Transient,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LlmAttempt {
    pub(crate) content_hash: String,
    pub(crate) model: String,
    pub(crate) version: String,
    pub(crate) outcome: LlmAttemptOutcome,
    attempted_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

type LlmAttemptKey = (String, String, String);

// Record of PDFs already sent to a model, stored next to the mail cache so the
// automatic sync does not pay for the same file twice.
struct LlmAttemptLedger {
    pub(crate) path: PathBuf,
    attempts: std::collections::BTreeMap<LlmAttemptKey, LlmAttempt>,
}

impl LlmAttemptLedger {
    fn load(path: PathBuf) -> Result<Self> {
        let mut attempts = std::collections::BTreeMap::new();
        match fs::read_to_string(&path) {
            Ok(text) => {
                for line in text.lines().filter(|line| !line.trim().is_empty()) {
                    // A damaged line only loses that entry; the rest still counts.
                    if let Ok(attempt) = serde_json::from_str::<LlmAttempt>(line) {
                        attempts.insert(llm_attempt_key(&attempt), attempt);
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("odczyt {}", path.display())),
        }
        Ok(Self { path, attempts })
    }

    pub(crate) fn get(&self, content_hash: &str, model: &str) -> Option<&LlmAttempt> {
        self.attempts.get(&(
            content_hash.to_string(),
            model.to_string(),
            LLM_EXTRACTION_VERSION.to_string(),
        ))
    }

    pub(crate) fn record(&mut self, attempt: LlmAttempt) -> Result<()> {
        self.attempts.insert(llm_attempt_key(&attempt), attempt);
        let mut out = Vec::new();
        for attempt in self.attempts.values() {
            serde_json::to_writer(&mut out, attempt)?;
            out.push(b'\n');
        }
        write_private_file(&self.path, &out)
    }
}

fn llm_attempt_key(attempt: &LlmAttempt) -> LlmAttemptKey {
    (
        attempt.content_hash.clone(),
        attempt.model.clone(),
        attempt.version.clone(),
    )
}

#[derive(Default)]
struct LlmAttemptLedgers {
    pub(crate) by_path: HashMap<PathBuf, LlmAttemptLedger>,
}

impl LlmAttemptLedgers {
    fn for_pdf(&mut self, pdf: &Path) -> Result<&mut LlmAttemptLedger> {
        let dir = pdf
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let path = dir.join(LLM_ATTEMPTS_FILE);
        if !self.by_path.contains_key(&path) {
            let ledger = LlmAttemptLedger::load(path.clone())?;
            self.by_path.insert(path.clone(), ledger);
        }
        Ok(self.by_path.get_mut(&path).expect("ledger loaded above"))
    }
}

fn llm_attempt_hash(record: &InvoiceRecord, pdf: &Path) -> Option<String> {
    if !record.content_hash.is_empty() {
        return Some(record.content_hash.clone());
    }
    fs::read(pdf)
        .ok()
        .map(|bytes| hex::encode(Sha256::digest(&bytes)))
}

fn llm_attempt_outcome(result: &Result<bool>) -> LlmAttemptOutcome {
    match result {
        Ok(true) => LlmAttemptOutcome::Applied,
        Ok(false) => LlmAttemptOutcome::Unchanged,
        Err(err) if llm_error_is_transient(err) => LlmAttemptOutcome::Transient,
        Err(_) => LlmAttemptOutcome::Rejected,
    }
}

/// Who asked for LLM enrichment; decides which filters apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LlmRequest {
    /// Sync or bulk run: paid-candidate filter and the attempt ledger apply.
    Automatic,
    /// `repair --llm`: paid-candidate filter applies, the ledger does not.
    Explicit,
    /// Rows selected in the TUI: neither filter applies.
    Forced,
}

fn llm_queue(
    records: &[InvoiceRecord],
    skip_hashes: &HashSet<String>,
    paid: bool,
    request: LlmRequest,
    model: &str,
    ledgers: &mut LlmAttemptLedgers,
) -> Result<Vec<usize>> {
    let mut queue = Vec::new();
    let mut already_sent = 0usize;
    for (idx, record) in records.iter().enumerate() {
        if !record_queued_for_llm(record, skip_hashes, paid, request == LlmRequest::Forced) {
            continue;
        }
        let Some(path) = record.source_path.as_deref().map(resolve_lab_source_path) else {
            continue;
        };
        if request == LlmRequest::Automatic
            && let Some(hash) = llm_attempt_hash(record, &path)
            && ledgers
                .for_pdf(&path)?
                .get(&hash, model)
                .is_some_and(|attempt| attempt.outcome != LlmAttemptOutcome::Transient)
        {
            already_sent += 1;
            continue;
        }
        queue.push(idx);
    }
    if already_sent > 0 {
        eprintln!(
            "  [Gmail/LLM] pominięto {already_sent} PDF już wysłanych do {model}; wybierz wiersze w TUI albo użyj repair --llm, żeby wysłać ponownie"
        );
    }
    Ok(queue)
}

pub(crate) fn enrich_candidates_with_gemma(
    records: &mut [InvoiceRecord],
    skip_hashes: &HashSet<String>,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<()> {
    enrich_candidates_with_request(
        records,
        skip_hashes,
        progress,
        LlmRequest::Automatic,
        |_, _| Ok(()),
    )
}

// `repair --llm`: the user asked for it, so earlier attempts do not block a resend.
pub(crate) fn enrich_candidates_with_llm_explicit(
    records: &mut [InvoiceRecord],
    skip_hashes: &HashSet<String>,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<()> {
    enrich_candidates_with_request(
        records,
        skip_hashes,
        progress,
        LlmRequest::Explicit,
        |_, _| Ok(()),
    )
}

pub(crate) fn enrich_candidates_with_gemma_with_hook<F>(
    records: &mut [InvoiceRecord],
    skip_hashes: &HashSet<String>,
    progress: Option<Arc<Mutex<String>>>,
    force: bool,
    after_record: F,
) -> Result<()>
where
    F: FnMut(&[InvoiceRecord], usize) -> Result<()>,
{
    let request = if force {
        LlmRequest::Forced
    } else {
        LlmRequest::Automatic
    };
    enrich_candidates_with_request(records, skip_hashes, progress, request, after_record)
}

fn enrich_candidates_with_request<F>(
    records: &mut [InvoiceRecord],
    skip_hashes: &HashSet<String>,
    progress: Option<Arc<Mutex<String>>>,
    request: LlmRequest,
    mut after_record: F,
) -> Result<()>
where
    F: FnMut(&[InvoiceRecord], usize) -> Result<()>,
{
    let use_openrouter = openrouter_configured();
    let force = request == LlmRequest::Forced;
    let model = if use_openrouter {
        openrouter_chat_model()
    } else {
        llm_model()
    };
    let mut ledgers = LlmAttemptLedgers::default();
    let queue = llm_queue(
        records,
        skip_hashes,
        use_openrouter,
        request,
        &model,
        &mut ledgers,
    )?;
    if use_openrouter && !force {
        let skipped = records
            .iter()
            .filter(|record| {
                record_queued_for_llm(record, skip_hashes, false, false)
                    && !record_needs_paid_llm(record)
            })
            .count();
        if skipped > 0 {
            eprintln!(
                "  [Gmail/OpenRouter] pominięto {skipped} łatwych albo bez sygnału faktury; Gemini tylko dla trudnych"
            );
        }
    }
    if queue.is_empty() {
        return Ok(());
    }
    let extract: fn(&mut InvoiceRecord, &Path) -> Result<bool> = if use_openrouter {
        openrouter_extract_invoice_fields
    } else {
        gemma_extract_invoice_fields
    };
    eprintln!(
        "  [Gmail/LLM] wzbogacanie {} kandydatów przez {}...",
        queue.len(),
        model
    );
    if !use_openrouter && let Err(err) = ensure_ppmlx_server() {
        eprintln!("  [Gmail/LLM] pominięto wzbogacanie: {err}");
        for idx in 0..records.len() {
            if !skip_hashes.contains(&records[idx].content_hash)
                && record_missing_core_fields(&records[idx])
            {
                let warning = format!("LLM niedostępny: {err}");
                if !records[idx].warnings.contains(&warning) {
                    records[idx].warnings.push(warning);
                }
                after_record(records, idx)?;
            }
        }
        return Ok(());
    }
    run_llm_queue(
        records,
        &queue,
        progress,
        use_openrouter,
        &model,
        &mut ledgers,
        extract,
        after_record,
    )?;
    eprintln!("  [Gmail/LLM] gotowe");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_llm_queue<F>(
    records: &mut [InvoiceRecord],
    queue: &[usize],
    progress: Option<Arc<Mutex<String>>>,
    use_openrouter: bool,
    model: &str,
    ledgers: &mut LlmAttemptLedgers,
    mut extract: impl FnMut(&mut InvoiceRecord, &Path) -> Result<bool>,
    mut after_record: F,
) -> Result<()>
where
    F: FnMut(&[InvoiceRecord], usize) -> Result<()>,
{
    let todo = queue.len();
    for (processed, &idx) in queue.iter().enumerate() {
        let Some(source_path) = records[idx].source_path.clone() else {
            continue;
        };
        let path = &resolve_lab_source_path(&source_path);
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("PDF");
        let status = if use_openrouter {
            openrouter_progress_line(processed + 1, todo, fname)
        } else {
            format!("LLM: {}/{} {}", processed + 1, todo, fname)
        };
        eprintln!("  [Gmail/LLM] {}", status);
        if let Some(ref p) = progress {
            set_progress(p, status);
        }
        let hash = llm_attempt_hash(&records[idx], path);
        let result = extract(&mut records[idx], path);
        let outcome = llm_attempt_outcome(&result);
        match &result {
            Ok(true) => {
                records[idx]
                    .warnings
                    .push(format!("LLM {model}{LLM_APPLIED_SUFFIX}"));
            }
            Ok(false) => {}
            Err(err) => {
                let warning = format!("LLM {model}: {err}");
                if !records[idx].warnings.contains(&warning) {
                    records[idx].warnings.push(warning);
                }
                eprintln!("  [Gmail/LLM] odrzucono uzupełnienie: {err}");
            }
        }
        if let Some(hash) = hash {
            let attempt = LlmAttempt {
                content_hash: hash,
                model: model.to_string(),
                version: LLM_EXTRACTION_VERSION.to_string(),
                outcome,
                attempted_at: Utc::now(),
                detail: result
                    .as_ref()
                    .err()
                    .map(|err| truncate(&format!("{err:#}"), 300)),
            };
            // A lost entry only costs one repeated request; do not drop the
            // answer already received for this record.
            if let Err(err) = ledgers
                .for_pdf(path)
                .and_then(|ledger| ledger.record(attempt))
            {
                eprintln!("  [Gmail/LLM] nie zapisano rejestru prób LLM: {err:#}");
            }
        }
        after_record(records, idx)?;
    }
    Ok(())
}

fn ppmlx_base_url() -> Result<String> {
    local_llm_base_url(
        &lab_config_var("PPMLX_BASE_URL").unwrap_or_else(|| "http://127.0.0.1:6767".to_string()),
    )
}

fn llm_model() -> String {
    lab_config_var("LAB_LLM_MODEL").unwrap_or_else(|| "gemma-4-e4b-it-optiq".to_string())
}

fn llm_timeout() -> Duration {
    Duration::from_secs(
        lab_config_var("LAB_LLM_TIMEOUT_SECS")
            .and_then(|value| value.parse().ok())
            .unwrap_or(45),
    )
}

fn ensure_ppmlx_server() -> Result<()> {
    let base = ppmlx_base_url()?;
    let model = llm_model();
    let client = Client::builder().timeout(Duration::from_secs(2)).build()?;
    if client
        .get(format!("{base}/v1/models"))
        .send()
        .is_ok_and(|r| r.status().is_success())
    {
        return Ok(());
    }
    Err(anyhow!(
        "ppmlx niedostępny: {base}; uruchom w osobnym terminalu: ppmlx serve --model {model}"
    ))
}

fn gemma_extract_invoice_fields(record: &mut InvoiceRecord, path: &Path) -> Result<bool> {
    let (text, extraction_warnings) = extract_document_text(path)?;
    if text.chars().count() > 60_000 {
        return Err(anyhow!(
            "Dokument przekracza limit 60000 znaków LLM; nie obcinam stron"
        ));
    }
    let prompt = format!(
        r#"Wyciągnij dane faktury z tekstu PDF.

Zwróć dokładnie jeden poprawny obiekt JSON: bez markdown, bez komentarzy, bez analizy, bez <|channel>thought.
Odpowiedź musi zaczynać się znakiem {{ i kończyć znakiem }}.

Użyj dokładnie tych kluczy. Jeśli brak pewności, wpisz null:
{{
  "invoice_number": null,
  "issue_date": null,
  "sale_date": null,
  "due_date": null,
  "gross_amount": null,
  "net_amount": null,
  "vat_amount": null,
  "currency": null,
  "seller_tax_id": null,
  "buyer_tax_id": null,
  "seller_name": null,
  "buyer_name": null
}}

Formaty wartości:
- Daty: string "YYYY-MM-DD" albo null.
- Kwoty: string z kropką i 2 miejscami, bez spacji i waluty, np. "1234.56", albo null.
- Waluta: "PLN", "EUR", "USD", "GBP" albo null. zł/PLN/zloty traktuj jako PLN.
- NIP/VAT PL: string z samymi 10 cyframi, bez PL/spacji/myślników; inaczej null.
- NIP/VAT widoczny w sekcji Bill to/Nabywca/Buyer przypisz do buyer_tax_id, nie do seller_tax_id.
- seller_tax_id to tylko identyfikator sprzedawcy/vendor/seller, jeśli jasno występuje przy sprzedawcy.
- Nazwy: pełna nazwa sprzedawcy/nabywcy z faktury albo null.
- Dla faktur zakupowych Productmesh zwykle buyer_name/buyer_tax_id to Productmesh; sprzedawca to kontrahent.
- Nie zgaduj i nie wyliczaj pól, jeśli nie wynikają jasno z tekstu.

Tekst PDF:
---
{}
---"#,
        text
    );
    let value = ppmlx_extract_json(&prompt)?;
    let changed = invoice_validation::apply_normalized_invoice_json(record, &value)?;
    for warning in extraction_warnings {
        if !record.warnings.contains(&warning) {
            record.warnings.push(warning);
        }
    }
    Ok(changed)
}

fn ppmlx_extract_json(prompt: &str) -> Result<Value> {
    let base = ppmlx_base_url()?;
    let model = llm_model();
    let client = Client::builder().timeout(llm_timeout()).build()?;
    let body = serde_json::json!({
        "model": model,
        "temperature": 0,
        "max_tokens": 4000,
        "messages": [
            {"role": "system", "content": "Jesteś ekstraktorem danych z faktur. Odpowiadasz tylko poprawnym JSON."},
            {"role": "user", "content": prompt}
        ]
    });

    let mut last_err = None;
    for attempt in 0..5 {
        match client
            .post(format!("{base}/v1/chat/completions"))
            .json(&body)
            .send()
        {
            Ok(resp) if resp.status().is_success() => {
                let response: Value = resp.json()?;
                return ppmlx_response_json(&response);
            }
            Ok(resp) if resp.status().as_u16() == 503 => {
                let delay = std::time::Duration::from_secs(2u64.pow(attempt));
                eprintln!(
                    "  [Gmail/Gemma] serwer zajęty (503), czekam {}s...",
                    delay.as_secs()
                );
                std::thread::sleep(delay);
                continue;
            }
            Ok(resp) => {
                let status = resp.status();
                let message = format!("ppmlx HTTP {}: {}", status, resp.text().unwrap_or_default());
                if http_status_is_transient(status.as_u16()) {
                    return Err(transient_llm_error(message));
                }
                return Err(anyhow!(message));
            }
            Err(err) => {
                last_err = Some(err);
                let delay = std::time::Duration::from_secs(2u64.pow(attempt));
                std::thread::sleep(delay);
            }
        }
    }
    Err(last_err.map(anyhow::Error::from).unwrap_or_else(|| {
        transient_llm_error("ppmlx nie odpowiedział po 5 próbach (503 Service Unavailable)")
    }))
}

fn ppmlx_response_json(response: &Value) -> Result<Value> {
    let choice = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
        .ok_or_else(|| anyhow!("ppmlx: brak odpowiedzi modelu"))?;
    if choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .is_some_and(|r| r != "stop")
    {
        return Err(anyhow!(
            "ppmlx: odpowiedź nie została zakończona poprawnie; nie zapisuję częściowego JSON"
        ));
    }
    let content = choice
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("ppmlx: brak treści odpowiedzi"))?;
    parse_json_from_llm(content)
}

#[cfg(test)]
mod llm_flow_tests;

pub(crate) fn parse_json_from_llm(content: &str) -> Result<Value> {
    let sanitized = sanitize_llm_content(content);
    let content = sanitized.trim();

    if content.is_empty() {
        return Err(anyhow!("LLM nie zwrócił JSON"));
    }
    if let Ok(value) = serde_json::from_str(content) {
        return Ok(value);
    }

    let candidates = json_object_candidates(content);
    if candidates.is_empty() {
        return Err(anyhow!("LLM nie zwrócił JSON"));
    }

    // After deterministic channel sanitization, prefer the last syntactically valid
    // JSON object to tolerate markdown fences or short explanatory prefixes.
    let mut last_err = None;
    for candidate in candidates.iter().rev() {
        match serde_json::from_str::<Value>(candidate) {
            Ok(value) => return Ok(value),
            Err(err) => last_err = Some(err),
        }
    }

    Err(last_err
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow!("LLM nie zwrócił JSON")))
    .context("niepoprawny JSON z LLM")
}

fn sanitize_llm_content(content: &str) -> String {
    if let Some(final_payload) = channel_payload(content, "final") {
        return strip_channel_tokens(final_payload).trim().to_string();
    }

    strip_channel_tokens(&remove_reasoning_channels(content))
        .trim()
        .to_string()
}

fn channel_payload<'a>(content: &'a str, channel: &str) -> Option<&'a str> {
    let marker = format!("<|channel>{channel}");
    let start = content.find(&marker)? + marker.len();
    let end = content[start..]
        .find("<|channel>")
        .map(|idx| start + idx)
        .unwrap_or(content.len());
    Some(&content[start..end])
}

fn remove_reasoning_channels(content: &str) -> String {
    let marker = "<|channel>";
    let mut output = String::new();
    let mut pos = 0usize;

    while let Some(rel_start) = content[pos..].find(marker) {
        let start = pos + rel_start;
        output.push_str(&content[pos..start]);

        let name_start = start + marker.len();
        let name_len = content[name_start..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
            .map(char::len_utf8)
            .sum::<usize>();
        let name = &content[name_start..name_start + name_len];
        let next = content[name_start + name_len..]
            .find(marker)
            .map(|idx| name_start + name_len + idx)
            .unwrap_or(content.len());

        if !matches!(name, "thought" | "analysis" | "reasoning") {
            output.push_str(&content[start..next]);
        }
        pos = next;
    }

    output.push_str(&content[pos..]);
    output
}

fn strip_channel_tokens(content: &str) -> String {
    let marker = "<|channel>";
    let mut output = String::new();
    let mut pos = 0usize;

    while let Some(rel_start) = content[pos..].find(marker) {
        let start = pos + rel_start;
        output.push_str(&content[pos..start]);
        let name_start = start + marker.len();
        let skip_len = content[name_start..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
            .map(char::len_utf8)
            .sum::<usize>();
        pos = name_start + skip_len;
    }

    output.push_str(&content[pos..]);
    output
}

fn json_object_candidates(content: &str) -> Vec<&str> {
    let mut candidates = Vec::new();
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in content.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' if depth > 0 => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0
                    && let Some(start_idx) = start.take()
                {
                    candidates.push(&content[start_idx..idx + ch.len_utf8()]);
                }
            }
            _ => {}
        }
    }

    candidates
}

// Fills empty fields only, with two exceptions: inconsistent amounts are replaced
// by a complete, consistent triple, and a number guessed from the file name is
// replaced by the one read from the document. Both leave a warning.
pub(crate) fn apply_extracted_invoice_json(record: &mut InvoiceRecord, value: &Value) {
    let llm_number =
        json_first_string(value, &["invoice_number", "number"]).map(|v| clean_invoice_number(&v));
    if record_number_guessed_from_filename(record)
        && let Some(number) = llm_number.clone().filter(|number| !number.is_empty())
    {
        if record.invoice_number.as_deref() != Some(number.as_str()) {
            record.warnings.push(format!(
                "LLM: zastąpiono numer faktury odczytany z nazwy pliku ({})",
                record.invoice_number.as_deref().unwrap_or("-")
            ));
            record.invoice_number = Some(number);
        }
        record
            .warnings
            .retain(|warning| !is_filename_number_warning(warning));
    }
    if record.invoice_number.is_none() {
        record.invoice_number = llm_number;
    }
    if record_amounts_inconsistent(record)
        && let (Some(net), Some(vat), Some(gross)) = (
            json_first_money_minor(value, &["net_amount", "amount_net"]),
            json_first_money_minor(value, &["vat_amount", "amount_vat"]),
            json_first_money_minor(value, &["gross_amount", "amount", "total"]),
        )
        && net.checked_add(vat) == Some(gross)
    {
        let old = |amount: Option<i64>| amount.map_or_else(|| "-".to_string(), format_minor_money);
        record.warnings.push(format!(
            "LLM: zastąpiono niespójne kwoty (netto {}, VAT {}, brutto {}) spójnym zestawem z dokumentu",
            old(record.net_amount_minor),
            old(record.vat_amount_minor),
            old(record.gross_amount_minor)
        ));
        record.net_amount_minor = Some(net);
        record.vat_amount_minor = Some(vat);
        record.gross_amount_minor = Some(gross);
    }
    if record.issue_date.is_none() {
        record.issue_date =
            json_first_string(value, &["issue_date", "date"]).and_then(|v| parse_date(&v));
    }
    if record.sale_date.is_none() {
        record.sale_date = json_first_string(value, &["sale_date"]).and_then(|v| parse_date(&v));
    }
    if record.due_date.is_none() {
        record.due_date =
            json_first_string(value, &["due_date", "date_due"]).and_then(|v| parse_date(&v));
    }
    if record.gross_amount_minor.is_none() {
        record.gross_amount_minor =
            json_first_money_minor(value, &["gross_amount", "amount", "total"]);
    }
    if record.net_amount_minor.is_none() {
        record.net_amount_minor = json_first_money_minor(value, &["net_amount", "amount_net"]);
    }
    if record.vat_amount_minor.is_none() {
        record.vat_amount_minor = json_first_money_minor(value, &["vat_amount", "amount_vat"]);
    }
    if record.currency.is_none() {
        record.currency =
            json_first_string(value, &["currency"]).and_then(|v| normalize_currency(&v));
    }
    if record.seller_tax_id.is_none() {
        record.seller_tax_id =
            json_first_string(value, &["seller_tax_id"]).and_then(|v| normalize_tax_id(&v));
    }
    if record.buyer_tax_id.is_none() {
        record.buyer_tax_id =
            json_first_string(value, &["buyer_tax_id"]).and_then(|v| normalize_tax_id(&v));
    }
    if record
        .seller_name
        .as_deref()
        .is_none_or(counterparty_name_is_placeholder)
        && let Some(name) = json_first_string(value, &["seller_name"])
            .and_then(|v| clean_name(&v))
            .filter(|v| !counterparty_name_is_placeholder(v))
    {
        record.seller_name = Some(name);
    }
    if record
        .buyer_name
        .as_deref()
        .is_none_or(counterparty_name_is_placeholder)
        && let Some(name) = json_first_string(value, &["buyer_name"])
            .and_then(|v| clean_name(&v))
            .filter(|v| !counterparty_name_is_placeholder(v))
    {
        record.buyer_name = Some(name);
    }
}
