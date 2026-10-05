use super::*;

#[test]
fn money_limits_do_not_overflow() {
    assert_eq!(parse_money_minor("92233720368547758.07"), Some(i64::MAX));
    assert_eq!(parse_money_minor("-92233720368547758.08"), Some(i64::MIN));
    assert_eq!(parse_money_minor("92233720368547758.08"), None);
    assert_eq!(parse_money_minor("9223372036854775807"), None);
}

// Syntetyczne fragmenty: bez nazw, adresów, identyfikatorów i pełnych faktur.
fn assert_amounts(text: &str, expected: (Option<i64>, Option<i64>, Option<i64>)) {
    let record = parse_text_invoice(SourceKind::Mail, text);
    assert_eq!(
        (
            record.net_amount_minor,
            record.vat_amount_minor,
            record.gross_amount_minor
        ),
        expected,
        "{text}"
    );
}

#[test]
fn elocity_ocr_summary_not_quantity() {
    for (quantity, price, net, vat, gross, expected) in [
        ("9.567", "1,94", "18,56", "4,27", "22,83", (1856, 427, 2283)),
        (
            "15.048",
            "1,38",
            "20,77",
            "4,78",
            "25,55",
            (2077, 478, 2555),
        ),
    ] {
        let text = format!(
            "łqczna kwota brutto\n{gross} PLN\n\nNazwa\ntowaru/usługi\nIlość\nJednostka\nCena\njednostki\nnetto\nWartość\nnetto\nVAT\nWartość\nVAT\nWartość\nbrutto\n\nUsługa testowa\n{quantity}\nkWh\n{price} PLN\n{net} PLN\n23%\n{vat} PLN\n{gross} PLN\n\nWartość netto: {net} PLN\nWartość VAT: {vat} PLN\nWartość brutto: {gross} PLN\nDo zapłaty: 0,00 PLN"
        );
        assert_amounts(
            &text,
            (Some(expected.0), Some(expected.1), Some(expected.2)),
        );
    }
}

#[test]
fn elocity_poppler_summary_not_quantity_or_paid_balance() {
    for (quantity, net, vat, gross, expected) in [
        ("9.567", "18,56", "4,27", "22,83", (1856, 427, 2283)),
        ("15.048", "20,77", "4,78", "25,55", (2077, 478, 2555)),
    ] {
        let text = format!(
            "                     Łączna kwota brutto\n\n\n{gross} PLN\nCena         Wartość           Wartość      Wartość\njednostki      netto     VAT     VAT          brutto\nnetto\nUsługa testowa    {quantity} kWh   1,00 PLN   {net} PLN   23%   {vat} PLN   {gross} PLN\n\n Wartość netto:   {net} PLN\n\n Wartość VAT:   {vat} PLN\n\n Wartość brutto:   {gross} PLN\nDo zapłaty: 0,00 PLN"
        );
        assert_amounts(
            &text,
            (Some(expected.0), Some(expected.1), Some(expected.2)),
        );
    }
}

#[test]
fn ryanair_poppler_total_columns() {
    assert_amounts(
        "SERVICE      RATE     NET (PLN)     VAT (PLN)     TOTAL (PLN)\nTest service  0%       158.00        0.00          158.00\n\n             TOTAL    158.00        0.00          158.00\n\nTOTAL VAT: 0.00",
        (Some(15800), Some(0), Some(15800)),
    );
}

#[test]
fn ryanair_ocr_total_columns() {
    assert_amounts(
        "RATE 0% NET (PLN) 158.00 VAT (PLN) 0.00 TOTAL (PLN) 158.00\n\nTOTAL 158.00 0.00 158.00\n\nTOTAL VAT: 0.00",
        (Some(15800), Some(0), Some(15800)),
    );
}

#[test]
fn table_summary_wins_over_item_values_without_calculation() {
    assert_amounts(
        "NET (EUR) 10.00 VAT (EUR) 2.00 TOTAL (EUR) 12.00\nTOTAL 30.00 7.00 99.00",
        (Some(3000), Some(700), Some(9900)),
    );
    assert_amounts(
        "NET (EUR)   VAT (EUR)   TOTAL (EUR)\nTOTAL\n30.00\n7.00\n99.00",
        (Some(3000), Some(700), Some(9900)),
    );
}

