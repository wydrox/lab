use super::*;

// Ostrzeżenia rekordów w tabeli: które są dla użytkownika, znacznik `!` w G/K/S, linia
// szczegółów zaznaczonego wiersza i szerokości kolumn na 80 kolumnach terminala.

fn record(source: SourceKind, hash: &str, warnings: &[&str]) -> InvoiceRecord {
    let mut record = empty_record(source);
    record.content_hash = hash.into();
    record.invoice_number = Some("FV/3/2026".into());
    record.issue_date = NaiveDate::from_ymd_opt(2026, 3, 4);
    record.gross_amount_minor = Some(123_456_789);
    record.currency = Some("PLN".into());
    record.warnings = warnings.iter().map(|warning| warning.to_string()).collect();
    record
}

fn table_row(
    mail: Option<InvoiceRecord>,
    ksef: Option<InvoiceRecord>,
    saldeo: Option<InvoiceRecord>,
) -> InvoiceTableRow {
    let row = TriRow {
        status: tri_status(mail.is_some(), ksef.is_some(), saldeo.is_some()).into(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: None,
        ksef_score_to_saldeo: None,
        mail,
        ksef,
        saldeo,
    };
    invoice_table_row_from_reconcile_row(&row, None).unwrap()
}

fn ambiguous_date_warning() -> String {
    format!(
        "{AMBIGUOUS_DATE_WARNING}: data wystawienia 03/04/2026 przyjęta jako 2026-04-03 (dzień przed miesiącem); możliwe 2026-03-04"
    )
}

const LLM_APPLIED: &str = "LLM gemma-3-27b: zastosowano zweryfikowane uzupełnienie";
const OVERRIDE: &str = "lab override applied: gross_amount_minor,seller_name";

fn internal_markers() -> Vec<&'static str> {
    vec![
        "lab-mail-parser:v4",
        "lab-mail-parser:v1",
        LLM_APPLIED,
        OVERRIDE,
        KSEF_ONLINE_METADATA_MARKER,
        SALDEO_ENRICHED_FROM_KSEF_MARKER,
        SALDEO_ENRICHED_FROM_DOWNLOAD_MARKER,
        "",
        "   ",
    ]
}

fn width(text: &str) -> usize {
    ratatui::text::Span::raw(text).width()
}

#[test]
fn internal_markers_are_recognised_and_user_warnings_are_not() {
    for marker in internal_markers() {
        assert!(is_internal_record_warning(marker), "{marker:?}");
    }
    // Markery są tymi samymi napisami, które rozpoznaje reszta kodu.
    let mut llm = record(SourceKind::Mail, "m", &[LLM_APPLIED]);
    assert!(record_has_llm_result(&llm));
    llm.warnings.clear();
    assert!(!record_has_llm_result(&llm));
    let saldeo = record(SourceKind::Saldeo, "s", &[OVERRIDE]);
    assert!(saldeo_record_has_override(&saldeo));

    let user_facing = [
        ambiguous_date_warning(),
        FILENAME_NUMBER_WARNING.to_string(),
        SALDEO_NO_NUMBER_WARNING.to_string(),
        PDF_PASSWORD_WARNING.to_string(),
        "LLM: zastąpiono numer faktury odczytany z nazwy pliku (scan_001)".to_string(),
        "LLM: zastąpiono niespójne kwoty (netto 1,00, VAT 0,23, brutto 9,99) spójnym zestawem z dokumentu".to_string(),
        "LLM niedostępny: serwer ppmlx nie odpowiada".to_string(),
        "LLM gemma-3-27b: odpowiedź bez numeru faktury".to_string(),
        "ocr niedostępny: brak tesseract".to_string(),
        "saldeo download fallback failed: 404".to_string(),
    ];
    for warning in &user_facing {
        assert!(!is_internal_record_warning(warning), "{warning}");
    }
}

#[test]
fn row_without_warnings_has_no_marker_and_no_detail() {
    let row = table_row(
        Some(record(SourceKind::Mail, "m", &[])),
        Some(record(SourceKind::Ksef, "k", &[])),
        Some(record(SourceKind::Saldeo, "s", &[])),
    );
    assert!(invoice_table_row_warnings(&row).is_empty());
    assert_eq!(invoice_table_sources_cell(&row), "G/K/S");
    for max_width in [0, 40, 80, 120] {
        assert_eq!(invoice_table_warning_detail(Some(&row), max_width), "");
    }
    assert_eq!(invoice_table_warning_detail(None, 80), "");
}

#[test]
fn row_with_only_internal_markers_looks_like_a_row_without_warnings() {
    let markers = internal_markers();
    let row = table_row(
        Some(record(SourceKind::Mail, "m", &markers)),
        Some(record(SourceKind::Ksef, "k", &markers)),
        None,
    );
    assert!(invoice_table_row_warnings(&row).is_empty());
    assert_eq!(invoice_table_sources_cell(&row), "G/K/-");
    assert_eq!(invoice_table_warning_detail(Some(&row), 200), "");
}

