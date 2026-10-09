use crate::*;

pub(crate) fn clean_name(value: &str) -> Option<String> {
    let cleaned = value
        .lines()
        .next()
        .unwrap_or(value)
        .trim()
        .trim_matches(|c: char| matches!(c, ':' | ',' | ';' | '-' | '|'))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.len() >= 3 && !cleaned.chars().all(|c| c.is_ascii_digit()) {
        Some(cleaned)
    } else {
        None
    }
}

pub(crate) fn counterparty_names_from_text(text: &str) -> (Option<String>, Option<String>) {
    let seller_labels = ["sprzedawca", "wystawca", "seller", "supplier"];
    let seller = name_after_label(text, &seller_labels).or_else(|| {
        if text
            .lines()
            .any(|line| name_label_match(line, &seller_labels).is_some())
        {
            None
        } else {
            name_before_label(text, &["bill to", "buyer", "customer"])
                .or_else(|| name_before_first_nip(text))
        }
    });
    let buyer_labels = [
        "nabywca",
        "odbiorca",
        "kupujący",
        "kupujacy",
        "buyer",
        "customer",
        "bill to",
    ];
    let buyer = name_after_label(text, &buyer_labels).or_else(|| {
        if text
            .lines()
            .any(|line| name_label_match(line, &buyer_labels).is_some())
        {
            None
        } else {
            name_before_nth_nip(text, 2)
        }
    });
    (seller, buyer)
}

fn name_label_match<'a>(line: &'a str, labels: &[&str]) -> Option<regex::Match<'a>> {
    let alternatives = labels
        .iter()
        .map(|label| regex::escape(label))
        .collect::<Vec<_>>()
        .join("|");
    Regex::new(&format!(r"(?i)\b(?:{alternatives})\b(?:[ \t]+details\b)?"))
        .ok()?
        .find(line)
}

fn any_name_label(line: &str) -> Option<regex::Match<'_>> {
    name_label_match(
        line,
        &[
            "sprzedawca",
            "wystawca",
            "seller",
            "supplier",
            "nabywca",
            "odbiorca",
            "kupujący",
            "kupujacy",
            "buyer",
            "customer",
            "bill to",
        ],
    )
}

pub(crate) fn name_after_label(text: &str, labels: &[&str]) -> Option<String> {
    let lines = raw_nonempty_lines(text);
    for (idx, line) in lines.iter().enumerate() {
        let Some(label) = name_label_match(line, labels) else {
            continue;
        };
        // Regex offsets refer to the original UTF-8 string, not its lowercase copy.
        let before = &line[..label.start()];
        let after = &line[label.end()..];
        let next_label = any_name_label(after);
        let right_column = !before.trim().is_empty();
        let paired = any_name_label(before).is_some() || next_label.is_some();
        let inline = &after[..next_label.map_or(after.len(), |m| m.start())];
        if let Some(name) = clean_name(inline).filter(|name| is_probable_name_line(name)) {
            return Some(name);
        }
        let column_start = line[..label.start()].chars().count();
        for candidate in lines.iter().skip(idx + 1).take(6) {
            if any_name_label(candidate).is_some() {
                break;
            }
            let segment = if paired || right_column {
                name_column_segment(candidate, right_column, column_start)
            } else {
                Some(candidate.as_str())
            };
            if let Some(name) = segment
                .and_then(clean_name)
                .filter(|name| is_probable_name_line(name))
            {
                return Some(name);
            }
        }
    }
    None
}

pub(crate) fn name_before_label(text: &str, labels: &[&str]) -> Option<String> {
    let lines = raw_nonempty_lines(text);
    for (idx, line) in lines.iter().enumerate() {
        let Some(label) = name_label_match(line, labels) else {
            continue;
        };
        if let Some(before_label) = clean_name(&line[..label.start()])
            && is_probable_name_line(&before_label)
        {
            return Some(before_label);
        }
        for candidate in lines[..idx].iter().rev().take(8) {
            let cleaned = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
            if is_probable_name_line(&cleaned) {
                return clean_name(&cleaned);
            }
        }
    }
    None
}

fn raw_nonempty_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim_end().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn name_column_segment(line: &str, right: bool, column_start: usize) -> Option<&str> {
    let separator = Regex::new(r" {2,}|\t+").ok()?;
    let trimmed = line.trim();
    if let Some(gap) = separator.find(trimmed) {
        return Some(if right {
            &trimmed[gap.end()..]
        } else {
            &trimmed[..gap.start()]
        });
    }
    // A row can contain only one populated column. Keep its indentation.
    let start = line.chars().take_while(|c| c.is_whitespace()).count();
    let leading_tab = line
        .chars()
        .take_while(|c| c.is_whitespace())
        .any(|c| c == '\t');
    if right {
        (leading_tab || start.saturating_add(2) >= column_start).then_some(trimmed)
    } else {
        (!leading_tab && start <= column_start.saturating_add(2)).then_some(trimmed)
    }
}

fn name_before_first_nip(text: &str) -> Option<String> {
    name_before_nth_nip(text, 1)
}

fn name_before_nth_nip(text: &str, nth: usize) -> Option<String> {
    let lines = clean_lines(text);
    let mut seen = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        if line.to_lowercase().contains("nip") {
            seen += 1;
            if seen == nth {
                for candidate in lines[..idx].iter().rev().take(4) {
                    if is_probable_name_line(candidate) {
                        return clean_name(candidate);
                    }
                }
            }
        }
    }
    None
}

fn clean_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect()
}

pub(crate) fn is_probable_name_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    line.chars().count() >= 3
        && !counterparty_name_is_placeholder(line)
        && !matches!(lower.trim(), "name" | "name:" | "nazwa" | "nazwa:")
        && !lower
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| word == "vat")
        && line.chars().count() <= 140
        && !lower.contains("nip")
        && !lower.contains("regon")
        && !lower.contains("adres")
        && !lower.contains("siedziba")
        && !lower.starts_with("ul.")
        && !lower.starts_with("ul ")
        && !lower.contains("data")
        && !lower.contains('@')
        && !lower.contains("http")
        && !line.chars().next().is_some_and(|c| c.is_ascii_digit())
        && !lower.contains("faktura")
        && !lower.contains("invoice")
        && !lower.contains("date")
        && !lower.contains("street")
        && !lower.contains("united states")
        && !lower.contains("poland")
        && !lower.contains("california")
        && !lower.contains("warszawa")
        && !lower.contains("pasadena")
        && !lower.contains("p.o. box")
        && !lower.contains("pmb ")
        && !lower.contains("razem")
        && !lower.contains("zapłaty")
        && !lower.contains("zapl")
        && line.chars().any(|c| c.is_alphabetic())
}
