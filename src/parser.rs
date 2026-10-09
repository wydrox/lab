use crate::*;

mod amounts;
mod names;
mod numbers;
mod tax_ids;

pub(crate) use amounts::*;
pub(crate) use names::*;
pub(crate) use numbers::*;
pub(crate) use tax_ids::*;

pub(crate) fn parse_file(source: SourceKind, path: &Path) -> Result<InvoiceRecord> {
    let bytes = fs::read(path).with_context(|| format!("odczyt {}", path.display()))?;
    let hash = hex::encode(Sha256::digest(&bytes));
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let mut warnings = Vec::new();
    let mut readable = true;
    let text = match ext.as_str() {
        "pdf" => match extract_document_text(path) {
            Ok((text, extraction_warnings)) => {
                warnings.extend(extraction_warnings);
                text
            }
            Err(err) if is_pdf_password_error(&err) => {
                readable = false;
                warnings.push(PDF_PASSWORD_WARNING.to_string());
                String::new()
            }
            Err(err) => {
                readable = false;
                warnings.push(format!("{PDF_TEXT_FAILED_WARNING}: {err}"));
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string()
            }
        },
        _ => String::from_utf8_lossy(&bytes).to_string(),
    };

    let mut record = if ext == "json" {
        parse_json_invoice(source, &text).unwrap_or_else(|err| {
            let mut record = parse_text_invoice(source, &text);
            record
                .warnings
                .push(format!("JSON sparsowany jako tekst: {err}"));
            record
        })
    } else if ext == "xml" {
        parse_xml_invoice(source, &text)
    } else {
        parse_text_invoice(source, &text)
    };

    let nip_check = if readable {
        own_nip::check_text(&text)
    } else {
        own_nip::OwnNipCheck::Unreadable
    };
    own_nip::apply_check(&mut record, nip_check);
    record.source_path = Some(path.display().to_string());
    record.content_hash = hash;
    record.warnings.extend(warnings);
    if record.invoice_number.is_none() && !record_is_password_protected(&record) {
        record.invoice_number = invoice_number_from_filename(path);
        if record.invoice_number.is_some() {
            record.warnings.push(FILENAME_NUMBER_WARNING.to_string());
        }
    }
    Ok(record)
}

pub(crate) fn record_amounts_inconsistent(record: &InvoiceRecord) -> bool {
    match (
        record.net_amount_minor,
        record.vat_amount_minor,
        record.gross_amount_minor,
    ) {
        (Some(net), Some(vat), Some(gross)) => net.checked_add(vat) != Some(gross),
        (Some(net), None, Some(gross)) if net >= 0 && gross >= 0 => net > gross,
        (None, Some(vat), Some(gross)) if vat >= 0 && gross >= 0 => vat > gross,
        _ => false,
    }
}

pub(crate) fn record_missing_core_fields(record: &InvoiceRecord) -> bool {
    record_amounts_inconsistent(record)
        || record.invoice_number.is_none()
        || record.issue_date.is_none()
        || record.gross_amount_minor.is_none()
        || record.currency.is_none()
        || (record
            .seller_name
            .as_deref()
            .is_none_or(counterparty_name_is_placeholder)
            && record
                .buyer_name
                .as_deref()
                .is_none_or(counterparty_name_is_placeholder))
        || record
            .seller_name
            .as_deref()
            .is_some_and(counterparty_name_is_placeholder)
        || record
            .buyer_name
            .as_deref()
            .is_some_and(counterparty_name_is_placeholder)
}

fn invoice_signal_re() -> &'static Regex {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?i)(invoice|faktura|faktury|rachunek|proforma|\bfv\b|ksef|billing@)").unwrap()
    });
    &RE
}

pub(crate) fn record_has_invoice_signal(record: &InvoiceRecord) -> bool {
    record
        .invoice_number
        .as_deref()
        .is_some_and(is_valid_invoice_number_candidate)
        || record.seller_tax_id.is_some()
        || record.buyer_tax_id.is_some()
        || record.ksef_reference.is_some()
        || record.gross_amount_minor.is_some()
        || [
            record.source_path.as_deref(),
            record.email_subject.as_deref(),
            record.email_from.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|text| invoice_signal_re().is_match(text))
}

