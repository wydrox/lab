use crate::*;

// Year Y holds invoices issued in Y; mail from early Y+1 is fetched for late
// invoices, so dated records from other years are dropped. Undated ones stay.
pub(crate) fn retain_mail_candidates_for_year(
    candidates: &mut Vec<InvoiceRecord>,
    year: i32,
) -> usize {
    let before = candidates.len();
    candidates.retain(|record| record.issue_date.is_none_or(|date| date.year() == year));
    before - candidates.len()
}

const MAIL_PARSER_VERSION_PREFIX: &str = "lab-mail-parser:";
const MAIL_PARSER_VERSION: &str = "lab-mail-parser:v5";

/// Marker of the parser version that read a cached mail record (any version); not a
/// note for the user.
pub(crate) fn is_mail_parser_version_marker(warning: &str) -> bool {
    warning.starts_with(MAIL_PARSER_VERSION_PREFIX)
}

#[cfg(test)]
mod mail_cache_tests;

mod default_candidates;
pub(crate) use default_candidates::*;

fn mail_parse_needs_retry(record: &InvoiceRecord) -> bool {
    record
        .warnings
        .iter()
        .any(|warning| mail_warning_needs_retry(warning))
}

fn mail_warning_needs_retry(warning: &str) -> bool {
    if is_pdf_password_warning(warning) {
        return false;
    }
    let warning = warning.to_lowercase();
    warning.starts_with("nie udało się wyciągnąć tekstu pdf")
        || warning.starts_with("pdf bez tekstu")
        || warning.starts_with("pdf nie zawiera tekstu")
        || warning.starts_with("ocr niedostępny")
        || warning.starts_with("ocr nie powiódł się")
        || warning.starts_with("błąd odczytu")
}

// A successful parse by the current parser replaces values an older parser read,
// field by field. Kept: values the new parse lacks (a name placeholder counts as
// lacking), cached numbers when the new one is only a file-name guess, cached
// amounts when the new set is inconsistent (or would make them so), and the
// e-mail header fields, which come from message metadata.
fn replace_mail_parser_fields(target: &mut InvoiceRecord, incoming: &InvoiceRecord) {
    macro_rules! replace {
        ($($field:ident),* $(,)?) => {$(
            if incoming.$field.is_some() {
                target.$field = incoming.$field.clone();
            }
        )*};
    }
    replace!(
        seller_tax_id,
        buyer_tax_id,
        issue_date,
        sale_date,
        due_date,
        currency,
        ksef_reference
    );
    if incoming.invoice_number.is_some()
        && (target.invoice_number.is_none() || !record_number_from_filename(incoming))
    {
        target.invoice_number = incoming.invoice_number.clone();
    }
    let mut amounts = target.clone();
    amounts.net_amount_minor = incoming.net_amount_minor.or(target.net_amount_minor);
    amounts.vat_amount_minor = incoming.vat_amount_minor.or(target.vat_amount_minor);
    amounts.gross_amount_minor = incoming.gross_amount_minor.or(target.gross_amount_minor);
    let keep_cached_amounts = !record_amounts_inconsistent(target)
        && (record_amounts_inconsistent(incoming) || record_amounts_inconsistent(&amounts));
    if !keep_cached_amounts {
        target.net_amount_minor = amounts.net_amount_minor;
        target.vat_amount_minor = amounts.vat_amount_minor;
        target.gross_amount_minor = amounts.gross_amount_minor;
    }
    for (target, incoming) in [
        (&mut target.seller_name, &incoming.seller_name),
        (&mut target.buyer_name, &incoming.buyer_name),
    ] {
        if incoming
            .as_deref()
            .is_some_and(|name| !counterparty_name_is_placeholder(name))
        {
            *target = incoming.clone();
        }
    }
    macro_rules! fill {
        ($($field:ident),* $(,)?) => {$(
            if target.$field.is_none() {
                target.$field = incoming.$field.clone();
            }
        )*};
    }
    fill!(source_path, email_message_id, email_subject, email_from);
}

