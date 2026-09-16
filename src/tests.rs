use super::*;

#[test]
fn maps_ksef_online_metadata_to_invoice_record() {
    let metadata = serde_json::json!({
        "ksefNumber": "5242920020-20260501-ABCDEF1234-12",
        "invoiceNumber": "FV/42/2026",
        "issueDate": "2026-05-01",
        "seller": {"nip": "521-000-00-01", "name": "Sprzedawca Sp. z o.o."},
        "buyer": {"identifier": {"type": "Nip", "value": "5242920020"}, "name": "Productmesh"},
        "netAmount": 100.0,
        "grossAmount": 123.0,
        "vatAmount": 23.0,
        "currency": "pln"
    });
    let record = ksef_metadata_to_record(&metadata).unwrap();
    assert_eq!(
        record.content_hash,
        "ksef:5242920020-20260501-ABCDEF1234-12"
    );
    assert_eq!(record.invoice_number.as_deref(), Some("FV/42/2026"));
    assert_eq!(
        record.ksef_reference.as_deref(),
        Some("5242920020-20260501-ABCDEF1234-12")
    );
    assert_eq!(record.seller_tax_id.as_deref(), Some("5210000001"));
    assert_eq!(record.buyer_tax_id.as_deref(), Some("5242920020"));
    assert_eq!(record.gross_amount_minor, Some(12300));
    assert_eq!(record.currency.as_deref(), Some("PLN"));
}

#[test]
fn ksef_year_ranges_are_quarterly() {
    let ranges = ksef_year_quarter_ranges(2026);
    assert_eq!(ranges.len(), 4);
    assert_eq!(ranges[0].0, "2026-01-01T00:00:00+00:00");
    assert_eq!(ranges[3].1, "2027-01-01T00:00:00+00:00");
}

#[test]
fn parses_ksef_like_xml() {
    let xml = r#"
    <Faktura>
      <Podmiot1><DaneIdentyfikacyjne><NIP>521-000-00-01</NIP></DaneIdentyfikacyjne></Podmiot1>
      <Podmiot2><DaneIdentyfikacyjne><NIP>5220000002</NIP></DaneIdentyfikacyjne></Podmiot2>
      <Fa><P_1>2026-05-01</P_1><P_2>FV/12/2026</P_2><P_15>1 230,50</P_15><KodWaluty>PLN</KodWaluty></Fa>
    </Faktura>"#;
    let record = parse_xml_invoice(SourceKind::Ksef, xml);
    assert_eq!(record.invoice_number.as_deref(), Some("FV/12/2026"));
    assert_eq!(record.seller_tax_id.as_deref(), Some("5210000001"));
    assert_eq!(record.buyer_tax_id.as_deref(), Some("5220000002"));
    assert_eq!(record.gross_amount_minor, Some(123050));
}

#[test]
fn scores_strong_match() {
    let mut ksef = empty_record(SourceKind::Ksef);
    ksef.invoice_number = Some("FV/12/2026".into());
    ksef.seller_tax_id = Some("5210000001".into());
    ksef.gross_amount_minor = Some(123050);
    ksef.issue_date = NaiveDate::from_ymd_opt(2026, 5, 1);
    ksef.currency = Some("PLN".into());
    let mut mail = empty_record(SourceKind::Mail);
    mail.invoice_number = Some("FV_12_2026".into());
    mail.seller_tax_id = Some("5210000001".into());
    mail.gross_amount_minor = Some(123050);
    mail.issue_date = NaiveDate::from_ymd_opt(2026, 5, 1);
    mail.currency = Some("PLN".into());
    let (score, reasons) = score_pair(&ksef, &mail);
    assert!(score >= 95, "score={score}, reasons={reasons:?}");
}

#[test]
fn parses_text_invoice() {
    let text = "Subject: Faktura FV/9/2026\nFrom: billing@example.com\nNIP 521-000-00-01\nData: 01.05.2026\nRazem do zapłaty: 99,90 zł";
    let record = parse_text_invoice(SourceKind::Mail, text);
    assert_eq!(record.invoice_number.as_deref(), Some("FV/9/2026"));
    assert_eq!(record.seller_tax_id.as_deref(), Some("5210000001"));
    assert_eq!(record.gross_amount_minor, Some(9990));
    assert_eq!(record.currency.as_deref(), Some("PLN"));
}