pub(crate) fn record_missing_hard_fields(record: &InvoiceRecord) -> bool {
    record_amounts_inconsistent(record)
        || record
            .invoice_number
            .as_deref()
            .is_none_or(|number| !is_valid_invoice_number_candidate(number))
        || record_number_from_filename(record)
        || record.issue_date.is_none()
        || record.gross_amount_minor.is_none()
}

pub(crate) fn record_needs_paid_llm(record: &InvoiceRecord) -> bool {
    !record_is_password_protected(record)
        && record_has_invoice_signal(record)
        && record_missing_hard_fields(record)
}

pub(crate) fn record_queued_for_llm(
    record: &InvoiceRecord,
    skip_hashes: &HashSet<String>,
    paid: bool,
    force: bool,
) -> bool {
    if skip_hashes.contains(&record.content_hash) || record_is_password_protected(record) {
        return false;
    }
    if !record.source_path.as_ref().is_some_and(|path| {
        Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
    }) {
        return false;
    }
    if paid && !force {
        record_needs_paid_llm(record)
    } else {
        record_missing_core_fields(record) || record_number_from_filename(record)
    }
}

pub(crate) fn json_first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|v| match v {
            Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    })
}

pub(crate) fn json_first_money_minor(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(json_money_minor))
}

// Liczba JSON ma zawsze kropkę dziesiętną; tekst może mieć zapis lokalny.
fn json_money_minor(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => parse_decimal_minor(&n.to_string()),
        Value::String(s) => parse_money_minor(s),
        _ => None,
    }
}

pub(crate) fn parse_xml_invoice(source: SourceKind, text: &str) -> InvoiceRecord {
    let mut record = empty_record(source);
    own_nip::apply_check(&mut record, own_nip::check_text(text));
    record.invoice_number = first_xml_text(
        text,
        &[
            "P_2",
            "NumerFaktury",
            "InvoiceNumber",
            "invoiceNumber",
            "NrFaktury",
            "number",
        ],
    )
    .map(|v| clean_invoice_number(&v));
    record.issue_date = first_xml_text(text, &["P_1", "DataWystawienia", "IssueDate", "issueDate"])
        .and_then(|v| parse_date(&v));
    record.sale_date = first_xml_text(text, &["P_6", "DataSprzedazy", "SaleDate", "saleDate"])
        .and_then(|v| parse_date(&v));
    record.due_date = first_xml_text(text, &["TerminPlatnosci", "DueDate", "PaymentDueDate"])
        .and_then(|v| parse_date(&v));
    record.gross_amount_minor = first_xml_text(
        text,
        &[
            "P_15",
            "KwotaNaleznosciOgolna",
            "GrossAmount",
            "grossAmount",
            "totalGross",
        ],
    )
    .and_then(|v| xml_money_minor(&v));
    record.net_amount_minor = xml_rate_amounts_minor(text, "P_13").or_else(|| {
        first_xml_text(text, &["NetAmount", "netAmount", "totalNet"])
            .and_then(|v| xml_money_minor(&v))
    });
    record.vat_amount_minor = xml_rate_amounts_minor(text, "P_14").or_else(|| {
        first_xml_text(text, &["VatAmount", "vatAmount", "totalVat"])
            .and_then(|v| xml_money_minor(&v))
    });
    record.currency = first_xml_text(text, &["KodWaluty", "Currency", "currency"])
        .and_then(|v| normalize_currency(&v));
    record.ksef_reference = first_xml_text(
        text,
        &["NrKSeF", "KsefNumber", "KSeFNumber", "ReferenceNumber"],
    )
    .map(|v| v.trim().to_string());

    if let Some(block) = first_xml_block(text, &["Podmiot1", "Seller", "Sprzedawca"]) {
        record.seller_tax_id = first_xml_text(&block, &["NIP", "TaxId", "VATID", "VatId"])
            .and_then(|v| normalize_tax_id(&v));
        record.seller_name =
            first_xml_text(&block, &["Nazwa", "Name", "FullName"]).and_then(|v| clean_name(&v));
    }
    if let Some(block) = first_xml_block(text, &["Podmiot2", "Buyer", "Nabywca"]) {
        record.buyer_tax_id = first_xml_text(&block, &["NIP", "TaxId", "VATID", "VatId"])
            .and_then(|v| normalize_tax_id(&v));
        record.buyer_name =
            first_xml_text(&block, &["Nazwa", "Name", "FullName"]).and_then(|v| clean_name(&v));
    }

    if record.seller_tax_id.is_none() || record.buyer_tax_id.is_none() {
        let ids = tax_ids_from_text(text);
        if record.seller_tax_id.is_none() {
            record.seller_tax_id = ids.first().cloned();
        }
        if record.buyer_tax_id.is_none() {
            record.buyer_tax_id = ids.get(1).cloned();
        }
    }

    record
}