// Keep established values; only fill gaps and replace known name placeholders.
fn merge_mail_fields(target: &mut InvoiceRecord, incoming: &InvoiceRecord) {
    macro_rules! fill {
        ($($field:ident),* $(,)?) => {$(
            if target.$field.is_none() {
                target.$field = incoming.$field.clone();
            }
        )*};
    }
    fill!(
        invoice_number,
        seller_tax_id,
        buyer_tax_id,
        issue_date,
        sale_date,
        due_date,
        gross_amount_minor,
        net_amount_minor,
        vat_amount_minor,
        currency,
        ksef_reference,
        source_path,
        email_message_id,
        email_subject,
        email_from
    );
    for (target, incoming) in [
        (&mut target.seller_name, &incoming.seller_name),
        (&mut target.buyer_name, &incoming.buyer_name),
    ] {
        if target
            .as_deref()
            .is_none_or(counterparty_name_is_placeholder)
            && incoming
                .as_deref()
                .is_some_and(|name| !counterparty_name_is_placeholder(name))
        {
            *target = incoming.clone();
        }
    }
}

pub(crate) fn sync_mail_records(
    mail_out: &Path,
    saved_files: &[String],
) -> Result<(Vec<InvoiceRecord>, usize)> {
    sync_mail_records_with_parser(mail_out, saved_files, |path| {
        parse_file(SourceKind::Mail, path)
    })
}

fn sync_mail_records_with_parser(
    mail_out: &Path,
    saved_files: &[String],
    mut parse: impl FnMut(&Path) -> Result<InvoiceRecord>,
) -> Result<(Vec<InvoiceRecord>, usize)> {
    let cache_path = mail_out.join("records.jsonl");
    let cache_exists = cache_path.exists();
    let files_on_disk = mail_candidate_files(mail_out)?;
    let mut records = if cache_exists {
        load_records(SourceKind::Mail, &cache_path)?
    } else {
        Vec::new()
    };
    let mut parsed_count = 0usize;
    for record in &mut records {
        let current_version = record
            .warnings
            .iter()
            .any(|warning| warning == MAIL_PARSER_VERSION);
        if current_version && !mail_parse_needs_retry(record) {
            continue;
        }
        let Some(path) = record.source_path.as_deref().map(resolve_lab_source_path) else {
            continue;
        };
        if !path.is_file()
            || !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
        {
            continue;
        }
        // A failed extraction can contain filename guesses. Preserve the original
        // record, including its parser version, so the next sync can try again.
        let Ok(parsed) = parse(&path) else { continue };
        if mail_parse_needs_retry(&parsed) {
            continue;
        }
        // An older parser's values give way to this parse (fixed parser bugs would
        // otherwise stay cached forever). A same-version retry only fills gaps:
        // its earlier values came from the same parser. LLM answers are never
        // written to this cache, but a record carrying one is kept as it is.
        let replace = !current_version && !record_has_llm_result(record);
        let cached_number = record.invoice_number.clone();
        if replace {
            replace_mail_parser_fields(record, &parsed);
        } else {
            merge_mail_fields(record, &parsed);
        }
        // The file-name warning follows the number that was kept: a cached number
        // must not be marked as guessed by this parse, and a stale mark goes when
        // this parse read the number from the document.
        let number_from_parse = parsed.invoice_number.is_some()
            && record.invoice_number == parsed.invoice_number
            && (cached_number.is_none() || (replace && !record_number_from_filename(&parsed)));
        record.content_hash = parsed.content_hash;
        record.warnings.retain(|warning| {
            !is_mail_parser_version_marker(warning)
                && !own_nip::is_check_warning(warning)
                && !mail_warning_needs_retry(warning)
                && !(number_from_parse && warning == FILENAME_NUMBER_WARNING)
        });
        for warning in parsed.warnings {
            if !number_from_parse && warning == FILENAME_NUMBER_WARNING {
                continue;
            }
            if !is_mail_parser_version_marker(&warning) && !record.warnings.contains(&warning) {
                record.warnings.push(warning);
            }
        }
        record.warnings.push(MAIL_PARSER_VERSION.to_string());
        parsed_count += 1;
    }
    // Reconcile the folder with the cache: a run interrupted after saving an
    // attachment leaves a file that only this scan can bring into records.jsonl.
    let mut seen = records
        .iter()
        .map(|record| record.content_hash.clone())
        .collect::<HashSet<_>>();
    let known_paths = records
        .iter()
        .filter_map(|record| record.source_path.as_deref())
        .filter_map(|path| fs::canonicalize(resolve_lab_source_path(path)).ok())
        .collect::<HashSet<_>>();
    let mut paths = saved_files.iter().map(PathBuf::from).collect::<Vec<_>>();
    paths.extend(files_on_disk);
    paths.sort();
    paths.dedup();
    for path in paths {
        if !path.is_file() || !is_mail_candidate_file(&path) {
            continue;
        }
        if fs::canonicalize(&path).is_ok_and(|path| known_paths.contains(&path)) {
            continue;
        }
        // Same bytes under another name (e.g. one PDF in two messages): skip the
        // parse, which may run OCR, instead of repeating it on every sync.
        if fs::read(&path).is_ok_and(|bytes| seen.contains(&hex::encode(Sha256::digest(&bytes)))) {
            continue;
        }
        let mut record = match parse(&path) {
            Ok(record) => record,
            Err(err) => {
                eprintln!("  [Gmail] pominięto {}: {err:#}", path.display());
                continue;
            }
        };
        if !mail_parse_needs_retry(&record) {
            record.warnings.push(MAIL_PARSER_VERSION.to_string());
        }
        if seen.insert(record.content_hash.clone()) {
            records.push(record);
            parsed_count += 1;
        }
    }
    // Also covers records cached before headers were joined, so old caches fill up.
    let mut message_headers = HashMap::new();
    let mut headers_joined = false;
    for record in &mut records {
        headers_joined |= join_gmail_message_headers(record, &mut message_headers);
    }
    if parsed_count > 0 || headers_joined || !cache_exists {
        write_records(&records, OutputFormat::Jsonl, Some(&cache_path))?;
    }
    Ok((records, parsed_count))
}