#[test]
fn ksef_pdf_reads_invoice_number_after_numer_faktury_label() {
    let text = "Krajowy System e-Faktur\nNumer Faktury:\n\n2026/01/1\nFaktura podstawowa\nNumer KSeF:5242920020-20260904-7B4DFEC00001-B8\nNazwa: Olga Borovska";
    let record = parse_text_invoice(SourceKind::Mail, text);
    assert_eq!(record.invoice_number.as_deref(), Some("2026/01/1"));
    assert!(!is_valid_invoice_number_candidate("FAKTURY"));
}

#[test]
fn parses_stripe_style_invoice_counterparties() {
    let text = r#"Invoice
Invoice number 44871C26-0020
Date of issue  January 5, 2026
Date due       January 5, 2026


Anthropic, PBC                                    Bill to
548 Market Street                                 Rafal Wyderka
PMB 90375                                         Mokra 33/49
San Francisco, California 94104                   03-562 Warszawa
United States                                     Poland
support@anthropic.com                             wyderkarafal@gmail.com
                                              PL VAT PL5242920020

€12.91 due January 5, 2026
Subtotal                                              €12.91
Total                                                 €12.91
Amount due                                           €12.91
"#;
    let record = parse_text_invoice(SourceKind::Saldeo, text);
    assert_eq!(record.invoice_number.as_deref(), Some("44871C26-0020"));
    assert_eq!(record.seller_name.as_deref(), Some("Anthropic, PBC"));
    assert_eq!(record.buyer_name.as_deref(), Some("Rafal Wyderka"));
    assert_eq!(record.buyer_tax_id.as_deref(), Some("5242920020"));
    assert_eq!(record.gross_amount_minor, Some(1291));
    assert_eq!(record.currency.as_deref(), Some("EUR"));
}

#[test]
fn normalizes_invoice_currency_variants() {
    assert_eq!(normalize_currency("zł"), Some("PLN".to_string()));
    assert_eq!(normalize_currency("waluta: euro"), Some("EUR".to_string()));
    assert_eq!(normalize_currency("Currency: usd"), Some("USD".to_string()));
    assert_eq!(
        currency_from_text("Total due: 42.00 €"),
        Some("EUR".to_string())
    );
}

#[test]
fn parses_llm_json_from_markdown() {
    let value = parse_json_from_llm("```json\n{\"ok\":true}\n```").unwrap();
    assert_eq!(value["ok"], true);
}

#[test]
fn parses_llm_json_after_reasoning_and_invalid_template() {
    let content = r#"
<|channel>thought
Template:
{
  "currency": "PLN" | "EUR" | null
}
<|channel>final
{"invoice_number":"FV/1/2026","currency":"PLN"}
"#;
    let value = parse_json_from_llm(content).unwrap();
    assert_eq!(value["invoice_number"], "FV/1/2026");
    assert_eq!(value["currency"], "PLN");
}

#[test]
fn final_channel_wins_over_valid_json_in_thought() {
    let content = r#"
<|channel>thought
{"invoice_number":"WRONG","currency":"EUR"}
<|channel>final
{"invoice_number":"RIGHT","currency":"PLN"}
"#;
    let value = parse_json_from_llm(content).unwrap();
    assert_eq!(value["invoice_number"], "RIGHT");
    assert_eq!(value["currency"], "PLN");
}

#[test]
fn rejects_thought_only_json() {
    let content = r#"
<|channel>thought
{"invoice_number":"WRONG","currency":"EUR"}
"#;
    assert!(parse_json_from_llm(content).is_err());
}

#[test]
fn productmesh_filter_normalizes_input_nip() {
    let mut record = empty_record(SourceKind::Mail);
    record.invoice_number = Some("FV/1/2026".into());
    record.buyer_tax_id = Some("5242920020".into());
    let candidates = productmesh_invoice_candidates(&[record], "PL 524-292-00-20");
    assert_eq!(candidates.len(), 1);
}

#[test]
fn year_specific_defaults_use_selected_year() {
    assert!(default_gmail_query(2025).contains("after:2025/01/01"));
    assert!(default_gmail_query(2025).contains("before:2026/01/01"));
    assert!(
        default_mail_out_path(2025)
            .to_string_lossy()
            .contains("2025")
    );
    assert!(
        default_saldeo_out_path(2025)
            .to_string_lossy()
            .contains("2025")
    );
    assert!(
        default_ksef_out_path(2025)
            .to_string_lossy()
            .contains("2025")
    );
}