// Element kwoty w XML: kropka dziesiętna; zapis lokalny tylko jako zapas.
fn xml_money_minor(value: &str) -> Option<i64> {
    parse_decimal_minor(value).or_else(|| parse_money_minor(value))
}

// Suma pól FA dla wszystkich stawek (P_13_1, P_13_2, P_13_6_1, ...). Warianty
// `W` (np. P_14_1W) to ten sam VAT przeliczony na PLN, więc ich nie dodajemy.
fn xml_rate_amounts_minor(text: &str, prefix: &str) -> Option<i64> {
    let re = Regex::new(&format!(
        r"<(?:[A-Za-z0-9_\-]+:)?({}_[0-9]+(?:_[0-9]+)*)(?:\s[^>]*)?>",
        regex::escape(prefix)
    ))
    .unwrap();
    let mut names: Vec<&str> = Vec::new();
    for caps in re.captures_iter(text) {
        let name = caps.get(1).map_or("", |m| m.as_str());
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let mut total: Option<i64> = None;
    for name in names {
        // first_xml_text zwraca pierwsze trafienie z listy, więc pytamy o każde pole osobno.
        if let Some(amount) = first_xml_text(text, &[name]).and_then(|v| xml_money_minor(&v)) {
            total = Some(total.unwrap_or(0).checked_add(amount)?);
        }
    }
    total
}

fn parse_json_invoice(source: SourceKind, text: &str) -> Result<InvoiceRecord> {
    let value: Value = serde_json::from_str(text)?;
    let mut flat = HashMap::new();
    flatten_json("", &value, &mut flat);
    let raw = |keys: &[&str]| keys.iter().find_map(|k| flat.get(&normalize_key(k)));
    let get = |keys: &[&str]| -> Option<String> {
        raw(keys)
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .filter(|v| !v.trim().is_empty())
    };
    let money = |keys: &[&str]| raw(keys).and_then(json_money_minor);

    let mut record = empty_record(source);
    own_nip::apply_check(&mut record, own_nip::check_text(text));
    record.invoice_number = get(&[
        "invoice_number",
        "invoiceNumber",
        "number",
        "numerFaktury",
        "p_2",
    ])
    .map(|v| clean_invoice_number(&v));
    record.issue_date =
        get(&["issue_date", "issueDate", "dataWystawienia", "p_1"]).and_then(|v| parse_date(&v));
    record.sale_date =
        get(&["sale_date", "saleDate", "dataSprzedazy", "p_6"]).and_then(|v| parse_date(&v));
    record.due_date = get(&["due_date", "dueDate", "paymentDueDate", "terminPlatnosci"])
        .and_then(|v| parse_date(&v));
    record.seller_tax_id = get(&[
        "seller_tax_id",
        "seller.nip",
        "podmiot1.nip",
        "sprzedawca.nip",
    ])
    .and_then(|v| normalize_tax_id(&v));
    record.buyer_tax_id = get(&["buyer_tax_id", "buyer.nip", "podmiot2.nip", "nabywca.nip"])
        .and_then(|v| normalize_tax_id(&v));
    record.seller_name = get(&[
        "seller_name",
        "seller.name",
        "podmiot1.nazwa",
        "sprzedawca.nazwa",
    ])
    .and_then(|v| clean_name(&v));
    record.buyer_name = get(&[
        "buyer_name",
        "buyer.name",
        "podmiot2.nazwa",
        "nabywca.nazwa",
    ])
    .and_then(|v| clean_name(&v));
    record.gross_amount_minor = money(&[
        "gross_amount",
        "grossAmount",
        "totalGross",
        "kwotaBrutto",
        "p_15",
    ]);
    record.net_amount_minor = money(&["net_amount", "netAmount", "totalNet"])
        .or_else(|| json_rate_amounts_minor(&value, "P_13"));
    record.vat_amount_minor = money(&["vat_amount", "vatAmount", "totalVat"])
        .or_else(|| json_rate_amounts_minor(&value, "P_14"));
    record.currency = get(&["currency", "kodWaluty"]).and_then(|v| normalize_currency(&v));
    record.ksef_reference = get(&["ksef_reference", "nrKSeF", "ksefNumber", "referenceNumber"]);
    record.email_message_id = get(&["email_message_id", "messageId", "id"]);
    record.email_subject = get(&["email_subject", "subject"]);
    record.email_from = get(&["email_from", "from"]);

    if record.seller_tax_id.is_none() || record.buyer_tax_id.is_none() {
        let ids = tax_ids_from_text(text);
        if record.seller_tax_id.is_none() {
            record.seller_tax_id = ids.first().cloned();
        }
        if record.buyer_tax_id.is_none() {
            record.buyer_tax_id = ids.get(1).cloned();
        }
    }
    Ok(record)
}

// Jak xml_rate_amounts_minor: suma pól wszystkich stawek (P_13_1, P_13_6_1, ...)
// na dowolnej głębokości, bez wariantów `W` przeliczonych na PLN.
fn json_rate_amounts_minor(value: &Value, prefix: &str) -> Option<i64> {
    fn collect<'a>(value: &'a Value, re: &Regex, out: &mut Vec<(String, &'a Value)>) {
        match value {
            Value::Object(map) => {
                for (key, item) in map {
                    let name = key.to_ascii_uppercase();
                    if re.is_match(key) {
                        if !out.iter().any(|(seen, _)| *seen == name) {
                            out.push((name, item));
                        }
                    } else {
                        collect(item, re, out);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect(item, re, out)),
            _ => {}
        }
    }
    let re = Regex::new(&format!(
        r"(?i)^{}_[0-9]+(?:_[0-9]+)*$",
        regex::escape(prefix)
    ))
    .unwrap();
    let mut fields = Vec::new();
    collect(value, &re, &mut fields);
    let mut total: Option<i64> = None;
    for (_, item) in fields {
        if let Some(amount) = json_money_minor(item) {
            total = Some(total.unwrap_or(0).checked_add(amount)?);
        }
    }
    total
}

fn flatten_json(prefix: &str, value: &Value, out: &mut HashMap<String, Value>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let next = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_json(&next, value, out);
            }
        }
        Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                flatten_json(&format!("{prefix}.{idx}"), item, out);
            }
        }
        Value::Null => {}
        // Typ zostaje: liczba JSON i napis z kwotą parsują się inaczej.
        other => {
            out.insert(normalize_key(prefix), other.clone());
        }
    }
}

