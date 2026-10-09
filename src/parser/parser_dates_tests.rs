use super::*;

fn date(y: i32, m: u32, d: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(y, m, d)
}

fn ambiguous_warnings(record: &InvoiceRecord) -> Vec<&String> {
    record
        .warnings
        .iter()
        .filter(|w| w.starts_with(AMBIGUOUS_DATE_WARNING))
        .collect()
}

#[test]
fn ambiguous_slash_date_without_evidence_is_day_first_with_warning() {
    for text in [
        "Faktura FV/1/2026\nData wystawienia: 03/04/2026\nRazem brutto: 123,00 PLN",
        "Faktura FV/1/2026\nData wystawienia: 03-04-2026\nRazem brutto: 123,00 PLN",
        // Evidence in both directions decides nothing.
        "Data wystawienia: 03/04/2026\nData sprzedaży: 25/03/2026\nTermin płatności: 04/30/2026",
    ] {
        let record = parse_text_invoice(SourceKind::Mail, text);
        assert_eq!(record.issue_date, date(2026, 4, 3), "{text}");
        let warnings = ambiguous_warnings(&record);
        assert_eq!(warnings.len(), 1, "{text}: {:?}", record.warnings);
        assert!(warnings[0].contains("data wystawienia"), "{}", warnings[0]);
        assert!(warnings[0].contains("2026-04-03"), "{}", warnings[0]);
        assert!(warnings[0].contains("2026-03-04"), "{}", warnings[0]);
    }
}

#[test]
fn unlabeled_ambiguous_date_is_flagged_too() {
    let record = parse_text_invoice(SourceKind::Mail, "Faktura FV/1/2026\n03/04/2026\n");
    assert_eq!(record.issue_date, date(2026, 4, 3));
    assert_eq!(ambiguous_warnings(&record).len(), 1);
}

#[test]
fn month_first_evidence_reads_ambiguous_date_month_first() {
    for text in [
        "Invoice date: 03/04/2026\nDue date: 03/25/2026",
        "Invoice date: 03-04-2026\nDue date: 03-25-2026",
        "Invoice date: 03/04/2026\nShipped on March 2, 2026",
        "Invoice date: 03/04/2026\nShipped on Mar 2nd 2026",
    ] {
        let record = parse_text_invoice(SourceKind::Mail, text);
        assert_eq!(record.issue_date, date(2026, 3, 4), "{text}");
        assert!(
            ambiguous_warnings(&record).is_empty(),
            "{text}: {:?}",
            record.warnings
        );
    }
    let record = parse_text_invoice(
        SourceKind::Mail,
        "Invoice date: 03/04/2026\nDue date: 03/25/2026",
    );
    assert_eq!(record.due_date, date(2026, 3, 25));
}

#[test]
fn day_first_evidence_removes_the_warning() {
    for text in [
        "Data wystawienia: 03/04/2026\nTermin płatności: 25/04/2026",
        "Data wystawienia: 03/04/2026\nData sprzedaży: 28.03.2026",
        "Data wystawienia: 03/04/2026\nwysłano 2 marca 2026",
        "Invoice date: 03/04/2026\nShipped on 2 March 2026",
    ] {
        let record = parse_text_invoice(SourceKind::Mail, text);
        assert_eq!(record.issue_date, date(2026, 4, 3), "{text}");
        assert!(
            ambiguous_warnings(&record).is_empty(),
            "{text}: {:?}",
            record.warnings
        );
    }
}

#[test]
fn unambiguous_and_dot_dates_are_unchanged_without_warning() {
    for (text, expected) in [
        ("Data wystawienia: 03.04.2026", date(2026, 4, 3)),
        ("Data wystawienia: 25/04/2026", date(2026, 4, 25)),
        ("Data wystawienia: 04/04/2026", date(2026, 4, 4)),
        ("Data wystawienia: 2026-03-04", date(2026, 3, 4)),
        ("Invoice date: March 4, 2026", date(2026, 3, 4)),
        // Valid only month-first; previously not read at all.
        ("Invoice date: 04/25/2026", date(2026, 4, 25)),
    ] {
        let record = parse_text_invoice(SourceKind::Mail, text);
        assert_eq!(record.issue_date, expected, "{text}");
        assert!(
            ambiguous_warnings(&record).is_empty(),
            "{text}: {:?}",
            record.warnings
        );
    }
    assert_eq!(parse_date("03/04/2026"), date(2026, 4, 3));
    assert_eq!(parse_date("03/25/2026"), date(2026, 3, 25));
    assert_eq!(parse_date("13/13/2026"), None);
}

#[test]
fn invoice_numbers_are_not_date_evidence() {
    assert_eq!(
        document_date_order("Numer: FV/12/25/2026/A\nData wystawienia: 03/04/2026"),
        None
    );
    assert_eq!(
        document_date_order("Numer: 12-25-2026-01\nData wystawienia: 03/04/2026"),
        None
    );
    assert_eq!(
        document_date_order("Data: 03/04/2026, termin 03/25/2026"),
        Some(DateOrder::MonthFirst)
    );
}

#[test]
fn ambiguous_date_warning_alone_does_not_queue_llm() {
    let parsed = parse_text_invoice(SourceKind::Mail, "Data wystawienia: 03/04/2026");
    let warning = ambiguous_warnings(&parsed)[0].clone();

    let mut record = empty_record(SourceKind::Mail);
    record.source_path = Some("data/mail/faktura.pdf".into());
    record.content_hash = "hash".into();
    record.invoice_number = Some("FV/12/2026".into());
    record.issue_date = parsed.issue_date;
    record.gross_amount_minor = Some(12300);
    record.currency = Some("PLN".into());
    record.seller_name = Some("Firma Testowa Sp. z o.o.".into());
    record.buyer_name = Some("Nabywca Testowy Sp. z o.o.".into());
    record.seller_tax_id = Some("5210000001".into());
    let skip = HashSet::new();
    assert!(!record_queued_for_llm(&record, &skip, true, false));
    assert!(!record_queued_for_llm(&record, &skip, false, false));

    record.warnings.push(warning);
    assert!(!record_needs_paid_llm(&record));
    assert!(!record_queued_for_llm(&record, &skip, true, false));
    assert!(!record_queued_for_llm(&record, &skip, false, false));
}
