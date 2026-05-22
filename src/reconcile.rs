use crate::*;

pub(crate) fn read_tri_report(path: &Path) -> Result<TriReconcileReport> {
    let text = fs::read_to_string(path).with_context(|| format!("odczyt {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("niepoprawny tri report {}", path.display()))
}

pub(crate) fn tri_reconcile(
    mail_records: Vec<InvoiceRecord>,
    ksef_records: Vec<InvoiceRecord>,
    saldeo_records: Vec<InvoiceRecord>,
    review_score: u8,
) -> TriReconcileReport {
    let mail_records = dedupe_reconcile_records(mail_records);
    let ksef_records = dedupe_reconcile_records(ksef_records);
    let saldeo_records = dedupe_reconcile_records(saldeo_records);
    let mut rows = Vec::new();
    let mut used_ksef = HashSet::new();
    let mut used_saldeo = HashSet::new();

    for mail in &mail_records {
        let best_ksef = best_match(mail, &ksef_records, &used_ksef, review_score);
        let best_saldeo = best_match(mail, &saldeo_records, &used_saldeo, review_score);
        if let Some((idx, _)) = best_ksef {
            used_ksef.insert(idx);
        }
        if let Some((idx, _)) = best_saldeo {
            used_saldeo.insert(idx);
        }
        let ksef = best_ksef.map(|(idx, _)| ksef_records[idx].clone());
        let saldeo = best_saldeo.map(|(idx, _)| saldeo_records[idx].clone());
        let ksef_score_to_saldeo = match (&ksef, &saldeo) {
            (Some(k), Some(s)) => Some(score_pair(k, s).0),
            _ => None,
        };
        rows.push(TriRow {
            status: tri_status(true, ksef.is_some(), saldeo.is_some()).to_string(),
            mail_score_to_ksef: best_ksef.map(|(_, score)| score),
            mail_score_to_saldeo: best_saldeo.map(|(_, score)| score),
            ksef_score_to_saldeo,
            mail: Some(mail.clone()),
            ksef,
            saldeo,
        });
    }

    let mut used_ksef_extra = HashSet::new();
    for (ksef_idx, ksef) in ksef_records.iter().enumerate() {
        if used_ksef.contains(&ksef_idx) {
            continue;
        }
        if let Some((saldeo_idx, score)) =
            best_match(ksef, &saldeo_records, &used_saldeo, review_score)
        {
            used_ksef.insert(ksef_idx);
            used_ksef_extra.insert(ksef_idx);
            used_saldeo.insert(saldeo_idx);
            rows.push(TriRow {
                status: tri_status(false, true, true).to_string(),
                mail_score_to_ksef: None,
                mail_score_to_saldeo: None,
                ksef_score_to_saldeo: Some(score),
                mail: None,
                ksef: Some(ksef.clone()),
                saldeo: Some(saldeo_records[saldeo_idx].clone()),
            });
        }
    }

    for (idx, ksef) in ksef_records.iter().enumerate() {
        if !used_ksef.contains(&idx) && !used_ksef_extra.contains(&idx) {
            rows.push(TriRow {
                status: tri_status(false, true, false).to_string(),
                mail_score_to_ksef: None,
                mail_score_to_saldeo: None,
                ksef_score_to_saldeo: None,
                mail: None,
                ksef: Some(ksef.clone()),
                saldeo: None,
            });
        }
    }
    for (idx, saldeo) in saldeo_records.iter().enumerate() {
        if !used_saldeo.contains(&idx) {
            rows.push(TriRow {
                status: tri_status(false, false, true).to_string(),
                mail_score_to_ksef: None,
                mail_score_to_saldeo: None,
                ksef_score_to_saldeo: None,
                mail: None,
                ksef: None,
                saldeo: Some(saldeo.clone()),
            });
        }
    }

    let summary = TriSummary {
        mail_count: mail_records.len(),
        ksef_count: ksef_records.len(),
        saldeo_count: saldeo_records.len(),
        in_all_three: rows.iter().filter(|r| r.status == "in_all_three").count(),
        gmail_ksef_missing_saldeo: rows
            .iter()
            .filter(|r| r.status == "gmail_ksef_missing_saldeo")
            .count(),
        gmail_saldeo_missing_ksef: rows
            .iter()
            .filter(|r| r.status == "gmail_saldeo_missing_ksef")
            .count(),
        gmail_only: rows.iter().filter(|r| r.status == "gmail_only").count(),
        ksef_saldeo_missing_gmail: rows
            .iter()
            .filter(|r| r.status == "ksef_saldeo_missing_gmail")
            .count(),
        ksef_only: rows.iter().filter(|r| r.status == "ksef_only").count(),
        saldeo_only: rows.iter().filter(|r| r.status == "saldeo_only").count(),
    };
    TriReconcileReport {
        generated_at: Utc::now(),
        review_score,
        summary,
        rows,
    }
}

pub(crate) fn dedupe_reconcile_records(records: Vec<InvoiceRecord>) -> Vec<InvoiceRecord> {
    let mut out = Vec::<InvoiceRecord>::new();
    let mut by_key = HashMap::<String, usize>::new();
    for record in records {
        let Some(key) = reconcile_dedupe_key(&record) else {
            out.push(record);
            continue;
        };
        if let Some(existing_idx) = by_key.get(&key).copied() {
            if record_completeness_score(&record) > record_completeness_score(&out[existing_idx]) {
                out[existing_idx] = record;
            }
        } else {
            by_key.insert(key, out.len());
            out.push(record);
        }
    }
    out
}

pub(crate) fn reconcile_dedupe_key(record: &InvoiceRecord) -> Option<String> {
    if record.source == SourceKind::Ksef
        && let Some(ksef_reference) = record
            .ksef_reference
            .as_deref()
            .filter(|v| !v.trim().is_empty())
    {
        return Some(format!("ksef:{}", ksef_reference.trim()));
    }
    let invoice = record.invoice_number.as_deref()?;
    let invoice = comparable_invoice_number(invoice);
    if invoice.is_empty() {
        return None;
    }
    let tax_ids = [
        record.seller_tax_id.as_deref(),
        record.buyer_tax_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|id| !id.trim().is_empty())
    .collect::<Vec<_>>();
    if record.issue_date.is_none() && record.gross_amount_minor.is_none() && tax_ids.is_empty() {
        return None;
    }
    let mut tax_ids = tax_ids;
    tax_ids.sort_unstable();
    Some(format!(
        "inv:{}|date:{}|gross:{}|cur:{}|tax:{}",
        invoice,
        record
            .issue_date
            .map(|date| date.to_string())
            .unwrap_or_default(),
        record.gross_amount_minor.unwrap_or_default(),
        record.currency.as_deref().unwrap_or(""),
        tax_ids.join(",")
    ))
}

pub(crate) fn record_completeness_score(record: &InvoiceRecord) -> usize {
    let mut score = 0usize;
    score += record.ksef_reference.is_some() as usize * 16;
    score += record.invoice_number.is_some() as usize * 8;
    score += record.seller_tax_id.is_some() as usize * 2;
    score += record.buyer_tax_id.is_some() as usize * 2;
    score += record.seller_name.is_some() as usize;
    score += record.buyer_name.is_some() as usize;
    score += record.issue_date.is_some() as usize * 2;
    score += record.sale_date.is_some() as usize;
    score += record.due_date.is_some() as usize;
    score += record.gross_amount_minor.is_some() as usize * 2;
    score += record.net_amount_minor.is_some() as usize;
    score += record.vat_amount_minor.is_some() as usize;
    score += record.currency.is_some() as usize;
    score += record.source_path.is_some() as usize;
    score
}

pub(crate) fn best_match(
    needle: &InvoiceRecord,
    haystack: &[InvoiceRecord],
    used: &HashSet<usize>,
    min_score: u8,
) -> Option<(usize, u8)> {
    haystack
        .iter()
        .enumerate()
        .filter(|(idx, _)| !used.contains(idx))
        .map(|(idx, candidate)| (idx, score_pair(needle, candidate).0))
        .filter(|(_, score)| *score >= min_score)
        .max_by_key(|(_, score)| *score)
}

pub(crate) fn tri_status(has_mail: bool, has_ksef: bool, has_saldeo: bool) -> &'static str {
    match (has_mail, has_ksef, has_saldeo) {
        (true, true, true) => "in_all_three",
        (true, true, false) => "gmail_ksef_missing_saldeo",
        (true, false, true) => "gmail_saldeo_missing_ksef",
        (true, false, false) => "gmail_only",
        (false, true, true) => "ksef_saldeo_missing_gmail",
        (false, true, false) => "ksef_only",
        (false, false, true) => "saldeo_only",
        (false, false, false) => "empty",
    }
}

pub(crate) fn tri_row_primary_record(row: &TriRow) -> Option<&InvoiceRecord> {
    row.mail
        .as_ref()
        .or(row.ksef.as_ref())
        .or(row.saldeo.as_ref())
}

pub(crate) fn tri_row_display_record(row: &TriRow) -> Option<InvoiceRecord> {
    let saldeo_corrected = row.saldeo.as_ref().is_some_and(saldeo_record_has_override);
    let mut record = if saldeo_corrected {
        row.saldeo
            .as_ref()
            .or(row.mail.as_ref())
            .or(row.ksef.as_ref())?
            .clone()
    } else {
        tri_row_primary_record(row)?.clone()
    };
    let metadata_sources = if saldeo_corrected {
        [row.saldeo.as_ref(), row.ksef.as_ref(), row.mail.as_ref()]
    } else {
        [row.ksef.as_ref(), row.saldeo.as_ref(), row.mail.as_ref()]
    };
    let name_sources = if saldeo_corrected {
        [row.saldeo.as_ref(), row.mail.as_ref(), row.ksef.as_ref()]
    } else {
        [row.mail.as_ref(), row.ksef.as_ref(), row.saldeo.as_ref()]
    };

    if record.invoice_number.is_none() {
        record.invoice_number = metadata_sources
            .iter()
            .find_map(|source| source.and_then(|r| r.invoice_number.clone()));
    }
    if record.ksef_reference.is_none() {
        record.ksef_reference = metadata_sources
            .iter()
            .find_map(|source| source.and_then(|r| r.ksef_reference.clone()));
    }

    if let Some(value) = name_sources
        .iter()
        .find_map(|source| source.and_then(|r| r.seller_name.clone()))
    {
        record.seller_name = Some(value);
    }
    if let Some(value) = name_sources
        .iter()
        .find_map(|source| source.and_then(|r| r.buyer_name.clone()))
    {
        record.buyer_name = Some(value);
    }
    if let Some(value) = metadata_sources
        .iter()
        .find_map(|source| source.and_then(|r| r.seller_tax_id.clone()))
    {
        record.seller_tax_id = Some(value);
    }
    if let Some(value) = metadata_sources
        .iter()
        .find_map(|source| source.and_then(|r| r.buyer_tax_id.clone()))
    {
        record.buyer_tax_id = Some(value);
    }
    if let Some(value) = metadata_sources
        .iter()
        .find_map(|source| source.and_then(|r| r.issue_date))
    {
        record.issue_date = Some(value);
    }
    if let Some(value) = metadata_sources
        .iter()
        .find_map(|source| source.and_then(|r| r.sale_date))
    {
        record.sale_date = Some(value);
    }
    if let Some(value) = metadata_sources
        .iter()
        .find_map(|source| source.and_then(|r| r.due_date))
    {
        record.due_date = Some(value);
    }
    if let Some(value) = metadata_sources
        .iter()
        .find_map(|source| source.and_then(|r| r.gross_amount_minor))
    {
        record.gross_amount_minor = Some(value);
    }
    if let Some(value) = metadata_sources
        .iter()
        .find_map(|source| source.and_then(|r| r.net_amount_minor))
    {
        record.net_amount_minor = Some(value);
    }
    if let Some(value) = metadata_sources
        .iter()
        .find_map(|source| source.and_then(|r| r.vat_amount_minor))
    {
        record.vat_amount_minor = Some(value);
    }
    if let Some(value) = metadata_sources
        .iter()
        .find_map(|source| source.and_then(|r| r.currency.clone()))
    {
        record.currency = Some(value);
    }

    Some(record)
}

pub(crate) fn write_reconcile_human(
    report: &TriReconcileReport,
    temporal_diff: Option<&TemporalDiffSummary>,
) -> Result<()> {
    let mut out = String::new();
    out.push_str("LAB reconcile\n");
    out.push_str(&format!(
        "generated: {} | review_score: {}\n\n",
        report.generated_at, report.review_score
    ));
    out.push_str(&format!(
        "Źródła: Gmail {} | KSeF {} | Saldeo {}\n",
        report.summary.mail_count, report.summary.ksef_count, report.summary.saldeo_count
    ));
    out.push_str("Statusy:\n");
    for (label, count) in reconcile_status_counts(&report.summary) {
        out.push_str(&format!("  {:30} {}\n", label, count));
    }
    if let Some(diff) = temporal_diff {
        out.push_str(&format!(
            "\nDiff vs poprzedni run: +{} -{} ~{} (run #{})\n",
            diff.added_count, diff.removed_count, diff.changed_count, diff.run_id
        ));
    }

    let missing_rows = report
        .rows
        .iter()
        .filter(|row| row.status != "in_all_three")
        .collect::<Vec<_>>();
    out.push_str(&format!(
        "\nBraki / do sprawdzenia: {} pozycji",
        missing_rows.len()
    ));
    if !missing_rows.is_empty() {
        out.push('\n');
        out.push_str(&format!(
            "{:<30} {:<28} {:<30} {:<12} {:>12} {:<4} {:<18}\n",
            "status", "faktura", "kontrahent", "data", "brutto", "wal", "źródła"
        ));
        out.push_str(&format!("{}\n", "-".repeat(142)));
        for row in missing_rows {
            let primary = tri_row_display_record(row);
            let primary = primary.as_ref();
            out.push_str(&format!(
                "{:<30} {:<28} {:<30} {:<12} {:>12} {:<4} {:<18}\n",
                truncate(&row.status, 30),
                truncate(
                    &primary
                        .and_then(|r| r.invoice_number.clone())
                        .unwrap_or_else(|| "-".to_string()),
                    28,
                ),
                truncate(&counterparty_name(primary), 30),
                primary
                    .and_then(|r| r.issue_date)
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                primary
                    .and_then(|r| r.gross_amount_minor)
                    .map(format_minor_money)
                    .unwrap_or_else(|| "-".to_string()),
                primary
                    .and_then(|r| r.currency.clone())
                    .unwrap_or_else(|| "-".to_string()),
                row_sources(row),
            ));
        }
    } else {
        out.push('\n');
    }
    out.push_str("\nPełny JSON: lab reconcile --raw\n");
    print!("{out}");
    Ok(())
}

pub(crate) fn reconcile_status_counts(summary: &TriSummary) -> Vec<(&'static str, usize)> {
    vec![
        ("in_all_three", summary.in_all_three),
        (
            "gmail_ksef_missing_saldeo",
            summary.gmail_ksef_missing_saldeo,
        ),
        (
            "gmail_saldeo_missing_ksef",
            summary.gmail_saldeo_missing_ksef,
        ),
        ("gmail_only", summary.gmail_only),
        (
            "ksef_saldeo_missing_gmail",
            summary.ksef_saldeo_missing_gmail,
        ),
        ("ksef_only", summary.ksef_only),
        ("saldeo_only", summary.saldeo_only),
    ]
}

pub(crate) fn counterparty_name(record: Option<&InvoiceRecord>) -> String {
    record
        .and_then(|r| r.seller_name.clone().or_else(|| r.buyer_name.clone()))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "-".to_string())
}

