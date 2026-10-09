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

const SELLER_NIP: &str = "5252344078";
const BUYER_NIP: &str = "5242920020";

fn assert_tax_ids(text: &str, seller: Option<&str>, buyer: Option<&str>) {
    let record = parse_text_invoice(SourceKind::Mail, text);
    assert_eq!(record.seller_tax_id.as_deref(), seller, "seller: {text}");
    assert_eq!(record.buyer_tax_id.as_deref(), buyer, "buyer: {text}");
}

#[test]
fn nip_checksum() {
    assert!(nip_checksum_valid(SELLER_NIP));
    assert!(nip_checksum_valid(BUYER_NIP));
    assert!(!nip_checksum_valid("5210000001"));
    assert!(!nip_checksum_valid("524292002"));
    assert!(!nip_checksum_valid("52429200200"));
    assert!(!nip_checksum_valid("524-292-00-20"));
}

#[test]
fn tax_ids_follow_text_order_not_pattern_order() {
    assert_eq!(
        tax_ids_from_text("VAT ID: PL5252344078\nNIP: 5242920020"),
        [SELLER_NIP, BUYER_NIP]
    );
    assert_eq!(
        tax_ids_from_text("NIP: 5242920020\nVAT ID: PL5252344078"),
        [BUYER_NIP, SELLER_NIP]
    );
    // NIP z prefiksem PL pasuje do obu wzorców; liczy się raz, na pierwszej pozycji.
    assert_eq!(
        tax_ids_from_text("NIP: PL5252344078\nNIP 5242920020"),
        [SELLER_NIP, BUYER_NIP]
    );
}

#[test]
fn seller_vat_id_with_pl_prefix_is_not_swapped_with_buyer_nip() {
    for text in [
        "Sprzedawca:\nACME Sp. z o.o.\nVAT ID: PL5252344078\nNabywca:\nProductMesh Sp. z o.o.\nNIP: 5242920020",
        "Sprzedawca: ACME Sp. z o.o. VAT ID: PL5252344078 Nabywca: ProductMesh NIP: 5242920020",
        "Seller:\nACME Ltd\nVAT ID: PL5252344078\nBuyer:\nProductMesh\nNIP 5242920020",
        "Sprzedawca:                         Nabywca:\nACME Sp. z o.o.                     ProductMesh Sp. z o.o.\nVAT ID: PL5252344078                NIP: 5242920020",
    ] {
        assert_tax_ids(text, Some(SELLER_NIP), Some(BUYER_NIP));
    }
}

#[test]
fn buyer_section_first_still_assigns_by_label() {
    for text in [
        "Nabywca:\nProductMesh Sp. z o.o.\nNIP: 5242920020\nSprzedawca:\nACME Sp. z o.o.\nVAT ID: PL5252344078",
        "Nabywca: ProductMesh NIP: 5242920020 Sprzedawca: ACME VAT ID: PL5252344078",
        "Bill to:\nProductMesh\nNIP 5242920020\nSeller:\nACME Ltd\nVAT: PL5252344078",
        "Nabywca:                            Sprzedawca:\nProductMesh Sp. z o.o.              ACME Sp. z o.o.\nNIP: 5242920020                     NIP: PL5252344078",
        "CUSTOMER DETAILS\tSUPPLIER DETAILS\nProductMesh\tACME Ltd\nNIP: 5242920020\tVAT: PL5252344078",
    ] {
        assert_tax_ids(text, Some(SELLER_NIP), Some(BUYER_NIP));
    }
}

#[test]
fn section_with_several_ids_prefers_valid_checksum() {
    assert_tax_ids(
        "Sprzedawca:\nACME\nNIP: 5210000001\nNIP: 5252344078\nNabywca:\nProductMesh\nNIP: 5242920020",
        Some(SELLER_NIP),
        Some(BUYER_NIP),
    );
    // Bez poprawnej sumy kontrolnej zostaje pierwszy w sekcji.
    assert_tax_ids(
        "Sprzedawca:\nNIP: 5210000001\nNIP: 5220000002\nNabywca:\nNIP: 5242920020",
        Some("5210000001"),
        Some(BUYER_NIP),
    );
}

#[test]
fn single_labelled_id_keeps_its_section() {
    // Nabywca bez NIP-u (konsument): jedyny NIP należy do sprzedawcy.
    assert_tax_ids(
        "Sprzedawca: ACME Sp. z o.o.\nNIP: 5252344078\nNabywca: Jan Kowalski",
        Some(SELLER_NIP),
        None,
    );
    assert_tax_ids(
        "Sprzedawca: Jan Kowalski\nNabywca: ProductMesh\nNIP: 5242920020",
        None,
        Some(BUYER_NIP),
    );
}

#[test]
fn unlabelled_texts_keep_previous_order_and_sole_buyer_rule() {
    assert_tax_ids(
        "ACME Sp. z o.o.\nNIP: 5210000001\nJan Kowalski\nNIP: 5242920020",
        Some("5210000001"),
        Some(BUYER_NIP),
    );
    // "customer" nie jest tu etykietą sekcji, więc działa dotychczasowa reguła.
    assert_tax_ids(
        "Contact customer support\nNIP: 5242920020",
        None,
        Some(BUYER_NIP),
    );
    assert_tax_ids("Faktura\nNIP: 5252344078", Some(SELLER_NIP), None);
    // "Customer number" nie otwiera sekcji nabywcy.
    assert_tax_ids(
        "Seller:\nACME\nCustomer number: 42\nVAT: PL5252344078\nBuyer:\nNIP 5242920020",
        Some(SELLER_NIP),
        Some(BUYER_NIP),
    );
}

#[test]
fn ryanair_and_stripe_tax_ids() {
    assert_tax_ids(
        "SUPPLIER DETAILS                   CUSTOMER DETAILS\nRyanair DAC                        Rafał Wyderka\nVAT: IE1234567                     VAT: PL5242920020",
        None,
        Some(BUYER_NIP),
    );
    assert_tax_ids(
        "Invoice\nAnthropic, PBC                                    Bill to\n548 Market Street                                 Rafal Wyderka\n                                              PL VAT PL5242920020",
        None,
        Some(BUYER_NIP),
    );
}
