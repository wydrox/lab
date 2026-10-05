use crate::*;

#[cfg(test)]
mod matching_tests;

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
    let groups = assign_tri_groups(
        [&mail_records, &ksef_records, &saldeo_records],
        review_score,
    );
    let rows = groups
        .into_iter()
        .map(|[mail, ksef, saldeo]| {
            build_tri_row(
                mail.map(|idx| mail_records[idx].clone()),
                ksef.map(|idx| ksef_records[idx].clone()),
                saldeo.map(|idx| saldeo_records[idx].clone()),
            )
        })
        .collect::<Vec<_>>();

    let rows = merge_duplicate_tri_rows(rows);
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
            if record_is_better(&record, &out[existing_idx]) {
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
    let date = if record.gross_amount_minor.is_none() && tax_ids.is_empty() {
        record
            .issue_date
            .map(|date| date.to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    Some(format!(
        "inv:{}|date:{}|gross:{}|cur:{}|tax:{}",
        invoice,
        date,
        record.gross_amount_minor.unwrap_or_default(),
        record.currency.as_deref().unwrap_or(""),
        tax_ids.join(",")
    ))
}

/// More complete record wins; ties resolve by content so input order does not matter.
fn record_is_better(candidate: &InvoiceRecord, current: &InvoiceRecord) -> bool {
    match record_completeness_score(candidate).cmp(&record_completeness_score(current)) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => {
            canonical_record_key(candidate) < canonical_record_key(current)
        }
    }
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

const TRI_MAIL: usize = 0;
const TRI_KSEF: usize = 1;
const TRI_SALDEO: usize = 2;

/// Record index per source slot (mail, KSeF, Saldeo) of one reconcile row.
type TriMembers = [Option<usize>; 3];

/// (source slot, record index)
type TriMember = (usize, usize);

struct TriLinkCandidate {
    score: u8,
    left: TriMember,
    right: TriMember,
}

/// Groups records into rows by global passes, so the outcome does not depend on
/// input order and a weak match cannot take a record that has an exact one:
/// (a) shared KSeF number, (b) invoice identity, (c) fuzzy score >= `review_score`.
/// Within a pass candidates are taken best score first. A row holds at most one
/// record per source and never two different KSeF numbers.
fn assign_tri_groups(sources: [&[InvoiceRecord]; 3], review_score: u8) -> Vec<TriMembers> {
    let ranks = sources.map(canonical_record_ranks);
    let mut by_reference = Vec::new();
    let mut by_identity = Vec::new();
    let mut fuzzy = Vec::new();
    for (left_slot, right_slot) in [
        (TRI_MAIL, TRI_KSEF),
        (TRI_MAIL, TRI_SALDEO),
        (TRI_KSEF, TRI_SALDEO),
    ] {
        for (left_idx, left) in sources[left_slot].iter().enumerate() {
            let left_reference = normalized_ksef_reference(left);
            for (right_idx, right) in sources[right_slot].iter().enumerate() {
                let right_reference = normalized_ksef_reference(right);
                if let (Some(a), Some(b)) = (&left_reference, &right_reference)
                    && a != b
                {
                    continue;
                }
                let candidate = TriLinkCandidate {
                    score: score_pair(left, right).0,
                    left: (left_slot, left_idx),
                    right: (right_slot, right_idx),
                };
                if left_reference.is_some() && left_reference == right_reference {
                    by_reference.push(candidate);
                } else if invoice_identity_match(left, right) {
                    by_identity.push(candidate);
                } else if candidate.score >= review_score {
                    fuzzy.push(candidate);
                }
            }
        }
    }

    let mut groups = TriGroups::new(sources);
    for mut pass in [by_reference, by_identity, fuzzy] {
        pass.sort_by(|a, b| {
            b.score.cmp(&a.score).then_with(|| {
                let key = |c: &TriLinkCandidate| {
                    (
                        c.left.0,
                        ranks[c.left.0][c.left.1],
                        c.right.0,
                        ranks[c.right.0][c.right.1],
                    )
                };
                key(a).cmp(&key(b))
            })
        });
        for candidate in pass {
            groups.try_link(candidate.left, candidate.right);
        }
    }
    groups.into_rows()
}

/// Position of each record in a content-based order; used as the tie-break so
/// equal scores resolve the same way whatever order the sources came in.
fn canonical_record_ranks(records: &[InvoiceRecord]) -> Vec<usize> {
    let keys = records.iter().map(canonical_record_key).collect::<Vec<_>>();
    let mut order = (0..records.len()).collect::<Vec<_>>();
    order.sort_by(|a, b| keys[*a].cmp(&keys[*b]).then(a.cmp(b)));
    let mut ranks = vec![0; records.len()];
    for (rank, idx) in order.into_iter().enumerate() {
        ranks[idx] = rank;
    }
    ranks
}

fn canonical_record_key(record: &InvoiceRecord) -> String {
    serde_json::to_string(record).unwrap_or_default()
}

struct TriGroups<'a> {
    sources: [&'a [InvoiceRecord]; 3],
    group_of: [Vec<usize>; 3],
    groups: Vec<TriMembers>,
}

impl<'a> TriGroups<'a> {
    fn new(sources: [&'a [InvoiceRecord]; 3]) -> Self {
        let mut group_of = [Vec::new(), Vec::new(), Vec::new()];
        let mut groups = Vec::new();
        for (slot, records) in sources.iter().enumerate() {
            for idx in 0..records.len() {
                group_of[slot].push(groups.len());
                let mut members = [None; 3];
                members[slot] = Some(idx);
                groups.push(members);
            }
        }
        Self {
            sources,
            group_of,
            groups,
        }
    }

    /// Merges the groups of `left` and `right` unless that would put two records
    /// of one source, or two different KSeF numbers, into one row.
    fn try_link(&mut self, left: TriMember, right: TriMember) -> bool {
        let left_group = self.group_of[left.0][left.1];
        let right_group = self.group_of[right.0][right.1];
        if left_group == right_group {
            return false;
        }
        let mut merged = self.groups[left_group];
        for (slot, idx) in self.groups[right_group].into_iter().enumerate() {
            if let Some(idx) = idx {
                if merged[slot].is_some() {
                    return false;
                }
                merged[slot] = Some(idx);
            }
        }
        let mut reference = None;
        for (slot, idx) in merged.into_iter().enumerate() {
            let Some(idx) = idx else { continue };
            let Some(value) = normalized_ksef_reference(&self.sources[slot][idx]) else {
                continue;
            };
            match &reference {
                Some(existing) if *existing != value => return false,
                Some(_) => {}
                None => reference = Some(value),
            }
        }
        for (slot, idx) in self.groups[right_group].into_iter().enumerate() {
            if let Some(idx) = idx {
                self.group_of[slot][idx] = left_group;
            }
        }
        self.groups[left_group] = merged;
        self.groups[right_group] = [None; 3];
        true
    }

    /// Rows with mail first (mail order), then KSeF+Saldeo, KSeF-only and
    /// Saldeo-only, each in source order.
    fn into_rows(self) -> Vec<TriMembers> {
        let mut rows = self
            .groups
            .into_iter()
            .filter(|members| members.iter().any(Option::is_some))
            .collect::<Vec<_>>();
        rows.sort_by_key(|members| match *members {
            [Some(mail), _, _] => (0, mail),
            [None, Some(ksef), Some(_)] => (1, ksef),
            [None, Some(ksef), None] => (2, ksef),
            [None, None, Some(saldeo)] => (3, saldeo),
            [None, None, None] => (4, 0),
        });
        rows
    }
}

fn build_tri_row(
    mail: Option<InvoiceRecord>,
    ksef: Option<InvoiceRecord>,
    saldeo: Option<InvoiceRecord>,
) -> TriRow {
    let mail_score_to_ksef = match (&mail, &ksef) {
        (Some(mail), Some(ksef)) => Some(score_pair(mail, ksef).0),
        _ => None,
    };
    let mail_score_to_saldeo = match (&mail, &saldeo) {
        (Some(mail), Some(saldeo)) => Some(score_pair(mail, saldeo).0),
        _ => None,
    };
    let ksef_score_to_saldeo = match (&ksef, &saldeo) {
        (Some(ksef), Some(saldeo)) => Some(score_pair(ksef, saldeo).0),
        _ => None,
    };
    TriRow {
        status: tri_status(mail.is_some(), ksef.is_some(), saldeo.is_some()).to_string(),
        mail_score_to_ksef,
        mail_score_to_saldeo,
        ksef_score_to_saldeo,
        mail,
        ksef,
        saldeo,
    }
}

fn invoice_number_is_mergeable(number: &str) -> bool {
    number.chars().filter(|c| c.is_ascii_digit()).count() >= 3
}

fn tri_row_merge_key(row: &TriRow) -> Option<String> {
    for record in [&row.ksef, &row.saldeo, &row.mail].into_iter().flatten() {
        if let Some(ksef_reference) = record
            .ksef_reference
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            return Some(format!("ksef:{ksef_reference}"));
        }
    }
    let record = tri_row_primary_record(row)?;
    let invoice = comparable_invoice_number(record.invoice_number.as_deref()?);
    if !invoice_number_is_mergeable(&invoice) {
        return None;
    }
    let mut taxes = scoring_tax_ids(record).into_iter().collect::<Vec<_>>();
    taxes.sort();
    Some(format!("inv:{invoice}|tax:{}", taxes.join(",")))
}

fn tri_row_amounts_compatible(left: &TriRow, right: &TriRow) -> bool {
    let Some(left_record) = tri_row_primary_record(left) else {
        return true;
    };
    let Some(right_record) = tri_row_primary_record(right) else {
        return true;
    };
    match (
        left_record.gross_amount_minor,
        right_record.gross_amount_minor,
    ) {
        (Some(left_amount), Some(right_amount)) if left_amount > 0 && right_amount > 0 => {
            (left_amount - right_amount).abs() <= 2
        }
        _ => true,
    }
}

fn pick_better_record(
    left: Option<InvoiceRecord>,
    right: Option<InvoiceRecord>,
) -> Option<InvoiceRecord> {
    match (left, right) {
        (None, record) | (record, None) => record,
        (Some(left), Some(right)) => Some(if record_is_better(&right, &left) {
            right
        } else {
            left
        }),
    }
}

fn merge_tri_row_pair(left: TriRow, right: TriRow) -> TriRow {
    build_tri_row(
        pick_better_record(left.mail, right.mail),
        pick_better_record(left.ksef, right.ksef),
        pick_better_record(left.saldeo, right.saldeo),
    )
}

/// Rows that share a source hold duplicates of one invoice within that source.
/// Rows with disjoint sources were already offered to `assign_tri_groups`, which
/// decides cross-source matches; merging them here would bypass its rules.
fn tri_rows_share_source(left: &TriRow, right: &TriRow) -> bool {
    (left.mail.is_some() && right.mail.is_some())
        || (left.ksef.is_some() && right.ksef.is_some())
        || (left.saldeo.is_some() && right.saldeo.is_some())
}

fn merge_duplicate_tri_rows(rows: Vec<TriRow>) -> Vec<TriRow> {
    let mut out = Vec::<TriRow>::new();
    let mut by_key = HashMap::<String, usize>::new();
    for row in rows {
        let Some(key) = tri_row_merge_key(&row) else {
            out.push(row);
            continue;
        };
        if let Some(existing_idx) = by_key.get(&key).copied()
            && tri_rows_share_source(&out[existing_idx], &row)
            && tri_row_amounts_compatible(&out[existing_idx], &row)
        {
            let existing = out[existing_idx].clone();
            out[existing_idx] = merge_tri_row_pair(existing, row);
            continue;
        }
        by_key.insert(key, out.len());
        out.push(row);
    }
    out
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
        [row.saldeo.as_ref(), row.ksef.as_ref(), row.mail.as_ref()]
    } else {
        [row.ksef.as_ref(), row.mail.as_ref(), row.saldeo.as_ref()]
    };

    if let Some(number) = first_useful_invoice_number(metadata_sources) {
        record.invoice_number = Some(number);
    } else if record
        .invoice_number
        .as_deref()
        .is_some_and(|number| !is_valid_invoice_number_candidate(number))
    {
        record.invoice_number = None;
    }
    if record.ksef_reference.is_none() {
        record.ksef_reference = metadata_sources
            .iter()
            .find_map(|source| source.and_then(|r| r.ksef_reference.clone()));
    }

    record.seller_name =
        first_useful_party_name(name_sources, |record| record.seller_name.as_deref());
    record.buyer_name =
        first_useful_party_name(name_sources, |record| record.buyer_name.as_deref());
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

fn useful_party_name(name: &str) -> Option<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty() || counterparty_name_is_placeholder(trimmed) {
        None
    } else {
        Some(trimmed)
    }
}

fn first_useful_party_name(
    sources: [Option<&InvoiceRecord>; 3],
    pick: impl Fn(&InvoiceRecord) -> Option<&str>,
) -> Option<String> {
    sources
        .into_iter()
        .flatten()
        .find_map(|record| pick(record).and_then(useful_party_name).map(str::to_string))
}

fn first_useful_invoice_number(sources: [Option<&InvoiceRecord>; 3]) -> Option<String> {
    sources.into_iter().flatten().find_map(|record| {
        record
            .invoice_number
            .as_deref()
            .filter(|number| is_valid_invoice_number_candidate(number))
            .map(str::to_string)
    })
}

fn name_looks_like_own_company(name: &str) -> bool {
    let lower = name.to_lowercase();
    let compact: String = lower.chars().filter(|c| c.is_alphanumeric()).collect();
    compact.contains("productmesh")
        || lower.contains("rafał wyderka")
        || lower.contains("rafal wyderka")
}

fn party_is_own_company(name: Option<&str>, tax_id: Option<&str>) -> bool {
    tax_id.and_then(normalize_tax_id).as_deref() == Some(DEFAULT_PRODUCTMESH_NIP)
        || name.is_some_and(name_looks_like_own_company)
}

pub(crate) fn counterparty_name(record: Option<&InvoiceRecord>) -> String {
    let Some(record) = record else {
        return "-".to_string();
    };
    let seller = record.seller_name.as_deref().and_then(useful_party_name);
    let buyer = record.buyer_name.as_deref().and_then(useful_party_name);
    if party_is_own_company(seller, record.seller_tax_id.as_deref())
        && let Some(name) = buyer.filter(|name| !name_looks_like_own_company(name))
    {
        return name.to_string();
    }
    if let Some(name) = seller.filter(|name| !name_looks_like_own_company(name)) {
        return name.to_string();
    }
    if let Some(name) = buyer.filter(|name| !name_looks_like_own_company(name)) {
        return name.to_string();
    }
    "-".to_string()
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
    if row.saldeo.as_ref().is_some_and(saldeo_record_has_override) {
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
