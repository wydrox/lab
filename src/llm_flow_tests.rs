use super::*;

#[test]
fn foreign_vat_numbers_do_not_replace_polish_buyer_nip() {
    let text = "Invoice number E3C69EBD-0001\nEleven Labs Inc.                 Bill to\n169 Madison Ave                 productmesh\nEU OSS VAT EU372062016           PL VAT PL5242920020\nGB VAT GB457922166\nZA VAT 4300323179\nTax ID 3312323741\n";
    let record = parse_text_invoice(SourceKind::Mail, text);
    assert_eq!(record.seller_tax_id, None);
    assert_eq!(record.buyer_tax_id.as_deref(), Some("5242920020"));
    assert_eq!(record.seller_name.as_deref(), Some("Eleven Labs Inc."));
}

#[test]
fn rejects_truncated_json_even_if_it_parses() {
    let response = serde_json::json!({"choices":[{"finish_reason":"length","message":{"content":"{\"seller_name\":\"Firma\"}"}}]});
    assert!(ppmlx_response_json(&response).is_err());
}

#[test]
fn accepts_complete_response_but_not_missing_content() {
    let response = serde_json::json!({"choices":[{"finish_reason":"stop","message":{"content":"{\"seller_name\":\"Firma\"}"}}]});
    assert_eq!(
        ppmlx_response_json(&response).unwrap()["seller_name"],
        "Firma"
    );
    assert!(ppmlx_response_json(&serde_json::json!({"choices":[]})).is_err());
    assert!(
        ppmlx_response_json(&serde_json::json!({"choices":[{"finish_reason":"stop"}]})).is_err()
    );
}