#[test]
fn amazon_gmail_query_targets_amazon_it_and_es() {
    let query = amazon_gmail_query(2026);
    assert!(query.contains("(from:amazon.it OR from:amazon.es)"));
    assert!(query.contains("has:attachment filename:pdf"));
    assert!(query.contains("after:2026/01/01"));
    assert!(query.contains("before:2027/01/01"));
}

#[test]
fn amazon_mail_out_path_uses_separate_cache() {
    assert!(
        default_amazon_mail_out_path(2026)
            .to_string_lossy()
            .contains("mail-amazon-2026-pdfs")
    );
}

#[test]
fn invoice_records_match_selected_year_by_issue_or_sale_date() {
    let mut issue_record = empty_record(SourceKind::Mail);
    issue_record.issue_date = NaiveDate::from_ymd_opt(2026, 1, 15);
    issue_record.sale_date = NaiveDate::from_ymd_opt(2025, 12, 31);
    assert!(invoice_record_matches_year(&issue_record, 2026));
    assert!(!invoice_record_matches_year(&issue_record, 2025));

    let mut sale_record = empty_record(SourceKind::Ksef);
    sale_record.issue_date = None;
    sale_record.sale_date = NaiveDate::from_ymd_opt(2025, 12, 31);
    assert!(invoice_record_matches_year(&sale_record, 2025));
    assert!(!invoice_record_matches_year(&sale_record, 2026));

    let empty = empty_record(SourceKind::Saldeo);
    assert!(!invoice_record_matches_year(&empty, 2025));
}

#[test]
fn invoice_table_rows_match_the_display_year_after_merge() {
    let mut mail = empty_record(SourceKind::Mail);
    mail.content_hash = "mail:year-merge".into();
    mail.invoice_number = Some("FV/1/2025".into());
    mail.issue_date = None;
    mail.sale_date = None;

    let mut saldeo = empty_record(SourceKind::Saldeo);
    saldeo.content_hash = "saldeo:year-merge".into();
    saldeo.invoice_number = Some("FV/1/2025".into());
    saldeo.issue_date = NaiveDate::from_ymd_opt(2026, 1, 5);
    saldeo.gross_amount_minor = Some(12300);
    saldeo.currency = Some("PLN".into());

    let row = TriRow {
        status: "gmail_saldeo_missing_ksef".into(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: Some(80),
        ksef_score_to_saldeo: None,
        mail: Some(mail),
        ksef: None,
        saldeo: Some(saldeo),
    };
    let table_row = invoice_table_row_from_reconcile_row(&row, None).unwrap();
    assert!(
        table_row
            .record
            .issue_date
            .is_some_and(|d| d.year() == 2026)
    );
    assert!(invoice_table_row_matches_year(&table_row, 2026));
    assert!(!invoice_table_row_matches_year(&table_row, 2025));
}

#[test]
fn menu_navigation_wraps_around() {
    assert_eq!(wrap_menu_selection(0, -1, 10), 9);
    assert_eq!(wrap_menu_selection(9, 1, 10), 0);
    assert_eq!(wrap_menu_selection(0, -1, 6), 5);
    assert_eq!(wrap_menu_selection(5, 1, 6), 0);
}

#[test]
fn saldeo_bad_placeholder_counterparty_triggers_fallback() {
    let mut bad = empty_record(SourceKind::Saldeo);
    bad.seller_name = Some("nabywca".into());
    bad.issue_date = NaiveDate::from_ymd_opt(2026, 5, 1);
    bad.gross_amount_minor = Some(12300);
    assert!(!record_has_counterparty(&bad));

    let mut target = bad.clone();
    let mut source = empty_record(SourceKind::Saldeo);
    source.seller_name = Some("Sprzedawca Sp. z o.o.".into());
    source.buyer_name = Some("Productmesh".into());
    source.seller_tax_id = Some("5210000001".into());
    source.buyer_tax_id = Some("5242920020".into());
    source.issue_date = NaiveDate::from_ymd_opt(2026, 5, 1);
    source.gross_amount_minor = Some(12300);
    source.currency = Some("PLN".into());

    assert!(merge_missing_invoice_metadata(&mut target, &source));
    assert_eq!(target.seller_name.as_deref(), Some("Sprzedawca Sp. z o.o."));
    assert_eq!(target.buyer_name.as_deref(), Some("Productmesh"));
    assert_eq!(target.seller_tax_id.as_deref(), Some("5210000001"));
    assert_eq!(target.buyer_tax_id.as_deref(), Some("5242920020"));
    assert!(record_has_counterparty(&target));
}

#[test]
fn tri_reconcile_dedupes_equivalent_saldeo_records() {
    let mut mail = empty_record(SourceKind::Mail);
    mail.content_hash = "mail:1".into();
    mail.invoice_number = Some("RL/31518/01/26".into());
    mail.seller_tax_id = Some("5222857117".into());
    mail.buyer_tax_id = Some("5242920020".into());
    mail.issue_date = NaiveDate::from_ymd_opt(2026, 1, 2);
    mail.gross_amount_minor = Some(433064);
    mail.currency = Some("PLN".into());

    let mut saldeo_a = mail.clone();
    saldeo_a.source = SourceKind::Saldeo;
    saldeo_a.content_hash = "saldeo:538124728".into();
    let mut saldeo_b = saldeo_a.clone();
    saldeo_b.content_hash = "saldeo:538668974".into();

    let report = tri_reconcile(vec![mail], vec![], vec![saldeo_a, saldeo_b], 70);
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.summary.saldeo_count, 1);
    assert_eq!(report.summary.gmail_saldeo_missing_ksef, 1);
    assert_eq!(report.summary.saldeo_only, 0);
}

