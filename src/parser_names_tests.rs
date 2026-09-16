use super::*;

fn assert_names(text: &str, seller: Option<&str>, buyer: Option<&str>) {
    let names = counterparty_names_from_text(text);
    assert_eq!(names.0.as_deref(), seller, "seller: {text}");
    assert_eq!(names.1.as_deref(), buyer, "buyer: {text}");
}

#[test]
fn elocity_columns() {
    assert_names(
        "Sprzedawca:                         Nabywca:\nElocity Sp. z o.o.                  Rafał Wyderka\nul. Przykładowa 1                   ul. Mokra 2\nNIP: 5210000001                     NIP: 5242920020",
        Some("Elocity Sp. z o.o."),
        Some("Rafał Wyderka"),
    );
}

#[test]
fn ryanair_columns_consume_full_details_labels() {
    assert_names(
        "SUPPLIER DETAILS                   CUSTOMER DETAILS\nRyanair DAC                        Rafał Wyderka\nVAT: IE1234567                     VAT: PL5242920020",
        Some("Ryanair DAC"),
        Some("Rafał Wyderka"),
    );
}

#[test]
fn reversed_columns_and_tabs() {
    assert_names(
        "CUSTOMER DETAILS\tSUPPLIER DETAILS\nRafał Wyderka\tRyanair DAC",
        Some("Ryanair DAC"),
        Some("Rafał Wyderka"),
    );
}

#[test]
fn inline_columns() {
    assert_names(
        "Sprzedawca: Elocity Sp. z o.o.    Nabywca: Rafał Wyderka",
        Some("Elocity Sp. z o.o."),
        Some("Rafał Wyderka"),
    );
    assert_names(
        "SUPPLIER DETAILS: Ryanair DAC    CUSTOMER DETAILS: Rafał Wyderka",
        Some("Ryanair DAC"),
        Some("Rafał Wyderka"),
    );
}

#[test]
fn sequential_sections() {
    assert_names(
        "SUPPLIER DETAILS\nRyanair DAC\nCUSTOMER DETAILS\nRafał Wyderka",
        Some("Ryanair DAC"),
        Some("Rafał Wyderka"),
    );
    assert_names(
        "Sprzedawca:\nElocity Sp. z o.o.\nNabywca:\nRafał Wyderka",
        Some("Elocity Sp. z o.o."),
        Some("Rafał Wyderka"),
    );
}

#[test]
fn missing_column_does_not_take_other_party_or_tax_id() {
    assert_names(
        "Sprzedawca:                         Nabywca:\nElocity Sp. z o.o.\nNIP: 5210000001                     NIP: 5242920020",
        Some("Elocity Sp. z o.o."),
        None,
    );
    assert_names(
        "Sprzedawca:                         Nabywca:\n                                   Rafał Wyderka",
        None,
        Some("Rafał Wyderka"),
    );
    assert_names(
        "Sprzedawca:\nNabywca:\nRafał Wyderka",
        None,
        Some("Rafał Wyderka"),
    );
}

#[test]
fn columns_skip_placeholders_independently() {
    assert_names(
        "SUPPLIER DETAILS                   CUSTOMER DETAILS\nName:                              Name:\nRyanair DAC                         VAT: PL5242920020\nVAT: IE1234567                      Rafał Wyderka",
        Some("Ryanair DAC"),
        Some("Rafał Wyderka"),
    );
}

#[test]
fn unicode_case_mapping_never_supplies_original_byte_offsets() {
    // İ expands on lowercasing; ẞ shrinks in UTF-8. Neither may shift offsets.
    for prefix in ["İİİ", "ẞẞẞ", "Łódź"] {
        assert_eq!(
            name_after_label(&format!("{prefix} Nabywca: Żółć Sp. z o.o."), &["nabywca"])
                .as_deref(),
            Some("Żółć Sp. z o.o.")
        );
        assert_eq!(
            name_before_label(&format!("{prefix} Bill to"), &["bill to"]).as_deref(),
            Some(prefix)
        );
    }
    assert_names(
        "SPRZEDAWCA: Żółć Sp. z o.o.\nKUPUJĄCY: Łukasz Ćma",
        Some("Żółć Sp. z o.o."),
        Some("Łukasz Ćma"),
    );
}

#[test]
fn unicode_column_positions_count_characters_not_bytes() {
    assert_names(
        "Sprzedawca: Żółć Sp. z o.o.         Nabywca:\n                                  Rafał Wyderka",
        Some("Żółć Sp. z o.o."),
        Some("Rafał Wyderka"),
    );
}

#[test]
fn labels_require_word_boundaries() {
    assert_eq!(
        name_after_label("Reseller: Acme\nCustomerology Ltd", &["seller"]),
        None
    );
}

#[test]
fn stripe_bill_to_regression() {
    assert_names(
        "Invoice\nAnthropic, PBC                                    Bill to\n548 Market Street                                 Rafal Wyderka\nPMB 90375                                         Mokra 33/49\nSan Francisco, California 94104                   03-562 Warszawa\nUnited States                                     Poland",
        Some("Anthropic, PBC"),
        Some("Rafal Wyderka"),
    );
}

#[test]
fn nip_fallback_regression() {
    assert_names(
        "Acme Sp. z o.o.\nNIP: 5210000001\nJan Kowalski\nNIP: 5242920020",
        Some("Acme Sp. z o.o."),
        Some("Jan Kowalski"),
    );
}
