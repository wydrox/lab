use crate::*;

pub(crate) fn tax_ids_from_text(text: &str) -> Vec<String> {
    tax_id_hits(text).into_iter().map(|hit| hit.0).collect()
}

// NIP-y w kolejności wystąpienia: (NIP, bajt początku dopasowania).
fn tax_id_hits(text: &str) -> Vec<(String, usize)> {
    let patterns = [
        r"(?i)\b(?:NIP[ \t:]*|PL[ \t]+VAT[ \t:]*)(?:PL[ \t]*)?([0-9][0-9\- \t]{8,20}[0-9])\b",
        r"(?i)\bPL[ \t]*([0-9]{10})\b",
    ];
    let mut hits: Vec<(String, usize)> = Vec::new();
    for pattern in patterns {
        let re = Regex::new(pattern).unwrap();
        for caps in re.captures_iter(text) {
            let (Some(whole), Some(id)) = (
                caps.get(0),
                caps.get(1).and_then(|m| normalize_tax_id(m.as_str())),
            ) else {
                continue;
            };
            match hits.iter_mut().find(|(seen, _)| *seen == id) {
                Some(existing) => existing.1 = existing.1.min(whole.start()),
                None => hits.push((id, whole.start())),
            }
        }
    }
    hits.sort_by_key(|(_, start)| *start);
    hits
}

pub(crate) fn nip_checksum_valid(nip: &str) -> bool {
    let digits: Vec<u32> = nip.chars().filter_map(|c| c.to_digit(10)).collect();
    if digits.len() != 10 || nip.chars().count() != 10 {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .zip([6, 5, 7, 2, 3, 4, 5, 6, 7])
        .map(|(d, w)| d * w)
        .sum();
    sum % 11 == digits[9]
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PartyRole {
    Seller,
    Buyer,
}

struct SectionLabel {
    pub(crate) start: usize,
    pub(crate) column: usize,
    pub(crate) role: PartyRole,
    // Przed etykietą w tej linii stoi tekst (np. "Anthropic, PBC    Bill to").
    pub(crate) right_column: bool,
}

// Etykiety sekcji ("Sprzedawca:", "SUPPLIER DETAILS", "Bill to") w jednej linii.
// Słowo musi kończyć linię, kolumnę albo mieć dwukropek, żeby "Customer number"
// czy "customer support" nie otwierały sekcji.
fn party_section_labels(line: &str) -> Vec<SectionLabel> {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?i)\b(sprzedawca|wystawca|seller|supplier|nabywca|odbiorca|kupujący|kupujacy|buyer|customer|bill to)\b(?:[ \t]+details\b)?[ \t]*(?::|$|[ \t]{2,}|\t)").unwrap()
    });
    RE.captures_iter(line)
        .filter_map(|caps| {
            let label = caps.get(1)?;
            let role = match label.as_str().to_lowercase().as_str() {
                "sprzedawca" | "wystawca" | "seller" | "supplier" => PartyRole::Seller,
                _ => PartyRole::Buyer,
            };
            let before = &line[..label.start()];
            Some(SectionLabel {
                start: label.start(),
                column: display_column(before),
                role,
                right_column: !before.trim().is_empty(),
            })
        })
        .collect()
}

fn display_column(prefix: &str) -> usize {
    prefix.chars().fold(0, |column, c| {
        if c == '\t' {
            (column / 8 + 1) * 8
        } else {
            column + 1
        }
    })
}

// Początki kolumn układu: fragmenty linii rozdzielone tabulatorem albo 2+ spacjami.
fn layout_segment_starts(line: &str) -> Vec<usize> {
    static RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"[^ \t](?:[^ \t]| [^ \t])*").unwrap());
    RE.find_iter(line).map(|m| m.start()).collect()
}

fn layout_segment_index(starts: &[usize], byte: usize) -> usize {
    starts
        .partition_point(|start| *start <= byte)
        .saturating_sub(1)
}

