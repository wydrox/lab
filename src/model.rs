use crate::*;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SourceKind {
    Ksef,
    Mail,
    Saldeo,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum OutputFormat {
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct InvoiceRecord {
    pub(crate) source: SourceKind,
    pub(crate) source_path: Option<String>,
    pub(crate) content_hash: String,
    pub(crate) invoice_number: Option<String>,
    pub(crate) seller_tax_id: Option<String>,
    pub(crate) buyer_tax_id: Option<String>,
    pub(crate) seller_name: Option<String>,
    pub(crate) buyer_name: Option<String>,
    pub(crate) issue_date: Option<NaiveDate>,
    pub(crate) sale_date: Option<NaiveDate>,
    pub(crate) due_date: Option<NaiveDate>,
    pub(crate) gross_amount_minor: Option<i64>,
    pub(crate) net_amount_minor: Option<i64>,
    pub(crate) vat_amount_minor: Option<i64>,
    pub(crate) currency: Option<String>,
    pub(crate) ksef_reference: Option<String>,
    pub(crate) email_message_id: Option<String>,
    pub(crate) email_subject: Option<String>,
    pub(crate) email_from: Option<String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct KsefSyncResult {
    pub(crate) summary: KsefSyncSummary,
    pub(crate) records: Vec<InvoiceRecord>,
}

#[derive(Debug, Serialize)]
pub(crate) struct KsefSyncSummary {
    pub(crate) year: i32,
    pub(crate) records_count: usize,
    pub(crate) input: String,
    pub(crate) json_output: String,
    pub(crate) jsonl_output: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SaldeoFetchResult {
    pub(crate) summary: SaldeoFetchSummary,
    pub(crate) records: Vec<InvoiceRecord>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SaldeoFetchSummary {
    pub(crate) year: i32,
    pub(crate) documents_count: usize,
    pub(crate) records_count: usize,
    pub(crate) raw_output: String,
    pub(crate) records_output: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TriReconcileReport {
    pub(crate) generated_at: DateTime<Utc>,
    pub(crate) review_score: u8,
    pub(crate) summary: TriSummary,
    pub(crate) rows: Vec<TriRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TriSummary {
    pub(crate) mail_count: usize,
    pub(crate) ksef_count: usize,
    pub(crate) saldeo_count: usize,
    pub(crate) in_all_three: usize,
    pub(crate) gmail_ksef_missing_saldeo: usize,
    pub(crate) gmail_saldeo_missing_ksef: usize,
    pub(crate) gmail_only: usize,
    pub(crate) ksef_saldeo_missing_gmail: usize,
    pub(crate) ksef_only: usize,
    pub(crate) saldeo_only: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TriRow {
    pub(crate) status: String,
    pub(crate) mail_score_to_ksef: Option<u8>,
    pub(crate) mail_score_to_saldeo: Option<u8>,
    pub(crate) ksef_score_to_saldeo: Option<u8>,
    pub(crate) mail: Option<InvoiceRecord>,
    pub(crate) ksef: Option<InvoiceRecord>,
    pub(crate) saldeo: Option<InvoiceRecord>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct SaldeoSyncPlan {
    pub(crate) generated_at: DateTime<Utc>,
    pub(crate) year: i32,
    pub(crate) confirm: bool,
    pub(crate) upload_url: Option<String>,
    pub(crate) summary: SaldeoSyncSummary,
    pub(crate) items: Vec<SaldeoSyncItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ksef_approve: Option<SaldeoApprovePlan>,
    /// Problemy przebiegu, które nie przerwały zapisu planu (np. nieudany refresh Saldeo).
    pub(crate) warnings: Vec<String>,
    /// Liczba rekordów Saldeo, z których policzono plan; `None`, gdy nieznana (plan z TUI).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) saldeo_record_count: Option<usize>,
    /// Baza z rejestrem wysłanych plików (`saldeo_upload_ledger`); upload bez niej odmawia.
    #[serde(skip)]
    pub(crate) ledger_db_path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SaldeoApprovePlan {
    pub(crate) generated_at: DateTime<Utc>,
    pub(crate) year: i32,
    pub(crate) confirm: bool,
    /// Limit z `--max-approve` / `max_approve`; `0` = bez limitu.
    pub(crate) max_approve: usize,
    pub(crate) require_mail: bool,
    pub(crate) summary: SaldeoApproveSummary,
    /// Dokumenty zakwalifikowane do zatwierdzenia (po filtrze `require_mail`).
    pub(crate) document_ids: Vec<i64>,
    /// Dokumenty faktycznie oznaczone w Saldeo w tym przebiegu.
    pub(crate) approved_document_ids: Vec<i64>,
    /// Dokumenty pominięte przez filtr `require_mail`, z powodem.
    pub(crate) skipped: Vec<SaldeoApproveSkip>,
    /// Ustawione, gdy limit wstrzymał zatwierdzanie (także w próbie na sucho).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) blocked_reason: Option<String>,
}

impl SaldeoApprovePlan {
    /// Błąd przebiegu z `confirm`: limit przekroczony, więc nic nie zatwierdzono.
    /// Próba na sucho pokazuje to samo w `blocked_reason`, ale nie kończy się błędem.
    pub(crate) fn blocking_error(&self) -> Option<String> {
        self.blocked_reason.clone().filter(|_| self.confirm)
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct SaldeoApproveSummary {
    pub(crate) pending_count: usize,
    pub(crate) approved_count: usize,
    pub(crate) skipped_count: usize,
    pub(crate) cap_exceeded: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SaldeoApproveSkip {
    pub(crate) document_id: i64,
    pub(crate) invoice_number: Option<String>,
    /// Maska źródeł wiersza (G/K/S), jak w TUI.
    pub(crate) sources: String,
    pub(crate) reason: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SaldeoRepairPlan {
    pub(crate) generated_at: DateTime<Utc>,
    pub(crate) year: i32,
    pub(crate) confirm: bool,
    pub(crate) llm: bool,
    pub(crate) summary: SaldeoRepairSummary,
    pub(crate) items: Vec<SaldeoRepairItem>,
    pub(crate) duplicates: Vec<SaldeoDuplicateGroup>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SaldeoRepairSummary {
    pub(crate) repaired_count: usize,
    pub(crate) duplicate_group_count: usize,
    pub(crate) duplicate_record_count: usize,
    pub(crate) llm_enriched_count: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct SaldeoRepairItem {
    pub(crate) content_hash: String,
    pub(crate) invoice_number: Option<String>,
    pub(crate) changed_fields: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SaldeoDuplicateGroup {
    pub(crate) key: String,
    pub(crate) invoice_number: Option<String>,
    pub(crate) content_hashes: Vec<String>,
    pub(crate) kept_content_hash: String,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct SaldeoSyncSummary {
    pub(crate) total_missing_saldeo: usize,
    pub(crate) uploadable_count: usize,
    pub(crate) missing_file_count: usize,
    pub(crate) already_uploaded_count: usize,
    pub(crate) unconfirmed_count: usize,
    pub(crate) other_year_count: usize,
    pub(crate) uploaded_count: usize,
    /// Pozycje, które w tym przebiegu nie trafiły do Saldeo albo nie mają potwierdzenia.
    pub(crate) failed_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SaldeoSyncItem {
    /// Presence in the source document; independent of inferred buyer/seller fields.
    pub(crate) own_nip_check: own_nip::OwnNipCheck,
    pub(crate) status: String,
    pub(crate) source: String,
    pub(crate) related_sources: Vec<String>,
    pub(crate) invoice_number: Option<String>,
    pub(crate) issue_date: Option<NaiveDate>,
    pub(crate) gross_amount_minor: Option<i64>,
    pub(crate) currency: Option<String>,
    pub(crate) contractor: Option<String>,
    pub(crate) source_path: Option<String>,
    pub(crate) can_upload: bool,
    pub(crate) upload_status: String,
    pub(crate) saldeo_response_status: Option<u16>,
    pub(crate) saldeo_response_body: Option<String>,
    pub(crate) error: Option<String>,
    /// sha256 bajtów pliku — klucz rejestru `saldeo_upload_ledger`.
    pub(crate) file_sha256: Option<String>,
    /// Okres Saldeo planowany, a po uploadzie faktycznie użyty (po fallbacku z zamkniętego miesiąca).
    pub(crate) saldeo_year: Option<i32>,
    pub(crate) saldeo_month: Option<u32>,
    pub(crate) saldeo_doc_upload_id: Option<i64>,
    /// Dlaczego pozycja nie jest wysyłana (rejestr, inny rok).
    pub(crate) skip_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TemporalDiffSummary {
    pub(crate) run_id: i64,
    pub(crate) previous_run_id: Option<i64>,
    pub(crate) added_count: usize,
    pub(crate) removed_count: usize,
    pub(crate) changed_count: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct SyncRunSummary {
    pub(crate) synced: Vec<String>,
    pub(crate) year: i32,
    pub(crate) records_count: usize,
    pub(crate) stored: bool,
}

pub(crate) fn empty_record(source: SourceKind) -> InvoiceRecord {
    InvoiceRecord {
        source,
        source_path: None,
        content_hash: String::new(),
        invoice_number: None,
        seller_tax_id: None,
        buyer_tax_id: None,
        seller_name: None,
        buyer_name: None,
        issue_date: None,
        sale_date: None,
        due_date: None,
        gross_amount_minor: None,
        net_amount_minor: None,
        vat_amount_minor: None,
        currency: None,
        ksef_reference: None,
        email_message_id: None,
        email_subject: None,
        email_from: None,
        warnings: Vec::new(),
    }
}
