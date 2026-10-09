use crate::*;

pub(crate) fn invoice_number_from_text(text: &str) -> Option<String> {
    let patterns = [
        r"(?is)numer\s+faktury[\s:#\-]*([0-9A-Z][A-Z0-9/_.\-]{2,})",
        r"(?im)^\s*invoice[ \t]*(?:no\.?|number)[ \t:#\-]*([A-Z0-9][A-Z0-9/_.\-]{2,})\s*$",
        r"(?i)(?:obraz\s+)?faktur(?:a|y)?\s*(?:vat)?\s*(?:korygując[aey]|korygujac[aey])?\s*(?:nr|numer)?[\s:#\-\n]*([A-Z0-9][A-Z0-9/_.\-]{2,})",
        r"(?i)(?:nr\s*faktury|invoice\s*(?:no\.?|number)?)[\s:#\-\n]*([A-Z0-9][A-Z0-9/_.\-]{2,})",
        r"(?i)\b(?:credit\s+note|nota\s+korygując[aey]|nota\s+korygujac[aey]|korekta)[ \t]*(?:no\.?|number|nr|numer)?[\s:#\-]*([A-Z0-9][A-Z0-9/_.\-]{2,})",
        r"(?i)\bFV[\s:#\-]*([A-Z0-9][A-Z0-9/_.\-]{2,})",
    ];
    // Korekta podaje też numer faktury korygowanej; ten bierzemy tylko, gdy innego brak.
    let mut reference = None;
    for pattern in patterns {
        let re = Regex::new(pattern).unwrap();
        for caps in re.captures_iter(text) {
            if let Some(value) = caps.get(1) {
                let cleaned = clean_invoice_number(value.as_str());
                if !is_valid_invoice_number_candidate(&cleaned) {
                    continue;
                }
                if !invoice_number_is_reference(text, value.start()) {
                    return Some(cleaned);
                }
                reference.get_or_insert(cleaned);
            }
        }
    }
    reference
}

// Numer poprzedzony frazą odsyłającą ("do faktury nr", "original invoice")
// wskazuje inny dokument niż ten, który czytamy.
fn invoice_number_is_reference(text: &str, start: usize) -> bool {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
            r"(?i)(?:\bdo\s+faktury|\bdotyczy(?:\s+faktury)?|\bdot\.\s*faktury|\bkorekta\s+do|\bfaktur[ay]\s+korygowan(?:a|ej)|\bkorygowan(?:a|ej)\s+faktur[ay]|\bfaktur[ay]\s+(?:pierwotn|oryginaln)(?:a|ej)|\b(?:pierwotn|oryginaln)(?:a|ej)\s+faktur[ay]|\bcorrection\s+(?:of|to)\s+invoice|\bcredit\s+note\s+(?:for|to)\s+invoice|\b(?:original|corrected|related)\s+invoice|\b(?:refers|relates)\s+to\s+invoice)(?:[\s:#.\-]+|\b(?:nr|numer|no|number|vat|faktury|faktura|invoice|korygującej|korygujacej)\b)*$",
        )
        .unwrap()
    });
    // Wystarczy krótki kontekst; fraza musi stać tuż przed numerem.
    let window_start = text[..start]
        .char_indices()
        .rev()
        .nth(80)
        .map_or(0, |(idx, _)| idx);
    RE.is_match(&text[window_start..start])
}

pub(crate) fn is_valid_invoice_number_candidate(value: &str) -> bool {
    let value = value.trim();
    if value.chars().count() < 3 || !value.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    !matches!(
        value,
        "ZOSTA"
            | "ZOSTAŁA"
            | "VAT"
            | "FOR"
            | "INVOICE"
            | "NUMBER"
            | "DATE"
            | "DUE"
            | "FAKTURY"
            | "FAKTURA"
            | "NUMER"
            | "PODSTAWOWA"
            | "SYSTEM"
            | "KSEF"
    )
}

pub(crate) const FILENAME_NUMBER_WARNING: &str = "numer faktury odczytany z nazwy pliku";

pub(crate) fn record_number_from_filename(record: &InvoiceRecord) -> bool {
    record
        .warnings
        .iter()
        .any(|warning| warning == FILENAME_NUMBER_WARNING)
}

pub(crate) fn invoice_number_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    // Załączniki z Gmaila mają nazwę `{id wiadomości}_{n}_{oryginał}`; id to nie numer faktury.
    let stem = Regex::new(r"^[0-9A-Fa-f]{12,}_[0-9]+_")
        .unwrap()
        .find(stem)
        .map_or(stem, |prefix| &stem[prefix.end()..]);
    for pattern in [
        r"(?i)Invoice-([A-Z0-9\-]+)",
        r"(?i)Faktura[_\-]([A-Z0-9/\-]+)",
        r"(?i)RYANAIR[_\-]([0-9\-]+[_\-]IE)",
        r"(?i)(?:fv|faktura|invoice)?[_\-\s]*([A-Z0-9]{1,8}[/_\-][A-Z0-9/_\-]{2,})",
    ] {
        let re = Regex::new(pattern).unwrap();
        if let Some(value) = re.captures(stem).and_then(|c| c.get(1)) {
            let cleaned = clean_invoice_number(value.as_str());
            if !matches!(cleaned.as_str(), "INVOICE" | "FAKTURA") {
                return Some(cleaned);
            }
        }
    }
    None
}

pub(crate) fn clean_invoice_number(value: &str) -> String {
    value
        .trim()
        .trim_matches(|c: char| c == ':' || c == '#' || c == '.' || c == ',')
        .replace('_', "/")
        .to_ascii_uppercase()
}

pub(crate) fn ksef_reference_from_text(text: &str) -> Option<String> {
    let patterns = [
        r"(?i)(?:Nr\s*KSeF|Numer\s+w\s*KSeF|KSeF)[\s:#\-]*([0-9]{10}-[0-9]{8}-[A-Z0-9]{10,}-[A-Z0-9]{2})",
        r"\b([0-9]{10}-[0-9]{8}-[A-Z0-9]{10,}-[A-Z0-9]{2})\b",
    ];
    for pattern in patterns {
        let re = Regex::new(pattern).unwrap();
        if let Some(value) = re.captures(text).and_then(|c| c.get(1)) {
            return Some(value.as_str().to_string());
        }
    }
    None
}
