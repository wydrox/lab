use super::*;

const OWN_NIP: &str = "5242920020";

fn scratch_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("lab-parser-records-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn gmail_message_id_is_not_an_invoice_number() {
    assert_eq!(
        invoice_number_from_filename(Path::new("data/mail/18c2f3a4b5d6e7f8_1_document.pdf")),
        None
    );
    assert_eq!(
        invoice_number_from_filename(Path::new(
            "data/mail/18c2f3a4b5d6e7f8_1_Faktura_FV-12-2026.pdf"
        ))
        .as_deref(),
        Some("FV-12-2026")
    );
    assert_eq!(
        invoice_number_from_filename(Path::new("Invoice-M73SJH5X-0005.pdf")).as_deref(),
        Some("M73SJH5X-0005")
    );
}

#[test]
fn parse_file_marks_filename_numbers() {
    let dir = scratch_dir("filename");
    let text = "Dokument bez numeru\nKwota brutto: 12,30 PLN\n";

    let message_id_only = dir.join("18c2f3a4b5d6e7f8_1_document.txt");
    fs::write(&message_id_only, text).unwrap();
    let record = parse_file(SourceKind::Mail, &message_id_only).unwrap();
    assert_eq!(record.invoice_number, None);
    assert!(!record_number_from_filename(&record));

    let genuine = dir.join("18c2f3a4b5d6e7f8_1_Faktura_FV-12-2026.txt");
    fs::write(&genuine, text).unwrap();
    let record = parse_file(SourceKind::Mail, &genuine).unwrap();
    assert_eq!(record.invoice_number.as_deref(), Some("FV-12-2026"));
    assert!(
        record
            .warnings
            .iter()
            .any(|w| w == "numer faktury odczytany z nazwy pliku")
    );
    assert!(record_number_from_filename(&record));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn filename_number_still_counts_as_missing_for_llm() {
    let mut record = empty_record(SourceKind::Mail);
    record.source_path = Some("data/mail/18c2f3a4b5d6e7f8_1_Faktura_FV-12-2026.pdf".into());
    record.content_hash = "hash".into();
    record.invoice_number = Some("FV-12-2026".into());
    record.issue_date = NaiveDate::from_ymd_opt(2026, 1, 2);
    record.gross_amount_minor = Some(12300);
    record.currency = Some("PLN".into());
    record.seller_name = Some("Firma Testowa Sp. z o.o.".into());
    record.buyer_name = Some("productmesh".into());
    record.buyer_tax_id = Some(OWN_NIP.into());
    let skip = HashSet::new();
    assert!(!record_missing_hard_fields(&record));
    assert!(!record_queued_for_llm(&record, &skip, true, false));
    assert!(!record_queued_for_llm(&record, &skip, false, false));

    record.warnings.push(FILENAME_NUMBER_WARNING.to_string());
    assert!(record_missing_hard_fields(&record));
    assert!(record_needs_paid_llm(&record));
    assert!(record_queued_for_llm(&record, &skip, true, false));
    assert!(record_queued_for_llm(&record, &skip, false, false));
}

fn mail_invoice(
    path: &str,
    number: &str,
    seller_nip: Option<&str>,
    gross: Option<i64>,
) -> InvoiceRecord {
    let mut record = empty_record(SourceKind::Mail);
    record.source_path = Some(path.into());
    record.content_hash = path.into();
    record.invoice_number = Some(number.into());
    record.seller_tax_id = seller_nip.map(Into::into);
    record.buyer_tax_id = Some(OWN_NIP.into());
    record.gross_amount_minor = gross;
    record
}

#[test]
fn same_number_from_two_sellers_is_kept_twice() {
    let a = mail_invoice("a.pdf", "1/01/2026", Some("5210000001"), Some(12300));
    let b = mail_invoice("b.pdf", "1/01/2026", Some("5220000002"), Some(45600));
    assert_eq!(productmesh_invoice_candidates(&[a, b], OWN_NIP).len(), 2);

    // Bez NIP-u sprzedawcy i z inną kwotą to też osobne faktury.
    let a = mail_invoice("a.pdf", "1/01/2026", Some("5210000001"), Some(12300));
    let b = mail_invoice("b.pdf", "1-01-2026", None, Some(45600));
    assert_eq!(productmesh_invoice_candidates(&[a, b], OWN_NIP).len(), 2);

    // Ta sama kwota, ale różne NIP-y: nadal dwie faktury.
    let a = mail_invoice("a.pdf", "1/01/2026", Some("5210000001"), Some(12300));
    let b = mail_invoice("b.pdf", "1/01/2026", Some("5220000002"), Some(12300));
    assert_eq!(productmesh_invoice_candidates(&[a, b], OWN_NIP).len(), 2);
}

#[test]
fn true_duplicates_still_merge_and_better_record_wins() {
    // Wspólny NIP kontrahenta.
    let poor = mail_invoice("a.pdf", "FV/1/2026", Some("5210000001"), None);
    let mut rich = mail_invoice("b.pdf", "FV_1_2026", Some("5210000001"), Some(12300));
    rich.issue_date = NaiveDate::from_ymd_opt(2026, 1, 2);
    for records in [[poor.clone(), rich.clone()], [rich, poor]] {
        let candidates = productmesh_invoice_candidates(&records, OWN_NIP);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source_path.as_deref(), Some("b.pdf"));
    }

    // Jeden bez NIP-u kontrahenta, ta sama kwota brutto.
    let a = mail_invoice("a.pdf", "FV/1/2026", Some("5210000001"), Some(12300));
    let b = mail_invoice("b.pdf", "FV/1/2026", None, Some(12300));
    assert_eq!(productmesh_invoice_candidates(&[a, b], OWN_NIP).len(), 1);

    // Ten sam plik (ten sam hash) z dwóch wiadomości.
    let a = mail_invoice("a.pdf", "FV/1/2026", None, None);
    let mut b = mail_invoice("b.pdf", "FV/1/2026", None, None);
    b.content_hash = a.content_hash.clone();
    assert_eq!(productmesh_invoice_candidates(&[a, b], OWN_NIP).len(), 1);
}

#[test]
fn filename_number_loses_to_parsed_number() {
    let mut guessed = mail_invoice("a.pdf", "FV/1/2026", Some("5210000001"), Some(12300));
    guessed.warnings.push(FILENAME_NUMBER_WARNING.to_string());
    let parsed = mail_invoice("b.pdf", "FV/1/2026", Some("5210000001"), Some(12300));
    let candidates = productmesh_invoice_candidates(&[guessed, parsed], OWN_NIP);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].source_path.as_deref(), Some("b.pdf"));
}