pub(crate) fn parse_text_invoice(source: SourceKind, text: &str) -> InvoiceRecord {
    let mut record = empty_record(source);
    own_nip::apply_check(&mut record, own_nip::check_text(text));
    record.invoice_number = invoice_number_from_text(text);
    let order = document_date_order(text);
    let issue_date = labeled_date_from_text(
        text,
        &[
            "data wystawienia",
            "wystawiono",
            "issue date",
            "invoice date",
        ],
        order,
    )
    .or_else(|| date_from_text(text, order));
    let sale_date = labeled_date_from_text(
        text,
        &[
            "data sprzedaży",
            "data sprzedazy",
            "data wykonania usługi",
            "data wykonania uslugi",
            "sale date",
            "service date",
        ],
        order,
    );
    let due_date = labeled_date_from_text(
        text,
        &[
            "data płatności",
            "data platnosci",
            "termin płatności",
            "termin platnosci",
            "due date",
            "date due",
            "payment due",
        ],
        order,
    );
    for (field, date) in [
        ("data wystawienia", &issue_date),
        ("data sprzedaży", &sale_date),
        ("data płatności", &due_date),
    ] {
        if let Some((date, alternative)) = date
            .as_ref()
            .and_then(|date| Some((date, date.unresolved_alternative?)))
        {
            record.warnings.push(format!(
                "{AMBIGUOUS_DATE_WARNING}: {field} {} przyjęta jako {} (dzień przed miesiącem); możliwe {alternative}",
                date.raw, date.date
            ));
        }
    }
    record.issue_date = issue_date.map(|date| date.date);
    record.sale_date = sale_date.map(|date| date.date);
    record.due_date = due_date.map(|date| date.date);
    let (seller_name, buyer_name) = counterparty_names_from_text(text);
    record.seller_name = seller_name;
    record.buyer_name = buyer_name;
    (record.seller_tax_id, record.buyer_tax_id) = counterparty_tax_ids_from_text(text);
    record.gross_amount_minor = amount_from_text(
        text,
        &[
            "łączna kwota brutto",
            "laczna kwota brutto",
            "wartość brutto",
            "wartosc brutto",
            "kwota brutto",
            "razem brutto",
            "total gross",
            "total",
            "brutto",
            "razem do zapłaty",
            "do zapłaty",
            "amount due",
        ],
    );
    record.net_amount_minor = amount_from_text(
        text,
        &[
            "total net",
            "wartość netto",
            "wartosc netto",
            "kwota netto",
            "netto",
            "net",
        ],
    );
    record.vat_amount_minor = amount_from_text(
        text,
        &[
            "total vat",
            "wartość vat",
            "wartosc vat",
            "kwota vat",
            "podatek vat",
            "vat amount",
            "tax amount",
            "podatek",
            "vat",
        ],
    );
    record.currency = currency_from_text(text);
    record.ksef_reference = ksef_reference_from_text(text);
    record.email_message_id = header_value(text, "Message-ID");
    record.email_subject = header_value(text, "Subject");
    record.email_from = header_value(text, "From");
    record
}