#[derive(Debug, Default)]
struct GmailMessageHeaders {
    pub(crate) message_id: Option<String>,
    pub(crate) subject: Option<String>,
    pub(crate) from: Option<String>,
}

// gmail_fetch saves attachments as `{message id}_{n}_{name}` next to
// `{message id}_message.json`.
fn gmail_metadata_path_for_attachment(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let (message_id, rest) = name.split_once('_')?;
    let (index, file) = rest.split_once('_')?;
    if message_id.is_empty()
        || index.is_empty()
        || !index.bytes().all(|b| b.is_ascii_digit())
        || file.is_empty()
    {
        return None;
    }
    Some(path.with_file_name(format!("{message_id}_message.json")))
}

fn read_gmail_message_headers(metadata_path: &Path) -> Option<GmailMessageHeaders> {
    let msg = serde_json::from_slice::<Value>(&fs::read(metadata_path).ok()?).ok()?;
    let headers = gmail_headers(&msg);
    let header = |key: &str| {
        headers
            .get(key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Some(GmailMessageHeaders {
        message_id: header("message-id").or_else(|| {
            msg.get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        }),
        subject: header("subject"),
        from: header("from"),
    })
}

/// Fills the e-mail fields of an attachment record from its message metadata;
/// values already present are kept. Returns whether anything changed.
fn join_gmail_message_headers(
    record: &mut InvoiceRecord,
    cache: &mut HashMap<PathBuf, Option<GmailMessageHeaders>>,
) -> bool {
    let Some(metadata_path) = record
        .source_path
        .as_deref()
        .and_then(|path| gmail_metadata_path_for_attachment(Path::new(path)))
    else {
        return false;
    };
    let Some(headers) = cache
        .entry(metadata_path)
        .or_insert_with_key(|path| read_gmail_message_headers(path))
    else {
        return false;
    };
    let mut changed = false;
    for (target, value) in [
        (&mut record.email_message_id, &headers.message_id),
        (&mut record.email_subject, &headers.subject),
        (&mut record.email_from, &headers.from),
    ] {
        if target
            .as_deref()
            .is_none_or(|current| current.trim().is_empty())
            && value.is_some()
        {
            *target = value.clone();
            changed = true;
        }
    }
    changed
}

fn mail_candidate_files(input: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if input.is_dir() {
        for entry in WalkDir::new(input).into_iter().filter_map(Result::ok) {
            let path = entry.path();
            if entry.file_type().is_file() && is_mail_candidate_file(path) {
                files.push(path.to_path_buf());
            }
        }
    } else if input.is_file() && is_mail_candidate_file(input) {
        files.push(input.to_path_buf());
    } else {
        return Err(anyhow!(
            "input nie istnieje albo nie jest wspieranym plikiem mail: {}",
            input.display()
        ));
    }

    files.sort();
    Ok(files)
}

pub(crate) fn is_mail_candidate_file(path: &Path) -> bool {
    is_supported_file(path) && !is_gmail_metadata_path(path)
}

fn is_gmail_metadata_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.ends_with("_message.json") || name == "records.json")
}

