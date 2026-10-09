//! Domyślne źródło Gmail roku: główny plik kandydatów i, gdy istnieje, plik presetu
//! `sync --amazon-mail` (węższe zapytanie Amazon.it/.es z osobnym cache).

use super::*;

/// Kandydaci Gmail roku dla reconcile, uploadu, repair, TUI, MCP i doctor.
pub(crate) fn load_default_mail_candidates(year: i32) -> Result<Vec<InvoiceRecord>> {
    load_mail_candidate_files(
        &default_mail_candidates_path(year),
        &default_amazon_mail_candidates_path(year),
    )
}

/// Jawny `--mail <ścieżka>` jest czytany sam; bez niego domyślne źródło roku.
pub(crate) fn load_mail_candidates_or_default(
    explicit: Option<&Path>,
    year: i32,
) -> Result<Vec<InvoiceRecord>> {
    match explicit {
        Some(path) => load_records(SourceKind::Mail, path),
        None => load_default_mail_candidates(year),
    }
}

/// Gmail dla reconcile roku (CLI `reconcile`, MCP `reconcile`): jawny `--mail` bez zmian,
/// domyślne źródło z regułą roku wystawienia tabeli TUI
/// (`filter_invoice_records_for_year_table`). Plik kandydatów roku trzyma też rekordy,
/// które LLM datował na inny rok (okno Gmaila to grudzień roku −1 … styczeń roku +1);
/// bez filtra wychodziłyby w raporcie jako `gmail_only`.
pub(crate) fn load_reconcile_mail_candidates(
    explicit: Option<&Path>,
    year: i32,
) -> Result<Vec<InvoiceRecord>> {
    let records = load_mail_candidates_or_default(explicit, year)?;
    Ok(match explicit {
        Some(_) => records,
        None => filter_invoice_records_for_year_table(records, year),
    })
}

/// Brak pliku Amazon: tylko główny (brak głównego to błąd jak dotąd). Brak głównego przy
/// pliku Amazon: tylko Amazon.
pub(crate) fn load_mail_candidate_files(main: &Path, amazon: &Path) -> Result<Vec<InvoiceRecord>> {
    if !amazon.exists() {
        return load_records(SourceKind::Mail, main);
    }
    let amazon_records = load_records(SourceKind::Mail, amazon)?;
    if !main.exists() {
        return Ok(amazon_records);
    }
    Ok(merge_mail_candidates(
        load_records(SourceKind::Mail, main)?,
        amazon_records,
    ))
}

/// Dokłada `extra` do `main`: pomija ten sam `content_hash`, a dla tego samego numeru
/// faktury i `same_mail_invoice` zostawia rekord lepszy wg `record_quality_score`
/// (przy remisie ten z `main`).
pub(crate) fn merge_mail_candidates(
    main: Vec<InvoiceRecord>,
    extra: Vec<InvoiceRecord>,
) -> Vec<InvoiceRecord> {
    let own_nip = normalize_tax_id(DEFAULT_PRODUCTMESH_NIP)
        .unwrap_or_else(|| DEFAULT_PRODUCTMESH_NIP.to_string());
    let number_key = |record: &InvoiceRecord| {
        record
            .invoice_number
            .as_deref()
            .map(comparable_invoice_number)
            .filter(|key| !key.is_empty())
    };
    let mut out = main;
    let mut hashes = out
        .iter()
        .filter(|record| !record.content_hash.is_empty())
        .map(|record| record.content_hash.clone())
        .collect::<HashSet<_>>();
    for record in extra {
        if !record.content_hash.is_empty() && !hashes.insert(record.content_hash.clone()) {
            continue;
        }
        if let Some(key) = number_key(&record)
            && let Some(existing) = out.iter_mut().find(|existing| {
                number_key(existing).as_deref() == Some(key.as_str())
                    && same_mail_invoice(existing, &record, &own_nip)
            })
        {
            if record_quality_score(&record) > record_quality_score(existing) {
                *existing = record;
            }
            continue;
        }
        out.push(record);
    }
    out
}

#[cfg(test)]
mod default_candidates_tests;