fn first_xml_text(text: &str, tags: &[&str]) -> Option<String> {
    for tag in tags {
        let pattern = format!(
            r"(?is)<(?:[A-Za-z0-9_\-]+:)?{}(?:\s[^>]*)?>(.*?)</(?:[A-Za-z0-9_\-]+:)?{}>",
            regex::escape(tag),
            regex::escape(tag)
        );
        if let Ok(re) = Regex::new(&pattern)
            && let Some(caps) = re.captures(text)
        {
            return caps
                .get(1)
                .map(|m| strip_xml(&m.as_str().replace("<![CDATA[", "").replace("]]>", "")));
        }
    }
    None
}

fn first_xml_block(text: &str, tags: &[&str]) -> Option<String> {
    for tag in tags {
        let pattern = format!(
            r"(?is)<(?:[A-Za-z0-9_\-]+:)?{}(?:\s[^>]*)?>(.*?)</(?:[A-Za-z0-9_\-]+:)?{}>",
            regex::escape(tag),
            regex::escape(tag)
        );
        if let Ok(re) = Regex::new(&pattern)
            && let Some(caps) = re.captures(text)
        {
            return caps.get(1).map(|m| m.as_str().to_string());
        }
    }
    None
}

fn strip_xml(value: &str) -> String {
    Regex::new(r"(?is)<[^>]+>")
        .unwrap()
        .replace_all(value, "")
        .trim()
        .to_string()
}

pub(crate) fn parse_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    for fmt in [
        "%Y-%m-%d",
        "%d.%m.%Y",
        "%d-%m-%Y",
        "%Y/%m/%d",
        "%d/%m/%Y",
        "%d %m %Y",
        // Only reached when day-first is impossible (`03/25/2026`).
        "%m/%d/%Y",
        "%m-%d-%Y",
        "%B %-d, %Y",
        "%B %d, %Y",
        "%b %-d, %Y",
        "%b %d, %Y",
    ] {
        if let Ok(date) = NaiveDate::parse_from_str(value, fmt) {
            return Some(date);
        }
    }
    None
}

/// Prefix of the record warning for a slash/dash date that reads as two
/// different days and the document gave no hint which one.
pub(crate) const AMBIGUOUS_DATE_WARNING: &str = "niejednoznaczny zapis daty";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DateOrder {
    DayFirst,
    MonthFirst,
}

/// A date read from document text. `unresolved_alternative` is the month-first
/// reading of an ambiguous date that was kept day-first for lack of evidence.
#[derive(Debug)]
pub(crate) struct TextDate {
    pub(crate) date: NaiveDate,
    pub(crate) raw: String,
    pub(crate) unresolved_alternative: Option<NaiveDate>,
}