pub(crate) fn row_sources(row: &TriRow) -> String {
    let mut sources = [
        ("G", row.mail.is_some()),
        ("K", row.ksef.is_some()),
        ("S", row.saldeo.is_some()),
    ]
    .into_iter()
    .filter_map(|(label, present)| present.then_some(label))
    .collect::<Vec<_>>()
    .join("+");
    if row
        .saldeo
        .as_ref()
        .is_some_and(saldeo_record_has_override)
    {
        sources.push('*');
    }
    sources
}

pub(crate) fn format_minor_money(value: i64) -> String {
    format!("{}.{:02}", value / 100, value.abs() % 100)
}

pub(crate) fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

pub(crate) fn write_tri_csv(report: &TriReconcileReport, path: &Path) -> Result<()> {
    let mut writer =
        csv::Writer::from_path(path).with_context(|| format!("zapis CSV {}", path.display()))?;
    writer.write_record([
        "status",
        "mail_invoice_number",
        "ksef_invoice_number",
        "saldeo_invoice_number",
        "ksef_number",
        "saldeo_ksef_number",
        "issue_date",
        "gross_amount_minor",
        "currency",
        "mail_score_to_ksef",
        "mail_score_to_saldeo",
        "ksef_score_to_saldeo",
    ])?;
    for row in &report.rows {
        let primary = tri_row_display_record(row);
        let primary = primary.as_ref();
        writer.write_record([
            row.status.clone(),
            row.mail
                .as_ref()
                .and_then(|r| r.invoice_number.clone())
                .unwrap_or_default(),
            row.ksef
                .as_ref()
                .and_then(|r| r.invoice_number.clone())
                .unwrap_or_default(),
            row.saldeo
                .as_ref()
                .and_then(|r| r.invoice_number.clone())
                .unwrap_or_default(),
            row.ksef
                .as_ref()
                .and_then(|r| r.ksef_reference.clone())
                .unwrap_or_default(),
            row.saldeo
                .as_ref()
                .and_then(|r| r.ksef_reference.clone())
                .unwrap_or_default(),
            primary
                .and_then(|r| r.issue_date)
                .map(|d| d.to_string())
                .unwrap_or_default(),
            primary
                .and_then(|r| r.gross_amount_minor)
                .map(|v| v.to_string())
                .unwrap_or_default(),
            primary.and_then(|r| r.currency.clone()).unwrap_or_default(),
            row.mail_score_to_ksef
                .map(|v| v.to_string())
                .unwrap_or_default(),
            row.mail_score_to_saldeo
                .map(|v| v.to_string())
                .unwrap_or_default(),
            row.ksef_score_to_saldeo
                .map(|v| v.to_string())
                .unwrap_or_default(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}
