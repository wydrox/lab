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

#[test]
fn credit_note_keeps_its_own_number() {
    for text in [
        "Faktura korygująca nr KOR/1/2026\ndo faktury nr FV/10/2025",
        "do faktury nr FV/10/2025\nFaktura korygująca nr KOR/1/2026",
        "Faktura korygująca nr KOR/1/2026 do faktury nr FV/10/2025",
        "Faktura VAT korygująca nr KOR/1/2026\nDotyczy faktury nr FV/10/2025",
        "Dotyczy faktury nr FV/10/2025\nFaktura VAT korygująca nr KOR/1/2026",
        "Korekta do faktury nr FV/10/2025\nKorekta nr KOR/1/2026",
        "Faktura korygowana: FV/10/2025\nNumer faktury: KOR/1/2026",
        "Numer faktury: KOR/1/2026\nFaktura korygowana nr FV/10/2025",
        "Faktura korygująca nr KOR/1/2026\ndo faktury nr\nFV/10/2025",
    ] {
        assert_eq!(
            invoice_number_from_text(text).as_deref(),
            Some("KOR/1/2026"),
            "{text}"
        );
    }
    for text in [
        "Credit note number CN-0001\nCorrection of invoice INV-0042",
        "Correction of invoice INV-0042\nCredit note number CN-0001",
        "Invoice number: CN-0001\nOriginal invoice number: INV-0042",
        "Original invoice number: INV-0042\nInvoice number: CN-0001",
    ] {
        assert_eq!(
            invoice_number_from_text(text).as_deref(),
            Some("CN-0001"),
            "{text}"
        );
    }
    let record = parse_text_invoice(
        SourceKind::Mail,
        "Faktura korygująca nr KOR/1/2026\ndo faktury nr FV/10/2025\nData wystawienia: 2026-01-05",
    );
    assert_eq!(record.invoice_number.as_deref(), Some("KOR/1/2026"));
}

#[test]
fn reference_number_is_used_when_it_is_the_only_one() {
    for (text, expected) in [
        ("Nota uznaniowa\ndo faktury nr FV/10/2025", "FV/10/2025"),
        ("Correction of invoice INV-0042", "INV-0042"),
        ("Original invoice number: INV-0042", "INV-0042"),
        (
            "Faktura VAT nr FV/1/2026\nTermin płatności: 14 dni",
            "FV/1/2026",
        ),
        ("Numer faktury: 2026/01/1", "2026/01/1"),
    ] {
        assert_eq!(
            invoice_number_from_text(text).as_deref(),
            Some(expected),
            "{text}"
        );
    }
}

const PDF_TEXT_FAILURE: &str = "nie udało się wyciągnąć tekstu PDF: PDF nie zawiera tekstu";

fn unread_pdf(path: &str) -> InvoiceRecord {
    let mut record = empty_record(SourceKind::Mail);
    record.source_path = Some(path.into());
    record.content_hash = path.into();
    record.warnings.push(PDF_TEXT_FAILURE.to_string());
    record
}

#[test]
fn unreadable_invoice_pdf_waits_for_llm_before_productmesh_rule() {
    let mut scan = unread_pdf("mail/0123456789abcdef_1_Faktura_123.pdf");
    scan.invoice_number = Some("123".into());
    scan.warnings.push(FILENAME_NUMBER_WARNING.to_string());
    let no_signal = unread_pdf("mail/0123456789abcdef_2_skan.pdf");
    let mut locked = unread_pdf("mail/0123456789abcdef_3_Faktura_7.pdf");
    locked.warnings.push(PDF_PASSWORD_WARNING.to_string());
    // Odczytany PDF z sygnałem faktury, ale bez związku z firmą: odpada jak dotąd.
    let mut unrelated = mail_invoice(
        "mail/0123456789abcdef_4_Faktura_FV-9.pdf",
        "FV/9/2026",
        Some("5210000001"),
        Some(12300),
    );
    unrelated.buyer_tax_id = Some("5220000002".into());
    let related = mail_invoice(
        "mail/0123456789abcdef_5_invoice.pdf",
        "FV/1/2026",
        Some("5210000001"),
        Some(12300),
    );
    assert!(record_text_unreadable(&scan));
    assert!(mail_parse_needs_retry(&scan));
    assert!(record_has_invoice_signal(&scan));
    assert!(!record_has_invoice_signal(&no_signal));

    let records = [scan.clone(), no_signal, locked, unrelated, related.clone()];
    let candidates = productmesh_invoice_candidates(&records, OWN_NIP);
    let paths: Vec<_> = candidates
        .iter()
        .map(|r| r.source_path.as_deref().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "mail/0123456789abcdef_1_Faktura_123.pdf",
            "mail/0123456789abcdef_5_invoice.pdf"
        ]
    );

    // Skan trafia do kolejki LLM z filtrem płatnym; cache nadal go wyklucza.
    let skip = HashSet::new();
    assert!(record_queued_for_llm(&scan, &skip, true, false));
    assert!(record_queued_for_llm(&scan, &skip, false, false));
    let cached = HashSet::from([scan.content_hash.clone()]);
    assert!(!record_queued_for_llm(&scan, &cached, true, false));

    // LLM nie znalazł powiązania: rekord odpada po kroku LLM.
    let after = productmesh_candidates_after_llm(&candidates, OWN_NIP);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].source_path, related.source_path);

    // LLM wpisał NIP firmy: rekord zostaje.
    let mut filled = candidates.clone();
    let scan_idx = filled
        .iter()
        .position(|r| r.content_hash == scan.content_hash)
        .unwrap();
    filled[scan_idx].buyer_tax_id = Some(OWN_NIP.into());
    filled[scan_idx].seller_tax_id = Some("5252344078".into());
    filled[scan_idx].gross_amount_minor = Some(4560);
    let after = productmesh_candidates_after_llm(&filled, OWN_NIP);
    assert_eq!(after.len(), 2);
    assert!(after.iter().any(|r| r.content_hash == scan.content_hash));

    // Nazwa firmy podana przez LLM też wystarcza.
    let mut named = scan.clone();
    named.buyer_name = Some("ProductMesh Sp. z o.o.".into());
    assert_eq!(productmesh_candidates_after_llm(&[named], OWN_NIP).len(), 1);
}

#[test]
fn readable_pdf_without_company_link_is_not_kept_for_llm() {
    // Tekst odczytany, sygnał faktury jest, ale brak NIP-u i nazwy firmy.
    let mut record = empty_record(SourceKind::Mail);
    record.source_path = Some("mail/0123456789abcdef_1_Faktura_123.pdf".into());
    record.content_hash = "readable".into();
    record.invoice_number = Some("123/2026".into());
    assert!(productmesh_invoice_candidates(&[record], OWN_NIP).is_empty());
}
