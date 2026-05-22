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
        ksef: None,
        saldeo: Some(saldeo),
    };
    let table_row = invoice_table_row_from_reconcile_row(&row, None).unwrap();
    assert_eq!(invoice_table_action_ksef_label(&table_row), "—");
}
