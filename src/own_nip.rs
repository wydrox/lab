use crate::*;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OwnNipCheck {
    Found,
    NotFound,
    Unreadable,
    NotChecked,
}

const FOUND_MARKER: &str = "lab-own-nip:found";
const NOT_CHECKED_MARKER: &str = "lab-own-nip:not_checked";
pub(crate) const NOT_FOUND_WARNING: &str = "Nie znaleziono NIP 5242920020 w odczytanym tekście dokumentu; sprawdź NIP ręcznie przed wysłaniem do Saldeo";
pub(crate) const UNREADABLE_WARNING: &str = "Nie można sprawdzić NIP 5242920020: tekst dokumentu jest niedostępny; sprawdź NIP ręcznie przed wysłaniem do Saldeo";

pub(crate) fn is_check_warning(warning: &str) -> bool {
    matches!(
        warning,
        FOUND_MARKER | NOT_CHECKED_MARKER | NOT_FOUND_WARNING | UNREADABLE_WARNING
    )
}

pub(crate) fn is_internal_marker(warning: &str) -> bool {
    matches!(warning, FOUND_MARKER | NOT_CHECKED_MARKER)
}

pub(crate) fn record_check(record: &InvoiceRecord) -> OwnNipCheck {
    record
        .warnings
        .iter()
        .rev()
        .find_map(|warning| match warning.as_str() {
            FOUND_MARKER => Some(OwnNipCheck::Found),
            NOT_FOUND_WARNING => Some(OwnNipCheck::NotFound),
            UNREADABLE_WARNING => Some(OwnNipCheck::Unreadable),
            NOT_CHECKED_MARKER => Some(OwnNipCheck::NotChecked),
            _ => None,
        })
        .unwrap_or(OwnNipCheck::NotChecked)
}

pub(crate) fn apply_check(record: &mut InvoiceRecord, check: OwnNipCheck) {
    record.warnings.retain(|warning| !is_check_warning(warning));
    record.warnings.push(
        match check {
            OwnNipCheck::Found => FOUND_MARKER,
            OwnNipCheck::NotFound => NOT_FOUND_WARNING,
            OwnNipCheck::Unreadable => UNREADABLE_WARNING,
            OwnNipCheck::NotChecked => NOT_CHECKED_MARKER,
        }
        .to_string(),
    );
}

/// Check the document text, without guesses from names, file names, or LLM fields.
pub(crate) fn check_text(text: &str) -> OwnNipCheck {
    if text.trim().is_empty() {
        return OwnNipCheck::Unreadable;
    }
    let normalized: String = text
        .chars()
        .map(|c| {
            if c.is_whitespace() {
                ' '
            } else if matches!(
                c,
                '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{fe63}' | '\u{ff0d}'
            ) {
                '-'
            } else {
                c
            }
        })
        .collect();
    // The first component of a KSeF reference is a NIP, but is not a party's
    // separately stated NIP. Remove all references before the presence check.
    static KSEF: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?i)\b[0-9]{10} *- *[0-9]{8} *- *[A-Z0-9]{10,} *- *[A-Z0-9]{2}\b").unwrap()
    });
    let text = KSEF.replace_all(&normalized, " ");
    static NIP: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        let digits = DEFAULT_PRODUCTMESH_NIP
            .chars()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join("[ .-]*");
        Regex::new(&format!(r"(?i)(?:NIP[ :]*(?:PL *)?|PL *)?{digits}")).unwrap()
    });
    for hit in NIP.find_iter(&text) {
        let before = &text[..hit.start()];
        let after = &text[hit.end()..];
        let identifier_char = |c: char| c.is_alphanumeric() || c == '_';
        if before.chars().next_back().is_some_and(identifier_char)
            || after.chars().next().is_some_and(identifier_char)
        {
            continue;
        }
        // Dot/dash groups must not allow a match inside a longer identifier.
        // Spaces alone can separate a NIP from an adjacent numeric table column.
        let attached_digits = |part: &str, reverse: bool| {
            let chars: Vec<char> = if reverse {
                part.chars().rev().collect()
            } else {
                part.chars().collect()
            };
            let separators = chars
                .iter()
                .take_while(|c| matches!(c, ' ' | '.' | '-'))
                .count();
            separators > 0
                && chars[..separators].iter().any(|c| matches!(c, '.' | '-'))
                && chars.get(separators).is_some_and(char::is_ascii_digit)
        };
        if attached_digits(before, true) || attached_digits(after, false) {
            continue;
        }
        return OwnNipCheck::Found;
    }
    OwnNipCheck::NotFound
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_document_nip_formats() {
        for text in [
            "5242920020",
            "NIP: 524-292-00-20",
            "nip: pl5242920020",
            "PL 524 292 00 20",
            "NIP: 524.292.00.20",
            "NIP: 524\u{a0}292\u{202f}00\u{a0}20",
            "NIP: 524\u{2011}292\u{2013}00\u{2212}20",
            "NIP: 5242920020    18.56 PLN",
            "Nr KSeF: 5242920020-20260904-7B4DFEC00001-B8\nNIP: 5242920020",
        ] {
            assert_eq!(check_text(text), OwnNipCheck::Found, "{text}");
        }
    }

    #[test]
    fn rejects_other_ids_references_and_ocr_guesses() {
        for text in [
            "NIP: 5242920021",
            "15242920020",
            "52429200201",
            "PL52429200201",
            "abc5242920020xyz",
            "1-524-292-00-20",
            "524-292-00-20-1",
            "524.292.00.20.1",
            "NIP: 524292OO20",
            "Nr KSeF: 5242920020-20260904-7B4DFEC00001-B8",
            "5242920020\u{2011}20260904\u{2011}7B4DFEC00001\u{2011}B8",
            "Productmesh",
        ] {
            assert_eq!(check_text(text), OwnNipCheck::NotFound, "{text}");
        }
        assert_eq!(check_text(" \n\t\u{a0}"), OwnNipCheck::Unreadable);
    }

    #[test]
    fn stores_one_current_check_and_preserves_other_warnings() {
        let mut record = empty_record(SourceKind::Mail);
        record.buyer_tax_id = Some(DEFAULT_PRODUCTMESH_NIP.into());
        record.warnings.push("unrelated warning".into());
        assert_eq!(record_check(&record), OwnNipCheck::NotChecked);
        for check in [
            OwnNipCheck::NotFound,
            OwnNipCheck::Unreadable,
            OwnNipCheck::Found,
            OwnNipCheck::NotChecked,
        ] {
            apply_check(&mut record, check);
            apply_check(&mut record, check);
            assert_eq!(record_check(&record), check);
            assert_eq!(record.warnings.len(), 2);
            assert_eq!(record.warnings[0], "unrelated warning");
            assert_eq!(
                is_internal_marker(&record.warnings[1]),
                matches!(check, OwnNipCheck::Found | OwnNipCheck::NotChecked)
            );
        }
    }

    #[test]
    fn serializes_status_as_snake_case() {
        for (check, expected) in [
            (OwnNipCheck::Found, "found"),
            (OwnNipCheck::NotFound, "not_found"),
            (OwnNipCheck::Unreadable, "unreadable"),
            (OwnNipCheck::NotChecked, "not_checked"),
        ] {
            assert_eq!(serde_json::to_value(check).unwrap(), expected);
        }
    }
}