#[test]
fn override_marker_keeps_its_star_and_is_not_a_warning() {
    let row = table_row(
        None,
        None,
        Some(record(SourceKind::Saldeo, "s", &[OVERRIDE])),
    );
    assert_eq!(row.sources, "-/-/S*");
    assert_eq!(invoice_table_sources_cell(&row), "-/-/S*");
    assert_eq!(invoice_table_warning_detail(Some(&row), 200), "");
}

#[test]
fn one_warning_per_source_is_marked_and_listed_with_its_source() {
    let date_warning = ambiguous_date_warning();
    let row = table_row(
        Some(record(
            SourceKind::Mail,
            "m",
            &["lab-mail-parser:v4", FILENAME_NUMBER_WARNING],
        )),
        Some(record(
            SourceKind::Ksef,
            "k",
            &[KSEF_ONLINE_METADATA_MARKER, &date_warning],
        )),
        Some(record(
            SourceKind::Saldeo,
            "s",
            &[OVERRIDE, SALDEO_NO_NUMBER_WARNING],
        )),
    );
    assert_eq!(
        invoice_table_row_warnings(&row),
        [
            ('G', FILENAME_NUMBER_WARNING),
            ('K', date_warning.as_str()),
            ('S', SALDEO_NO_NUMBER_WARNING),
        ]
    );
    // `*` poprawki zostaje w `sources`; `!` dochodzi tylko w komórce.
    assert_eq!(row.sources, "G/K/S*");
    assert_eq!(invoice_table_sources_cell(&row), "G/K/S*!");
    assert_eq!(
        invoice_table_warning_detail(Some(&row), 1000),
        format!(
            " ! G: {FILENAME_NUMBER_WARNING} · K: {date_warning} · S: {SALDEO_NO_NUMBER_WARNING} "
        )
    );
    // Ostrzeżenie tylko w jednym źródle też daje znacznik.
    let row = table_row(
        Some(record(SourceKind::Mail, "m", &[])),
        None,
        Some(record(SourceKind::Saldeo, "s", &[SALDEO_NO_NUMBER_WARNING])),
    );
    assert_eq!(invoice_table_sources_cell(&row), "G/-/S!");
    assert_eq!(
        invoice_table_warning_detail(Some(&row), 1000),
        format!(" ! S: {SALDEO_NO_NUMBER_WARNING} ")
    );
}

#[test]
fn detail_comes_from_source_records_not_the_merged_display_record() {
    let mut row = table_row(Some(record(SourceKind::Mail, "m", &[])), None, None);
    row.record.warnings = vec![FILENAME_NUMBER_WARNING.into()];
    assert_eq!(invoice_table_sources_cell(&row), "G/-/-");
    // Wynik LLM podmienia `mail_record`; znacznik idzie za nim.
    row.mail_record = Some(record(SourceKind::Mail, "m", &[FILENAME_NUMBER_WARNING]));
    assert_eq!(invoice_table_sources_cell(&row), "G/-/-!");
}

#[test]
fn long_polish_warnings_are_cut_to_the_terminal_width_without_panic() {
    let long = format!(
        "{} · {}",
        ambiguous_date_warning(),
        "LLM: zastąpiono niespójne kwoty (netto 1 234,56, VAT 283,95, brutto 9 999,99) spójnym zestawem z dokumentu; źródło: żółta faktura „Łódź”"
    );
    let row = table_row(
        Some(record(
            SourceKind::Mail,
            "m",
            &[&long, "błąd odczytu\nlinia 2\twięcej"],
        )),
        None,
        Some(record(SourceKind::Saldeo, "s", &[SALDEO_NO_NUMBER_WARNING])),
    );
    let full = invoice_table_warning_detail(Some(&row), usize::MAX);
    assert!(full.contains("G: błąd odczytu linia 2 więcej"), "{full}");
    for max_width in [0, 1, 2, 3, 40, 80, 120] {
        let text = invoice_table_warning_detail(Some(&row), max_width);
        assert!(width(&text) <= max_width, "{max_width}: {text:?}");
        assert!(!text.chars().any(char::is_control), "{text:?}");
        if max_width > 0 {
            assert!(text.ends_with('…'), "{max_width}: {text:?}");
        }
        if max_width >= 40 {
            assert!(
                text.starts_with(" ! G: niejednoznaczny zapis daty"),
                "{text:?}"
            );
        }
    }
}