// Rola NIP-u według najbliższej poprzedzającej etykiety sekcji. Gdy etykiety
// stoją obok siebie w kolumnach, decyduje kolumna NIP-u.
fn tax_id_role(lines: &[&str], line_idx: usize, byte: usize) -> Option<PartyRole> {
    let line = lines[line_idx];
    if let Some(label) = party_section_labels(line)
        .into_iter()
        .filter(|label| label.start < byte)
        .max_by_key(|label| label.start)
    {
        return Some(label.role);
    }
    let (header, labels) = (0..line_idx).rev().find_map(|idx| {
        let labels = party_section_labels(lines[idx]);
        (!labels.is_empty()).then_some((lines[idx], labels))
    })?;
    let column = display_column(&line[..byte]);
    if let [label] = labels.as_slice() {
        // Jedna etykieta w prawej kolumnie ("Bill to") nie obejmuje lewej kolumny.
        let in_left_column = column.saturating_add(4) < label.column;
        return (!(label.right_column && in_left_column)).then_some(label.role);
    }
    // Ta sama liczba kolumn w nagłówku i w linii NIP-u: porównujemy numer kolumny
    // (odporne na tabulatory); w innym razie pozycję znaku.
    let (header_starts, line_starts) = (layout_segment_starts(header), layout_segment_starts(line));
    if header_starts.len() == line_starts.len() {
        let segment = layout_segment_index(&line_starts, byte);
        if let Some(label) = labels
            .iter()
            .filter(|label| layout_segment_index(&header_starts, label.start) <= segment)
            .max_by_key(|label| label.start)
        {
            return Some(label.role);
        }
    }
    labels
        .iter()
        .filter(|label| label.column <= column.saturating_add(2))
        .max_by_key(|label| label.column)
        .or_else(|| labels.iter().min_by_key(|label| label.column))
        .map(|label| label.role)
}

// (NIP sprzedawcy, NIP nabywcy) z tekstu faktury.
pub(crate) fn counterparty_tax_ids_from_text(text: &str) -> (Option<String>, Option<String>) {
    let hits = tax_id_hits(text);
    let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let mut line_starts = Vec::with_capacity(lines.len());
    let mut offset = 0usize;
    for raw in text.split('\n') {
        line_starts.push(offset);
        offset += raw.len() + 1;
    }
    let mut sellers = Vec::new();
    let mut buyers = Vec::new();
    let mut unassigned = Vec::new();
    for (id, start) in &hits {
        let line_idx = line_starts.partition_point(|s| *s <= *start) - 1;
        match tax_id_role(&lines, line_idx, start - line_starts[line_idx]) {
            Some(PartyRole::Seller) => sellers.push(id.clone()),
            Some(PartyRole::Buyer) => buyers.push(id.clone()),
            None => unassigned.push(id.clone()),
        }
    }
    if sellers.is_empty() && buyers.is_empty() {
        let ids: Vec<String> = hits.into_iter().map(|(id, _)| id).collect();
        return if ids.len() == 1 && sole_tax_id_looks_like_buyer(text) {
            (None, ids.first().cloned())
        } else {
            (ids.first().cloned(), ids.get(1).cloned())
        };
    }
    // Kilka NIP-ów w jednej sekcji: wygrywa pierwszy z poprawną sumą kontrolną.
    let pick = |ids: &[String]| {
        ids.iter()
            .find(|id| nip_checksum_valid(id))
            .or_else(|| ids.first())
            .cloned()
    };
    let mut seller = pick(&sellers);
    let mut buyer = pick(&buyers).filter(|id| seller.as_ref() != Some(id));
    let mut spare = unassigned.into_iter();
    if seller.is_none() {
        seller = spare.next();
    }
    if buyer.is_none() {
        buyer = spare.next();
    }
    // Bez etykiety drugiej strony nadmiarowy NIP z jednej sekcji należy do niej.
    let has_label = |role: PartyRole| {
        lines.iter().any(|line| {
            party_section_labels(line)
                .iter()
                .any(|label| label.role == role)
        })
    };
    if seller.is_none() && !has_label(PartyRole::Seller) {
        seller = buyers
            .iter()
            .find(|id| buyer.as_ref() != Some(*id))
            .cloned();
    }
    if buyer.is_none() && !has_label(PartyRole::Buyer) {
        buyer = sellers
            .iter()
            .find(|id| seller.as_ref() != Some(*id))
            .cloned();
    }
    (seller, buyer)
}

fn sole_tax_id_looks_like_buyer(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "bill to",
        "nabywca",
        "odbiorca",
        "kupujący",
        "kupujacy",
        "buyer",
        "customer",
    ]
    .iter()
    .any(|label| lower.contains(label))
}

pub(crate) fn normalize_tax_id(value: &str) -> Option<String> {
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 10 {
        Some(digits)
    } else {
        None
    }
}
