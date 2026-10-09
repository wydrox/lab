use super::*;

const OWN_NIP: &str = "5242920020";
const LANDLORD_NIP: &str = "5210000001";

fn invoice(
    source: SourceKind,
    hash: &str,
    number: &str,
    date: (i32, u32, u32),
    gross: i64,
) -> InvoiceRecord {
    let mut record = empty_record(source);
    record.content_hash = hash.into();
    record.invoice_number = Some(number.into());
    record.issue_date = NaiveDate::from_ymd_opt(date.0, date.1, date.2);
    record.gross_amount_minor = Some(gross);
    record.currency = Some("PLN".into());
    record
}

fn rent(source: SourceKind, hash: &str, number: &str, date: (i32, u32, u32)) -> InvoiceRecord {
    let mut record = invoice(source, hash, number, date, 123000);
    record.seller_tax_id = Some(LANDLORD_NIP.into());
    record.buyer_tax_id = Some(OWN_NIP.into());
    record
}

/// (status, mail hash, ksef hash, saldeo hash), sorted — compares pairings
/// independently of row order.
fn pairing(report: &TriReconcileReport) -> Vec<(String, String, String, String)> {
    let hash = |record: &Option<InvoiceRecord>| {
        record
            .as_ref()
            .map(|r| r.content_hash.clone())
            .unwrap_or_default()
    };
    let mut rows = report
        .rows
        .iter()
        .map(|row| {
            (
                row.status.clone(),
                hash(&row.mail),
                hash(&row.ksef),
                hash(&row.saldeo),
            )
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

fn assert_no_conflicting_references(report: &TriReconcileReport) {
    for row in &report.rows {
        let refs = [&row.mail, &row.ksef, &row.saldeo]
            .into_iter()
            .flatten()
            .filter_map(normalized_ksef_reference)
            .collect::<HashSet<_>>();
        assert!(refs.len() <= 1, "row mixes KSeF numbers: {row:?}");
    }
}

fn rent_scenario() -> (InvoiceRecord, Vec<InvoiceRecord>, Vec<InvoiceRecord>) {
    let mail = rent(SourceKind::Mail, "mail:nov", "11/2026", (2026, 11, 5));
    let mut ksef_nov = rent(SourceKind::Ksef, "ksef:nov", "11/2026", (2026, 11, 5));
    ksef_nov.ksef_reference = Some("5210000001-20261105-AAAA-01".into());
    let mut ksef_jan = rent(SourceKind::Ksef, "ksef:jan", "1/2026", (2026, 1, 5));
    ksef_jan.ksef_reference = Some("5210000001-20260105-BBBB-02".into());
    let mut saldeo_jan = rent(SourceKind::Saldeo, "saldeo:jan", "1/2026", (2026, 1, 5));
    saldeo_jan.ksef_reference = ksef_jan.ksef_reference.clone();
    (mail, vec![ksef_nov, ksef_jan], vec![saldeo_jan])
}

#[test]
fn rent_invoices_pair_by_ksef_number_not_by_amount() {
    for review_score in [70, 45] {
        let (mail, ksef, saldeo) = rent_scenario();
        let report = tri_reconcile(vec![mail], ksef, saldeo, review_score);
        assert_eq!(
            pairing(&report),
            vec![
                (
                    "gmail_ksef_missing_saldeo".to_string(),
                    "mail:nov".to_string(),
                    "ksef:nov".to_string(),
                    String::new(),
                ),
                (
                    "ksef_saldeo_missing_gmail".to_string(),
                    String::new(),
                    "ksef:jan".to_string(),
                    "saldeo:jan".to_string(),
                ),
            ],
            "review_score={review_score}"
        );
        assert_eq!(report.summary.in_all_three, 0);
        assert_eq!(report.summary.ksef_only, 0);
        assert_no_conflicting_references(&report);
    }
}

#[test]
fn rent_pairing_does_not_depend_on_input_order() {
    let (mail, ksef, saldeo) = rent_scenario();
    let mut mail_b = rent(SourceKind::Mail, "mail:jan", "1/2026", (2026, 1, 5));
    mail_b.seller_name = Some("Wynajmujący".into());
    let mails = vec![mail, mail_b];
    let forward = tri_reconcile(mails.clone(), ksef.clone(), saldeo.clone(), 45);
    let mut mails_rev = mails;
    mails_rev.reverse();
    let mut ksef_rev = ksef;
    ksef_rev.reverse();
    let mut saldeo_rev = saldeo;
    saldeo_rev.reverse();
    let reversed = tri_reconcile(mails_rev, ksef_rev, saldeo_rev, 45);
    assert_eq!(pairing(&forward), pairing(&reversed));
    assert_eq!(forward.summary.in_all_three, 1);
    assert_eq!(forward.summary.gmail_ksef_missing_saldeo, 1);
    assert_no_conflicting_references(&forward);
}

#[test]
fn tied_candidates_resolve_the_same_way_in_any_order() {
    // Two mails that score the same against one KSeF record: the winner must
    // not depend on which mail came first.
    let mut ksef = invoice(SourceKind::Ksef, "ksef:1", "A/7/2026", (2026, 3, 1), 0);
    ksef.gross_amount_minor = None;
    ksef.seller_tax_id = Some(LANDLORD_NIP.into());
    let mut mail_a = invoice(SourceKind::Mail, "mail:a", "7/2026", (2026, 3, 1), 5000);
    mail_a.seller_tax_id = Some(LANDLORD_NIP.into());
    let mut mail_b = mail_a.clone();
    mail_b.content_hash = "mail:b".into();
    mail_b.gross_amount_minor = Some(6000);
    assert_eq!(score_pair(&mail_a, &ksef).0, score_pair(&mail_b, &ksef).0);
    let forward = tri_reconcile(
        vec![mail_a.clone(), mail_b.clone()],
        vec![ksef.clone()],
        vec![],
        50,
    );
    let reversed = tri_reconcile(vec![mail_b, mail_a], vec![ksef], vec![], 50);
    assert_eq!(forward.summary.gmail_ksef_missing_saldeo, 1);
    assert_eq!(forward.summary.gmail_only, 1);
    assert_eq!(pairing(&forward), pairing(&reversed));
}

#[test]
fn conflicting_ksef_references_never_share_a_row() {
    let mut mail = rent(SourceKind::Mail, "mail:1", "FV/5/2026", (2026, 5, 1));
    mail.ksef_reference = Some("5210000001-20260501-CCCC-03".into());
    let mut ksef = rent(SourceKind::Ksef, "ksef:1", "FV/5/2026", (2026, 5, 1));
    ksef.ksef_reference = Some("5210000001-20260501-AAAA-01".into());
    let mut saldeo = rent(SourceKind::Saldeo, "saldeo:1", "FV/5/2026", (2026, 5, 1));
    saldeo.ksef_reference = Some("5210000001-20260501-BBBB-02".into());

    let (score, reasons) = score_pair(&ksef, &saldeo);
    assert_eq!(score, 0, "reasons={reasons:?}");
    assert!(!invoice_identity_match(&ksef, &saldeo));

    for review_score in [0, 70] {
        let report = tri_reconcile(
            vec![mail.clone()],
            vec![ksef.clone()],
            vec![saldeo.clone()],
            review_score,
        );
        assert_no_conflicting_references(&report);
        assert_eq!(report.rows.len(), 3, "review_score={review_score}");
    }

    // A mail without a KSeF number may join only one of them.
    mail.ksef_reference = None;
    let report = tri_reconcile(vec![mail], vec![ksef], vec![saldeo], 70);
    assert_no_conflicting_references(&report);
    assert_eq!(report.rows.len(), 2);
    assert_eq!(report.summary.gmail_ksef_missing_saldeo, 1);
    assert_eq!(report.summary.saldeo_only, 1);
}

#[test]
fn shared_ksef_reference_links_records_even_with_other_numbers() {
    let mut mail = invoice(SourceKind::Mail, "mail:1", "FAKTURY", (2026, 9, 4), 100);
    mail.ksef_reference = Some("5242920020-20260904-7B4DFEC00001-B8".into());
    let mut ksef = invoice(SourceKind::Ksef, "ksef:1", "2026/01/1", (2026, 9, 4), 100);
    ksef.ksef_reference = Some(" 5242920020-20260904-7b4dfec00001-b8 ".into());
    let report = tri_reconcile(vec![mail], vec![ksef], vec![], 70);
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.summary.gmail_ksef_missing_saldeo, 1);
}

#[test]
fn invoice_number_partial_needs_token_boundaries() {
    let mut ksef = empty_record(SourceKind::Ksef);
    let mut mail = empty_record(SourceKind::Mail);
    for (a, b) in [
        ("11/2026", "1/2026"),
        ("12/01/2026", "112/01/2026"),
        ("FV/1/2026", "FV/11/2026"),
        ("2026/10", "2026/1"),
    ] {
        ksef.invoice_number = Some(a.into());
        mail.invoice_number = Some(b.into());
        let (score, reasons) = score_pair(&ksef, &mail);
        assert_eq!(score, 0, "{a} vs {b}: reasons={reasons:?}");
        assert!(!invoice_number_token_contains(a, b), "{a} vs {b}");
    }
}

#[test]
fn invoice_number_containment_on_token_boundaries_still_matches() {
    let mut ksef = empty_record(SourceKind::Ksef);
    let mut mail = empty_record(SourceKind::Mail);
    for (a, b, reason, points) in [
        (
            "FV/12/2026",
            "FV/12/2026/A",
            "invoice_number embedded exact",
            45,
        ),
        (
            "Faktura FV/12/2026",
            "FV/12/2026",
            "invoice_number embedded exact",
            45,
        ),
        ("12/2026", "FV/12/2026", "invoice_number partial", 25),
        (
            "fv-7/2026",
            "Faktura FV 7 2026",
            "invoice_number partial",
            25,
        ),
        ("FV_12_2026", "FV/12/2026", "invoice_number exact", 45),
    ] {
        ksef.invoice_number = Some(a.into());
        mail.invoice_number = Some(b.into());
        let (score, reasons) = score_pair(&ksef, &mail);
        assert_eq!(score, points, "{a} vs {b}: reasons={reasons:?}");
        assert_eq!(reasons, vec![reason.to_string()], "{a} vs {b}");
    }
}

#[test]
fn empty_invoice_numbers_do_not_score() {
    let mut ksef = empty_record(SourceKind::Ksef);
    let mut mail = empty_record(SourceKind::Mail);
    ksef.invoice_number = Some("-".into());
    mail.invoice_number = Some("FV/1/2026".into());
    assert_eq!(score_pair(&ksef, &mail).0, 0);
    mail.invoice_number = Some("/".into());
    assert_eq!(score_pair(&ksef, &mail).0, 0);
}

#[test]
fn identity_without_counterparty_nip_needs_matching_amount() {
    let mut mail = invoice(
        SourceKind::Mail,
        "mail:1",
        "1/01/2026",
        (2026, 1, 2),
        999900,
    );
    mail.buyer_tax_id = Some(OWN_NIP.into());
    let mut saldeo = invoice(
        SourceKind::Saldeo,
        "saldeo:1",
        "1/01/2026",
        (2026, 1, 2),
        5000,
    );
    saldeo.seller_tax_id = Some("5260000002".into());

    assert!(!invoice_identity_match(&mail, &saldeo));
    let report = tri_reconcile(vec![mail.clone()], vec![], vec![saldeo.clone()], 70);
    assert_eq!(report.summary.gmail_only, 1);
    assert_eq!(report.summary.saldeo_only, 1);

    // Same gross amount (within 2 gr) corroborates the number.
    saldeo.gross_amount_minor = Some(999901);
    assert!(invoice_identity_match(&mail, &saldeo));
    let report = tri_reconcile(vec![mail.clone()], vec![], vec![saldeo.clone()], 100);
    assert_eq!(report.summary.gmail_saldeo_missing_ksef, 1);

    // Missing amount: no identity, the normal score threshold decides.
    saldeo.gross_amount_minor = None;
    assert!(!invoice_identity_match(&mail, &saldeo));
    let report = tri_reconcile(vec![mail.clone()], vec![], vec![saldeo.clone()], 70);
    assert_eq!(report.rows.len(), 2);
    let report = tri_reconcile(vec![mail.clone()], vec![], vec![saldeo.clone()], 55);
    assert_eq!(report.summary.gmail_saldeo_missing_ksef, 1);

    // Zero amounts do not corroborate anything.
    mail.gross_amount_minor = Some(0);
    saldeo.gross_amount_minor = Some(0);
    assert!(!invoice_identity_match(&mail, &saldeo));
}

#[test]
fn identity_with_matching_counterparty_nip_ignores_amount() {
    let mut mail = invoice(SourceKind::Mail, "mail:1", "FV/3/2026", (2026, 1, 2), 1000);
    mail.seller_tax_id = Some(LANDLORD_NIP.into());
    let mut saldeo = invoice(
        SourceKind::Saldeo,
        "saldeo:1",
        "FV/3/2026",
        (2026, 2, 2),
        9000,
    );
    saldeo.seller_tax_id = Some(LANDLORD_NIP.into());
    assert!(invoice_identity_match(&mail, &saldeo));
}

#[test]
fn best_identity_candidate_wins_regardless_of_order() {
    let mut mail = invoice(SourceKind::Mail, "mail:1", "FV/9/2026", (2026, 4, 1), 12300);
    mail.seller_tax_id = Some(LANDLORD_NIP.into());
    let mut weak = invoice(
        SourceKind::Saldeo,
        "saldeo:weak",
        "FV/9/2026",
        (2026, 8, 1),
        777,
    );
    weak.seller_tax_id = Some(LANDLORD_NIP.into());
    let mut strong = invoice(
        SourceKind::Saldeo,
        "saldeo:strong",
        "FV/9/2026",
        (2026, 4, 1),
        12300,
    );
    strong.seller_tax_id = Some(LANDLORD_NIP.into());
    for saldeo in [
        vec![weak.clone(), strong.clone()],
        vec![strong.clone(), weak.clone()],
    ] {
        let report = tri_reconcile(vec![mail.clone()], vec![], saldeo, 70);
        let row = report
            .rows
            .iter()
            .find(|row| row.mail.is_some())
            .expect("mail row");
        assert_eq!(
            row.saldeo.as_ref().map(|r| r.content_hash.as_str()),
            Some("saldeo:strong")
        );
        assert_eq!(report.summary.saldeo_only, 1);
    }
}

#[test]
fn different_currencies_get_no_amount_points() {
    let mut ksef = empty_record(SourceKind::Ksef);
    ksef.gross_amount_minor = Some(10000);
    ksef.currency = Some("EUR".into());
    let mut mail = empty_record(SourceKind::Mail);
    mail.gross_amount_minor = Some(10000);
    mail.currency = Some("PLN".into());
    let (score, reasons) = score_pair(&ksef, &mail);
    assert_eq!(score, 0, "reasons={reasons:?}");

    mail.currency = Some(" eur".into());
    let (score, reasons) = score_pair(&ksef, &mail);
    assert_eq!(score, 25, "reasons={reasons:?}");

    // One side without a currency still gets the amount points.
    mail.currency = None;
    assert_eq!(score_pair(&ksef, &mail).0, 20);

    // Same number without NIPs: a currency conflict blocks identity.
    ksef.invoice_number = Some("INV-100".into());
    mail.invoice_number = Some("INV-100".into());
    mail.currency = Some("PLN".into());
    assert!(!invoice_identity_match(&ksef, &mail));
}

#[test]
fn exact_invoice_number_compares_tokens_not_stripped_digits() {
    let mut ksef = empty_record(SourceKind::Ksef);
    let mut mail = empty_record(SourceKind::Mail);
    for (a, b) in [
        ("FV12/2026", "FV/12/2026"),
        ("FV12/2026", "fv-12-2026"),
        ("FV/12/2026", "fv-12-2026"),
        ("FV/001/2026", "FV/1/2026"),
        ("FV/0/2026", "FV/000/2026"),
    ] {
        for (x, y) in [(a, b), (b, a)] {
            assert_eq!(comparable_invoice_number(x), comparable_invoice_number(y));
            ksef.invoice_number = Some(x.into());
            mail.invoice_number = Some(y.into());
            let (score, reasons) = score_pair(&ksef, &mail);
            assert_eq!(score, 45, "{x} vs {y}: reasons={reasons:?}");
            assert_eq!(reasons, vec!["invoice_number exact".to_string()]);
        }
    }
    for (a, b) in [("1/1/2026", "11/2026"), ("12/2026", "1/22026")] {
        for (x, y) in [(a, b), (b, a)] {
            assert_ne!(comparable_invoice_number(x), comparable_invoice_number(y));
            ksef.invoice_number = Some(x.into());
            mail.invoice_number = Some(y.into());
            let (score, reasons) = score_pair(&ksef, &mail);
            assert_eq!(score, 0, "{x} vs {y}: reasons={reasons:?}");
        }
    }
    assert_eq!(comparable_invoice_number("-"), "");
}

#[test]
fn identity_uses_token_equality_of_invoice_numbers() {
    let mut left = rent(SourceKind::Mail, "mail:1", "1/1/2026", (2026, 1, 5));
    let mut right = rent(SourceKind::Saldeo, "saldeo:1", "11/2026", (2026, 1, 5));
    assert!(!invoice_identity_match(&left, &right));
    assert!(!invoice_identity_match(&right, &left));
    left.invoice_number = Some("FV12/2026".into());
    right.invoice_number = Some("fv-12-2026".into());
    assert!(invoice_identity_match(&left, &right));
    assert!(invoice_identity_match(&right, &left));
}

#[test]
fn within_source_dedupe_keeps_numbers_that_only_share_digits() {
    let first = rent(SourceKind::Saldeo, "saldeo:a", "1/1/2026", (2026, 1, 5));
    let second = rent(SourceKind::Saldeo, "saldeo:b", "11/2026", (2026, 1, 5));
    assert_ne!(reconcile_dedupe_key(&first), reconcile_dedupe_key(&second));
    let kept = dedupe_reconcile_records(vec![first.clone(), second.clone()]);
    assert_eq!(kept.len(), 2);

    // The same number written differently is still one document.
    let mut same = second.clone();
    same.content_hash = "saldeo:c".into();
    same.invoice_number = Some("1/01/2026".into());
    assert_eq!(reconcile_dedupe_key(&first), reconcile_dedupe_key(&same));
    assert_eq!(dedupe_reconcile_records(vec![first, same]).len(), 1);
}
