use crate::*;

pub(crate) fn score_pair(ksef: &InvoiceRecord, mail: &InvoiceRecord) -> (u8, Vec<String>) {
    let mut score: u16 = 0;
    let mut reasons = Vec::new();

    if let (Some(a), Some(b)) = (
        normalized_ksef_reference(ksef),
        normalized_ksef_reference(mail),
    ) {
        if a != b {
            // Two different KSeF numbers are two different documents.
            return (0, vec!["ksef_reference conflict".to_string()]);
        }
        score += 100;
        reasons.push("ksef_reference exact".to_string());
    }

    if let (Some(a), Some(b)) = (&ksef.invoice_number, &mail.invoice_number) {
        let comparable_a = comparable_invoice_number(a);
        let comparable_b = comparable_invoice_number(b);
        if !comparable_a.is_empty() && comparable_a == comparable_b {
            score += 45;
            reasons.push("invoice_number exact".to_string());
        } else if invoice_number_strong_contains(a, b) {
            score += 45;
            reasons.push("invoice_number embedded exact".to_string());
        } else if invoice_number_token_contains(a, b) {
            score += 25;
            reasons.push("invoice_number partial".to_string());
        }
    }

    let ksef_ids = scoring_tax_ids(ksef);
    let mail_ids = scoring_tax_ids(mail);
    if !ksef_ids.is_empty() && ksef_ids.iter().any(|id| mail_ids.contains(id)) {
        score += 20;
        reasons.push("tax_id match".to_string());
    }
    if let (Some(a), Some(b)) = (&ksef.seller_tax_id, &mail.seller_tax_id) {
        let a = normalize_tax_id(a);
        let b = normalize_tax_id(b);
        if a.is_some() && a == b && a.as_deref() != Some(DEFAULT_PRODUCTMESH_NIP) {
            score += 5;
            reasons.push("seller_tax_id same position".to_string());
        }
    }

    if !currencies_conflict(ksef, mail)
        && let (Some(a), Some(b)) = (ksef.gross_amount_minor, mail.gross_amount_minor)
        && a != 0
        && b != 0
    {
        let diff = (a - b).abs();
        if diff == 0 {
            score += 20;
            reasons.push("gross_amount exact".to_string());
        } else if diff <= 2 {
            score += 17;
            reasons.push("gross_amount near".to_string());
        }
    }

    if let (Some(a), Some(b)) = (ksef.issue_date, mail.issue_date) {
        let diff = (a - b).num_days().abs();
        if diff == 0 {
            score += 10;
            reasons.push("issue_date exact".to_string());
        } else if diff <= 7 {
            score += 4;
            reasons.push("issue_date near".to_string());
        }
    }

    if let (Some(a), Some(b)) = (&ksef.currency, &mail.currency)
        && a.trim().eq_ignore_ascii_case(b.trim())
    {
        score += 5;
        reasons.push("currency match".to_string());
    }

    (score.min(100) as u8, reasons)
}

pub(crate) fn scoring_tax_ids(record: &InvoiceRecord) -> HashSet<String> {
    [
        record.seller_tax_id.as_deref(),
        record.buyer_tax_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(normalize_tax_id)
    .filter(|id| id != DEFAULT_PRODUCTMESH_NIP)
    .collect()
}

/// Same invoice regardless of the score threshold: shared KSeF number, or the
/// same invoice number with a shared counterparty NIP. When either side has no
/// counterparty NIP, the number alone is not enough — the gross amount (±2 gr)
/// must agree and currencies must not conflict.
pub(crate) fn invoice_identity_match(left: &InvoiceRecord, right: &InvoiceRecord) -> bool {
    if let (Some(a), Some(b)) = (
        normalized_ksef_reference(left),
        normalized_ksef_reference(right),
    ) {
        return a == b;
    }
    let Some(left_number) = left
        .invoice_number
        .as_deref()
        .map(comparable_invoice_number)
        .filter(|number| !number.is_empty())
    else {
        return false;
    };
    let Some(right_number) = right
        .invoice_number
        .as_deref()
        .map(comparable_invoice_number)
        .filter(|number| !number.is_empty())
    else {
        return false;
    };
    if left_number != right_number {
        return false;
    }
    let left_ids = scoring_tax_ids(left);
    let right_ids = scoring_tax_ids(right);
    if !left_ids.is_empty() && !right_ids.is_empty() {
        return !left_ids.is_disjoint(&right_ids);
    }
    if currencies_conflict(left, right) {
        return false;
    }
    matches!(
        (left.gross_amount_minor, right.gross_amount_minor),
        (Some(a), Some(b)) if a != 0 && b != 0 && (a - b).abs() <= 2
    )
}

/// Canonical form of an invoice number for equality and grouping keys: the
/// number split on separators and at letter/digit boundaries, upper-cased,
/// leading zeros dropped from numeric tokens, joined with `/`.
///
/// `FV12/2026`, `FV/12/2026`, `fv-12-2026` and `FV/012/2026` are all
/// `FV/12/2026`, while `1/1/2026` and `11/2026` (or `12/2026` and `1/22026`)
/// stay different. Leading zeros are dropped because they never tell apart two real
/// invoices of one seller (a seller numbers with a fixed width), but do get lost
/// when a number is retyped or OCR-ed in Saldeo. Empty when the value has no
/// alphanumerics.
pub(crate) fn comparable_invoice_number(value: &str) -> String {
    invoice_number_identity_tokens(value).join("/")
}

fn invoice_number_identity_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for c in value.chars() {
        let boundary = !c.is_ascii_alphanumeric()
            || current
                .chars()
                .last()
                .is_some_and(|last| last.is_ascii_digit() != c.is_ascii_digit());
        if boundary && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
        if c.is_ascii_alphanumeric() {
            current.push(c.to_ascii_uppercase());
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    for token in &mut tokens {
        if token.starts_with('0') && token.bytes().all(|b| b.is_ascii_digit()) {
            let trimmed = token.trim_start_matches('0');
            *token = if trimmed.is_empty() { "0" } else { trimmed }.to_string();
        }
    }
    tokens
}

/// Token-bounded containment of a long (>= 8 alphanumerics) invoice number.
fn invoice_number_strong_contains(a: &str, b: &str) -> bool {
    let alphanumerics = |value: &str| value.chars().filter(|c| c.is_ascii_alphanumeric()).count();
    let min_len = alphanumerics(a).min(alphanumerics(b));
    min_len >= 8 && invoice_number_token_contains(a, b)
}

/// Invoice number split on its original separators (`FV/12/2026` -> FV, 12, 2026).
fn invoice_number_tokens(value: &str) -> Vec<String> {
    value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_uppercase)
        .collect()
}

/// True when one number's tokens are a contiguous run of the other's tokens, so
/// `FV/12/2026` is inside `FV/12/2026/A`, but `1/2026` is not inside `11/2026`.
pub(crate) fn invoice_number_token_contains(a: &str, b: &str) -> bool {
    let a = invoice_number_tokens(a);
    let b = invoice_number_tokens(b);
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    !short.is_empty() && long.windows(short.len()).any(|run| run == short.as_slice())
}

pub(crate) fn normalized_ksef_reference(record: &InvoiceRecord) -> Option<String> {
    record
        .ksef_reference
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_uppercase)
}

fn currencies_conflict(left: &InvoiceRecord, right: &InvoiceRecord) -> bool {
    let normalize = |record: &InvoiceRecord| {
        record
            .currency
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_uppercase)
    };
    matches!((normalize(left), normalize(right)), (Some(a), Some(b)) if a != b)
}