#[test]
fn warnings_do_not_change_sorting_filtering_or_attention() {
    let plain = |hash: &str, number: &str, warnings: &[&str]| {
        let mut mail = record(SourceKind::Mail, hash, warnings);
        mail.invoice_number = Some(number.into());
        table_row(Some(mail), None, None)
    };
    let without = vec![plain("a", "FV/2", &[]), plain("b", "FV/1", &[])];
    let with = vec![
        plain("a", "FV/2", &[FILENAME_NUMBER_WARNING]),
        plain("b", "FV/1", &[]),
    ];
    for sort_column in 0..SORT_LABELS.len() {
        for descending in [false, true] {
            for hide in [false, true] {
                assert_eq!(
                    invoice_table_visible_indices(&with, hide, sort_column, descending),
                    invoice_table_visible_indices(&without, hide, sort_column, descending),
                    "column {sort_column} desc {descending} hide {hide}"
                );
            }
        }
    }
    for (a, b) in with.iter().zip(&without) {
        assert_eq!(a.sources, b.sources);
        assert_eq!(a.needs_attention(), b.needs_attention());
        assert_eq!(a.is_actionable(), b.is_actionable());
        assert_eq!(
            invoice_table_row_passes_filter(a, true),
            invoice_table_row_passes_filter(b, true)
        );
    }
}

/// Renders the table with the TUI's header, constraints and cells; returns the body lines
/// and the column widths measured from a row of distinct letters.
fn render_table(rows: &[InvoiceTableRow], width: u16) -> (Vec<String>, Vec<usize>) {
    let letters = ["a", "b", "c", "d", "e", "f", "g", "h"];
    let mut table_rows = rows
        .iter()
        .map(|row| Row::new(invoice_table_row_cells(row, "[ ]".into()).map(Cell::from)))
        .collect::<Vec<_>>();
    table_rows.push(Row::new(
        letters.map(|letter| Cell::from(letter.repeat(60))),
    ));
    let height = table_rows.len() as u16 + 3;
    let table = Table::new(table_rows, invoice_table_column_constraints())
        .header(Row::new(INVOICE_TABLE_HEADERS.map(Cell::from)))
        .block(Block::default().borders(Borders::ALL));
    let area = ratatui::layout::Rect::new(0, 0, width, height);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(table, area, &mut buf);
    let lines = (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    let widths = letters
        .iter()
        .map(|letter| lines[height as usize - 2].matches(letter).count())
        .collect();
    (lines, widths)
}

#[test]
fn sources_column_shows_both_markers_without_narrowing_other_columns_at_80() {
    let row = table_row(
        Some(record(SourceKind::Mail, "m", &[FILENAME_NUMBER_WARNING])),
        Some(record(SourceKind::Ksef, "k", &[])),
        Some(record(SourceKind::Saldeo, "s", &[OVERRIDE])),
    );
    // Przed znacznikami (Percentage 4/14/7/20/22/10/16/7) na 80 kolumnach:
    // sel 3, akcja 6, G/K/S 5 (gwiazdka ucięta), faktura 16, kontrahent 15, data 8,
    // brutto 13, wal 5. Dwie kolumny waluty przechodzą do G/K/S, reszta bez zmian.
    let (lines, widths) = render_table(std::slice::from_ref(&row), 80);
    assert_eq!(widths, [3, 6, 7, 16, 15, 8, 13, 3], "{lines:#?}");
    assert!(lines[2].contains(" G/K/S*! "), "{lines:#?}");
    assert!(
        lines[2].contains(&format_minor_money(123_456_789)),
        "{lines:#?}"
    );
    assert!(lines[2].contains("PLN"), "{lines:#?}");
    for terminal_width in [100, 120, 160] {
        let (lines, wider) = render_table(std::slice::from_ref(&row), terminal_width);
        assert_eq!(wider[2], INVOICE_TABLE_SOURCES_WIDTH as usize);
        assert!(lines[2].contains(" G/K/S*! "), "{lines:#?}");
        assert!(lines[2].contains("2026-03-04"), "{lines:#?}");
        for (column, (now, at_80)) in wider.iter().zip(&widths).enumerate() {
            assert!(now >= at_80, "{terminal_width}: column {column} {wider:?}");
        }
    }
}

#[test]
fn warning_detail_fits_the_bottom_border() {
    let row = table_row(
        Some(record(SourceKind::Mail, "m", &[&ambiguous_date_warning()])),
        None,
        None,
    );
    for terminal_width in [40u16, 80, 120] {
        let detail =
            invoice_table_warning_detail(Some(&row), terminal_width.saturating_sub(2) as usize);
        let area = ratatui::layout::Rect::new(0, 0, terminal_width, 3);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(
            Block::default()
                .borders(Borders::ALL)
                .title_bottom(ratatui::text::Line::raw(detail.clone())),
            area,
            &mut buf,
        );
        let bottom = (0..terminal_width)
            .map(|x| buf[(x, 2)].symbol().to_string())
            .collect::<String>();
        assert!(bottom.starts_with("└ ! G: niejednoznaczny"), "{bottom}");
        assert!(bottom.ends_with('┘'), "{bottom}");
        assert!(bottom.contains(detail.trim_end()), "{bottom}");
    }
}