/// `(first, second, year)` of a `a/b/yyyy` or `a-b-yyyy` date (one separator kind).
fn slash_or_dash_date_parts(value: &str) -> Option<(u32, u32, i32)> {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^(\d{1,2})/(\d{1,2})/(\d{4})$|^(\d{1,2})-(\d{1,2})-(\d{4})$").unwrap()
    });
    let caps = RE.captures(value)?;
    let part = |idx: usize| {
        caps.get(idx)
            .or_else(|| caps.get(idx + 3))
            .and_then(|m| m.as_str().parse().ok())
    };
    Some((part(1)?, part(2)?, part(3)? as i32))
}

/// Reads a date found in document text. A slash/dash date whose first two
/// numbers are both valid months and differ follows `order`; without it the
/// day-first reading is kept and the month-first one is reported.
pub(crate) fn parse_text_date(value: &str, order: Option<DateOrder>) -> Option<TextDate> {
    let value = value.trim();
    if let Some((first, second, year)) = slash_or_dash_date_parts(value)
        && (1..=12).contains(&first)
        && (1..=12).contains(&second)
        && first != second
    {
        let day_first = NaiveDate::from_ymd_opt(year, second, first)?;
        let month_first = NaiveDate::from_ymd_opt(year, first, second)?;
        let (date, unresolved_alternative) = match order {
            Some(DateOrder::MonthFirst) => (month_first, None),
            Some(DateOrder::DayFirst) => (day_first, None),
            None => (day_first, Some(month_first)),
        };
        return Some(TextDate {
            date,
            raw: value.to_string(),
            unresolved_alternative,
        });
    }
    parse_date(value).map(|date| TextDate {
        date,
        raw: value.to_string(),
        unresolved_alternative: None,
    })
}

/// Day/month order the document itself shows: numeric dates that are valid in
/// only one order (`25/03/2026`, `03/25/2026`), else dates with a month name
/// (`March 4, 2026` vs `4 March 2026` / `4 marca 2026`). Mixed evidence of one
/// kind decides nothing.
pub(crate) fn document_date_order(text: &str) -> Option<DateOrder> {
    static NUMERIC: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"\b(\d{1,2})([./-])(\d{1,2})([./-])(\d{4})\b").unwrap()
    });
    static MONTH_NAME_FIRST: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
            r"(?i)\b(?:january|february|march|april|may|june|july|august|september|october|november|december|jan|feb|mar|apr|jun|jul|aug|sept|sep|oct|nov|dec)\.?\s+\d{1,2}(?:st|nd|rd|th)?,?\s+\d{4}\b",
        )
        .unwrap()
    });
    static DAY_BEFORE_MONTH_NAME: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
            r"(?i)\b\d{1,2}(?:st|nd|rd|th)?\.?\s+(?:january|february|march|april|may|june|july|august|september|october|november|december|jan|feb|mar|apr|jun|jul|aug|sept|sep|oct|nov|dec|stycznia|lutego|marca|kwietnia|maja|czerwca|lipca|sierpnia|września|wrzesnia|października|pazdziernika|listopada|grudnia)\.?,?\s+\d{4}\b",
        )
        .unwrap()
    });
    let decide = |day_first: bool, month_first: bool| match (day_first, month_first) {
        (true, false) => Some(Some(DateOrder::DayFirst)),
        (false, true) => Some(Some(DateOrder::MonthFirst)),
        (true, true) => Some(None),
        (false, false) => None,
    };

    let (mut day_first, mut month_first) = (false, false);
    for caps in NUMERIC.captures_iter(text) {
        let whole = caps.get(0).expect("match");
        // Skip parts of longer slash/dash numbers such as `FV/12/25/2026/A`.
        let before = text[..whole.start()].chars().next_back();
        let after = text[whole.end()..].chars().next();
        if before.is_some_and(|c| matches!(c, '/' | '-' | '.'))
            || after.is_some_and(|c| matches!(c, '/' | '-'))
            || caps[2] != caps[4]
        {
            continue;
        }
        let (Ok(first), Ok(second)) = (caps[1].parse::<u32>(), caps[3].parse::<u32>()) else {
            continue;
        };
        match (first, second) {
            (13..=31, 1..=12) => day_first = true,
            (1..=12, 13..=31) => month_first = true,
            _ => {}
        }
    }
    if let Some(order) = decide(day_first, month_first) {
        return order;
    }
    decide(
        DAY_BEFORE_MONTH_NAME.is_match(text),
        MONTH_NAME_FIRST.is_match(text),
    )
    .flatten()
}