#[test]
fn tri_reconcile_dedupes_same_invoice_number_on_different_dates() {
    let mut mail = empty_record(SourceKind::Mail);
    mail.content_hash = "mail:eleven".into();
    mail.invoice_number = Some("E3C69EBD-0003".into());
    mail.issue_date = NaiveDate::from_ymd_opt(2026, 4, 16);
    mail.gross_amount_minor = Some(2200);
    mail.currency = Some("USD".into());

    let mut saldeo_a = mail.clone();
    saldeo_a.source = SourceKind::Saldeo;
    saldeo_a.content_hash = "saldeo:1".into();
    let mut saldeo_b = saldeo_a.clone();
    saldeo_b.content_hash = "saldeo:2".into();
    saldeo_b.issue_date = NaiveDate::from_ymd_opt(2026, 4, 20);

    let report = tri_reconcile(vec![mail], vec![], vec![saldeo_a, saldeo_b], 70);
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.summary.saldeo_only, 0);
}

#[test]
fn own_buyer_nip_and_zero_amount_do_not_match_unrelated_invoices() {
    let mut mail = empty_record(SourceKind::Mail);
    mail.invoice_number = Some("405421-2026/IE".into());
    mail.buyer_tax_id = Some("5242920020".into());
    mail.issue_date = NaiveDate::from_ymd_opt(2026, 2, 28);
    mail.gross_amount_minor = Some(0);
    mail.currency = Some("PLN".into());

    let mut ksef = empty_record(SourceKind::Ksef);
    ksef.invoice_number = Some("PL6552160".into());
    ksef.buyer_tax_id = Some("5242920020".into());
    ksef.seller_tax_id = Some("8992520556".into());
    ksef.issue_date = NaiveDate::from_ymd_opt(2026, 3, 1);
    ksef.gross_amount_minor = Some(0);
    ksef.currency = Some("PLN".into());

    let (score, reasons) = score_pair(&ksef, &mail);
    assert!(
        score < 45,
        "score={score}, reasons={reasons:?} — Ryanair nie może trafić w OVH"
    );
    assert!(!invoice_identity_match(&mail, &ksef));
}

