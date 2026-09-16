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
