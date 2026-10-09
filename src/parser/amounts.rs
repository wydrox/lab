use crate::*;

// Format maszynowy (liczba JSON, kwota w XML): kropka zawsze dziesiętna,
// dowolna liczba miejsc po przecinku, zaokrąglenie do groszy (połówki od zera).
pub(crate) fn parse_decimal_minor(value: &str) -> Option<i64> {
    let value = value.trim();
    let (negative, unsigned) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().ok()?),
        None => (unsigned, 0),
    };
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if (int_part.is_empty() && frac_part.is_empty())
        || !int_part
            .bytes()
            .chain(frac_part.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let digits = format!("{int_part}{frac_part}");
    // Liczba cyfr przed przecinkiem po przeliczeniu na grosze.
    let point = int_part.len() as i64 + i64::from(exponent) + 2;
    if point < 0 {
        return Some(0);
    }
    let point = point.min(64) as usize;
    let mut kept: String = digits.chars().take(point).collect();
    kept.push_str(&"0".repeat(point.saturating_sub(digits.len())));
    let round_up = digits.as_bytes().get(point).is_some_and(|d| *d >= b'5');
    let kept = kept.trim_start_matches('0');
    if kept.len() > 20 {
        return None;
    }
    let mut minor: i128 = if kept.is_empty() {
        0
    } else {
        kept.parse().ok()?
    };
    if round_up {
        minor += 1;
    }
    i64::try_from(if negative { -minor } else { minor }).ok()
}

// Kwota z tekstu: `1.234,56`, `1,234.56`, `1 234,56`, `1234,5`, `0.5`.
// Pojedynczy separator z dokładnie trzema cyframi (`1.234`, `1,234`) to separator tysięcy.
pub(crate) fn parse_money_minor(value: &str) -> Option<i64> {
    let s: String = value
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == ',' || *c == '.' || *c == '-')
        .collect();
    let (negative, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.as_str()),
    };
    if body.contains('-') || !body.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    let decimal = match (body.rfind(','), body.rfind('.')) {
        (Some(comma), Some(dot)) => {
            let decimal = if comma > dot { ',' } else { '.' };
            if body.matches(decimal).count() != 1 {
                return None;
            }
            Some(decimal)
        }
        (Some(_), None) => single_kind_decimal_separator(body, ',')?,
        (None, Some(_)) => single_kind_decimal_separator(body, '.')?,
        (None, None) => None,
    };
    let normalized = match decimal.and_then(|sep| body.rsplit_once(sep)) {
        Some((int_part, frac_part)) => {
            let int_part: String = int_part.chars().filter(char::is_ascii_digit).collect();
            format!("{int_part}.{frac_part}")
        }
        None => body.chars().filter(char::is_ascii_digit).collect(),
    };
    let sign = if negative { "-" } else { "" };
    parse_decimal_minor(&format!("{sign}{normalized}"))
}

// Tylko jeden rodzaj separatora: Some(Some(sep)) dziesiętny, Some(None) same
// grupy tysięcy, None gdy zapis nie jest poprawną kwotą (np. `1.2.3`).
fn single_kind_decimal_separator(body: &str, sep: char) -> Option<Option<char>> {
    let groups: Vec<&str> = body.split(sep).collect();
    let thousands = groups[0].len() <= 3
        && !groups[0].is_empty()
        && !groups[0].starts_with('0')
        && groups[1..].iter().all(|group| group.len() == 3);
    match groups.len() {
        2 if !thousands => Some(Some(sep)),
        _ if thousands => Some(None),
        _ => None,
    }
}