#[test]
fn display_prefers_ksef_name_over_mail_placeholder() {
    let mut mail = empty_record(SourceKind::Mail);
    mail.invoice_number = Some("17364/ELOCITY/FUL/03/2026".into());
    mail.seller_name = Some("Nabywca".into());
    mail.buyer_name = Some("Nabywca".into());
    let mut ksef = empty_record(SourceKind::Ksef);
    ksef.invoice_number = mail.invoice_number.clone();
    ksef.seller_name = Some("Elocity sp. z o.o.".into());
    ksef.buyer_name = Some("Productmesh".into());
    let mut saldeo = empty_record(SourceKind::Saldeo);
    saldeo.invoice_number = mail.invoice_number.clone();
    saldeo.seller_name = Some("ELOCITY".into());
    let row = TriRow {
        status: "in_all_three".into(),
        mail_score_to_ksef: Some(80),
        mail_score_to_saldeo: Some(80),
        ksef_score_to_saldeo: Some(80),
        mail: Some(mail),
        ksef: Some(ksef),
        saldeo: Some(saldeo),
    };
    let table_row = invoice_table_row_from_reconcile_row(&row, None).unwrap();
    assert_eq!(
        counterparty_name(Some(&table_row.record)),
        "Elocity sp. z o.o."
    );
}

#[test]
fn display_shows_buyer_when_seller_is_own_company() {
    let mut ksef = empty_record(SourceKind::Ksef);
    ksef.invoice_number = Some("2026/01/1".into());
    ksef.seller_name = Some("productMesh - RAFAŁ WYDERKA".into());
    ksef.seller_tax_id = Some("5242920020".into());
    ksef.buyer_name = Some("Olga Borovska".into());
    let row = TriRow {
        status: "ksef_saldeo_missing_gmail".into(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: None,
        ksef_score_to_saldeo: Some(100),
        mail: None,
        ksef: Some(ksef),
        saldeo: None,
    };
    let table_row = invoice_table_row_from_reconcile_row(&row, None).unwrap();
    assert_eq!(
        counterparty_name(Some(&table_row.record)),
        "Olga Borovska"
    );
}

#[test]
fn display_replaces_mail_heading_with_ksef_invoice_number() {
    let mut mail = empty_record(SourceKind::Mail);
    mail.invoice_number = Some("FAKTURY".into());
    mail.ksef_reference = Some("5242920020-20260904-7B4DFEC00001-B8".into());
    let mut ksef = empty_record(SourceKind::Ksef);
    ksef.invoice_number = Some("2026/01/1".into());
    ksef.ksef_reference = mail.ksef_reference.clone();
    ksef.buyer_name = Some("Olga Borovska".into());
    let row = TriRow {
        status: "gmail_ksef_missing_saldeo".into(),
        mail_score_to_ksef: Some(100),
        mail_score_to_saldeo: None,
        ksef_score_to_saldeo: None,
        mail: Some(mail),
        ksef: Some(ksef),
        saldeo: None,
    };
    let table_row = invoice_table_row_from_reconcile_row(&row, None).unwrap();
    assert_eq!(table_row.record.invoice_number.as_deref(), Some("2026/01/1"));
}

#[test]
fn exact_invoice_number_matches_across_sources() {
    let mut mail = empty_record(SourceKind::Mail);
    mail.invoice_number = Some("PL6552160".into());
    mail.currency = Some("PLN".into());
    let mut saldeo = empty_record(SourceKind::Saldeo);
    saldeo.invoice_number = Some("PL6552160".into());
    saldeo.ksef_reference = Some("8992520556-20260301-537139000008-F4".into());
    saldeo.currency = Some("PLN".into());
    assert!(invoice_identity_match(&mail, &saldeo));
    let report = tri_reconcile(vec![mail], vec![], vec![saldeo], 70);
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.summary.gmail_saldeo_missing_ksef, 1);
}

