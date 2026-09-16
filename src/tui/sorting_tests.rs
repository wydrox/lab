use super::*;
use std::cmp::Ordering;

#[test]
fn missing_values_stay_last_in_both_directions() {
    for descending in [false, true] {
        assert_eq!(
            compare_sort_values(Some(10), None, descending),
            Ordering::Less
        );
        assert_eq!(
            compare_sort_values(None, Some(10), descending),
            Ordering::Greater
        );
        assert_eq!(
            compare_sort_values::<i64>(None, None, descending),
            Ordering::Equal
        );
    }
}

#[test]
fn amount_sort_is_numeric_and_reversible() {
    let mut a = empty_record(SourceKind::Mail);
    let mut b = empty_record(SourceKind::Ksef);
    a.gross_amount_minor = Some(900);
    b.gross_amount_minor = Some(10000);
    assert_eq!(compare_invoice_sort(&a, &b, 4, false), Ordering::Less);
    assert_eq!(compare_invoice_sort(&a, &b, 4, true), Ordering::Greater);
}

#[test]
fn dates_and_text_use_the_selected_column() {
    let mut a = empty_record(SourceKind::Mail);
    let mut b = empty_record(SourceKind::Ksef);
    a.issue_date = NaiveDate::from_ymd_opt(2026, 2, 1);
    b.issue_date = NaiveDate::from_ymd_opt(2026, 1, 1);
    a.invoice_number = Some("abc".into());
    b.invoice_number = Some("ABC".into());
    a.currency = Some("eur".into());
    b.currency = Some("PLN".into());
    assert_eq!(compare_invoice_sort(&a, &b, 1, false), Ordering::Greater);
    assert_eq!(compare_invoice_sort(&a, &b, 2, false), Ordering::Equal);
    assert_eq!(compare_invoice_sort(&a, &b, 5, false), Ordering::Less);
    assert_eq!(compare_invoice_sort(&a, &b, 0, false), Ordering::Equal);
}