// Kandydaci przed krokiem LLM: poza fakturami powiązanymi z firmą zostają też
// PDF-y bez odczytanego tekstu (skan, brak OCR) z sygnałem faktury, bo dopiero
// LLM może podać ich NIP-y. Po LLM obowiązuje productmesh_candidates_after_llm.
pub(crate) fn productmesh_invoice_candidates(
    records: &[InvoiceRecord],
    productmesh_nip: &str,
) -> Vec<InvoiceRecord> {
    select_productmesh_candidates(records, productmesh_nip, true)
}

// Po kroku LLM: reguła ProductMesh dla wszystkich, także skanów uzupełnionych
// przez model; bez powiązania z firmą rekord odpada jak wcześniej.
pub(crate) fn productmesh_candidates_after_llm(
    records: &[InvoiceRecord],
    productmesh_nip: &str,
) -> Vec<InvoiceRecord> {
    select_productmesh_candidates(records, productmesh_nip, false)
}

pub(crate) const PDF_TEXT_FAILED_WARNING: &str = "nie udało się wyciągnąć tekstu PDF";

fn record_text_unreadable(record: &InvoiceRecord) -> bool {
    record
        .warnings
        .iter()
        .any(|warning| warning.starts_with(PDF_TEXT_FAILED_WARNING))
}

fn select_productmesh_candidates(
    records: &[InvoiceRecord],
    productmesh_nip: &str,
    keep_unread: bool,
) -> Vec<InvoiceRecord> {
    let productmesh_nip =
        normalize_tax_id(productmesh_nip).unwrap_or_else(|| productmesh_nip.to_string());
    let excluded = Regex::new(r"(?i)(receipt|statement|regulamin|warunki|informacje|upowa|oferta|umowa|order|label|bilet|dr_skan|wypowiedzenie|grafklient|cennik|polityka|pasek|wishlist|terms|portfolio|kosztorys|formularz|prawo_jazdy|zalacznik)").unwrap();
    let mut by_invoice: HashMap<String, Vec<InvoiceRecord>> = HashMap::new();
    let mut fallback_seen = HashSet::new();
    let mut fallback_out = Vec::new();
    for record in records {
        if record_is_password_protected(record) {
            continue;
        }
        let names = format!(
            "{} {}",
            record.seller_name.clone().unwrap_or_default(),
            record.buyer_name.clone().unwrap_or_default()
        );
        let related = record.seller_tax_id.as_deref() == Some(productmesh_nip.as_str())
            || record.buyer_tax_id.as_deref() == Some(productmesh_nip.as_str())
            || names.to_lowercase().contains("productmesh");
        let awaiting_llm =
            keep_unread && record_text_unreadable(record) && record_has_invoice_signal(record);
        if !related && !awaiting_llm {
            continue;
        }
        if let Some(path) = &record.source_path {
            let file_name = Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if excluded.is_match(file_name) {
                continue;
            }
        }
        if let Some(invoice_number) = &record.invoice_number {
            let key = comparable_invoice_number(invoice_number);
            if !key.is_empty() {
                let group = by_invoice.entry(key).or_default();
                match group
                    .iter_mut()
                    .find(|existing| same_mail_invoice(existing, record, &productmesh_nip))
                {
                    Some(existing)
                        if record_quality_score(existing) >= record_quality_score(record) => {}
                    Some(existing) => *existing = record.clone(),
                    None => group.push(record.clone()),
                }
                continue;
            }
        }
        let key = format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            record.invoice_number,
            record.issue_date,
            record.gross_amount_minor,
            record.seller_tax_id,
            record.buyer_tax_id,
            record.seller_name,
            record.buyer_name
        );
        if fallback_seen.insert(key) {
            fallback_out.push(record.clone());
        }
    }
    let mut out = by_invoice.into_values().flatten().collect::<Vec<_>>();
    out.extend(fallback_out);
    out.sort_by(|a, b| a.source_path.cmp(&b.source_path));
    out
}

// Ten sam numer u dwóch kontrahentów to dwie faktury. Scalamy tylko ten sam plik,
// wspólny NIP kontrahenta albo (gdy któregoś NIP brak) tę samą kwotę brutto.
fn same_mail_invoice(left: &InvoiceRecord, right: &InvoiceRecord, own_nip: &str) -> bool {
    if !left.content_hash.is_empty() && left.content_hash == right.content_hash {
        return true;
    }
    let counterparty_ids = |record: &InvoiceRecord| {
        let mut ids = scoring_tax_ids(record);
        ids.remove(own_nip);
        ids
    };
    let (left_ids, right_ids) = (counterparty_ids(left), counterparty_ids(right));
    if !left_ids.is_empty() && !right_ids.is_empty() {
        return !left_ids.is_disjoint(&right_ids);
    }
    left.gross_amount_minor.is_some() && left.gross_amount_minor == right.gross_amount_minor
}