fn date_from_text(text: &str, order: Option<DateOrder>) -> Option<TextDate> {
    let patterns = [
        r"\b\d{4}-\d{2}-\d{2}\b",
        r"\b\d{2}[./-]\d{2}[./-]\d{4}\b",
        r"\b[A-Za-z]{3,9}\s+\d{1,2},\s+\d{4}\b",
    ];
    for pattern in patterns {
        let re = Regex::new(pattern).unwrap();
        for m in re.find_iter(text) {
            if let Some(date) = parse_text_date(m.as_str(), order) {
                return Some(date);
            }
        }
    }
    None
}

fn labeled_date_from_text(
    text: &str,
    labels: &[&str],
    order: Option<DateOrder>,
) -> Option<TextDate> {
    for label in labels {
        let pattern = format!(
            r"(?i){}[^0-9A-Z]{{0,30}}(\d{{4}}-\d{{2}}-\d{{2}}|\d{{2}}[./-]\d{{2}}[./-]\d{{4}}|[A-Z]{{3,9}}\s+\d{{1,2}},\s+\d{{4}})",
            regex::escape(label)
        );
        let re = Regex::new(&pattern).unwrap();
        if let Some(caps) = re.captures(text)
            && let Some(date) = caps.get(1).and_then(|m| parse_text_date(m.as_str(), order))
        {
            return Some(date);
        }
    }
    None
}

#[cfg(test)]
mod parser_names_tests;

#[cfg(test)]
mod parser_amounts_tests;

#[cfg(test)]
mod parser_dates_tests;

fn header_value(text: &str, name: &str) -> Option<String> {
    let pattern = format!(r"(?im)^{}:\s*(.+)$", regex::escape(name));
    Regex::new(&pattern)
        .ok()?
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

pub(crate) const PDF_PASSWORD_WARNING: &str = "PDF chroniony hasłem; pominięto";

pub(crate) fn poppler_reports_password(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("incorrect password")
        || stderr.contains("password required")
        || stderr.contains("needs a password")
}

pub(crate) fn is_pdf_password_warning(warning: &str) -> bool {
    warning.starts_with("PDF chroniony hasłem")
}

pub(crate) fn is_pdf_password_error(err: &anyhow::Error) -> bool {
    is_pdf_password_warning(&err.to_string())
}

pub(crate) fn record_is_password_protected(record: &InvoiceRecord) -> bool {
    record
        .warnings
        .iter()
        .any(|warning| is_pdf_password_warning(warning))
}

pub(crate) fn pdf_is_password_protected(path: &Path) -> bool {
    if let Ok(pdfinfo) = local_tool("pdfinfo") {
        let mut command = Command::new(pdfinfo);
        apply_isolated_env(&mut command);
        if let Ok(output) = command.arg(path).output() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            if poppler_reports_password(&stderr) || poppler_reports_password(&stdout) {
                return true;
            }
            if output.status.success() {
                return false;
            }
        }
    }
    extract_pdf_text(path).is_err_and(|err| is_pdf_password_error(&err))
}

pub(crate) fn extract_pdf_text(path: &Path) -> Result<String> {
    let mut command = Command::new(local_tool("pdftotext")?);
    apply_isolated_env(&mut command);
    let pdftotext = command.arg("-layout").arg(path).arg("-").output();
    match pdftotext {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            if !text.trim().is_empty() {
                return Ok(text);
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if poppler_reports_password(&stderr) {
                return Err(anyhow!(PDF_PASSWORD_WARNING));
            }
            if !stderr.trim().is_empty() {
                eprintln!("pdftotext failed for {}: {}", path.display(), stderr.trim());
            }
        }
        Err(_) => {}
    }

    Err(anyhow!(
        "ekstrakcja PDF nie powiodła się; zainstaluj poppler (`brew install poppler`)"
    ))
}