#[test]
fn strict_labels_and_no_header_skipping() {
    for text in [
        "TOTAL VAT: 0.00",
        "SUBTOTAL: 12.00",
        "TOTALITY: 12.00",
        "TOTAL tax amount: 12.00",
        "TOTAL\nHeader\n12.00",
        "TOTAL\n\n12.00",
        "TOTAL: 23%",
        "TOTAL: 9.567 kWh",
        "TOTAL: 12.345",
        "TOTAL: 12.00 34.00",
        "TOTAL: 12 34",
        "TOTAL: 12\n34",
        "TOTAL: 99999999999999999999999.00",
    ] {
        // Samodzielna liczba całkowita w następnej linii nie jest jej częścią.
        let expected = if text == "TOTAL: 12\n34" {
            Some(1200)
        } else {
            None
        };
        assert_eq!(amount_from_text(text, &["total"]), expected, "{text}");
    }
    assert_amounts("TOTAL VAT: 0.00", (None, Some(0), None));
    assert_eq!(
        amount_from_text("netto\nWartość\nVAT\n9.567", &["netto"]),
        None
    );
}

#[test]
fn single_values_and_money_formats() {
    for (value, expected) in [
        ("158.00", 15800),
        ("22,83 PLN", 2283),
        ("€ 12.34", 1234),
        ("-0,25", -25),
        ("1 234,56", 123456),
        ("1\u{a0}234,56", 123456),
        ("1\u{202f}234,56", 123456),
        ("1,234.56", 123456),
        ("1.234,56", 123456),
        ("0", 0),
        ("158", 15800),
    ] {
        assert_eq!(
            amount_from_text(&format!("TOTAL: {value}"), &["total"]),
            Some(expected)
        );
        assert_eq!(
            amount_from_text(&format!("TOTAL (PLN)\n{value}"), &["total"]),
            Some(expected)
        );
    }
}

#[test]
fn ambiguous_or_missing_columns_are_not_inferred() {
    for text in [
        "TOTAL 10.00 2.00 12.00",
        "VAT NET TOTAL\nTOTAL 10.00 2.00 12.00",
        "NET VAT TOTAL\nTOTAL 10.00 12.00",
        "NET VAT TOTAL\nTOTAL 10.00 2.00 12.00 99.00",
    ] {
        assert_amounts(text, (None, None, None));
    }
    assert_amounts(
        "Wartość netto: 10,00 PLN\nWartość brutto: 12,30 PLN",
        (Some(1000), None, Some(1230)),
    );
}

#[test]
fn free_text_money_decimal_and_thousands_separators() {
    for (value, expected) in [
        ("1.234,56", Some(123456)),
        ("1,234.56", Some(123456)),
        ("1 234,56", Some(123456)),
        ("1234,5", Some(123450)),
        ("1234.5", Some(123450)),
        ("100.5", Some(10050)),
        ("100.0", Some(10000)),
        ("0.5", Some(50)),
        ("0,5", Some(50)),
        ("-0.5", Some(-50)),
        // Pojedynczy separator i dokładnie trzy cyfry: separator tysięcy.
        ("1.234", Some(123400)),
        ("1,234", Some(123400)),
        ("12,345", Some(1234500)),
        ("1.234.567", Some(123456700)),
        ("1,234,567.89", Some(123456789)),
        // Nie może to być grupa tysięcy, więc to część dziesiętna (zaokrąglona).
        ("0,500", Some(50)),
        ("12345.678", Some(1234568)),
        ("1.2.3", None),
        ("1,234.56.78", None),
        ("12-34", None),
        ("-", None),
        ("", None),
    ] {
        assert_eq!(parse_money_minor(value), expected, "{value}");
    }
}

#[test]
fn machine_decimal_dot_is_always_decimal() {
    for (value, expected) in [
        ("100.5", Some(10050)),
        ("108.5", Some(10850)),
        ("1.234", Some(123)),
        ("1.235", Some(124)),
        ("-1.235", Some(-124)),
        ("0.005", Some(1)),
        ("0.0049", Some(0)),
        ("12", Some(1200)),
        ("1e3", Some(100000)),
        ("1.5E-1", Some(15)),
        ("1 234,56", None),
        ("1,5", None),
        ("abc", None),
        ("92233720368547758.07", Some(i64::MAX)),
        ("92233720368547758.08", None),
    ] {
        assert_eq!(parse_decimal_minor(value), expected, "{value}");
    }
}