pub(crate) fn amount_from_text(text: &str, labels: &[&str]) -> Option<i64> {
    // Separatory grup wymagają pełnej części dziesiętnej. Nie łączymy kolumn ani linii.
    let decimal = r"-?(?:[0-9]{1,3}(?:[ \u{00a0}\u{202f}][0-9]{3})+[.,][0-9]{2}|[0-9]{1,3}(?:\.[0-9]{3})+,[0-9]{2}|[0-9]{1,3}(?:,[0-9]{3})+\.[0-9]{2}|[0-9]+[.,][0-9]{2})";
    let number = format!(r"(?:{decimal}|-?[0-9]+)");
    let currency = r"(?:PLN|EUR|USD|GBP|CHF|CZK|SEK|NOK|DKK|zł|€|\$|£)";
    let unit = format!(r"(?:\({currency}\)|{currency})");
    let h = r"[\t \u{00a0}\u{202f}]*";
    let value_re = Regex::new(&format!(
        r"(?i)^{h}(?:(?P<lead>{unit}){h})?(?P<value>{number})(?:{h}(?P<trail>{currency}))?{h}(?:$|[ \t]+(?:NET|VAT|TOTAL)\b)"
    ))
    .unwrap();
    // "Net 30" / "Payment terms: net 14" to termin płatności, nie kwota.
    let payment_terms_re =
        Regex::new(r"(?i)(?:\bterms|płatności|platnosci)[ \t]*[:\-]?[ \t]*$").unwrap();
    let parse_value = |value: &str| {
        // Limit chroni również starszy parse_money_minor przed przepełnieniem.
        if value.bytes().filter(u8::is_ascii_digit).count() > 16 {
            return None;
        }
        parse_money_minor(value)
    };

    // Układ kolumn musi wynikać z nagłówka, nigdy z arytmetyki kwot.
    // OCR może umieścić wartości w nagłówku, Poppler zachowuje osobny wiersz.
    let header_re = Regex::new(&format!(
        r"(?i)\bNET\b{h}(?:{unit}{h})?(?:{decimal}{h})?VAT\b{h}(?:{unit}{h})?(?:{decimal}{h})?TOTAL\b"
    )).unwrap();
    let row_re = Regex::new(&format!(
        r"(?im)^[ \t]*TOTAL\b[ \t]*:?[ \t\r\n]+({decimal})[ \t\r\n]+({decimal})[ \t\r\n]+({decimal})[ \t]*\r?$"
    )).unwrap();
    let table = header_re.find_iter(text).find_map(|header| {
        row_re.captures(&text[header.end()..]).map(|caps| {
            [1, 2, 3].map(|column| caps.get(column).and_then(|m| parse_value(m.as_str())))
        })
    });

    for label in labels {
        let column = match *label {
            "net" => Some(0),
            "vat" => Some(1),
            "total" => Some(2),
            _ => None,
        };
        if let Some(value) = column.and_then(|column| table.and_then(|row| row[column])) {
            return Some(value);
        }
        let label_re = Regex::new(&format!(
            r"(?i)\b{}\b{h}(?P<unit>{unit}{h})?[:=]?{h}",
            regex::escape(label)
        ))
        .unwrap();
        // Samo "net"/"netto" bywa częścią warunków płatności ("Net 30");
        // wymaga kwoty z groszami albo waluty.
        let bare_net = matches!(*label, "net" | "netto");
        let lines: Vec<_> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            for label_caps in label_re.captures_iter(line) {
                let label_match = label_caps.get(0).unwrap();
                if bare_net && payment_terms_re.is_match(&line[..label_match.start()]) {
                    continue;
                }
                let tail = &line[label_match.end()..];
                let candidate = if tail.is_empty() && line[..label_match.start()].trim().is_empty()
                {
                    // Tylko bezpośrednio następna linia; bez przeskakiwania nagłówków.
                    lines.get(index + 1).copied().unwrap_or_default()
                } else {
                    tail
                };
                let Some(caps) = value_re.captures(candidate) else {
                    continue;
                };
                let Some(raw) = caps.name("value") else {
                    continue;
                };
                let amount_like = raw.as_str().contains(['.', ','])
                    || label_caps.name("unit").is_some()
                    || caps.name("lead").is_some()
                    || caps.name("trail").is_some();
                if bare_net && !amount_like {
                    continue;
                }
                if let Some(value) = parse_value(raw.as_str()) {
                    return Some(value);
                }
            }
        }
    }
    None
}

pub(crate) fn normalize_currency(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let code_re = Regex::new(r"(?i)\b(PLN|EUR|USD|GBP|CHF|CZK|SEK|NOK|DKK)\b").unwrap();
    if let Some(code) = code_re.captures(value).and_then(|caps| caps.get(1)) {
        return Some(code.as_str().to_ascii_uppercase());
    }

    let lower = value.to_lowercase();
    if lower.contains('€') || Regex::new(r"(?i)\beuro\b").unwrap().is_match(value) {
        return Some("EUR".to_string());
    }
    if lower.contains("zł")
        || Regex::new(r"(?i)\bzl\b|\bzlot(?:y|ych|e)?\b|\bzłot(?:y|ych|e)?\b")
            .unwrap()
            .is_match(value)
    {
        return Some("PLN".to_string());
    }
    if value.contains('$')
        || Regex::new(r"(?i)\bdol(?:ar|lar|lars?)\b")
            .unwrap()
            .is_match(value)
    {
        return Some("USD".to_string());
    }
    if value.contains('£')
        || Regex::new(r"(?i)\b(?:gbp|pound|funt)\b")
            .unwrap()
            .is_match(value)
    {
        return Some("GBP".to_string());
    }
    if Regex::new(r"(?i)\bfrank(?:a|ów)?\b")
        .unwrap()
        .is_match(value)
    {
        return Some("CHF".to_string());
    }

    None
}

pub(crate) fn currency_from_text(text: &str) -> Option<String> {
    let labels = [
        "waluta",
        "currency",
        "currency code",
        "kod waluty",
        "kwota",
        "razem",
        "total",
        "amount",
        "do zapłaty",
        "do zaplaty",
    ];
    for label in labels {
        let pattern = format!(r"(?i){}[^\n]{{0,80}}", regex::escape(label));
        let re = Regex::new(&pattern).unwrap();
        for value in re.find_iter(text) {
            if let Some(currency) = normalize_currency(value.as_str()) {
                return Some(currency);
            }
        }
    }
    normalize_currency(text)
}
