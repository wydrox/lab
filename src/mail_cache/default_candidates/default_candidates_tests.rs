use super::*;

/// Unikalny katalog na test (pid + licznik), usuwany przy drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "lab-mail-sources-{name}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn invoice(hash: &str, number: &str, seller_nip: &str, gross: i64) -> InvoiceRecord {
    let mut record = empty_record(SourceKind::Mail);
    record.content_hash = hash.into();
    record.source_path = Some(format!("data/mail/{hash}.pdf"));
    record.invoice_number = Some(number.into());
    record.seller_tax_id = Some(seller_nip.into());
    record.buyer_tax_id = Some(DEFAULT_PRODUCTMESH_NIP.into());
    record.gross_amount_minor = Some(gross);
    record
}

fn write(path: &Path, records: &[InvoiceRecord]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_records(records, OutputFormat::Jsonl, Some(path)).unwrap();
}

fn hashes(records: &[InvoiceRecord]) -> Vec<&str> {
    records
        .iter()
        .map(|record| record.content_hash.as_str())
        .collect()
}

// --- złączenie ---

#[test]
fn merge_adds_records_found_only_by_the_amazon_preset() {
    let main = vec![invoice("a", "FV/1", "1111111111", 100)];
    let amazon = vec![invoice("b", "IT-2026-7", "2222222222", 200)];
    assert_eq!(hashes(&merge_mail_candidates(main, amazon)), ["a", "b"]);
}

#[test]
fn merge_skips_the_same_pdf_by_content_hash() {
    let main = vec![invoice("a", "FV/1", "1111111111", 100)];
    let mut amazon_copy = invoice("a", "FV/1", "1111111111", 100);
    amazon_copy.source_path = Some("data/mail-amazon-2026-pdfs/a.pdf".into());
    let merged = merge_mail_candidates(main, vec![amazon_copy]);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].source_path.as_deref(), Some("data/mail/a.pdf"));
}

#[test]
fn merge_skips_the_same_invoice_in_another_pdf() {
    // Ten sam numer i NIP sprzedawcy, inny plik (np. ta sama faktura w dwóch wiadomościach).
    let main = vec![invoice("a", "IT-2026-7", "2222222222", 200)];
    let amazon = vec![invoice("b", "it 2026 7", "2222222222", 200)];
    assert_eq!(hashes(&merge_mail_candidates(main, amazon)), ["a"]);
}

#[test]
fn merge_keeps_the_better_record_of_the_same_invoice() {
    let mut weak = invoice("a", "IT-2026-7", "2222222222", 200);
    weak.currency = None;
    weak.issue_date = None;
    let mut strong = invoice("b", "IT-2026-7", "2222222222", 200);
    strong.currency = Some("EUR".into());
    strong.issue_date = NaiveDate::from_ymd_opt(2026, 3, 4);
    assert_eq!(
        hashes(&merge_mail_candidates(vec![weak], vec![strong])),
        ["b"]
    );
}

#[test]
fn merge_keeps_the_same_number_from_another_seller() {
    let main = vec![invoice("a", "1/2026", "1111111111", 100)];
    let amazon = vec![invoice("b", "1/2026", "2222222222", 100)];
    assert_eq!(hashes(&merge_mail_candidates(main, amazon)), ["a", "b"]);
}

// --- pliki ---

#[test]
fn files_without_amazon_candidates_read_the_main_file_only() {
    let tmp = TempDir::new("main-only");
    let main = tmp.0.join("main/candidates.jsonl");
    write(&main, &[invoice("a", "FV/1", "1111111111", 100)]);
    let records = load_mail_candidate_files(&main, &tmp.0.join("amazon/candidates.jsonl")).unwrap();
    assert_eq!(hashes(&records), ["a"]);
}

#[test]
fn files_without_main_candidates_read_the_amazon_file() {
    let tmp = TempDir::new("amazon-only");
    let amazon = tmp.0.join("amazon/candidates.jsonl");
    write(&amazon, &[invoice("b", "IT-1", "2222222222", 200)]);
    let records = load_mail_candidate_files(&tmp.0.join("main/candidates.jsonl"), &amazon).unwrap();
    assert_eq!(hashes(&records), ["b"]);
}

#[test]
fn files_missing_both_is_an_error_as_before() {
    let tmp = TempDir::new("none");
    assert!(
        load_mail_candidate_files(
            &tmp.0.join("main/candidates.jsonl"),
            &tmp.0.join("amazon/candidates.jsonl")
        )
        .is_err()
    );
}

#[test]
fn default_candidates_of_the_year_include_the_amazon_preset() {
    let tmp = TempDir::new("default");
    let _root = set_test_lab_root(&tmp.0);
    write(
        &default_mail_candidates_path(2026),
        &[invoice("a", "FV/1", "1111111111", 100)],
    );
    write(
        &default_amazon_mail_candidates_path(2026),
        &[
            invoice("a", "FV/1", "1111111111", 100),
            invoice("b", "IT-1", "2222222222", 200),
        ],
    );
    assert_eq!(
        hashes(&load_default_mail_candidates(2026).unwrap()),
        ["a", "b"]
    );
    assert_eq!(
        hashes(&load_mail_candidates_or_default(None, 2026).unwrap()),
        ["a", "b"]
    );
}

#[test]
fn explicit_mail_path_is_read_alone() {
    let tmp = TempDir::new("explicit");
    let _root = set_test_lab_root(&tmp.0);
    write(
        &default_amazon_mail_candidates_path(2026),
        &[invoice("b", "IT-1", "2222222222", 200)],
    );
    let explicit = tmp.0.join("custom.jsonl");
    write(&explicit, &[invoice("c", "FV/9", "3333333333", 300)]);
    assert_eq!(
        hashes(&load_mail_candidates_or_default(Some(&explicit), 2026).unwrap()),
        ["c"]
    );
}