#[cfg(test)]
mod parser_records_tests;

pub(crate) fn record_quality_score(record: &InvoiceRecord) -> usize {
    [
        record.invoice_number.is_some() && !record_number_from_filename(record),
        record.issue_date.is_some(),
        record.gross_amount_minor.is_some(),
        record.currency.is_some(),
        record.seller_tax_id.is_some(),
        record.buyer_tax_id.is_some(),
        record.seller_name.is_some(),
        record.buyer_name.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count()
}

// A cache entry must cover and agree with every established invoice field.
fn mail_cache_covers_fresh(cached: &InvoiceRecord, fresh: &InvoiceRecord) -> bool {
    macro_rules! covers {
        ($($field:ident),* $(,)?) => { true $(
            && (fresh.$field.is_none() || fresh.$field == cached.$field)
        )* };
    }
    covers!(
        invoice_number,
        seller_tax_id,
        buyer_tax_id,
        issue_date,
        sale_date,
        due_date,
        gross_amount_minor,
        net_amount_minor,
        vat_amount_minor,
        currency,
        ksef_reference
    ) && [
        (&fresh.seller_name, &cached.seller_name),
        (&fresh.buyer_name, &cached.buyer_name),
    ]
    .into_iter()
    .all(|(fresh, cached)| {
        fresh
            .as_deref()
            .is_none_or(counterparty_name_is_placeholder)
            || fresh == cached
    })
}

pub(crate) fn apply_cached_mail_candidates(
    path: &Path,
    candidates: &mut [InvoiceRecord],
) -> Result<HashSet<String>> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let cached = load_records(SourceKind::Mail, path)?;
    // A verified LLM answer comes from the PDF itself, so it stays valid when the
    // local parse is marked for retry (e.g. OCR not installed) or unversioned.
    let by_hash = cached
        .into_iter()
        .filter(|record| {
            record_has_llm_result(record)
                || record
                    .warnings
                    .iter()
                    .any(|warning| warning == MAIL_PARSER_VERSION)
        })
        .map(|record| (record.content_hash.clone(), record))
        .collect::<HashMap<_, _>>();
    let mut cached_hashes = HashSet::new();
    for candidate in candidates {
        if let Some(cached) = by_hash.get(&candidate.content_hash) {
            let llm_result = record_has_llm_result(cached);
            if !llm_result && mail_parse_needs_retry(cached) {
                continue;
            }
            // The LLM may have replaced inconsistent amounts or a number guessed
            // from the file name; the fresh parse still has those values.
            let replace_amounts = llm_result
                && record_amounts_inconsistent(candidate)
                && !record_amounts_inconsistent(cached)
                && cached.gross_amount_minor.is_some();
            let replace_number = llm_result
                && record_number_guessed_from_filename(candidate)
                && !record_number_guessed_from_filename(cached)
                && cached.invoice_number.is_some();
            let mut fresh = candidate.clone();
            if replace_amounts {
                fresh.net_amount_minor = None;
                fresh.vat_amount_minor = None;
                fresh.gross_amount_minor = None;
            }
            if replace_number {
                fresh.invoice_number = None;
            }
            if !mail_cache_covers_fresh(cached, &fresh) {
                continue;
            }
            if replace_amounts {
                candidate.net_amount_minor = cached.net_amount_minor;
                candidate.vat_amount_minor = cached.vat_amount_minor;
                candidate.gross_amount_minor = cached.gross_amount_minor;
            }
            if replace_number {
                candidate.invoice_number = cached.invoice_number.clone();
                candidate
                    .warnings
                    .retain(|warning| !is_filename_number_warning(warning));
            }
            merge_mail_fields(candidate, cached);
            for warning in &cached.warnings {
                if !own_nip::is_check_warning(warning) && !candidate.warnings.contains(warning) {
                    candidate.warnings.push(warning.clone());
                }
            }
            let complete = if llm_result {
                !record_missing_core_fields(candidate)
            } else {
                !record_missing_core_fields(cached) && !mail_parse_needs_retry(candidate)
            };
            if complete {
                cached_hashes.insert(candidate.content_hash.clone());
            }
        }
    }
    Ok(cached_hashes)
}