#[test]
fn json_numbers_use_machine_decimal() {
    let record = parse_json_invoice(
        SourceKind::Ksef,
        r#"{"gross_amount":100.5,"net_amount":1.234}"#,
    )
    .unwrap();
    assert_eq!(record.gross_amount_minor, Some(10050));
    assert_eq!(record.net_amount_minor, Some(123));
    // Napis jest tekstem: zapis lokalny i grupy tysięcy.
    let record = parse_json_invoice(
        SourceKind::Ksef,
        r#"{"gross_amount":"1.234","net_amount":"1 234,5"}"#,
    )
    .unwrap();
    assert_eq!(record.gross_amount_minor, Some(123400));
    assert_eq!(record.net_amount_minor, Some(123450));

    let value: Value = serde_json::json!({"gross_amount": 100.5, "net_amount": "100,5"});
    assert_eq!(
        json_first_money_minor(&value, &["gross_amount"]),
        Some(10050)
    );
    assert_eq!(json_first_money_minor(&value, &["net_amount"]), Some(10050));
}

fn xml_amounts(xml: &str) -> (Option<i64>, Option<i64>, Option<i64>) {
    let record = parse_xml_invoice(SourceKind::Ksef, xml);
    (
        record.net_amount_minor,
        record.vat_amount_minor,
        record.gross_amount_minor,
    )
}

#[test]
fn ksef_xml_one_decimal_amount() {
    let xml =
        "<Faktura><Fa><P_13_1>88.2</P_13_1><P_14_1>20.3</P_14_1><P_15>108.5</P_15></Fa></Faktura>";
    assert_eq!(xml_amounts(xml), (Some(8820), Some(2030), Some(10850)));
    assert!(!record_amounts_inconsistent(&parse_xml_invoice(
        SourceKind::Ksef,
        xml
    )));
}

#[test]
fn ksef_xml_sums_all_rates() {
    let xml = "<tns:Faktura xmlns:tns=\"x\"><tns:Fa><tns:P_13_1>100.00</tns:P_13_1><tns:P_14_1>23.00</tns:P_14_1><tns:P_13_2>50</tns:P_13_2><tns:P_14_2>4.00</tns:P_14_2><tns:P_15>177.00</tns:P_15></tns:Fa></tns:Faktura>";
    assert_eq!(xml_amounts(xml), (Some(15000), Some(2700), Some(17700)));
    assert!(!record_amounts_inconsistent(&parse_xml_invoice(
        SourceKind::Ksef,
        xml
    )));

    // Tylko 8%: wcześniej netto i VAT znikały.
    assert_eq!(
        xml_amounts(
            "<Faktura><Fa><P_13_2>100.00</P_13_2><P_14_2>8.00</P_14_2><P_15>108.00</P_15></Fa></Faktura>"
        ),
        (Some(10000), Some(800), Some(10800))
    );

    // 0% (P_13_6_1) i zw (P_13_7) też są netto; P_13_10 nie myli się z P_13_1.
    assert_eq!(
        xml_amounts(
            "<Faktura><Fa><P_13_1>10.00</P_13_1><P_14_1>2.30</P_14_1><P_13_6_1>5.00</P_13_6_1><P_13_7>1.00</P_13_7><P_13_10>3.00</P_13_10><P_15>21.30</P_15></Fa></Faktura>"
        ),
        (Some(1900), Some(230), Some(2130))
    );
}

#[test]
fn ksef_xml_foreign_currency_skips_pln_vat_w_fields() {
    let xml = "<Faktura><Fa><KodWaluty>EUR</KodWaluty><P_13_1>100.00</P_13_1><P_14_1>23.00</P_14_1><P_14_1W>98.21</P_14_1W><P_13_2>50.00</P_13_2><P_14_2>4.00</P_14_2><P_14_2W>17.08</P_14_2W><P_15>177.00</P_15></Fa></Faktura>";
    let record = parse_xml_invoice(SourceKind::Ksef, xml);
    assert_eq!(record.currency.as_deref(), Some("EUR"));
    assert_eq!(xml_amounts(xml), (Some(15000), Some(2700), Some(17700)));
    assert!(!record_amounts_inconsistent(&record));
}

#[test]
fn generic_xml_amount_names_still_work() {
    assert_eq!(
        xml_amounts(
            "<Invoice><NetAmount>100.5</NetAmount><VatAmount>23.12</VatAmount><GrossAmount>1 236,2</GrossAmount></Invoice>"
        ),
        (Some(10050), Some(2312), Some(123620))
    );
}