#[test]
fn saldeo_without_ksef_reference_is_not_ksef_approvable_in_tui() {
    let mut mail = empty_record(SourceKind::Mail);
    mail.content_hash = "mail:anthropic".into();
    mail.invoice_number = Some("44871C26-0020".into());

    let mut saldeo = empty_record(SourceKind::Saldeo);
    saldeo.content_hash = "saldeo:538124729".into();
    saldeo.invoice_number = Some("44871C26-0020".into());
    saldeo.ksef_reference = None;

    let row = TriRow {
        status: "gmail_saldeo_missing_ksef".into(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: Some(80),
        ksef_score_to_saldeo: None,
        mail: Some(mail),
        ksef: None,
        saldeo: Some(saldeo),
    };
    let statuses = HashMap::new();
    let table_row = invoice_table_row_from_reconcile_row(&row, Some(&statuses)).unwrap();
    assert_eq!(invoice_table_action_ksef_label(&table_row), "—");
}

#[test]
fn saldeo_overrides_show_star_and_replace_fields() {
    let mut saldeo = empty_record(SourceKind::Saldeo);
    saldeo.content_hash = "saldeo:42".into();
    saldeo.invoice_number = Some("WRONG".into());
    saldeo.seller_name = Some("nabywca".into());
    saldeo.buyer_name = Some("Someone".into());

    let override_row = SaldeoRecordOverride {
        content_hash: saldeo.content_hash.clone(),
        invoice_number: Some("FV/1/2026".into()),
        seller_tax_id: Some("5210000001".into()),
        buyer_tax_id: Some("5242920020".into()),
        seller_name: Some("Sprzedawca Sp. z o.o.".into()),
        buyer_name: Some("Productmesh".into()),
        issue_date: NaiveDate::from_ymd_opt(2026, 5, 1),
        gross_amount_minor: Some(12300),
        currency: Some("PLN".into()),
    };
    assert!(apply_saldeo_record_override(&mut saldeo, &override_row));
    assert_eq!(saldeo.invoice_number.as_deref(), Some("FV/1/2026"));
    assert!(saldeo_record_has_override(&saldeo));

    let row = TriRow {
        status: "gmail_saldeo_missing_ksef".into(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: Some(80),
        ksef_score_to_saldeo: None,
        mail: None,
        ksef: None,
        saldeo: Some(saldeo),
    };
    let table_row = invoice_table_row_from_reconcile_row(&row, None).unwrap();
    assert!(table_row.sources.ends_with('*'));
    assert_eq!(
        table_row.record.invoice_number.as_deref(),
        Some("FV/1/2026")
    );
}

#[test]
fn saldeo_only_rows_do_not_make_ksef_actions_available() {
    let mut saldeo = empty_record(SourceKind::Saldeo);
    saldeo.content_hash = "saldeo:123".into();
    saldeo.invoice_number = Some("FV/1/2026".into());
    saldeo.ksef_reference = Some("KSEF-REF".into());

    let row = TriRow {
        status: "saldeo_only".into(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: None,
        ksef_score_to_saldeo: None,
        mail: None,
        ksef: None,
        saldeo: Some(saldeo),
    };
    let table_row = invoice_table_row_from_reconcile_row(&row, None).unwrap();
    assert_eq!(invoice_table_action_ksef_label(&table_row), "—");
}

#[test]
fn saldeo_legacy_overrides_are_imported_into_sqlite() {
    struct HomeGuard(Option<std::ffi::OsString>);

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            if let Some(old_home) = self.0.take() {
                unsafe { std::env::set_var("HOME", old_home) };
            } else {
                unsafe { std::env::remove_var("HOME") };
            }
        }
    }

    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(format!("lab-saldeo-{nonce}"));
    let home = root.join("home");
    let legacy_path = home.join(".config/lab/saldeo-overrides.json");
    let db_path = root.join("lab.sqlite");
    std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();

    let legacy_override = SaldeoRecordOverride {
        content_hash: "saldeo:legacy".into(),
        invoice_number: Some("FV/legacy/2026".into()),
        seller_tax_id: Some("5210000001".into()),
        buyer_tax_id: Some("5242920020".into()),
        seller_name: Some("Legacy Seller".into()),
        buyer_name: Some("Productmesh".into()),
        issue_date: NaiveDate::from_ymd_opt(2026, 5, 2),
        gross_amount_minor: Some(12345),
        currency: Some("PLN".into()),
    };
    std::fs::write(
        &legacy_path,
        serde_json::to_vec_pretty(&vec![legacy_override.clone()]).unwrap(),
    )
    .unwrap();

    let new_override = SaldeoRecordOverride {
        content_hash: "saldeo:new".into(),
        invoice_number: Some("FV/new/2026".into()),
        seller_tax_id: Some("5210000002".into()),
        buyer_tax_id: Some("5242920020".into()),
        seller_name: Some("New Seller".into()),
        buyer_name: Some("Productmesh".into()),
        issue_date: NaiveDate::from_ymd_opt(2026, 5, 3),
        gross_amount_minor: Some(98765),
        currency: Some("EUR".into()),
    };

    let _home_guard = HomeGuard(std::env::var_os("HOME"));
    unsafe { std::env::set_var("HOME", &home) };

    save_saldeo_record_override(&db_path, &new_override).unwrap();

    let overrides = load_saldeo_record_overrides(Some(&db_path)).unwrap();
    assert_eq!(overrides.len(), 2);
    assert_eq!(
        overrides.get(&legacy_override.content_hash),
        Some(&legacy_override)
    );
    assert_eq!(
        overrides.get(&new_override.content_hash),
        Some(&new_override)
    );
}

#[test]
fn missing_saldeo_statuses_do_not_make_ksef_rows_actionable() {
    let mut saldeo = empty_record(SourceKind::Saldeo);
    saldeo.content_hash = "saldeo:123".into();
    saldeo.invoice_number = Some("FV/1/2026".into());
    saldeo.ksef_reference = Some("KSEF-REF".into());

    let row = TriRow {
        status: "ksef_saldeo_missing_gmail".into(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: None,
        ksef_score_to_saldeo: Some(100),
        mail: None,
        ksef: Some(empty_record(SourceKind::Ksef)),
        saldeo: Some(saldeo),
    };
    let table_row = invoice_table_row_from_reconcile_row(&row, None).unwrap();
    assert_eq!(invoice_table_action_ksef_label(&table_row), "—");
}

#[test]
fn detects_closed_saldeo_month() {
    let closed = serde_json::json!({
        "status": "VALIDATION_ERROR",
        "data": [{"field": "month", "message": "Miesiąc jest zamknięty"}]
    });
    assert!(saldeo_period_is_closed(&closed));
    let success = serde_json::json!({"status": "SUCCESS", "data": {}});
    assert!(!saldeo_period_is_closed(&success));
    let other = serde_json::json!({
        "status": "VALIDATION_ERROR",
        "data": [{"field": "filename", "message": "Nieprawidłowa nazwa"}]
    });
    assert!(!saldeo_period_is_closed(&other));
}

#[test]
fn fallback_upload_period_uses_current_open_month() {
    let now = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
    assert_eq!(saldeo_fallback_upload_period(2026, 7, now), (2026, 9));
    assert_eq!(saldeo_fallback_upload_period(2026, 9, now), (2026, 10));
    assert_eq!(saldeo_fallback_upload_period(2026, 12, now), (2027, 1));
    let now = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
    assert_eq!(saldeo_fallback_upload_period(2026, 7, now), (2026, 8));
}

#[test]
fn filter_hides_approved_invoices_in_ksef_and_saldeo() {
    let mut ksef = empty_record(SourceKind::Ksef);
    ksef.ksef_reference = Some("KSEF-REF".into());
    let mut saldeo = empty_record(SourceKind::Saldeo);
    saldeo.content_hash = "saldeo:123".into();
    saldeo.ksef_reference = Some("KSEF-REF".into());
    let row = TriRow {
        status: "ksef_saldeo_missing_gmail".into(),
        mail_score_to_ksef: None,
        mail_score_to_saldeo: None,
        ksef_score_to_saldeo: Some(100),
        mail: None,
        ksef: Some(ksef),
        saldeo: Some(saldeo),
    };
    let mut statuses = std::collections::HashMap::new();
    statuses.insert(123, Some(true));
    let approved = invoice_table_row_from_reconcile_row(&row, Some(&statuses)).unwrap();
    assert_eq!(invoice_table_ksef_status(&approved), "zatw.");
    assert!(approved.sources.contains('-'));
    assert!(!invoice_table_row_passes_filter(&approved, true));
    assert!(invoice_table_row_passes_filter(&approved, false));

    statuses.insert(123, None);
    let unmarked = invoice_table_row_from_reconcile_row(&row, Some(&statuses)).unwrap();
    assert_eq!(invoice_table_ksef_status(&unmarked), "nieozn.");
    assert!(invoice_table_row_passes_filter(&unmarked, true));

    statuses.insert(123, Some(false));
    let rejected = invoice_table_row_from_reconcile_row(&row, Some(&statuses)).unwrap();
    assert_eq!(invoice_table_ksef_status(&rejected), "odrz.");
    assert!(invoice_table_row_passes_filter(&rejected, true));
}
