use crate::*;

pub(crate) const FIELDS: [&str; 12] = [
    "invoice_number",
    "issue_date",
    "sale_date",
    "due_date",
    "gross_amount",
    "net_amount",
    "vat_amount",
    "currency",
    "seller_tax_id",
    "buyer_tax_id",
    "seller_name",
    "buyer_name",
];

pub(crate) fn counterparty_name_is_placeholder(name: &str) -> bool {
    let normalized = name
        .trim_matches(|c: char| c.is_whitespace() || c == ':' || c == '-')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    matches!(
        normalized.as_str(),
        "" | "nabywca"
            | "sprzedawca"
            | "wystawca"
            | "odbiorca"
            | "kupujący"
            | "kupujacy"
            | "buyer"
            | "seller"
            | "supplier"
            | "customer"
            | "bill to"
            | "details"
            | "supplier details"
            | "customer details"
            | "seller details"
            | "buyer details"
            | "brak identyfikatora"
    )
}

fn validate_payload(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("LLM: wymagany obiekt JSON"))?;
    if object.len() != FIELDS.len() || FIELDS.iter().any(|key| !object.contains_key(*key)) {
        return Err(anyhow!("LLM: wymagane dokładnie 12 pól schematu faktury"));
    }
    for key in FIELDS {
        let value = &object[key];
        if value.is_null() {
            continue;
        }
        let text = value
            .as_str()
            .ok_or_else(|| anyhow!("LLM: pole {key} wymaga tekstu albo null"))?;
        if text.trim().is_empty() {
            return Err(anyhow!("LLM: puste pole {key}; użyj null"));
        }
        match key {
            "issue_date" | "sale_date" | "due_date" => {
                let date = NaiveDate::parse_from_str(text, "%Y-%m-%d")
                    .map_err(|_| anyhow!("LLM: nieprawidłowa data w {key}"))?;
                if date.to_string() != text {
                    return Err(anyhow!("LLM: data w {key} wymaga formatu YYYY-MM-DD"));
                }
            }
            "gross_amount" | "net_amount" | "vat_amount" => {
                let unsigned = text.strip_prefix('-').unwrap_or(text);
                let valid = unsigned.split_once('.').is_some_and(|(whole, fraction)| {
                    !whole.is_empty()
                        && whole.bytes().all(|b| b.is_ascii_digit())
                        && fraction.len() == 2
                        && fraction.bytes().all(|b| b.is_ascii_digit())
                });
                if !valid || text.replace('.', "").parse::<i64>().is_err() {
                    return Err(anyhow!(
                        "LLM: kwota w {key} wymaga kropki i dwóch cyfr dziesiętnych"
                    ));
                }
            }
            "seller_tax_id" | "buyer_tax_id" => {
                if text.len() != 10 || !text.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(anyhow!("LLM: {key} wymaga 10 cyfr polskiego NIP albo null"));
                }
                let digits: Vec<u32> = text.bytes().map(|b| u32::from(b - b'0')).collect();
                let sum: u32 = digits
                    .iter()
                    .zip([6, 5, 7, 2, 3, 4, 5, 6, 7])
                    .map(|(d, w)| d * w)
                    .sum();
                if sum % 11 != digits[9] {
                    return Err(anyhow!("LLM: nieprawidłowa suma kontrolna NIP w {key}"));
                }
            }
            "currency" => {
                if normalize_currency(text).as_deref() != Some(text) {
                    return Err(anyhow!("LLM: nieprawidłowy kod waluty"));
                }
            }
            "seller_name" | "buyer_name" if counterparty_name_is_placeholder(text) => {
                return Err(anyhow!("LLM: {key} zawiera nagłówek zamiast nazwy"));
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn apply_normalized_invoice_json(
    record: &mut InvoiceRecord,
    value: &Value,
) -> Result<bool> {
    let mut normalized = value.clone();
    let mut warnings = Vec::new();
    let decimal_comma = Regex::new(r"^-?[0-9]+,[0-9]{2}$").unwrap();
    let polish = Regex::new(r"(?i)^PL[ 0-9\-]+$").unwrap();
    let foreign = Regex::new(r"^(?:IE|GB|EU|DE|FR|IT|ES|NL|BE|AT|CZ|SK|SE|DK|FI|PT|HU|RO|BG|HR|SI|LT|LV|EE|LU|CY|MT|EL)[A-Z0-9]+$").unwrap();
    if let Some(object) = normalized.as_object_mut() {
        for key in ["gross_amount", "net_amount", "vat_amount"] {
            if let Some(text) = object.get(key).and_then(Value::as_str)
                && decimal_comma.is_match(text)
            {
                object.insert(key.into(), Value::String(text.replace(',', ".")));
                warnings.push(format!("LLM: normalizacja separatora dziesiętnego w {key}"));
            }
        }
        for key in ["seller_tax_id", "buyer_tax_id"] {
            if let Some(text) = object.get(key).and_then(Value::as_str) {
                if polish.is_match(text) {
                    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
                    object.insert(key.into(), Value::String(digits));
                    warnings.push(format!("LLM: usunięto prefiks PL i separatory w {key}"));
                } else if foreign.is_match(text) {
                    object.insert(key.into(), Value::Null);
                    warnings.push(format!(
                        "LLM: pominięto zagraniczny VAT w polu polskiego NIP {key}"
                    ));
                }
            }
        }
    }
    let mut candidate = record.clone();
    apply_validated_invoice_json(&mut candidate, &normalized)?;
    for warning in warnings {
        if !candidate.warnings.contains(&warning) {
            candidate.warnings.push(warning);
        }
    }
    let changed = serde_json::to_string(record)? != serde_json::to_string(&candidate)?;
    *record = candidate;
    Ok(changed)
}

pub(crate) fn apply_validated_invoice_json(
    record: &mut InvoiceRecord,
    value: &Value,
) -> Result<bool> {
    validate_payload(value)?;
    let mut candidate = record.clone();
    apply_extracted_invoice_json(&mut candidate, value);
    if record_missing_core_fields(&candidate) {
        return Err(anyhow!(
            "LLM: po uzupełnieniu nadal brakuje podstawowych danych faktury lub nazwa jest nagłówkiem"
        ));
    }
    if let (Some(net), Some(vat), Some(gross)) = (
        candidate.net_amount_minor,
        candidate.vat_amount_minor,
        candidate.gross_amount_minor,
    ) && net.checked_add(vat) != Some(gross)
    {
        return Err(anyhow!(
            "LLM: kwoty są niespójne; netto + VAT musi być równe brutto"
        ));
    }
    let changed = serde_json::to_string(record)? != serde_json::to_string(&candidate)?;
    *record = candidate;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Value {
        serde_json::json!({
            "invoice_number":"FV/1/2026", "issue_date":"2026-03-27",
            "sale_date":null, "due_date":null, "gross_amount":"22.83",
            "net_amount":"18.56", "vat_amount":"4.27", "currency":"PLN",
            "seller_tax_id":"6762531182", "buyer_tax_id":"5242920020",
            "seller_name":"Elocity sp. z o.o.", "buyer_name":"Productmesh"
        })
    }

    #[test]
    fn normalizes_only_unambiguous_llm_formats() {
        let mut value = payload();
        value["gross_amount"] = serde_json::json!("22,83");
        value["buyer_tax_id"] = serde_json::json!("PL524-292-00-20");
        value["seller_tax_id"] = serde_json::json!("IE4749148U");
        let mut r = empty_record(SourceKind::Mail);
        assert!(apply_normalized_invoice_json(&mut r, &value).unwrap());
        assert_eq!(r.gross_amount_minor, Some(2283));
        assert_eq!(r.buyer_tax_id.as_deref(), Some("5242920020"));
        assert_eq!(r.seller_tax_id, None);
        assert_eq!(r.warnings.len(), 3);
        for (field, bad) in [
            ("gross_amount", "1,234.56"),
            ("gross_amount", "22,830"),
            ("buyer_tax_id", "PL5242920021"),
        ] {
            let mut v = value.clone();
            v[field] = serde_json::json!(bad);
            let before = serde_json::to_string(&r).unwrap();
            assert!(apply_normalized_invoice_json(&mut r, &v).is_err());
            assert_eq!(serde_json::to_string(&r).unwrap(), before);
        }
    }

    #[test]
    fn incomplete_vat_cannot_hide_impossible_net_amount() {
        let mut v = payload();
        v["net_amount"] = serde_json::json!("9567.00");
        v["vat_amount"] = Value::Null;
        assert!(apply_validated_invoice_json(&mut empty_record(SourceKind::Mail), &v).is_err());
    }

    #[test]
    fn replaces_headers_but_preserves_real_names() {
        let mut r = empty_record(SourceKind::Mail);
        r.seller_name = Some("Nabywca".into());
        r.buyer_name = Some("DETAILS".into());
        assert!(apply_validated_invoice_json(&mut r, &payload()).unwrap());
        assert_eq!(r.seller_name.as_deref(), Some("Elocity sp. z o.o."));
        assert_eq!(r.buyer_name.as_deref(), Some("Productmesh"));
        let mut different = payload();
        different["seller_name"] = Value::String("Inna firma".into());
        assert!(!apply_validated_invoice_json(&mut r, &different).unwrap());
    }

    #[test]
    fn rejects_bad_fields_atomically() {
        for (field, bad) in [
            ("seller_name", "Nabywca:"),
            ("buyer_name", "DETAILS"),
            ("gross_amount", "25,55"),
            ("gross_amount", "23.00"),
            ("issue_date", "2026-02-30"),
            ("seller_tax_id", "IE4749148U"),
            ("buyer_tax_id", "5242920021"),
            ("currency", "zł"),
        ] {
            let mut r = empty_record(SourceKind::Mail);
            r.seller_name = Some("Nabywca".into());
            let before = serde_json::to_string(&r).unwrap();
            let mut v = payload();
            v[field] = Value::String(bad.into());
            assert!(apply_validated_invoice_json(&mut r, &v).is_err(), "{field}");
            assert_eq!(serde_json::to_string(&r).unwrap(), before);
        }
    }

    #[test]
    fn rejects_missing_fields_wrong_types_and_incomplete_records() {
        let mut values = vec![serde_json::json!([])];
        let mut v = payload();
        v.as_object_mut().unwrap().remove("invoice_number");
        values.push(v);
        let mut v = payload();
        v["gross_amount"] = serde_json::json!(22.83);
        values.push(v);
        let mut v = payload();
        v["invoice_number"] = Value::Null;
        values.push(v);
        let mut v = payload();
        v["extra"] = Value::Null;
        values.push(v);
        for v in values {
            assert!(apply_validated_invoice_json(&mut empty_record(SourceKind::Mail), &v).is_err());
        }
    }

    #[test]
    fn partial_reply_can_fill_names_when_core_fields_already_exist() {
        let mut r = empty_record(SourceKind::Mail);
        apply_validated_invoice_json(&mut r, &payload()).unwrap();
        r.seller_name = Some("Nabywca".into());
        let mut v = payload();
        for key in FIELDS {
            if key != "seller_name" {
                v[key] = Value::Null;
            }
        }
        assert!(record_missing_core_fields(&r));
        assert!(apply_validated_invoice_json(&mut r, &v).unwrap());
        assert_eq!(r.gross_amount_minor, Some(2283));
        assert!(!record_missing_core_fields(&r));
    }

    #[test]
    fn parser_rejects_headings_not_company_names() {
        for name in [
            "Nabywca",
            " NABYWCA: ",
            "DETAILS",
            "Supplier details",
            "Brak identyfikatora",
        ] {
            assert!(counterparty_name_is_placeholder(name));
            assert!(!is_probable_name_line(name));
        }
        assert!(!counterparty_name_is_placeholder("Details Sp. z o.o."));
        assert!(!counterparty_name_is_placeholder("Elocity sp. z o.o."));
    }
}
