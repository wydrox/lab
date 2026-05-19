use anyhow::{Context, Result, anyhow};
use base64::{Engine, engine::general_purpose::URL_SAFE, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use regex::Regex;
use reqwest::blocking::Client;
use rusqlite::{Connection, params, types::Type};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(name = "lab-cli")]
#[command(about = "LAB — Lazy Accounting Buddy", long_about = None)]
struct Cli {
    /// Dedykowana baza SQLite na rekordy, przebiegi i dopasowania.
    #[arg(long, global = true, default_value = "lab.sqlite")]
    db: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Skanuje katalog/plik z fakturami i wypisuje znormalizowane rekordy.
    Scan {
        /// Źródło danych: ksef albo mail.
        #[arg(long, value_enum)]
        source: SourceKind,
        /// Katalog albo plik wejściowy (.xml, .json, .txt, .eml, .pdf).
        #[arg(long)]
        input: PathBuf,
        /// Format wyjścia.
        #[arg(long, value_enum, default_value = "jsonl")]
        format: OutputFormat,
        /// Plik wyjściowy, domyślnie stdout.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Zapisz znormalizowane rekordy do SQLite.
        #[arg(long)]
        store: bool,
    },
    /// Uzgadnia rekordy KSeF z rekordami mailowymi.
    Reconcile {
        /// Katalog, plik JSONL albo JSON z rekordami KSeF.
        #[arg(long)]
        ksef: PathBuf,
        /// Katalog, plik JSONL albo JSON z rekordami z maila.
        #[arg(long)]
        mail: PathBuf,
        /// Minimalny wynik dla pewnego dopasowania.
        #[arg(long, default_value_t = 70)]
        match_score: u8,
        /// Minimalny wynik dla statusu needs_review.
        #[arg(long, default_value_t = 45)]
        review_score: u8,
        /// Opcjonalny CSV z dopasowaniami.
        #[arg(long)]
        csv: Option<PathBuf>,
        /// Plik JSON z raportem, domyślnie stdout.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Zapisz rekordy i wynik uzgodnienia do SQLite.
        #[arg(long)]
        store: bool,
    },
    /// Uzgadnia rekordy zapisane wcześniej w SQLite przez scan --store.
    ReconcileDb {
        /// Minimalny wynik dla pewnego dopasowania.
        #[arg(long, default_value_t = 70)]
        match_score: u8,
        /// Minimalny wynik dla statusu needs_review.
        #[arg(long, default_value_t = 45)]
        review_score: u8,
        /// Opcjonalny CSV z dopasowaniami.
        #[arg(long)]
        csv: Option<PathBuf>,
        /// Plik JSON z raportem, domyślnie stdout.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Zapisz wynik uzgodnienia do SQLite.
        #[arg(long, default_value_t = true)]
        store: bool,
    },
    /// Autoryzuje Gmail OAuth i zapisuje token poza repo.
    GmailAuth {
        /// Google OAuth Desktop Client JSON.
        #[arg(long)]
        client_secret: PathBuf,
        /// Plik tokenu; domyślnie ~/.config/lab/gmail_token.json.
        #[arg(long)]
        token_file: Option<PathBuf>,
        /// Nie otwieraj przeglądarki automatycznie; tylko wypisz URL.
        #[arg(long)]
        no_browser: bool,
    },
    /// Pobiera załączniki z Gmail API. Używa GMAIL_ACCESS_TOKEN albo token-file.
    GmailFetch {
        /// Zapytanie Gmail, np. "has:attachment filename:pdf newer_than:90d".
        #[arg(long)]
        query: String,
        /// Katalog, do którego zapisać załączniki i metadane wiadomości.
        #[arg(long)]
        out: PathBuf,
        /// Maksymalna liczba wiadomości do pobrania.
        #[arg(long, default_value_t = 50)]
        max: usize,
        /// Użytkownik Gmail API, zwykle "me".
        #[arg(long, default_value = "me")]
        user: String,
        /// Nazwa env var z tokenem OAuth; jeśli ustawiony, ma priorytet.
        #[arg(long, default_value = "GMAIL_ACCESS_TOKEN")]
        token_env: String,
        /// Plik tokenu z gmail-auth; domyślnie ~/.config/lab/gmail_token.json.
        #[arg(long)]
        token_file: Option<PathBuf>,
        /// Google OAuth Desktop Client JSON, potrzebny do odświeżenia tokenu.
        #[arg(long)]
        client_secret: Option<PathBuf>,
        /// Pobierane rozszerzenia załączników.
        #[arg(long, value_delimiter = ',', default_value = "pdf,xml,json,txt")]
        extensions: Vec<String>,
    },
    /// Synchronizuje eksport KSeF do znormalizowanych rekordów LAB.
    KsefSync {
        /// Rok rozliczeniowy.
        #[arg(long, default_value_t = 2026)]
        year: i32,
        /// Katalog, plik JSONL, JSON albo XML z eksportem/rekordami KSeF.
        #[arg(long)]
        input: PathBuf,
        /// Katalog na records.json i records.jsonl.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Zapisz rekordy do SQLite.
        #[arg(long)]
        store: bool,
    },
    /// Planuje albo wykonuje upload brakujących faktur do SaldeoSMART.
    SaldeoSync {
        /// Rok raportu/synchronizacji.
        #[arg(long, default_value_t = 2026)]
        year: i32,
        /// Raport tri-reconcile JSON. Jeśli brak, podaj --mail, --ksef i --saldeo.
        #[arg(long)]
        tri_report: Option<PathBuf>,
        /// JSON/JSONL z rekordami Gmail/PDF.
        #[arg(long)]
        mail: Option<PathBuf>,
        /// JSON/JSONL z rekordami KSeF.
        #[arg(long)]
        ksef: Option<PathBuf>,
        /// Raw documents.json z Saldeo albo JSON/JSONL z rekordami Saldeo.
        #[arg(long)]
        saldeo: Option<PathBuf>,
        /// Minimalny score, gdy raport jest liczony z wejść.
        #[arg(long, default_value_t = 70)]
        review_score: u8,
        /// Plik Playwright storage state z sesją Saldeo.
        #[arg(long)]
        storage_state: Option<PathBuf>,
        /// Endpoint generowania URL-i uploadu Saldeo.
        #[arg(long)]
        upload_url: Option<String>,
        /// Nazwa pola multipart dla starszego trybu; obecny endpoint używa signed URL.
        #[arg(long, default_value = "file")]
        file_field: String,
        /// Wykonaj upload. Bez tej flagi komenda tylko planuje.
        #[arg(long)]
        confirm: bool,
        /// Plik JSON z planem/wynikiem.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Opcjonalny CSV z planem/wynikiem.
        #[arg(long)]
        csv: Option<PathBuf>,
    },
    /// Pobiera dokumenty z SaldeoSMART przez zapisaną sesję webową.
    SaldeoFetch {
        /// Rok dokumentów do pobrania.
        #[arg(long, default_value_t = 2026)]
        year: i32,
        /// Plik Playwright storage state z sesją Saldeo.
        #[arg(long)]
        storage_state: Option<PathBuf>,
        /// Katalog na raw JSON i znormalizowany JSONL.
        #[arg(long, default_value = "data/saldeo-2026")]
        out: PathBuf,
        /// Zapisz rekordy do SQLite.
        #[arg(long)]
        store: bool,
    },
    /// Porównuje trzy źródła: Gmail/PDF, KSeF i Saldeo.
    TriReconcile {
        /// JSON/JSONL z rekordami Gmail/PDF.
        #[arg(long)]
        mail: PathBuf,
        /// JSON/JSONL z rekordami KSeF.
        #[arg(long)]
        ksef: PathBuf,
        /// Raw documents.json z Saldeo albo JSON/JSONL z rekordami Saldeo.
        #[arg(long)]
        saldeo: PathBuf,
        /// Minimalny score dopasowania.
        #[arg(long, default_value_t = 45)]
        review_score: u8,
        /// Plik JSON z raportem.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Opcjonalny CSV z raportem.
        #[arg(long)]
        csv: Option<PathBuf>,
        /// Zapisz temporalny snapshot tri-reconcile w SQLite.
        #[arg(long)]
        store: bool,
        /// Rok snapshotu przy --store.
        #[arg(long, default_value_t = 2026)]
        year: i32,
    },
    /// Cykliczny flow 1/14 miesiąca: Gmail → skan PDF → Saldeo → tri-reconcile → braki dla księgowej.
    Cycle {
        #[command(subcommand)]
        command: CycleCommands,
    },
    /// Uruchamia prosty serwer MCP po stdio dla agentów.
    Mcp,
    /// Operacje na dedykowanej bazie SQLite.
    Db {
        #[command(subcommand)]
        command: DbCommands,
    },
    /// Sprawdza zależności i konfigurację środowiska.
    Doctor {
        /// Nazwa env var z tokenem OAuth do Gmaila.
        #[arg(long, default_value = "GMAIL_ACCESS_TOKEN")]
        token_env: String,
    },
}

#[derive(Subcommand, Debug)]
enum CycleCommands {
    /// Wykonuje cały cykliczny flow dla roku.
    Run {
        /// Rok rozliczeniowy.
        #[arg(long, default_value_t = 2026)]
        year: i32,
        /// NIP ProductMesh używany do filtrowania PDF-ów z Gmaila.
        #[arg(long, default_value = "5242920020")]
        productmesh_nip: String,
        /// Rekordy KSeF JSON/JSONL. Domyślnie data/ksef-productmesh-<year>/ksef_records.jsonl.
        #[arg(long)]
        ksef: Option<PathBuf>,
        /// Google OAuth Desktop Client JSON, potrzebny gdy token Gmail wymaga odświeżenia.
        #[arg(long)]
        gmail_client_secret: Option<PathBuf>,
        /// Plik tokenu Gmail. Domyślnie ~/.config/lab/gmail_token.json.
        #[arg(long)]
        gmail_token_file: Option<PathBuf>,
        /// Zapytanie Gmail. Domyślnie PDF-y z całego roku.
        #[arg(long)]
        gmail_query: Option<String>,
        /// Maksymalna liczba wiadomości Gmail do pobrania.
        #[arg(long, default_value_t = 500)]
        gmail_max: usize,
        /// Pomiń Gmail fetch i użyj istniejącego katalogu PDF.
        #[arg(long)]
        skip_gmail_fetch: bool,
        /// Katalog PDF-ów z Gmaila. Domyślnie data/mail-all-pdf-<year>-pdfs.
        #[arg(long)]
        mail_out: Option<PathBuf>,
        /// Pomiń Saldeo fetch i użyj istniejących data/saldeo-<year>/records.jsonl.
        #[arg(long)]
        skip_saldeo_fetch: bool,
        /// Katalog danych Saldeo. Domyślnie data/saldeo-<year>.
        #[arg(long)]
        saldeo_out: Option<PathBuf>,
        /// Katalog raportów. Domyślnie out/.
        #[arg(long, default_value = "out")]
        out: PathBuf,
        /// Zapisz rekordy do SQLite.
        #[arg(long)]
        store: bool,
        /// Minimalny score dla tri-reconcile.
        #[arg(long, default_value_t = 70)]
        review_score: u8,
        /// Skopiuj braki z Gmaila do folderu non-KSeF files.
        #[arg(long)]
        copy_missing_non_ksef: bool,
        /// Root księgowy. Domyślnie ./ACCOUNTING albo PRODUCTMESH_ACCOUNTING_ROOT.
        #[arg(long)]
        accounting_root: Option<PathBuf>,
    },
    /// Wypisuje/składa raport faktur z Gmaila, których nie ma w Saldeo.
    Missing {
        /// Raport tri-reconcile JSON. Domyślnie out/tri-reconcile-<year>.json.
        #[arg(long)]
        tri_report: Option<PathBuf>,
        /// Rok użyty do domyślnych ścieżek.
        #[arg(long, default_value_t = 2026)]
        year: i32,
        /// Plik JSON z brakami.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Plik CSV z brakami.
        #[arg(long)]
        csv: Option<PathBuf>,
        /// Skopiuj braki do folderu non-KSeF files.
        #[arg(long)]
        copy_non_ksef: bool,
        /// Root księgowy. Domyślnie ./ACCOUNTING albo PRODUCTMESH_ACCOUNTING_ROOT.
        #[arg(long)]
        accounting_root: Option<PathBuf>,
    },
    /// Generuje przykładowe polecenia do uruchamiania 1. i 14. dnia miesiąca.
    Schedule {
        /// Rok rozliczeniowy.
        #[arg(long, default_value_t = 2026)]
        year: i32,
        /// Rekordy KSeF JSON/JSONL.
        #[arg(long)]
        ksef: Option<PathBuf>,
        /// Ścieżka do binarki. Domyślnie bieżący executable.
        #[arg(long)]
        bin: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum DbCommands {
    /// Tworzy tabele w bazie, jeśli jeszcze ich nie ma.
    Init,
    /// Pokazuje liczbę rekordów w bazie.
    Stats,
    /// Wypisuje rekordy faktur z SQLite jako JSON.
    List {
        /// Opcjonalny filtr: ksef albo mail.
        #[arg(long, value_enum)]
        source: Option<SourceKind>,
        /// Maksymalna liczba rekordów.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Lista temporalnych przebiegów tri-reconcile z licznikami diffów.
    TriRuns {
        /// Maksymalna liczba przebiegów.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum SourceKind {
    Ksef,
    Mail,
    Saldeo,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct InvoiceRecord {
    source: SourceKind,
    source_path: Option<String>,
    content_hash: String,
    invoice_number: Option<String>,
    seller_tax_id: Option<String>,
    buyer_tax_id: Option<String>,
    seller_name: Option<String>,
    buyer_name: Option<String>,
    issue_date: Option<NaiveDate>,
    sale_date: Option<NaiveDate>,
    due_date: Option<NaiveDate>,
    gross_amount_minor: Option<i64>,
    net_amount_minor: Option<i64>,
    vat_amount_minor: Option<i64>,
    currency: Option<String>,
    ksef_reference: Option<String>,
    email_message_id: Option<String>,
    email_subject: Option<String>,
    email_from: Option<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ReconcileReport {
    generated_at: DateTime<Utc>,
    match_score: u8,
    review_score: u8,
    summary: ReconcileSummary,
    matches: Vec<InvoiceMatch>,
    unmatched_ksef: Vec<InvoiceRecord>,
    unmatched_mail: Vec<InvoiceRecord>,
}

#[derive(Debug, Serialize)]
struct ReconcileSummary {
    ksef_count: usize,
    mail_count: usize,
    matched_count: usize,
    review_count: usize,
    unmatched_ksef_count: usize,
    unmatched_mail_count: usize,
}

#[derive(Debug, Serialize)]
struct KsefSyncResult {
    summary: KsefSyncSummary,
    records: Vec<InvoiceRecord>,
}

#[derive(Debug, Serialize)]
struct KsefSyncSummary {
    year: i32,
    records_count: usize,
    input: String,
    json_output: String,
    jsonl_output: String,
}

#[derive(Debug, Serialize)]
struct SaldeoFetchResult {
    summary: SaldeoFetchSummary,
    records: Vec<InvoiceRecord>,
}

#[derive(Debug, Serialize)]
struct SaldeoFetchSummary {
    year: i32,
    documents_count: usize,
    records_count: usize,
    raw_output: String,
    records_output: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct TriReconcileReport {
    generated_at: DateTime<Utc>,
    review_score: u8,
    summary: TriSummary,
    rows: Vec<TriRow>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TriSummary {
    mail_count: usize,
    ksef_count: usize,
    saldeo_count: usize,
    in_all_three: usize,
    gmail_ksef_missing_saldeo: usize,
    gmail_saldeo_missing_ksef: usize,
    gmail_only: usize,
    ksef_saldeo_missing_gmail: usize,
    ksef_only: usize,
    saldeo_only: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TriRow {
    status: String,
    mail_score_to_ksef: Option<u8>,
    mail_score_to_saldeo: Option<u8>,
    ksef_score_to_saldeo: Option<u8>,
    mail: Option<InvoiceRecord>,
    ksef: Option<InvoiceRecord>,
    saldeo: Option<InvoiceRecord>,
}

#[derive(Debug, Serialize)]
struct SaldeoSyncPlan {
    generated_at: DateTime<Utc>,
    year: i32,
    confirm: bool,
    upload_url: Option<String>,
    summary: SaldeoSyncSummary,
    items: Vec<SaldeoSyncItem>,
}

#[derive(Debug, Serialize)]
struct SaldeoSyncSummary {
    total_missing_saldeo: usize,
    uploadable_count: usize,
    missing_file_count: usize,
    uploaded_count: usize,
    failed_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct SaldeoSyncItem {
    status: String,
    source: String,
    related_sources: Vec<String>,
    invoice_number: Option<String>,
    issue_date: Option<NaiveDate>,
    gross_amount_minor: Option<i64>,
    currency: Option<String>,
    contractor: Option<String>,
    source_path: Option<String>,
    can_upload: bool,
    upload_status: String,
    saldeo_response_status: Option<u16>,
    saldeo_response_body: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct CycleRunSummary {
    generated_at: DateTime<Utc>,
    year: i32,
    gmail: Option<GmailFetchResult>,
    scanned_mail_pdfs: usize,
    productmesh_candidates: usize,
    ksef_records: usize,
    saldeo_documents: Option<usize>,
    tri_summary: TriSummary,
    temporal_diff: Option<TemporalDiffSummary>,
    missing_for_accountant_count: usize,
    copied_missing_count: usize,
    paths: CycleRunPaths,
}

#[derive(Debug, Serialize)]
struct CycleRunPaths {
    mail_scan_json: String,
    mail_candidates_json: String,
    mail_candidates_jsonl: String,
    ksef_records_jsonl: String,
    saldeo_records_jsonl: String,
    tri_json: String,
    tri_csv: String,
    missing_json: String,
    missing_csv: String,
    non_ksef_dir: String,
}

#[derive(Debug, Clone, Serialize)]
struct TemporalDiffSummary {
    run_id: i64,
    previous_run_id: Option<i64>,
    added_count: usize,
    removed_count: usize,
    changed_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MissingInvoice {
    invoice_number: String,
    contractor: String,
    issue_date: Option<NaiveDate>,
    gross_amount_minor: Option<i64>,
    amount: String,
    currency: String,
    source_path: String,
    target_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct MissingReport {
    generated_at: DateTime<Utc>,
    count: usize,
    invoices: Vec<MissingInvoice>,
}

#[derive(Debug, Serialize)]
struct CycleSchedule {
    note: String,
    cron_line: String,
    command: String,
}

#[derive(Debug, Clone, Serialize)]
struct InvoiceMatch {
    status: MatchStatus,
    score: u8,
    reasons: Vec<String>,
    ksef: InvoiceRecord,
    mail: InvoiceRecord,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MatchStatus {
    Matched,
    NeedsReview,
}

#[derive(Debug, Clone)]
struct Candidate {
    ksef_idx: usize,
    mail_idx: usize,
    score: u8,
    reasons: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let db_path = cli.db;
    match cli.command {
        Commands::Scan {
            source,
            input,
            format,
            output,
            store,
        } => {
            let records = scan_input(source, &input)?;
            if store {
                let conn = open_db(&db_path)?;
                store_records(&conn, &records)?;
            }
            write_records(&records, format, output.as_deref())?;
        }
        Commands::Reconcile {
            ksef,
            mail,
            match_score,
            review_score,
            csv,
            output,
            store,
        } => {
            if review_score > match_score {
                return Err(anyhow!("review-score nie może być większy niż match-score"));
            }
            let ksef_records = load_records(SourceKind::Ksef, &ksef)?;
            let mail_records = load_records(SourceKind::Mail, &mail)?;
            let report = reconcile(ksef_records, mail_records, match_score, review_score);
            if store {
                let conn = open_db(&db_path)?;
                store_reconcile_report(&conn, &report)?;
            }
            if let Some(csv_path) = csv {
                write_matches_csv(&report.matches, &csv_path)?;
            }
            write_json(&report, output.as_deref())?;
        }
        Commands::ReconcileDb {
            match_score,
            review_score,
            csv,
            output,
            store,
        } => {
            if review_score > match_score {
                return Err(anyhow!("review-score nie może być większy niż match-score"));
            }
            let conn = open_db(&db_path)?;
            let ksef_records = load_records_from_db(&conn, Some(SourceKind::Ksef), None)?;
            let mail_records = load_records_from_db(&conn, Some(SourceKind::Mail), None)?;
            let report = reconcile(ksef_records, mail_records, match_score, review_score);
            if store {
                store_reconcile_report(&conn, &report)?;
            }
            if let Some(csv_path) = csv {
                write_matches_csv(&report.matches, &csv_path)?;
            }
            write_json(&report, output.as_deref())?;
        }
        Commands::GmailAuth {
            client_secret,
            token_file,
            no_browser,
        } => {
            let token_path = token_file.unwrap_or_else(default_gmail_token_path);
            let result = gmail_auth(&client_secret, &token_path, no_browser)?;
            write_json(&result, None)?;
        }
        Commands::GmailFetch {
            query,
            out,
            max,
            user,
            token_env,
            token_file,
            client_secret,
            extensions,
        } => {
            let token_path = token_file.unwrap_or_else(default_gmail_token_path);
            let token = gmail_access_token(&token_env, &token_path, client_secret.as_deref())?;
            let result = gmail_fetch(&token, &user, &query, &out, max, &extensions)?;
            write_json(&result, None)?;
        }
        Commands::KsefSync {
            year,
            input,
            out,
            store,
        } => {
            let result = ksef_sync(year, &input, out.as_deref())?;
            if store {
                let conn = open_db(&db_path)?;
                store_records(&conn, &result.records)?;
            }
            write_json(&result.summary, None)?;
        }
        Commands::SaldeoSync {
            year,
            tri_report,
            mail,
            ksef,
            saldeo,
            review_score,
            storage_state,
            upload_url,
            file_field,
            confirm,
            output,
            csv,
        } => {
            let mut plan = saldeo_sync_plan(SaldeoSyncPlanConfig {
                year,
                tri_report: tri_report.as_deref(),
                mail: mail.as_deref(),
                ksef: ksef.as_deref(),
                saldeo: saldeo.as_deref(),
                review_score,
                confirm,
                upload_url: upload_url.clone(),
            })?;
            if confirm {
                let upload_url = upload_url.as_deref().unwrap_or(DEFAULT_SALDEO_UPLOAD_URL);
                let storage_state = storage_state.unwrap_or_else(default_saldeo_storage_state_path);
                saldeo_upload_plan(&mut plan, &storage_state, upload_url, &file_field)?;
            }
            if let Some(csv_path) = csv {
                write_saldeo_sync_csv(&plan, &csv_path)?;
            }
            write_json(&plan, output.as_deref())?;
        }
        Commands::SaldeoFetch {
            year,
            storage_state,
            out,
            store,
        } => {
            let storage_state = storage_state.unwrap_or_else(default_saldeo_storage_state_path);
            let result = saldeo_fetch(year, &storage_state, &out)?;
            if store {
                let conn = open_db(&db_path)?;
                store_records(&conn, &result.records)?;
            }
            write_json(&result.summary, None)?;
        }
        Commands::TriReconcile {
            mail,
            ksef,
            saldeo,
            review_score,
            output,
            csv,
            store,
            year,
        } => {
            let mail_records = load_records(SourceKind::Mail, &mail)?;
            let ksef_records = load_records(SourceKind::Ksef, &ksef)?;
            let saldeo_records = load_saldeo_records(&saldeo)?;
            let report = tri_reconcile(mail_records, ksef_records, saldeo_records, review_score);
            let temporal_diff = if store {
                let conn = open_db(&db_path)?;
                Some(store_tri_reconcile_report(&conn, year, &report)?)
            } else {
                None
            };
            if let Some(csv_path) = csv {
                write_tri_csv(&report, &csv_path)?;
            }
            if temporal_diff.is_some() && output.is_none() {
                write_json(
                    &serde_json::json!({"report": report, "temporal_diff": temporal_diff}),
                    None,
                )?;
            } else {
                write_json(&report, output.as_deref())?;
            }
        }
        Commands::Cycle { command } => handle_cycle_command(&db_path, command)?,
        Commands::Mcp => run_mcp_server(&db_path)?,
        Commands::Db { command } => handle_db_command(&db_path, command)?,
        Commands::Doctor { token_env } => doctor(&token_env)?,
    }
    Ok(())
}

fn write_records(
    records: &[InvoiceRecord],
    format: OutputFormat,
    output: Option<&Path>,
) -> Result<()> {
    let bytes = match format {
        OutputFormat::Json => serde_json::to_vec_pretty(records)?,
        OutputFormat::Jsonl => {
            let mut out = Vec::new();
            for record in records {
                serde_json::to_writer(&mut out, record)?;
                out.push(b'\n');
            }
            out
        }
    };
    write_bytes(&bytes, output)
}

fn write_json<T: Serialize>(value: &T, output: Option<&Path>) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_bytes(&bytes, output)?;
    if output.is_none() {
        println!();
    }
    Ok(())
}

fn write_bytes(bytes: &[u8], output: Option<&Path>) -> Result<()> {
    match output {
        Some(path) => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent)
                    .with_context(|| format!("mkdir {}", parent.display()))?;
            }
            fs::write(path, bytes).with_context(|| format!("zapis {}", path.display()))
        }
        None => {
            io::stdout().write_all(bytes)?;
            Ok(())
        }
    }
}

fn open_db(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let conn =
        Connection::open(path).with_context(|| format!("otwarcie SQLite {}", path.display()))?;
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS invoices (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source TEXT NOT NULL,
            source_path TEXT,
            content_hash TEXT NOT NULL,
            invoice_number TEXT,
            seller_tax_id TEXT,
            buyer_tax_id TEXT,
            seller_name TEXT,
            buyer_name TEXT,
            issue_date TEXT,
            sale_date TEXT,
            due_date TEXT,
            gross_amount_minor INTEGER,
            net_amount_minor INTEGER,
            vat_amount_minor INTEGER,
            currency TEXT,
            ksef_reference TEXT,
            email_message_id TEXT,
            email_subject TEXT,
            email_from TEXT,
            warnings_json TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(source, content_hash)
        );
        CREATE INDEX IF NOT EXISTS idx_invoices_source ON invoices(source);
        CREATE INDEX IF NOT EXISTS idx_invoices_invoice_number ON invoices(invoice_number);
        CREATE INDEX IF NOT EXISTS idx_invoices_tax_ids ON invoices(seller_tax_id, buyer_tax_id);
        CREATE TABLE IF NOT EXISTS reconcile_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            generated_at TEXT NOT NULL,
            match_score INTEGER NOT NULL,
            review_score INTEGER NOT NULL,
            ksef_count INTEGER NOT NULL,
            mail_count INTEGER NOT NULL,
            matched_count INTEGER NOT NULL,
            review_count INTEGER NOT NULL,
            unmatched_ksef_count INTEGER NOT NULL,
            unmatched_mail_count INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS invoice_matches (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id INTEGER NOT NULL REFERENCES reconcile_runs(id) ON DELETE CASCADE,
            status TEXT NOT NULL,
            score INTEGER NOT NULL,
            reasons_json TEXT NOT NULL,
            ksef_invoice_id INTEGER NOT NULL REFERENCES invoices(id),
            mail_invoice_id INTEGER NOT NULL REFERENCES invoices(id)
        );
        CREATE TABLE IF NOT EXISTS tri_reconcile_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            generated_at TEXT NOT NULL,
            year INTEGER NOT NULL,
            review_score INTEGER NOT NULL,
            mail_count INTEGER NOT NULL,
            ksef_count INTEGER NOT NULL,
            saldeo_count INTEGER NOT NULL,
            summary_json TEXT NOT NULL,
            report_hash TEXT NOT NULL,
            previous_run_id INTEGER,
            added_count INTEGER NOT NULL,
            removed_count INTEGER NOT NULL,
            changed_count INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_tri_reconcile_runs_year ON tri_reconcile_runs(year, id);
        CREATE TABLE IF NOT EXISTS tri_reconcile_rows (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id INTEGER NOT NULL REFERENCES tri_reconcile_runs(id) ON DELETE CASCADE,
            row_key TEXT NOT NULL,
            row_hash TEXT NOT NULL,
            status TEXT NOT NULL,
            mail_invoice_number TEXT,
            ksef_invoice_number TEXT,
            saldeo_invoice_number TEXT,
            issue_date TEXT,
            gross_amount_minor INTEGER,
            currency TEXT,
            row_json TEXT NOT NULL,
            UNIQUE(run_id, row_key)
        );
        CREATE INDEX IF NOT EXISTS idx_tri_reconcile_rows_run_key ON tri_reconcile_rows(run_id, row_key);
        "#,
    )?;
    ensure_invoice_columns(&conn)?;
    Ok(conn)
}

fn ensure_invoice_columns(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(invoices)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = HashSet::new();
    for row in rows {
        columns.insert(row?);
    }
    for (name, sql_type) in [
        ("seller_name", "TEXT"),
        ("buyer_name", "TEXT"),
        ("sale_date", "TEXT"),
        ("due_date", "TEXT"),
    ] {
        if !columns.contains(name) {
            conn.execute(
                &format!("ALTER TABLE invoices ADD COLUMN {name} {sql_type}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn source_as_str(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Ksef => "ksef",
        SourceKind::Mail => "mail",
        SourceKind::Saldeo => "saldeo",
    }
}

fn source_from_db(value: &str) -> rusqlite::Result<SourceKind> {
    match value {
        "ksef" => Ok(SourceKind::Ksef),
        "mail" => Ok(SourceKind::Mail),
        "saldeo" => Ok(SourceKind::Saldeo),
        _ => Err(rusqlite::Error::InvalidColumnType(
            0,
            "source".to_string(),
            Type::Text,
        )),
    }
}

fn match_status_as_str(status: &MatchStatus) -> &'static str {
    match status {
        MatchStatus::Matched => "matched",
        MatchStatus::NeedsReview => "needs_review",
    }
}

fn store_records(conn: &Connection, records: &[InvoiceRecord]) -> Result<Vec<i64>> {
    records
        .iter()
        .map(|record| upsert_invoice(conn, record))
        .collect()
}

fn upsert_invoice(conn: &Connection, record: &InvoiceRecord) -> Result<i64> {
    let now = Utc::now().to_rfc3339();
    let issue_date = record.issue_date.map(|d| d.to_string());
    let sale_date = record.sale_date.map(|d| d.to_string());
    let due_date = record.due_date.map(|d| d.to_string());
    let warnings_json = serde_json::to_string(&record.warnings)?;
    conn.execute(
        r#"
        INSERT INTO invoices (
            source, source_path, content_hash, invoice_number, seller_tax_id, buyer_tax_id,
            seller_name, buyer_name, issue_date, sale_date, due_date, gross_amount_minor,
            net_amount_minor, vat_amount_minor, currency, ksef_reference, email_message_id,
            email_subject, email_from, warnings_json, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?21)
        ON CONFLICT(source, content_hash) DO UPDATE SET
            source_path = excluded.source_path,
            invoice_number = excluded.invoice_number,
            seller_tax_id = excluded.seller_tax_id,
            buyer_tax_id = excluded.buyer_tax_id,
            seller_name = excluded.seller_name,
            buyer_name = excluded.buyer_name,
            issue_date = excluded.issue_date,
            sale_date = excluded.sale_date,
            due_date = excluded.due_date,
            gross_amount_minor = excluded.gross_amount_minor,
            net_amount_minor = excluded.net_amount_minor,
            vat_amount_minor = excluded.vat_amount_minor,
            currency = excluded.currency,
            ksef_reference = excluded.ksef_reference,
            email_message_id = excluded.email_message_id,
            email_subject = excluded.email_subject,
            email_from = excluded.email_from,
            warnings_json = excluded.warnings_json,
            updated_at = excluded.updated_at
        "#,
        params![
            source_as_str(record.source),
            record.source_path,
            record.content_hash,
            record.invoice_number,
            record.seller_tax_id,
            record.buyer_tax_id,
            record.seller_name,
            record.buyer_name,
            issue_date,
            sale_date,
            due_date,
            record.gross_amount_minor,
            record.net_amount_minor,
            record.vat_amount_minor,
            record.currency,
            record.ksef_reference,
            record.email_message_id,
            record.email_subject,
            record.email_from,
            warnings_json,
            now,
        ],
    )?;
    let id = conn.query_row(
        "SELECT id FROM invoices WHERE source = ?1 AND content_hash = ?2",
        params![source_as_str(record.source), record.content_hash],
        |row| row.get(0),
    )?;
    Ok(id)
}

fn load_records_from_db(
    conn: &Connection,
    source: Option<SourceKind>,
    limit: Option<usize>,
) -> Result<Vec<InvoiceRecord>> {
    let limit = limit.unwrap_or(usize::MAX).min(i64::MAX as usize) as i64;
    let mut records = Vec::new();
    if let Some(source) = source {
        let mut stmt = conn.prepare(
            r#"
            SELECT source, source_path, content_hash, invoice_number, seller_tax_id, buyer_tax_id,
                   seller_name, buyer_name, issue_date, sale_date, due_date, gross_amount_minor,
                   net_amount_minor, vat_amount_minor, currency, ksef_reference, email_message_id,
                   email_subject, email_from, warnings_json
            FROM invoices WHERE source = ?1 ORDER BY updated_at DESC, id DESC LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![source_as_str(source), limit], invoice_from_row)?;
        for row in rows {
            records.push(row?);
        }
    } else {
        let mut stmt = conn.prepare(
            r#"
            SELECT source, source_path, content_hash, invoice_number, seller_tax_id, buyer_tax_id,
                   seller_name, buyer_name, issue_date, sale_date, due_date, gross_amount_minor,
                   net_amount_minor, vat_amount_minor, currency, ksef_reference, email_message_id,
                   email_subject, email_from, warnings_json
            FROM invoices ORDER BY updated_at DESC, id DESC LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(params![limit], invoice_from_row)?;
        for row in rows {
            records.push(row?);
        }
    }
    Ok(records)
}

fn invoice_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InvoiceRecord> {
    let source_text: String = row.get(0)?;
    let issue_date_text: Option<String> = row.get(8)?;
    let sale_date_text: Option<String> = row.get(9)?;
    let due_date_text: Option<String> = row.get(10)?;
    let warnings_json: String = row.get(19)?;
    Ok(InvoiceRecord {
        source: source_from_db(&source_text)?,
        source_path: row.get(1)?,
        content_hash: row.get(2)?,
        invoice_number: row.get(3)?,
        seller_tax_id: row.get(4)?,
        buyer_tax_id: row.get(5)?,
        seller_name: row.get(6)?,
        buyer_name: row.get(7)?,
        issue_date: issue_date_text.as_deref().and_then(parse_date),
        sale_date: sale_date_text.as_deref().and_then(parse_date),
        due_date: due_date_text.as_deref().and_then(parse_date),
        gross_amount_minor: row.get(11)?,
        net_amount_minor: row.get(12)?,
        vat_amount_minor: row.get(13)?,
        currency: row.get(14)?,
        ksef_reference: row.get(15)?,
        email_message_id: row.get(16)?,
        email_subject: row.get(17)?,
        email_from: row.get(18)?,
        warnings: serde_json::from_str(&warnings_json).unwrap_or_default(),
    })
}

fn store_reconcile_report(conn: &Connection, report: &ReconcileReport) -> Result<i64> {
    let run_id = insert_reconcile_run(conn, report)?;
    for item in &report.matches {
        let ksef_id = upsert_invoice(conn, &item.ksef)?;
        let mail_id = upsert_invoice(conn, &item.mail)?;
        conn.execute(
            r#"
            INSERT INTO invoice_matches (run_id, status, score, reasons_json, ksef_invoice_id, mail_invoice_id)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                run_id,
                match_status_as_str(&item.status),
                item.score,
                serde_json::to_string(&item.reasons)?,
                ksef_id,
                mail_id,
            ],
        )?;
    }
    store_records(conn, &report.unmatched_ksef)?;
    store_records(conn, &report.unmatched_mail)?;
    Ok(run_id)
}

fn insert_reconcile_run(conn: &Connection, report: &ReconcileReport) -> Result<i64> {
    conn.execute(
        r#"
        INSERT INTO reconcile_runs (
            generated_at, match_score, review_score, ksef_count, mail_count, matched_count,
            review_count, unmatched_ksef_count, unmatched_mail_count
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        "#,
        params![
            report.generated_at.to_rfc3339(),
            report.match_score,
            report.review_score,
            report.summary.ksef_count as i64,
            report.summary.mail_count as i64,
            report.summary.matched_count as i64,
            report.summary.review_count as i64,
            report.summary.unmatched_ksef_count as i64,
            report.summary.unmatched_mail_count as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn store_tri_reconcile_report(
    conn: &Connection,
    year: i32,
    report: &TriReconcileReport,
) -> Result<TemporalDiffSummary> {
    let previous_run_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM tri_reconcile_runs WHERE year = ?1 ORDER BY id DESC LIMIT 1",
            params![year],
            |row| row.get(0),
        )
        .ok();
    let previous_rows = if let Some(run_id) = previous_run_id {
        load_tri_row_hashes(conn, run_id)?
    } else {
        HashMap::new()
    };
    let current_rows = tri_row_hashes(report)?;
    let added_count = current_rows
        .keys()
        .filter(|key| !previous_rows.contains_key(*key))
        .count();
    let removed_count = previous_rows
        .keys()
        .filter(|key| !current_rows.contains_key(*key))
        .count();
    let changed_count = current_rows
        .iter()
        .filter(|(key, hash)| previous_rows.get(*key).is_some_and(|old| old != *hash))
        .count();
    let report_json = serde_json::to_vec(report)?;
    let report_hash = hex::encode(Sha256::digest(&report_json));
    conn.execute(
        r#"
        INSERT INTO tri_reconcile_runs (
            generated_at, year, review_score, mail_count, ksef_count, saldeo_count,
            summary_json, report_hash, previous_run_id, added_count, removed_count, changed_count
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        "#,
        params![
            report.generated_at.to_rfc3339(),
            year,
            report.review_score,
            report.summary.mail_count as i64,
            report.summary.ksef_count as i64,
            report.summary.saldeo_count as i64,
            serde_json::to_string(&report.summary)?,
            report_hash,
            previous_run_id,
            added_count as i64,
            removed_count as i64,
            changed_count as i64,
        ],
    )?;
    let run_id = conn.last_insert_rowid();
    for row in &report.rows {
        let row_key = tri_row_key(row);
        let row_json = serde_json::to_string(row)?;
        let row_hash = hex::encode(Sha256::digest(row_json.as_bytes()));
        let primary = row
            .mail
            .as_ref()
            .or(row.ksef.as_ref())
            .or(row.saldeo.as_ref());
        conn.execute(
            r#"
            INSERT INTO tri_reconcile_rows (
                run_id, row_key, row_hash, status, mail_invoice_number, ksef_invoice_number,
                saldeo_invoice_number, issue_date, gross_amount_minor, currency, row_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                run_id,
                row_key,
                row_hash,
                row.status,
                row.mail.as_ref().and_then(|r| r.invoice_number.clone()),
                row.ksef.as_ref().and_then(|r| r.invoice_number.clone()),
                row.saldeo.as_ref().and_then(|r| r.invoice_number.clone()),
                primary.and_then(|r| r.issue_date).map(|d| d.to_string()),
                primary.and_then(|r| r.gross_amount_minor),
                primary.and_then(|r| r.currency.clone()),
                row_json,
            ],
        )?;
    }
    Ok(TemporalDiffSummary {
        run_id,
        previous_run_id,
        added_count,
        removed_count,
        changed_count,
    })
}

fn load_tri_row_hashes(conn: &Connection, run_id: i64) -> Result<HashMap<String, String>> {
    let mut stmt =
        conn.prepare("SELECT row_key, row_hash FROM tri_reconcile_rows WHERE run_id = ?1")?;
    let rows = stmt.query_map(params![run_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (key, hash) = row?;
        out.insert(key, hash);
    }
    Ok(out)
}

fn tri_row_hashes(report: &TriReconcileReport) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for row in &report.rows {
        let json = serde_json::to_string(row)?;
        out.insert(
            tri_row_key(row),
            hex::encode(Sha256::digest(json.as_bytes())),
        );
    }
    Ok(out)
}

fn tri_row_key(row: &TriRow) -> String {
    for record in [row.ksef.as_ref(), row.mail.as_ref(), row.saldeo.as_ref()]
        .into_iter()
        .flatten()
    {
        if let Some(reference) = &record.ksef_reference {
            return format!("ksef:{reference}");
        }
    }
    let primary = row
        .mail
        .as_ref()
        .or(row.ksef.as_ref())
        .or(row.saldeo.as_ref());
    if let Some(record) = primary {
        return format!(
            "inv:{}|date:{}|gross:{}|cur:{}",
            record.invoice_number.clone().unwrap_or_default(),
            record.issue_date.map(|d| d.to_string()).unwrap_or_default(),
            record
                .gross_amount_minor
                .map(|v| v.to_string())
                .unwrap_or_default(),
            record.currency.clone().unwrap_or_default()
        );
    }
    "empty".to_string()
}

fn handle_db_command(path: &Path, command: DbCommands) -> Result<()> {
    let conn = open_db(path)?;
    match command {
        DbCommands::Init => write_json(
            &serde_json::json!({ "db": path, "initialized": true }),
            None,
        ),
        DbCommands::Stats => write_json(&db_stats(&conn)?, None),
        DbCommands::List { source, limit } => {
            let records = load_records_from_db(&conn, source, Some(limit))?;
            write_json(&records, None)
        }
        DbCommands::TriRuns { limit } => write_json(&list_tri_runs(&conn, limit)?, None),
    }
}

fn list_tri_runs(conn: &Connection, limit: usize) -> Result<Value> {
    let mut stmt = conn.prepare(
        r#"
        SELECT id, generated_at, year, mail_count, ksef_count, saldeo_count,
               previous_run_id, added_count, removed_count, changed_count, report_hash
        FROM tri_reconcile_runs
        ORDER BY id DESC
        LIMIT ?1
        "#,
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, i64>(0)?,
            "generated_at": row.get::<_, String>(1)?,
            "year": row.get::<_, i64>(2)?,
            "mail_count": row.get::<_, i64>(3)?,
            "ksef_count": row.get::<_, i64>(4)?,
            "saldeo_count": row.get::<_, i64>(5)?,
            "previous_run_id": row.get::<_, Option<i64>>(6)?,
            "added_count": row.get::<_, i64>(7)?,
            "removed_count": row.get::<_, i64>(8)?,
            "changed_count": row.get::<_, i64>(9)?,
            "report_hash": row.get::<_, String>(10)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(serde_json::json!(out))
}

fn db_stats(conn: &Connection) -> Result<Value> {
    let mut stmt =
        conn.prepare("SELECT source, COUNT(*) FROM invoices GROUP BY source ORDER BY source")?;
    let mut by_source = serde_json::Map::new();
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (source, count) = row?;
        by_source.insert(source, serde_json::json!(count));
    }
    let runs: i64 = conn.query_row("SELECT COUNT(*) FROM reconcile_runs", [], |row| row.get(0))?;
    let matches: i64 =
        conn.query_row("SELECT COUNT(*) FROM invoice_matches", [], |row| row.get(0))?;
    let tri_runs: i64 = conn.query_row("SELECT COUNT(*) FROM tri_reconcile_runs", [], |row| {
        row.get(0)
    })?;
    let tri_rows: i64 = conn.query_row("SELECT COUNT(*) FROM tri_reconcile_rows", [], |row| {
        row.get(0)
    })?;
    Ok(serde_json::json!({
        "invoices_by_source": by_source,
        "reconcile_runs": runs,
        "invoice_matches": matches,
        "tri_reconcile_runs": tri_runs,
        "tri_reconcile_rows": tri_rows,
    }))
}

fn load_records(source: SourceKind, input: &Path) -> Result<Vec<InvoiceRecord>> {
    if input.is_dir() {
        return scan_input(source, input);
    }
    if input.extension().and_then(|e| e.to_str()) == Some("jsonl") {
        let file = fs::File::open(input).with_context(|| format!("odczyt {}", input.display()))?;
        let reader = io::BufReader::new(file);
        let mut records = Vec::new();
        for (idx, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let mut record: InvoiceRecord = serde_json::from_str(&line).with_context(|| {
                format!("niepoprawny JSONL {} linia {}", input.display(), idx + 1)
            })?;
            record.source = source;
            records.push(record);
        }
        return Ok(records);
    }
    if input.extension().and_then(|e| e.to_str()) == Some("json") {
        let text =
            fs::read_to_string(input).with_context(|| format!("odczyt {}", input.display()))?;
        if let Ok(mut records) = serde_json::from_str::<Vec<InvoiceRecord>>(&text) {
            for record in &mut records {
                record.source = source;
            }
            return Ok(records);
        }
    }
    scan_input(source, input)
}

fn scan_input(source: SourceKind, input: &Path) -> Result<Vec<InvoiceRecord>> {
    let mut files = Vec::new();
    if input.is_dir() {
        for entry in WalkDir::new(input).into_iter().filter_map(Result::ok) {
            let path = entry.path();
            if entry.file_type().is_file() && is_supported_file(path) {
                files.push(path.to_path_buf());
            }
        }
    } else if input.is_file() {
        files.push(input.to_path_buf());
    } else {
        return Err(anyhow!("input nie istnieje: {}", input.display()));
    }

    files.sort();
    files
        .iter()
        .map(|path| parse_file(source, path))
        .collect::<Result<Vec<_>>>()
}

fn is_supported_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("xml" | "json" | "txt" | "eml" | "pdf")
    )
}

fn parse_file(source: SourceKind, path: &Path) -> Result<InvoiceRecord> {
    let bytes = fs::read(path).with_context(|| format!("odczyt {}", path.display()))?;
    let hash = hex::encode(Sha256::digest(&bytes));
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let mut warnings = Vec::new();
    let text = match ext.as_str() {
        "pdf" => match extract_pdf_text(path) {
            Ok(text) if !text.trim().is_empty() => text,
            Ok(_) => {
                warnings.push("PDF bez tekstu; użyto tylko nazwy pliku i hasha".to_string());
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string()
            }
            Err(err) => {
                warnings.push(format!("nie udało się wyciągnąć tekstu PDF: {err}"));
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string()
            }
        },
        _ => String::from_utf8_lossy(&bytes).to_string(),
    };

    let mut record = if ext == "json" {
        parse_json_invoice(source, &text).unwrap_or_else(|err| {
            let mut record = parse_text_invoice(source, &text);
            record
                .warnings
                .push(format!("JSON sparsowany jako tekst: {err}"));
            record
        })
    } else if ext == "xml" {
        parse_xml_invoice(source, &text)
    } else {
        parse_text_invoice(source, &text)
    };

    record.source_path = Some(path.display().to_string());
    record.content_hash = hash;
    record.warnings.extend(warnings);
    if record.invoice_number.is_none() {
        record.invoice_number = invoice_number_from_filename(path);
    }
    Ok(record)
}

fn empty_record(source: SourceKind) -> InvoiceRecord {
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

fn parse_xml_invoice(source: SourceKind, text: &str) -> InvoiceRecord {
    let mut record = empty_record(source);
    record.invoice_number = first_xml_text(
        text,
        &[
            "P_2",
            "NumerFaktury",
            "InvoiceNumber",
            "invoiceNumber",
            "NrFaktury",
            "number",
        ],
    )
    .map(|v| clean_invoice_number(&v));
    record.issue_date = first_xml_text(text, &["P_1", "DataWystawienia", "IssueDate", "issueDate"])
        .and_then(|v| parse_date(&v));
    record.sale_date = first_xml_text(text, &["P_6", "DataSprzedazy", "SaleDate", "saleDate"])
        .and_then(|v| parse_date(&v));
    record.due_date = first_xml_text(text, &["TerminPlatnosci", "DueDate", "PaymentDueDate"])
        .and_then(|v| parse_date(&v));
    record.gross_amount_minor = first_xml_text(
        text,
        &[
            "P_15",
            "KwotaNaleznosciOgolna",
            "GrossAmount",
            "grossAmount",
            "totalGross",
        ],
    )
    .and_then(|v| parse_money_minor(&v));
    record.net_amount_minor =
        first_xml_text(text, &["P_13_1", "NetAmount", "netAmount", "totalNet"])
            .and_then(|v| parse_money_minor(&v));
    record.vat_amount_minor =
        first_xml_text(text, &["P_14_1", "VatAmount", "vatAmount", "totalVat"])
            .and_then(|v| parse_money_minor(&v));
    record.currency = first_xml_text(text, &["KodWaluty", "Currency", "currency"])
        .map(|v| v.trim().to_ascii_uppercase());
    record.ksef_reference = first_xml_text(
        text,
        &["NrKSeF", "KsefNumber", "KSeFNumber", "ReferenceNumber"],
    )
    .map(|v| v.trim().to_string());

    if let Some(block) = first_xml_block(text, &["Podmiot1", "Seller", "Sprzedawca"]) {
        record.seller_tax_id = first_xml_text(&block, &["NIP", "TaxId", "VATID", "VatId"])
            .and_then(|v| normalize_tax_id(&v));
        record.seller_name =
            first_xml_text(&block, &["Nazwa", "Name", "FullName"]).and_then(|v| clean_name(&v));
    }
    if let Some(block) = first_xml_block(text, &["Podmiot2", "Buyer", "Nabywca"]) {
        record.buyer_tax_id = first_xml_text(&block, &["NIP", "TaxId", "VATID", "VatId"])
            .and_then(|v| normalize_tax_id(&v));
        record.buyer_name =
            first_xml_text(&block, &["Nazwa", "Name", "FullName"]).and_then(|v| clean_name(&v));
    }

    if record.seller_tax_id.is_none() || record.buyer_tax_id.is_none() {
        let ids = tax_ids_from_text(text);
        if record.seller_tax_id.is_none() {
            record.seller_tax_id = ids.first().cloned();
        }
        if record.buyer_tax_id.is_none() {
            record.buyer_tax_id = ids.get(1).cloned();
        }
    }

    record
}

fn parse_json_invoice(source: SourceKind, text: &str) -> Result<InvoiceRecord> {
    let value: Value = serde_json::from_str(text)?;
    let mut flat = HashMap::new();
    flatten_json("", &value, &mut flat);
    let get = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| flat.get(&normalize_key(k)).cloned())
            .filter(|v| !v.trim().is_empty())
    };

    let mut record = empty_record(source);
    record.invoice_number = get(&[
        "invoice_number",
        "invoiceNumber",
        "number",
        "numerFaktury",
        "p_2",
    ])
    .map(|v| clean_invoice_number(&v));
    record.issue_date =
        get(&["issue_date", "issueDate", "dataWystawienia", "p_1"]).and_then(|v| parse_date(&v));
    record.sale_date =
        get(&["sale_date", "saleDate", "dataSprzedazy", "p_6"]).and_then(|v| parse_date(&v));
    record.due_date = get(&["due_date", "dueDate", "paymentDueDate", "terminPlatnosci"])
        .and_then(|v| parse_date(&v));
    record.seller_tax_id = get(&[
        "seller_tax_id",
        "seller.nip",
        "podmiot1.nip",
        "sprzedawca.nip",
    ])
    .and_then(|v| normalize_tax_id(&v));
    record.buyer_tax_id = get(&["buyer_tax_id", "buyer.nip", "podmiot2.nip", "nabywca.nip"])
        .and_then(|v| normalize_tax_id(&v));
    record.seller_name = get(&[
        "seller_name",
        "seller.name",
        "podmiot1.nazwa",
        "sprzedawca.nazwa",
    ])
    .and_then(|v| clean_name(&v));
    record.buyer_name = get(&[
        "buyer_name",
        "buyer.name",
        "podmiot2.nazwa",
        "nabywca.nazwa",
    ])
    .and_then(|v| clean_name(&v));
    record.gross_amount_minor = get(&[
        "gross_amount",
        "grossAmount",
        "totalGross",
        "kwotaBrutto",
        "p_15",
    ])
    .and_then(|v| parse_money_minor(&v));
    record.net_amount_minor =
        get(&["net_amount", "netAmount", "totalNet", "p_13_1"]).and_then(|v| parse_money_minor(&v));
    record.vat_amount_minor =
        get(&["vat_amount", "vatAmount", "totalVat", "p_14_1"]).and_then(|v| parse_money_minor(&v));
    record.currency = get(&["currency", "kodWaluty"]).map(|v| v.trim().to_ascii_uppercase());
    record.ksef_reference = get(&["ksef_reference", "nrKSeF", "ksefNumber", "referenceNumber"]);
    record.email_message_id = get(&["email_message_id", "messageId", "id"]);
    record.email_subject = get(&["email_subject", "subject"]);
    record.email_from = get(&["email_from", "from"]);

    if record.seller_tax_id.is_none() || record.buyer_tax_id.is_none() {
        let ids = tax_ids_from_text(text);
        if record.seller_tax_id.is_none() {
            record.seller_tax_id = ids.first().cloned();
        }
        if record.buyer_tax_id.is_none() {
            record.buyer_tax_id = ids.get(1).cloned();
        }
    }
    Ok(record)
}

fn flatten_json(prefix: &str, value: &Value, out: &mut HashMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let next = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_json(&next, value, out);
            }
        }
        Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                flatten_json(&format!("{prefix}.{idx}"), item, out);
            }
        }
        Value::Null => {}
        Value::String(s) => {
            out.insert(normalize_key(prefix), s.clone());
        }
        other => {
            out.insert(normalize_key(prefix), other.to_string());
        }
    }
}

fn parse_text_invoice(source: SourceKind, text: &str) -> InvoiceRecord {
    let mut record = empty_record(source);
    record.invoice_number = invoice_number_from_text(text);
    record.issue_date = labeled_date_from_text(
        text,
        &[
            "data wystawienia",
            "wystawiono",
            "issue date",
            "invoice date",
        ],
    )
    .or_else(|| date_from_text(text));
    record.sale_date = labeled_date_from_text(
        text,
        &[
            "data sprzedaży",
            "data sprzedazy",
            "data wykonania usługi",
            "data wykonania uslugi",
            "sale date",
            "service date",
        ],
    );
    record.due_date = labeled_date_from_text(
        text,
        &[
            "data płatności",
            "data platnosci",
            "termin płatności",
            "termin platnosci",
            "due date",
            "payment due",
        ],
    );
    let tax_ids = tax_ids_from_text(text);
    record.seller_tax_id = tax_ids.first().cloned();
    record.buyer_tax_id = tax_ids.get(1).cloned();
    let (seller_name, buyer_name) = counterparty_names_from_text(text);
    record.seller_name = seller_name;
    record.buyer_name = buyer_name;
    record.gross_amount_minor = amount_from_text(
        text,
        &[
            "wartość brutto",
            "wartosc brutto",
            "łączna kwota brutto",
            "laczna kwota brutto",
            "razem do zapłaty",
            "do zapłaty",
            "kwota brutto",
            "razem brutto",
            "brutto",
            "total gross",
            "amount due",
            "total",
        ],
    );
    record.net_amount_minor = amount_from_text(text, &["kwota netto", "netto", "total net", "net"]);
    record.vat_amount_minor = amount_from_text(text, &["kwota vat", "vat", "podatek"]);
    record.currency = currency_from_text(text);
    record.ksef_reference = ksef_reference_from_text(text);
    record.email_message_id = header_value(text, "Message-ID");
    record.email_subject = header_value(text, "Subject");
    record.email_from = header_value(text, "From");
    record
}

fn first_xml_text(text: &str, tags: &[&str]) -> Option<String> {
    for tag in tags {
        let pattern = format!(
            r"(?is)<(?:[A-Za-z0-9_\-]+:)?{}(?:\s[^>]*)?>(.*?)</(?:[A-Za-z0-9_\-]+:)?{}>",
            regex::escape(tag),
            regex::escape(tag)
        );
        if let Ok(re) = Regex::new(&pattern)
            && let Some(caps) = re.captures(text)
        {
            return caps
                .get(1)
                .map(|m| strip_xml(&m.as_str().replace("<![CDATA[", "").replace("]]>", "")));
        }
    }
    None
}

fn first_xml_block(text: &str, tags: &[&str]) -> Option<String> {
    for tag in tags {
        let pattern = format!(
            r"(?is)<(?:[A-Za-z0-9_\-]+:)?{}(?:\s[^>]*)?>(.*?)</(?:[A-Za-z0-9_\-]+:)?{}>",
            regex::escape(tag),
            regex::escape(tag)
        );
        if let Ok(re) = Regex::new(&pattern)
            && let Some(caps) = re.captures(text)
        {
            return caps.get(1).map(|m| m.as_str().to_string());
        }
    }
    None
}

fn strip_xml(value: &str) -> String {
    Regex::new(r"(?is)<[^>]+>")
        .unwrap()
        .replace_all(value, "")
        .trim()
        .to_string()
}

fn invoice_number_from_text(text: &str) -> Option<String> {
    let patterns = [
        r"(?i)(?:obraz\s+)?faktur(?:a|y)?\s*(?:vat)?\s*(?:nr|numer)?[\s:#\-\n]*([A-Z0-9][A-Z0-9/_.\-]{2,})",
        r"(?i)(?:nr\s*faktury|invoice\s*(?:no\.?|number)?|numer)[\s:#\-\n]*([A-Z0-9][A-Z0-9/_.\-]{2,})",
        r"(?i)\bFV[\s:#\-]*([A-Z0-9][A-Z0-9/_.\-]{2,})",
    ];
    for pattern in patterns {
        let re = Regex::new(pattern).unwrap();
        if let Some(caps) = re.captures(text)
            && let Some(value) = caps.get(1)
        {
            let cleaned = clean_invoice_number(value.as_str());
            if !matches!(
                cleaned.as_str(),
                "ZOSTA" | "ZOSTAŁA" | "VAT" | "FOR" | "INVOICE"
            ) {
                return Some(cleaned);
            }
        }
    }
    None
}

fn invoice_number_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    for pattern in [
        r"(?i)Invoice-([A-Z0-9\-]+)",
        r"(?i)Faktura[_\-]([A-Z0-9/\-]+)",
        r"(?i)RYANAIR[_\-]([0-9\-]+[_\-]IE)",
        r"(?i)(?:fv|faktura|invoice)?[_\-\s]*([A-Z0-9]{1,8}[/_\-][A-Z0-9/_\-]{2,})",
    ] {
        let re = Regex::new(pattern).unwrap();
        if let Some(value) = re.captures(stem).and_then(|c| c.get(1)) {
            let cleaned = clean_invoice_number(value.as_str());
            if !matches!(cleaned.as_str(), "INVOICE" | "FAKTURA") {
                return Some(cleaned);
            }
        }
    }
    None
}

fn clean_invoice_number(value: &str) -> String {
    value
        .trim()
        .trim_matches(|c: char| c == ':' || c == '#' || c == '.' || c == ',')
        .replace('_', "/")
        .to_ascii_uppercase()
}

fn tax_ids_from_text(text: &str) -> Vec<String> {
    let re = Regex::new(r"(?i)(?:NIP|VAT\s*ID|Tax\s*ID)?\s*(?:PL)?\s*([0-9][0-9\-\s]{8,}[0-9])")
        .unwrap();
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for caps in re.captures_iter(text) {
        if let Some(id) = caps.get(1).and_then(|m| normalize_tax_id(m.as_str()))
            && id.len() >= 10
            && seen.insert(id.clone())
        {
            ids.push(id);
        }
    }
    ids
}

fn normalize_tax_id(value: &str) -> Option<String> {
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 10 {
        Some(digits)
    } else {
        None
    }
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    for fmt in [
        "%Y-%m-%d", "%d.%m.%Y", "%d-%m-%Y", "%Y/%m/%d", "%d/%m/%Y", "%d %m %Y",
    ] {
        if let Ok(date) = NaiveDate::parse_from_str(value, fmt) {
            return Some(date);
        }
    }
    None
}

fn date_from_text(text: &str) -> Option<NaiveDate> {
    let patterns = [r"\b\d{4}-\d{2}-\d{2}\b", r"\b\d{2}[./-]\d{2}[./-]\d{4}\b"];
    for pattern in patterns {
        let re = Regex::new(pattern).unwrap();
        for m in re.find_iter(text) {
            if let Some(date) = parse_date(m.as_str()) {
                return Some(date);
            }
        }
    }
    None
}

fn labeled_date_from_text(text: &str, labels: &[&str]) -> Option<NaiveDate> {
    for label in labels {
        let pattern = format!(
            r"(?i){}[^0-9]{{0,30}}(\d{{4}}-\d{{2}}-\d{{2}}|\d{{2}}[./-]\d{{2}}[./-]\d{{4}})",
            regex::escape(label)
        );
        let re = Regex::new(&pattern).unwrap();
        if let Some(caps) = re.captures(text)
            && let Some(date) = caps.get(1).and_then(|m| parse_date(m.as_str()))
        {
            return Some(date);
        }
    }
    None
}

fn clean_name(value: &str) -> Option<String> {
    let cleaned = value
        .lines()
        .next()
        .unwrap_or(value)
        .trim()
        .trim_matches(|c: char| matches!(c, ':' | ',' | ';' | '-' | '|'))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.len() >= 3 && !cleaned.chars().all(|c| c.is_ascii_digit()) {
        Some(cleaned)
    } else {
        None
    }
}

fn counterparty_names_from_text(text: &str) -> (Option<String>, Option<String>) {
    let seller = name_after_label(text, &["sprzedawca", "wystawca", "seller", "supplier"])
        .or_else(|| name_before_first_nip(text));
    let buyer = name_after_label(
        text,
        &[
            "nabywca",
            "odbiorca",
            "kupujący",
            "kupujacy",
            "buyer",
            "customer",
        ],
    )
    .or_else(|| name_before_nth_nip(text, 2));
    (seller, buyer)
}

fn name_after_label(text: &str, labels: &[&str]) -> Option<String> {
    let lines = clean_lines(text);
    for (idx, line) in lines.iter().enumerate() {
        let lower = line.to_lowercase();
        if labels.iter().any(|label| lower.contains(label)) {
            if let Some(after_colon) = line
                .split_once(':')
                .and_then(|(_, value)| clean_name(value))
                && is_probable_name_line(&after_colon)
            {
                return Some(after_colon);
            }
            for candidate in lines.iter().skip(idx + 1).take(4) {
                if is_probable_name_line(candidate) {
                    return clean_name(candidate);
                }
            }
        }
    }
    None
}

fn name_before_first_nip(text: &str) -> Option<String> {
    name_before_nth_nip(text, 1)
}

fn name_before_nth_nip(text: &str, nth: usize) -> Option<String> {
    let lines = clean_lines(text);
    let mut seen = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        if line.to_lowercase().contains("nip") {
            seen += 1;
            if seen == nth {
                for candidate in lines[..idx].iter().rev().take(4) {
                    if is_probable_name_line(candidate) {
                        return clean_name(candidate);
                    }
                }
            }
        }
    }
    None
}

fn clean_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect()
}

fn is_probable_name_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    line.len() >= 3
        && line.len() <= 140
        && !lower.contains("nip")
        && !lower.contains("regon")
        && !lower.contains("adres")
        && !lower.contains("siedziba")
        && !lower.starts_with("ul.")
        && !lower.starts_with("ul ")
        && !lower.contains("data")
        && !lower.contains("faktura")
        && !lower.contains("razem")
        && !lower.contains("zapłaty")
        && !lower.contains("zapl")
        && line.chars().any(|c| c.is_alphabetic())
}

fn parse_money_minor(value: &str) -> Option<i64> {
    let mut s: String = value
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == ',' || *c == '.' || *c == '-')
        .collect();
    if s.is_empty() || s == "-" {
        return None;
    }
    let last_comma = s.rfind(',');
    let last_dot = s.rfind('.');
    let decimal_pos = match (last_comma, last_dot) {
        (Some(c), Some(d)) => Some(c.max(d)),
        (Some(c), None) => Some(c),
        (None, Some(d)) => {
            if s.len().saturating_sub(d + 1) == 2 {
                Some(d)
            } else {
                None
            }
        }
        (None, None) => None,
    };

    if let Some(pos) = decimal_pos {
        let int_part: String = s[..pos]
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        let frac_part: String = s[pos + 1..]
            .chars()
            .filter(|c| c.is_ascii_digit())
            .take(2)
            .collect();
        let sign = if int_part.starts_with('-') { -1 } else { 1 };
        let units: i64 = int_part.replace('-', "").parse().ok()?;
        let cents: i64 = match frac_part.len() {
            0 => 0,
            1 => frac_part.parse::<i64>().ok()? * 10,
            _ => frac_part.parse::<i64>().ok()?,
        };
        Some(sign * (units * 100 + cents))
    } else {
        s.retain(|c| c.is_ascii_digit() || c == '-');
        s.parse::<i64>().ok().map(|v| v * 100)
    }
}

fn amount_from_text(text: &str, labels: &[&str]) -> Option<i64> {
    for label in labels {
        let pattern = format!(
            r"(?i){}[^0-9\-]{{0,40}}(-?[0-9][0-9\s.,]{{0,20}})",
            regex::escape(label)
        );
        let re = Regex::new(&pattern).unwrap();
        if let Some(caps) = re.captures(text)
            && let Some(value) = caps.get(1).and_then(|m| parse_money_minor(m.as_str()))
        {
            return Some(value);
        }
    }
    None
}

fn currency_from_text(text: &str) -> Option<String> {
    let re = Regex::new(r"\b(PLN|EUR|USD|GBP|CHF)\b").unwrap();
    re.captures(text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_ascii_uppercase())
}

fn ksef_reference_from_text(text: &str) -> Option<String> {
    let patterns = [
        r"(?i)(?:Nr\s*KSeF|Numer\s+w\s*KSeF|KSeF)[\s:#\-]*([0-9]{10}-[0-9]{8}-[A-Z0-9]{10,}-[A-Z0-9]{2})",
        r"\b([0-9]{10}-[0-9]{8}-[A-Z0-9]{10,}-[A-Z0-9]{2})\b",
    ];
    for pattern in patterns {
        let re = Regex::new(pattern).unwrap();
        if let Some(value) = re.captures(text).and_then(|c| c.get(1)) {
            return Some(value.as_str().to_string());
        }
    }
    None
}

fn header_value(text: &str, name: &str) -> Option<String> {
    let pattern = format!(r"(?im)^{}:\s*(.+)$", regex::escape(name));
    Regex::new(&pattern)
        .ok()?
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn extract_pdf_text(path: &Path) -> Result<String> {
    let pdftotext = Command::new("pdftotext")
        .arg("-layout")
        .arg(path)
        .arg("-")
        .output();
    match pdftotext {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            if !text.trim().is_empty() {
                return Ok(text);
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                eprintln!("pdftotext failed for {}: {}", path.display(), stderr.trim());
            }
        }
        Err(_) => {}
    }

    extract_pdf_text_with_pypdf(path).context(
        "ekstrakcja PDF nie powiodła się; zainstaluj poppler (`brew install poppler`) albo pypdf (`python3 -m pip install pypdf`)",
    )
}

fn extract_pdf_text_with_pypdf(path: &Path) -> Result<String> {
    let script = r#"
import sys
from pypdf import PdfReader
reader = PdfReader(sys.argv[1])
for page in reader.pages:
    text = page.extract_text() or ""
    if text:
        print(text)
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(path)
        .output()
        .context("uruchomienie python3/pypdf")?;
    if !output.status.success() {
        return Err(anyhow!(
            String::from_utf8_lossy(&output.stderr).trim().to_string()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn reconcile(
    ksef_records: Vec<InvoiceRecord>,
    mail_records: Vec<InvoiceRecord>,
    match_score: u8,
    review_score: u8,
) -> ReconcileReport {
    let mut candidates = Vec::new();
    for (ksef_idx, ksef) in ksef_records.iter().enumerate() {
        for (mail_idx, mail) in mail_records.iter().enumerate() {
            let (score, reasons) = score_pair(ksef, mail);
            if score >= review_score {
                candidates.push(Candidate {
                    ksef_idx,
                    mail_idx,
                    score,
                    reasons,
                });
            }
        }
    }
    candidates.sort_by(|a, b| b.score.cmp(&a.score));

    let mut used_ksef = HashSet::new();
    let mut used_mail = HashSet::new();
    let mut matches = Vec::new();
    for candidate in candidates {
        if used_ksef.contains(&candidate.ksef_idx) || used_mail.contains(&candidate.mail_idx) {
            continue;
        }
        used_ksef.insert(candidate.ksef_idx);
        used_mail.insert(candidate.mail_idx);
        matches.push(InvoiceMatch {
            status: if candidate.score >= match_score {
                MatchStatus::Matched
            } else {
                MatchStatus::NeedsReview
            },
            score: candidate.score,
            reasons: candidate.reasons,
            ksef: ksef_records[candidate.ksef_idx].clone(),
            mail: mail_records[candidate.mail_idx].clone(),
        });
    }

    let unmatched_ksef = ksef_records
        .iter()
        .enumerate()
        .filter(|(idx, _)| !used_ksef.contains(idx))
        .map(|(_, record)| record.clone())
        .collect::<Vec<_>>();
    let unmatched_mail = mail_records
        .iter()
        .enumerate()
        .filter(|(idx, _)| !used_mail.contains(idx))
        .map(|(_, record)| record.clone())
        .collect::<Vec<_>>();
    let matched_count = matches
        .iter()
        .filter(|m| m.status == MatchStatus::Matched)
        .count();
    let review_count = matches
        .iter()
        .filter(|m| m.status == MatchStatus::NeedsReview)
        .count();

    ReconcileReport {
        generated_at: Utc::now(),
        match_score,
        review_score,
        summary: ReconcileSummary {
            ksef_count: ksef_records.len(),
            mail_count: mail_records.len(),
            matched_count,
            review_count,
            unmatched_ksef_count: unmatched_ksef.len(),
            unmatched_mail_count: unmatched_mail.len(),
        },
        matches,
        unmatched_ksef,
        unmatched_mail,
    }
}

fn score_pair(ksef: &InvoiceRecord, mail: &InvoiceRecord) -> (u8, Vec<String>) {
    let mut score: u16 = 0;
    let mut reasons = Vec::new();

    if let (Some(a), Some(b)) = (&ksef.ksef_reference, &mail.ksef_reference)
        && a == b
    {
        score += 100;
        reasons.push("ksef_reference exact".to_string());
    }

    if let (Some(a), Some(b)) = (&ksef.invoice_number, &mail.invoice_number) {
        if comparable_invoice_number(a) == comparable_invoice_number(b) {
            score += 45;
            reasons.push("invoice_number exact".to_string());
        } else if comparable_invoice_number(a).contains(&comparable_invoice_number(b))
            || comparable_invoice_number(b).contains(&comparable_invoice_number(a))
        {
            score += 25;
            reasons.push("invoice_number partial".to_string());
        }
    }

    let ksef_ids = [ksef.seller_tax_id.as_ref(), ksef.buyer_tax_id.as_ref()]
        .into_iter()
        .flatten()
        .collect::<HashSet<_>>();
    let mail_ids = [mail.seller_tax_id.as_ref(), mail.buyer_tax_id.as_ref()]
        .into_iter()
        .flatten()
        .collect::<HashSet<_>>();
    if !ksef_ids.is_empty() && ksef_ids.iter().any(|id| mail_ids.contains(id)) {
        score += 20;
        reasons.push("tax_id match".to_string());
    }
    if let (Some(a), Some(b)) = (&ksef.seller_tax_id, &mail.seller_tax_id)
        && a == b
    {
        score += 5;
        reasons.push("seller_tax_id same position".to_string());
    }

    if let (Some(a), Some(b)) = (ksef.gross_amount_minor, mail.gross_amount_minor) {
        let diff = (a - b).abs();
        if diff == 0 {
            score += 20;
            reasons.push("gross_amount exact".to_string());
        } else if diff <= 2 {
            score += 17;
            reasons.push("gross_amount near".to_string());
        }
    }

    if let (Some(a), Some(b)) = (ksef.issue_date, mail.issue_date) {
        let diff = (a - b).num_days().abs();
        if diff == 0 {
            score += 10;
            reasons.push("issue_date exact".to_string());
        } else if diff <= 7 {
            score += 4;
            reasons.push("issue_date near".to_string());
        }
    }

    if let (Some(a), Some(b)) = (&ksef.currency, &mail.currency)
        && a.eq_ignore_ascii_case(b)
    {
        score += 5;
        reasons.push("currency match".to_string());
    }

    (score.min(100) as u8, reasons)
}

fn comparable_invoice_number(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_uppercase())
        .collect()
}

fn write_matches_csv(matches: &[InvoiceMatch], path: &Path) -> Result<()> {
    let mut writer =
        csv::Writer::from_path(path).with_context(|| format!("zapis CSV {}", path.display()))?;
    writer.write_record([
        "status",
        "score",
        "reasons",
        "ksef_path",
        "mail_path",
        "invoice_number",
        "seller_tax_id",
        "buyer_tax_id",
        "gross_amount_minor",
        "issue_date",
    ])?;
    for m in matches {
        writer.write_record([
            format!("{:?}", m.status),
            m.score.to_string(),
            m.reasons.join(";"),
            m.ksef.source_path.clone().unwrap_or_default(),
            m.mail.source_path.clone().unwrap_or_default(),
            m.ksef.invoice_number.clone().unwrap_or_default(),
            m.ksef.seller_tax_id.clone().unwrap_or_default(),
            m.ksef.buyer_tax_id.clone().unwrap_or_default(),
            m.ksef
                .gross_amount_minor
                .map(|v| v.to_string())
                .unwrap_or_default(),
            m.ksef.issue_date.map(|v| v.to_string()).unwrap_or_default(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct GoogleClientSecretFile {
    installed: Option<GoogleClientSecret>,
    web: Option<GoogleClientSecret>,
}

#[derive(Debug, Deserialize)]
struct GoogleClientSecret {
    client_id: String,
    client_secret: String,
    auth_uri: String,
    token_uri: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct GmailTokenFile {
    access_token: String,
    refresh_token: Option<String>,
    token_type: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

#[derive(Debug, Serialize)]
struct GmailAuthResult {
    token_file: String,
    refresh_token_saved: bool,
    expires_at: Option<DateTime<Utc>>,
    scope: String,
}

fn default_gmail_token_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("lab")
        .join("gmail_token.json")
}

fn read_google_client_secret(path: &Path) -> Result<GoogleClientSecret> {
    let text = fs::read_to_string(path).with_context(|| format!("odczyt {}", path.display()))?;
    let file: GoogleClientSecretFile = serde_json::from_str(&text)
        .with_context(|| format!("niepoprawny Google client secret JSON: {}", path.display()))?;
    file.installed
        .or(file.web)
        .ok_or_else(|| anyhow!("client secret JSON musi mieć sekcję installed albo web"))
}

fn gmail_auth(
    client_secret_path: &Path,
    token_file: &Path,
    no_browser: bool,
) -> Result<GmailAuthResult> {
    let secret = read_google_client_secret(client_secret_path)?;
    let listener =
        TcpListener::bind("127.0.0.1:0").context("uruchomienie lokalnego OAuth listenera")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let scope = "https://www.googleapis.com/auth/gmail.readonly";
    let state = oauth_state();
    let auth_url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}",
        secret.auth_uri,
        urlencoding::encode(&secret.client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(scope),
        urlencoding::encode(&state),
    );

    if no_browser || Command::new("open").arg(&auth_url).status().is_err() {
        eprintln!("Otwórz URL w przeglądarce:\n{auth_url}");
    }

    let (code, returned_state) = wait_for_oauth_code(&listener)?;
    if returned_state.as_deref() != Some(&state) {
        return Err(anyhow!("OAuth state mismatch"));
    }

    let client = Client::builder().build()?;
    let token_response: TokenResponse = client
        .post(&secret.token_uri)
        .form(&[
            ("code", code.as_str()),
            ("client_id", secret.client_id.as_str()),
            ("client_secret", secret.client_secret.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()?
        .error_for_status()?
        .json()?;
    let token = token_response_to_file(token_response, None);
    save_gmail_token(token_file, &token)?;

    Ok(GmailAuthResult {
        token_file: token_file.display().to_string(),
        refresh_token_saved: token.refresh_token.is_some(),
        expires_at: token.expires_at,
        scope: scope.to_string(),
    })
}

fn oauth_state() -> String {
    let raw = format!(
        "{}:{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    hex::encode(Sha256::digest(raw.as_bytes()))
}

fn wait_for_oauth_code(listener: &TcpListener) -> Result<(String, Option<String>)> {
    let (mut stream, _) = listener.accept().context("oczekiwanie na redirect OAuth")?;
    let mut buffer = [0u8; 8192];
    let len = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..len]);
    let first_line = request.lines().next().unwrap_or_default();
    let path = first_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("niepoprawny request OAuth"))?;
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or_default();
    let params = parse_query(query);
    let response_body = if params.contains_key("code") {
        "Autoryzacja Gmail zakończona. Możesz wrócić do terminala."
    } else {
        "Autoryzacja Gmail nie powiodła się. Wróć do terminala."
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response_body.len(),
        response_body
    );
    stream.write_all(response.as_bytes())?;

    if let Some(error) = params.get("error") {
        return Err(anyhow!("OAuth error: {error}"));
    }
    let code = params
        .get("code")
        .cloned()
        .ok_or_else(|| anyhow!("OAuth redirect nie zawiera code"))?;
    Ok((code, params.get("state").cloned()))
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((
                urlencoding::decode(key).ok()?.to_string(),
                urlencoding::decode(value).ok()?.to_string(),
            ))
        })
        .collect()
}

fn token_response_to_file(
    response: TokenResponse,
    previous_refresh_token: Option<String>,
) -> GmailTokenFile {
    GmailTokenFile {
        access_token: response.access_token,
        refresh_token: response.refresh_token.or(previous_refresh_token),
        token_type: response.token_type,
        expires_at: response
            .expires_in
            .map(|seconds| Utc::now() + chrono::Duration::seconds(seconds)),
        scope: response.scope,
    }
}

fn save_gmail_token(path: &Path, token: &GmailTokenFile) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    fs::write(path, serde_json::to_vec_pretty(token)?)
        .with_context(|| format!("zapis tokenu Gmail {}", path.display()))?;
    Ok(())
}

fn read_gmail_token(path: &Path) -> Result<GmailTokenFile> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("odczyt tokenu Gmail {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("niepoprawny token Gmail {}", path.display()))
}

fn gmail_access_token(
    token_env: &str,
    token_file: &Path,
    client_secret_path: Option<&Path>,
) -> Result<String> {
    if let Ok(token) = std::env::var(token_env)
        && !token.trim().is_empty()
    {
        return Ok(token);
    }

    let token = read_gmail_token(token_file).with_context(|| {
        format!(
            "brak {token_env}; uruchom `gmail-auth --client-secret ...` albo wskaż --token-file {}",
            token_file.display()
        )
    })?;
    if token
        .expires_at
        .map(|t| t > Utc::now() + chrono::Duration::seconds(60))
        .unwrap_or(true)
    {
        return Ok(token.access_token);
    }

    let client_secret_path = client_secret_path
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("GOOGLE_CLIENT_SECRET_PATH").map(PathBuf::from))
        .ok_or_else(|| anyhow!("token Gmail wygasł; podaj --client-secret albo GOOGLE_CLIENT_SECRET_PATH, żeby go odświeżyć"))?;
    refresh_gmail_token(&client_secret_path, token_file, token)
}

fn refresh_gmail_token(
    client_secret_path: &Path,
    token_file: &Path,
    previous: GmailTokenFile,
) -> Result<String> {
    let refresh_token = previous.refresh_token.clone().ok_or_else(|| {
        anyhow!("token-file nie zawiera refresh_token; uruchom gmail-auth ponownie")
    })?;
    let secret = read_google_client_secret(client_secret_path)?;
    let client = Client::builder().build()?;
    let response: TokenResponse = client
        .post(&secret.token_uri)
        .form(&[
            ("client_id", secret.client_id.as_str()),
            ("client_secret", secret.client_secret.as_str()),
            ("refresh_token", refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()?
        .error_for_status()?
        .json()?;
    let token = token_response_to_file(response, previous.refresh_token);
    let access_token = token.access_token.clone();
    save_gmail_token(token_file, &token)?;
    Ok(access_token)
}

#[derive(Debug, Serialize)]
struct GmailFetchResult {
    query: String,
    messages_seen: usize,
    files_saved: usize,
    out_dir: String,
    saved_files: Vec<String>,
}

fn gmail_fetch(
    token: &str,
    user: &str,
    query: &str,
    out_dir: &Path,
    max: usize,
    extensions: &[String],
) -> Result<GmailFetchResult> {
    fs::create_dir_all(out_dir).with_context(|| format!("mkdir {}", out_dir.display()))?;
    let client = Client::builder().build()?;
    let allowed_exts = extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut page_token: Option<String> = None;
    let mut message_ids = Vec::new();

    while message_ids.len() < max {
        let mut req = client
            .get(format!(
                "https://gmail.googleapis.com/gmail/v1/users/{}/messages",
                user
            ))
            .bearer_auth(token)
            .query(&[("q", query), ("maxResults", "100")]);
        if let Some(token) = &page_token {
            req = req.query(&[("pageToken", token)]);
        }
        let value: Value = req.send()?.error_for_status()?.json()?;
        if let Some(messages) = value.get("messages").and_then(|v| v.as_array()) {
            for msg in messages {
                if let Some(id) = msg.get("id").and_then(|v| v.as_str())
                    && message_ids.len() < max
                {
                    message_ids.push(id.to_string());
                }
            }
        }
        page_token = value
            .get("nextPageToken")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if page_token.is_none() {
            break;
        }
    }

    let mut saved_files = Vec::new();
    for id in &message_ids {
        let msg: Value = client
            .get(format!(
                "https://gmail.googleapis.com/gmail/v1/users/{}/messages/{}",
                user, id
            ))
            .bearer_auth(token)
            .query(&[("format", "full")])
            .send()?
            .error_for_status()?
            .json()?;

        let headers = gmail_headers(&msg);
        let metadata_path = out_dir.join(format!("{}_message.json", sanitize_filename(id)));
        fs::write(&metadata_path, serde_json::to_vec_pretty(&msg)?)?;
        saved_files.push(metadata_path.display().to_string());

        let mut part_index = 0usize;
        collect_gmail_parts(
            &client,
            token,
            user,
            id,
            &msg["payload"],
            out_dir,
            &allowed_exts,
            &headers,
            &mut part_index,
            &mut saved_files,
        )?;
    }

    Ok(GmailFetchResult {
        query: query.to_string(),
        messages_seen: message_ids.len(),
        files_saved: saved_files.len(),
        out_dir: out_dir.display().to_string(),
        saved_files,
    })
}

fn gmail_headers(msg: &Value) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    if let Some(items) = msg
        .get("payload")
        .and_then(|p| p.get("headers"))
        .and_then(|h| h.as_array())
    {
        for item in items {
            if let (Some(name), Some(value)) = (
                item.get("name").and_then(|v| v.as_str()),
                item.get("value").and_then(|v| v.as_str()),
            ) {
                headers.insert(name.to_ascii_lowercase(), value.to_string());
            }
        }
    }
    headers
}

#[allow(clippy::too_many_arguments)]
fn collect_gmail_parts(
    client: &Client,
    token: &str,
    user: &str,
    message_id: &str,
    part: &Value,
    out_dir: &Path,
    allowed_exts: &HashSet<String>,
    headers: &HashMap<String, String>,
    part_index: &mut usize,
    saved_files: &mut Vec<String>,
) -> Result<()> {
    if let Some(parts) = part.get("parts").and_then(|v| v.as_array()) {
        for child in parts {
            collect_gmail_parts(
                client,
                token,
                user,
                message_id,
                child,
                out_dir,
                allowed_exts,
                headers,
                part_index,
                saved_files,
            )?;
        }
    }

    let filename = part.get("filename").and_then(|v| v.as_str()).unwrap_or("");
    let mime_type = part.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");
    let body = &part["body"];
    let data = if let Some(attachment_id) = body.get("attachmentId").and_then(|v| v.as_str()) {
        let attachment: Value = client
            .get(format!(
                "https://gmail.googleapis.com/gmail/v1/users/{}/messages/{}/attachments/{}",
                user, message_id, attachment_id
            ))
            .bearer_auth(token)
            .send()?
            .error_for_status()?
            .json()?;
        attachment
            .get("data")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        body.get("data")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    let Some(data) = data else {
        return Ok(());
    };
    let decoded = decode_gmail_base64(&data)?;

    let mut save_name = if !filename.is_empty() {
        filename.to_string()
    } else if mime_type == "text/plain" {
        format!("{}_body_{}.txt", message_id, *part_index)
    } else {
        return Ok(());
    };
    let ext = Path::new(&save_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !allowed_exts.contains(&ext) {
        return Ok(());
    }

    *part_index += 1;
    save_name = format!(
        "{}_{}_{}",
        sanitize_filename(message_id),
        *part_index,
        sanitize_filename(&save_name)
    );
    let path = out_dir.join(save_name);
    if ext == "txt" && !headers.is_empty() {
        let mut with_headers = String::new();
        for key in ["message-id", "subject", "from", "date"] {
            if let Some(value) = headers.get(key) {
                with_headers.push_str(&format!("{}: {}\n", canonical_header(key), value));
            }
        }
        with_headers.push('\n');
        with_headers.push_str(&String::from_utf8_lossy(&decoded));
        fs::write(&path, with_headers.as_bytes())?;
    } else {
        fs::write(&path, decoded)?;
    }
    saved_files.push(path.display().to_string());
    Ok(())
}

fn canonical_header(key: &str) -> &str {
    match key {
        "message-id" => "Message-ID",
        "subject" => "Subject",
        "from" => "From",
        "date" => "Date",
        _ => key,
    }
}

fn decode_gmail_base64(value: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(value)
        .or_else(|_| URL_SAFE.decode(value))
        .context("dekodowanie base64url Gmail")
}

fn sanitize_filename(value: &str) -> String {
    let s: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    s.trim_matches('_').chars().take(180).collect()
}

fn ksef_sync(year: i32, input: &Path, out_dir: Option<&Path>) -> Result<KsefSyncResult> {
    let records = load_records(SourceKind::Ksef, input)?;
    let out_dir = out_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| default_ksef_out_path(year));
    fs::create_dir_all(&out_dir).with_context(|| format!("mkdir {}", out_dir.display()))?;
    let json_output = out_dir.join("records.json");
    let jsonl_output = out_dir.join("records.jsonl");
    write_records(&records, OutputFormat::Json, Some(&json_output))?;
    write_records(&records, OutputFormat::Jsonl, Some(&jsonl_output))?;
    Ok(KsefSyncResult {
        summary: KsefSyncSummary {
            year,
            records_count: records.len(),
            input: input.display().to_string(),
            json_output: json_output.display().to_string(),
            jsonl_output: jsonl_output.display().to_string(),
        },
        records,
    })
}

fn handle_cycle_command(db_path: &Path, command: CycleCommands) -> Result<()> {
    match command {
        CycleCommands::Run {
            year,
            productmesh_nip,
            ksef,
            gmail_client_secret,
            gmail_token_file,
            gmail_query,
            gmail_max,
            skip_gmail_fetch,
            mail_out,
            skip_saldeo_fetch,
            saldeo_out,
            out,
            store,
            review_score,
            copy_missing_non_ksef,
            accounting_root,
        } => {
            let summary = run_cycle(CycleRunConfig {
                db_path: db_path.to_path_buf(),
                year,
                productmesh_nip,
                ksef: ksef.unwrap_or_else(|| default_ksef_records_path(year)),
                gmail_client_secret,
                gmail_token_file,
                gmail_query,
                gmail_max,
                skip_gmail_fetch,
                mail_out: mail_out.unwrap_or_else(|| default_mail_out_path(year)),
                skip_saldeo_fetch,
                saldeo_out: saldeo_out.unwrap_or_else(|| default_saldeo_out_path(year)),
                out_dir: out,
                store,
                review_score,
                copy_missing_non_ksef,
                accounting_root: accounting_root.unwrap_or_else(default_accounting_root),
            })?;
            write_json(&summary, None)?;
        }
        CycleCommands::Missing {
            tri_report,
            year,
            output,
            csv,
            copy_non_ksef,
            accounting_root,
        } => {
            let tri_report = tri_report
                .unwrap_or_else(|| PathBuf::from(format!("out/tri-reconcile-{year}.json")));
            let report = read_tri_report(&tri_report)?;
            let accounting_root = accounting_root.unwrap_or_else(default_accounting_root);
            let non_ksef_dir = non_ksef_dir(&accounting_root, year);
            let mut missing = missing_report_from_tri(&report);
            let copied = if copy_non_ksef {
                copy_missing_invoices(&mut missing.invoices, &non_ksef_dir)?
            } else {
                0
            };
            if let Some(csv_path) = csv {
                write_missing_csv(&missing, &csv_path)?;
            }
            if let Some(output_path) = output {
                write_json(&missing, Some(&output_path))?;
            } else {
                write_json(
                    &serde_json::json!({
                        "copied": copied,
                        "report": missing,
                    }),
                    None,
                )?;
            }
        }
        CycleCommands::Schedule { year, ksef, bin } => {
            let bin = bin
                .or_else(|| std::env::current_exe().ok())
                .unwrap_or_else(|| PathBuf::from("lab-cli"));
            let ksef = ksef.unwrap_or_else(|| default_ksef_records_path(year));
            let command = format!(
                "{} --db {} cycle run --year {} --ksef {} --store --copy-missing-non-ksef",
                shell_quote(&bin.display().to_string()),
                shell_quote(&db_path.display().to_string()),
                year,
                shell_quote(&ksef.display().to_string())
            );
            let schedule = CycleSchedule {
                note: "Uruchamiaj 1. i 14. dnia miesiąca, np. cron/launchd. Komenda jest idempotentna względem SQLite i nazw plików.".to_string(),
                cron_line: format!("0 8 1,14 * * cd {} && {}", shell_quote(&std::env::current_dir()?.display().to_string()), command),
                command,
            };
            write_json(&schedule, None)?;
        }
    }
    Ok(())
}

struct CycleRunConfig {
    db_path: PathBuf,
    year: i32,
    productmesh_nip: String,
    ksef: PathBuf,
    gmail_client_secret: Option<PathBuf>,
    gmail_token_file: Option<PathBuf>,
    gmail_query: Option<String>,
    gmail_max: usize,
    skip_gmail_fetch: bool,
    mail_out: PathBuf,
    skip_saldeo_fetch: bool,
    saldeo_out: PathBuf,
    out_dir: PathBuf,
    store: bool,
    review_score: u8,
    copy_missing_non_ksef: bool,
    accounting_root: PathBuf,
}

fn run_cycle(config: CycleRunConfig) -> Result<CycleRunSummary> {
    fs::create_dir_all(&config.out_dir)
        .with_context(|| format!("mkdir {}", config.out_dir.display()))?;

    let gmail = if config.skip_gmail_fetch {
        None
    } else {
        let token_path = config
            .gmail_token_file
            .unwrap_or_else(default_gmail_token_path);
        let token = gmail_access_token(
            "GMAIL_ACCESS_TOKEN",
            &token_path,
            config.gmail_client_secret.as_deref(),
        )?;
        let query = config
            .gmail_query
            .unwrap_or_else(|| default_gmail_query(config.year));
        Some(gmail_fetch(
            &token,
            "me",
            &query,
            &config.mail_out,
            config.gmail_max,
            &["pdf".to_string()],
        )?)
    };

    let mail_records = scan_input(SourceKind::Mail, &config.mail_out)?;
    let candidates = productmesh_invoice_candidates(&mail_records, &config.productmesh_nip);
    if config.store {
        let conn = open_db(&config.db_path)?;
        store_records(&conn, &candidates)?;
    }

    let mail_scan_json = config
        .out_dir
        .join(format!("mail-all-pdf-{}-scan.json", config.year));
    let mail_candidates_json = config.out_dir.join(format!(
        "mail-all-pdf-{}-productmesh-candidates.json",
        config.year
    ));
    let mail_candidates_jsonl = config.out_dir.join(format!(
        "mail-all-pdf-{}-productmesh-candidates.jsonl",
        config.year
    ));
    write_records(&mail_records, OutputFormat::Json, Some(&mail_scan_json))?;
    write_records(&candidates, OutputFormat::Json, Some(&mail_candidates_json))?;
    write_records(
        &candidates,
        OutputFormat::Jsonl,
        Some(&mail_candidates_jsonl),
    )?;

    let ksef_sync = ksef_sync(config.year, &config.ksef, None)?;
    if config.store {
        let conn = open_db(&config.db_path)?;
        store_records(&conn, &ksef_sync.records)?;
    }
    let ksef_records_path = PathBuf::from(&ksef_sync.summary.jsonl_output);

    let (saldeo_records_path, saldeo_documents) = if config.skip_saldeo_fetch {
        (config.saldeo_out.join("records.jsonl"), None)
    } else {
        let result = saldeo_fetch(
            config.year,
            &default_saldeo_storage_state_path(),
            &config.saldeo_out,
        )?;
        if config.store {
            let conn = open_db(&config.db_path)?;
            store_records(&conn, &result.records)?;
        }
        (
            PathBuf::from(&result.summary.records_output),
            Some(result.summary.documents_count),
        )
    };

    let ksef_records = ksef_sync.records;
    let saldeo_records = load_saldeo_records(&saldeo_records_path)?;
    let tri = tri_reconcile(
        candidates,
        ksef_records,
        saldeo_records,
        config.review_score,
    );
    let tri_json = config
        .out_dir
        .join(format!("tri-reconcile-{}.json", config.year));
    let tri_csv = config
        .out_dir
        .join(format!("tri-reconcile-{}.csv", config.year));
    write_tri_csv(&tri, &tri_csv)?;
    write_json(&tri, Some(&tri_json))?;
    let temporal_diff = if config.store {
        let conn = open_db(&config.db_path)?;
        Some(store_tri_reconcile_report(&conn, config.year, &tri)?)
    } else {
        None
    };

    let missing_json = config
        .out_dir
        .join(format!("accountant-missing-{}.json", config.year));
    let missing_csv = config
        .out_dir
        .join(format!("accountant-missing-{}.csv", config.year));
    let non_ksef_dir = non_ksef_dir(&config.accounting_root, config.year);
    let mut missing = missing_report_from_tri(&tri);
    let copied_missing_count = if config.copy_missing_non_ksef {
        copy_missing_invoices(&mut missing.invoices, &non_ksef_dir)?
    } else {
        0
    };
    write_missing_csv(&missing, &missing_csv)?;
    write_json(&missing, Some(&missing_json))?;

    Ok(CycleRunSummary {
        generated_at: Utc::now(),
        year: config.year,
        gmail,
        scanned_mail_pdfs: mail_records.len(),
        productmesh_candidates: tri.summary.mail_count,
        ksef_records: tri.summary.ksef_count,
        saldeo_documents,
        tri_summary: tri.summary,
        temporal_diff,
        missing_for_accountant_count: missing.count,
        copied_missing_count,
        paths: CycleRunPaths {
            mail_scan_json: mail_scan_json.display().to_string(),
            mail_candidates_json: mail_candidates_json.display().to_string(),
            mail_candidates_jsonl: mail_candidates_jsonl.display().to_string(),
            ksef_records_jsonl: ksef_records_path.display().to_string(),
            saldeo_records_jsonl: saldeo_records_path.display().to_string(),
            tri_json: tri_json.display().to_string(),
            tri_csv: tri_csv.display().to_string(),
            missing_json: missing_json.display().to_string(),
            missing_csv: missing_csv.display().to_string(),
            non_ksef_dir: non_ksef_dir.display().to_string(),
        },
    })
}

fn default_gmail_query(year: i32) -> String {
    format!(
        "after:{year}/01/01 before:{}/01/01 has:attachment filename:pdf",
        year + 1
    )
}

fn default_mail_out_path(year: i32) -> PathBuf {
    PathBuf::from(format!("data/mail-all-pdf-{year}-pdfs"))
}

fn default_saldeo_out_path(year: i32) -> PathBuf {
    PathBuf::from(format!("data/saldeo-{year}"))
}

fn default_ksef_out_path(year: i32) -> PathBuf {
    PathBuf::from(format!("data/ksef-{year}"))
}

fn default_ksef_records_path(year: i32) -> PathBuf {
    default_ksef_out_path(year).join("records.jsonl")
}

fn non_ksef_dir(accounting_root: &Path, year: i32) -> PathBuf {
    accounting_root
        .join("cost invoices")
        .join(year.to_string())
        .join("non-KSeF files")
}

fn productmesh_invoice_candidates(
    records: &[InvoiceRecord],
    productmesh_nip: &str,
) -> Vec<InvoiceRecord> {
    let excluded = Regex::new(r"(?i)(receipt|statement|regulamin|warunki|informacje|upowa|oferta|umowa|order|label|bilet|dr_skan|wypowiedzenie|grafklient|cennik|polityka|pasek|wishlist|terms|portfolio|kosztorys|formularz|prawo_jazdy|zalacznik)").unwrap();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for record in records {
        let names = format!(
            "{} {}",
            record.seller_name.clone().unwrap_or_default(),
            record.buyer_name.clone().unwrap_or_default()
        );
        let related = record.seller_tax_id.as_deref() == Some(productmesh_nip)
            || record.buyer_tax_id.as_deref() == Some(productmesh_nip)
            || names.to_lowercase().contains("productmesh");
        if !related {
            continue;
        }
        if let Some(path) = &record.source_path {
            let file_name = Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if excluded.is_match(file_name) {
                continue;
            }
        }
        let key = format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            record.invoice_number,
            record.issue_date,
            record.gross_amount_minor,
            record.seller_tax_id,
            record.buyer_tax_id,
            record.seller_name,
            record.buyer_name
        );
        if seen.insert(key) {
            out.push(record.clone());
        }
    }
    out
}

fn read_tri_report(path: &Path) -> Result<TriReconcileReport> {
    let text = fs::read_to_string(path).with_context(|| format!("odczyt {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("niepoprawny tri report {}", path.display()))
}

fn missing_report_from_tri(report: &TriReconcileReport) -> MissingReport {
    let mut invoices = report
        .rows
        .iter()
        .filter(|row| row.status == "gmail_only")
        .filter_map(|row| row.mail.as_ref())
        .map(missing_invoice_from_record)
        .collect::<Vec<_>>();
    invoices.sort_by(|a, b| {
        (a.issue_date, &a.contractor, &a.invoice_number).cmp(&(
            b.issue_date,
            &b.contractor,
            &b.invoice_number,
        ))
    });
    MissingReport {
        generated_at: Utc::now(),
        count: invoices.len(),
        invoices,
    }
}

fn missing_invoice_from_record(record: &InvoiceRecord) -> MissingInvoice {
    let mut invoice_number = record
        .invoice_number
        .clone()
        .unwrap_or_else(|| "UNKNOWN".to_string());
    let mut contractor = record
        .seller_name
        .clone()
        .or_else(|| record.buyer_name.clone())
        .unwrap_or_default();
    let mut issue_date = record.issue_date;
    let mut gross_amount_minor = record.gross_amount_minor;
    let mut currency = record.currency.clone().unwrap_or_default();

    if let Some(path) = record.source_path.as_ref().map(PathBuf::from)
        && path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("pdf"))
            .unwrap_or(false)
        && let Ok(text) = extract_pdf_text(&path)
    {
        if (invoice_number == "UNKNOWN" || invoice_number == "INVOICE")
            && let Some(value) = english_invoice_number(&text)
        {
            invoice_number = value;
        }
        if issue_date.is_none() {
            issue_date = english_issue_date(&text);
        }
        if let Some(known) = known_contractor_from_text(&text)
            && (contractor.is_empty() || contractor.to_lowercase().contains("booking reference"))
        {
            contractor = known;
        }
        if (gross_amount_minor.is_none()
            || gross_amount_minor.unwrap_or_default().abs() > 100_000_000)
            && let Some((amount, cur)) = english_total_amount(&text)
        {
            gross_amount_minor = Some(amount);
            if currency.is_empty() {
                currency = cur;
            }
        }
        if currency.is_empty()
            && let Some((_, cur)) = english_total_amount(&text)
        {
            currency = cur;
        }
    }

    if contractor.is_empty() {
        contractor = "UNKNOWN".to_string();
    }
    let amount = gross_amount_minor
        .map(|v| format!("{:.2}", v as f64 / 100.0))
        .unwrap_or_default();

    MissingInvoice {
        invoice_number,
        contractor,
        issue_date,
        gross_amount_minor,
        amount,
        currency,
        source_path: record.source_path.clone().unwrap_or_default(),
        target_path: None,
    }
}

fn english_invoice_number(text: &str) -> Option<String> {
    for pattern in [
        r"(?i)Invoice number\s+([^\n]+)",
        r"(?i)Invoice number:\s*([^\n]+)",
    ] {
        let re = Regex::new(pattern).unwrap();
        if let Some(value) = re.captures(text).and_then(|c| c.get(1)) {
            return Some(clean_invoice_number(value.as_str()));
        }
    }
    None
}

fn english_issue_date(text: &str) -> Option<NaiveDate> {
    let re = Regex::new(r"(?i)Date of issue\s+([A-Za-z]+)\s+(\d{1,2}),\s*(\d{4})").unwrap();
    if let Some(caps) = re.captures(text) {
        let month = match caps.get(1)?.as_str().to_ascii_lowercase().as_str() {
            "january" => 1,
            "february" => 2,
            "march" => 3,
            "april" => 4,
            "may" => 5,
            "june" => 6,
            "july" => 7,
            "august" => 8,
            "september" => 9,
            "october" => 10,
            "november" => 11,
            "december" => 12,
            _ => return None,
        };
        let day: u32 = caps.get(2)?.as_str().parse().ok()?;
        let year: i32 = caps.get(3)?.as_str().parse().ok()?;
        return NaiveDate::from_ymd_opt(year, month, day);
    }
    let re = Regex::new(r"(?i)Date of issue:\s*(\d{2})/(\d{2})/(\d{4})").unwrap();
    re.captures(text).and_then(|caps| {
        let day: u32 = caps.get(1)?.as_str().parse().ok()?;
        let month: u32 = caps.get(2)?.as_str().parse().ok()?;
        let year: i32 = caps.get(3)?.as_str().parse().ok()?;
        NaiveDate::from_ymd_opt(year, month, day)
    })
}

fn known_contractor_from_text(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for (needle, name) in [
        ("anthropic", "Anthropic PBC"),
        ("openrouter", "OpenRouter Inc"),
        ("moonshot", "Moonshot AI PTE LTD"),
        ("ryanair", "Ryanair DAC"),
        (
            "santander consumer multirent",
            "Santander Consumer Multirent Sp. z o.o.",
        ),
        ("elocity", "Elocity sp. z o.o."),
    ] {
        if lower.contains(needle) {
            return Some(name.to_string());
        }
    }
    None
}

fn english_total_amount(text: &str) -> Option<(i64, String)> {
    let re = Regex::new(r"(?i)Total\s*([€$])\s*([0-9]+(?:[.,][0-9]{2})?)").unwrap();
    if let Some(caps) = re.captures(text) {
        let currency = match caps.get(1)?.as_str() {
            "€" => "EUR",
            "$" => "USD",
            _ => "",
        };
        let amount = parse_money_minor(caps.get(2)?.as_str())?;
        return Some((amount, currency.to_string()));
    }
    let re = Regex::new(r"(?im)^TOTAL\s+[0-9]+(?:[.,][0-9]{2})?\s+[0-9]+(?:[.,][0-9]{2})?\s+([0-9]+(?:[.,][0-9]{2})?)").unwrap();
    if text.to_lowercase().contains("ryanair")
        && let Some(caps) = re.captures(text)
    {
        return Some((parse_money_minor(caps.get(1)?.as_str())?, "PLN".to_string()));
    }
    None
}

fn write_missing_csv(report: &MissingReport, path: &Path) -> Result<()> {
    let mut writer =
        csv::Writer::from_path(path).with_context(|| format!("zapis CSV {}", path.display()))?;
    writer.write_record([
        "issue_date",
        "invoice_number",
        "contractor",
        "amount",
        "currency",
        "source_path",
        "target_path",
    ])?;
    for invoice in &report.invoices {
        writer.write_record([
            invoice
                .issue_date
                .map(|d| d.to_string())
                .unwrap_or_default(),
            invoice.invoice_number.clone(),
            invoice.contractor.clone(),
            invoice.amount.clone(),
            invoice.currency.clone(),
            invoice.source_path.clone(),
            invoice.target_path.clone().unwrap_or_default(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn copy_missing_invoices(invoices: &mut [MissingInvoice], non_ksef_dir: &Path) -> Result<usize> {
    fs::create_dir_all(non_ksef_dir)
        .with_context(|| format!("mkdir {}", non_ksef_dir.display()))?;
    let mut copied = 0usize;
    for invoice in &mut *invoices {
        if invoice.source_path.is_empty() {
            continue;
        }
        let source = PathBuf::from(&invoice.source_path);
        if !source.exists() {
            continue;
        }
        let file_name = standardized_invoice_filename(invoice);
        let target = non_ksef_dir.join(file_name);
        if !target.exists() {
            fs::copy(&source, &target).with_context(|| {
                format!("kopiowanie {} -> {}", source.display(), target.display())
            })?;
            copied += 1;
        }
        invoice.target_path = Some(target.display().to_string());
    }
    write_non_ksef_manifest(invoices, non_ksef_dir)?;
    Ok(copied)
}

fn standardized_invoice_filename(invoice: &MissingInvoice) -> String {
    let date = invoice
        .issue_date
        .map(|d| d.to_string())
        .unwrap_or_else(|| "unknown-date".to_string());
    let amount = if invoice.amount.is_empty() {
        "unknown-amount".to_string()
    } else {
        invoice.amount.clone()
    };
    let currency = if invoice.currency.is_empty() {
        "XXX".to_string()
    } else {
        invoice.currency.clone()
    };
    format!(
        "{} - {} - {} - {} {}.pdf",
        safe_filename_part(&invoice.invoice_number),
        safe_filename_part(&invoice.contractor),
        date,
        amount,
        safe_filename_part(&currency)
    )
}

fn safe_filename_part(value: &str) -> String {
    value
        .replace(['/', ':', '\\'], "_")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn write_non_ksef_manifest(invoices: &[MissingInvoice], non_ksef_dir: &Path) -> Result<()> {
    let manifest = non_ksef_dir.join("_manifest.csv");
    let mut writer = csv::Writer::from_path(&manifest)
        .with_context(|| format!("zapis CSV {}", manifest.display()))?;
    writer.write_record(["source", "target"])?;
    for invoice in invoices {
        if let Some(target) = &invoice.target_path {
            writer.write_record([invoice.source_path.clone(), target.clone()])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn run_mcp_server(db_path: &Path) -> Result<()> {
    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin.lock());
    let mut stdout = io::stdout();
    while let Some(message) = read_mcp_message(&mut reader)? {
        let request: Value = match serde_json::from_str(&message) {
            Ok(value) => value,
            Err(err) => {
                write_mcp_error(
                    &mut stdout,
                    Value::Null,
                    -32700,
                    &format!("Parse error: {err}"),
                )?;
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if method.starts_with("notifications/") {
            continue;
        }
        match handle_mcp_request(
            db_path,
            method,
            request.get("params").cloned().unwrap_or(Value::Null),
        ) {
            Ok(result) => write_mcp_result(&mut stdout, id, result)?,
            Err(err) => write_mcp_error(&mut stdout, id, -32603, &err.to_string())?,
        }
    }
    Ok(())
}

fn read_mcp_message<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut first = String::new();
    if reader.read_line(&mut first)? == 0 {
        return Ok(None);
    }
    if first.trim_start().starts_with('{') {
        return Ok(Some(first));
    }
    let mut content_length = None;
    let mut line = first;
    loop {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>()?);
        }
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
    }
    let len = content_length.ok_or_else(|| anyhow!("MCP message without Content-Length"))?;
    let mut bytes = vec![0u8; len];
    reader.read_exact(&mut bytes)?;
    Ok(Some(String::from_utf8(bytes)?))
}

fn write_mcp_result<W: Write>(writer: &mut W, id: Value, result: Value) -> Result<()> {
    write_mcp_message(
        writer,
        &serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}),
    )
}

fn write_mcp_error<W: Write>(writer: &mut W, id: Value, code: i32, message: &str) -> Result<()> {
    write_mcp_message(
        writer,
        &serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
    )
}

fn write_mcp_message<W: Write>(writer: &mut W, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

fn handle_mcp_request(db_path: &Path, method: &str, params: Value) -> Result<Value> {
    match method {
        "initialize" => Ok(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "lab-mcp", "version": env!("CARGO_PKG_VERSION") }
        })),
        "tools/list" => Ok(serde_json::json!({ "tools": mcp_tools() })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("tools/call missing name"))?;
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let value = call_mcp_tool(db_path, name, &args)?;
            Ok(serde_json::json!({
                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value)? }]
            }))
        }
        _ => Err(anyhow!("unsupported MCP method: {method}")),
    }
}

fn mcp_tools() -> Value {
    serde_json::json!([
        {
            "name": "cycle_run",
            "description": "Run the recurring ProductMesh invoice flow: Gmail PDFs, KSeF sync, Saldeo fetch, tri-reconcile, missing report, temporal diff.",
            "inputSchema": {"type":"object","properties":{
                "year":{"type":"integer","default":2026},
                "ksef":{"type":"string","description":"Path to KSeF records JSON/JSONL"},
                "skip_gmail_fetch":{"type":"boolean","default":false},
                "skip_saldeo_fetch":{"type":"boolean","default":false},
                "copy_missing_non_ksef":{"type":"boolean","default":false},
                "gmail_max":{"type":"integer","default":500}
            }}
        },
        {
            "name": "cycle_missing",
            "description": "Read tri-reconcile report and list Gmail-only invoices missing in Saldeo; optionally copy to non-KSeF files.",
            "inputSchema": {"type":"object","properties":{
                "year":{"type":"integer","default":2026},
                "tri_report":{"type":"string"},
                "copy_non_ksef":{"type":"boolean","default":false}
            }}
        },
        {
            "name": "ksef_sync",
            "description": "Sync a local KSeF export/records path into LAB normalized records and optionally store in SQLite.",
            "inputSchema": {"type":"object","required":["input"],"properties":{"year":{"type":"integer","default":2026},"input":{"type":"string"},"out":{"type":"string"},"store":{"type":"boolean","default":false}}}
        },
        {
            "name": "saldeo_sync",
            "description": "Plan or execute upload of invoices missing in Saldeo from tri-reconcile or source records. Upload requires confirm=true; upload_url defaults to the verified generate-urls endpoint.",
            "inputSchema": {"type":"object","properties":{"year":{"type":"integer","default":2026},"tri_report":{"type":"string"},"mail":{"type":"string"},"ksef":{"type":"string"},"saldeo":{"type":"string"},"review_score":{"type":"integer","default":70},"confirm":{"type":"boolean","default":false},"upload_url":{"type":"string"},"file_field":{"type":"string","default":"file"}}}
        },
        {
            "name": "saldeo_fetch",
            "description": "Fetch Saldeo documents for a year using saved Playwright storage state.",
            "inputSchema": {"type":"object","properties":{"year":{"type":"integer","default":2026},"out":{"type":"string"},"store":{"type":"boolean","default":false}}}
        },
        {
            "name": "tri_reconcile",
            "description": "Compare Gmail/PDF records, KSeF records and Saldeo records.",
            "inputSchema": {"type":"object","required":["mail","ksef","saldeo"],"properties":{"mail":{"type":"string"},"ksef":{"type":"string"},"saldeo":{"type":"string"},"review_score":{"type":"integer","default":70},"store":{"type":"boolean","default":false},"year":{"type":"integer","default":2026}}}
        },
        {
            "name": "db_stats",
            "description": "Return SQLite record counts.",
            "inputSchema": {"type":"object","properties":{}}
        },
        {
            "name": "tri_runs",
            "description": "List temporal tri-reconcile runs and diff counters.",
            "inputSchema": {"type":"object","properties":{"limit":{"type":"integer","default":20}}}
        }
    ])
}

fn call_mcp_tool(db_path: &Path, name: &str, args: &Value) -> Result<Value> {
    match name {
        "cycle_run" => {
            let year = json_i32(args, "year", 2026);
            let summary = run_cycle(CycleRunConfig {
                db_path: db_path.to_path_buf(),
                year,
                productmesh_nip: json_string_arg(args, "productmesh_nip")
                    .unwrap_or_else(|| "5242920020".to_string()),
                ksef: json_path_arg(args, "ksef")
                    .unwrap_or_else(|| default_ksef_records_path(year)),
                gmail_client_secret: json_path_arg(args, "gmail_client_secret"),
                gmail_token_file: json_path_arg(args, "gmail_token_file"),
                gmail_query: json_string_arg(args, "gmail_query"),
                gmail_max: json_usize(args, "gmail_max", 500),
                skip_gmail_fetch: json_bool(args, "skip_gmail_fetch", false),
                mail_out: json_path_arg(args, "mail_out")
                    .unwrap_or_else(|| default_mail_out_path(year)),
                skip_saldeo_fetch: json_bool(args, "skip_saldeo_fetch", false),
                saldeo_out: json_path_arg(args, "saldeo_out")
                    .unwrap_or_else(|| default_saldeo_out_path(year)),
                out_dir: json_path_arg(args, "out").unwrap_or_else(|| PathBuf::from("out")),
                store: json_bool(args, "store", false),
                review_score: json_u8(args, "review_score", 70),
                copy_missing_non_ksef: json_bool(args, "copy_missing_non_ksef", false),
                accounting_root: json_path_arg(args, "accounting_root")
                    .unwrap_or_else(default_accounting_root),
            })?;
            Ok(serde_json::to_value(summary)?)
        }
        "cycle_missing" => {
            let year = json_i32(args, "year", 2026);
            let tri_report = json_path_arg(args, "tri_report")
                .unwrap_or_else(|| PathBuf::from(format!("out/tri-reconcile-{year}.json")));
            let report = read_tri_report(&tri_report)?;
            let mut missing = missing_report_from_tri(&report);
            if json_bool(args, "copy_non_ksef", false) {
                let dir = non_ksef_dir(
                    &json_path_arg(args, "accounting_root").unwrap_or_else(default_accounting_root),
                    year,
                );
                let copied = copy_missing_invoices(&mut missing.invoices, &dir)?;
                return Ok(serde_json::json!({"copied": copied, "report": missing}));
            }
            Ok(serde_json::to_value(missing)?)
        }
        "ksef_sync" => {
            let year = json_i32(args, "year", 2026);
            let input = json_path_arg(args, "input").ok_or_else(|| anyhow!("missing input"))?;
            let result = ksef_sync(year, &input, json_path_arg(args, "out").as_deref())?;
            if json_bool(args, "store", false) {
                let conn = open_db(db_path)?;
                store_records(&conn, &result.records)?;
            }
            Ok(serde_json::to_value(result.summary)?)
        }
        "saldeo_sync" => {
            let year = json_i32(args, "year", 2026);
            let tri_report = json_path_arg(args, "tri_report");
            let mail = json_path_arg(args, "mail");
            let ksef = json_path_arg(args, "ksef");
            let saldeo = json_path_arg(args, "saldeo");
            let mut plan = saldeo_sync_plan(SaldeoSyncPlanConfig {
                year,
                tri_report: tri_report.as_deref(),
                mail: mail.as_deref(),
                ksef: ksef.as_deref(),
                saldeo: saldeo.as_deref(),
                review_score: json_u8(args, "review_score", 70),
                confirm: json_bool(args, "confirm", false),
                upload_url: json_string_arg(args, "upload_url"),
            })?;
            if json_bool(args, "confirm", false) {
                let upload_url = json_string_arg(args, "upload_url")
                    .ok_or_else(|| anyhow!("upload_url is required when confirm=true"))?;
                saldeo_upload_plan(
                    &mut plan,
                    &json_path_arg(args, "storage_state")
                        .unwrap_or_else(default_saldeo_storage_state_path),
                    &upload_url,
                    &json_string_arg(args, "file_field").unwrap_or_else(|| "file".to_string()),
                )?;
            }
            Ok(serde_json::to_value(plan)?)
        }
        "saldeo_fetch" => {
            let year = json_i32(args, "year", 2026);
            let out = json_path_arg(args, "out").unwrap_or_else(|| default_saldeo_out_path(year));
            let result = saldeo_fetch(year, &default_saldeo_storage_state_path(), &out)?;
            if json_bool(args, "store", false) {
                let conn = open_db(db_path)?;
                store_records(&conn, &result.records)?;
            }
            Ok(serde_json::to_value(result.summary)?)
        }
        "tri_reconcile" => {
            let mail = json_path_arg(args, "mail").ok_or_else(|| anyhow!("missing mail"))?;
            let ksef = json_path_arg(args, "ksef").ok_or_else(|| anyhow!("missing ksef"))?;
            let saldeo = json_path_arg(args, "saldeo").ok_or_else(|| anyhow!("missing saldeo"))?;
            let report = tri_reconcile(
                load_records(SourceKind::Mail, &mail)?,
                load_records(SourceKind::Ksef, &ksef)?,
                load_saldeo_records(&saldeo)?,
                json_u8(args, "review_score", 70),
            );
            if json_bool(args, "store", false) {
                let conn = open_db(db_path)?;
                let diff =
                    store_tri_reconcile_report(&conn, json_i32(args, "year", 2026), &report)?;
                return Ok(serde_json::json!({"report": report, "temporal_diff": diff}));
            }
            Ok(serde_json::to_value(report)?)
        }
        "db_stats" => {
            let conn = open_db(db_path)?;
            Ok(serde_json::to_value(db_stats(&conn)?)?)
        }
        "tri_runs" => {
            let conn = open_db(db_path)?;
            Ok(list_tri_runs(&conn, json_usize(args, "limit", 20))?)
        }
        _ => Err(anyhow!("unknown MCP tool: {name}")),
    }
}

fn json_string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn json_path_arg(args: &Value, key: &str) -> Option<PathBuf> {
    json_string_arg(args, key).map(PathBuf::from)
}

fn json_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

fn json_i32(args: &Value, key: &str, default: i32) -> i32 {
    args.get(key)
        .and_then(|v| v.as_i64())
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(default)
}

fn json_usize(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(default)
}

fn json_u8(args: &Value, key: &str, default: u8) -> u8 {
    args.get(key)
        .and_then(|v| v.as_u64())
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(default)
}

fn default_accounting_root() -> PathBuf {
    std::env::var_os("PRODUCTMESH_ACCOUNTING_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ACCOUNTING"))
}

struct SaldeoSyncPlanConfig<'a> {
    year: i32,
    tri_report: Option<&'a Path>,
    mail: Option<&'a Path>,
    ksef: Option<&'a Path>,
    saldeo: Option<&'a Path>,
    review_score: u8,
    confirm: bool,
    upload_url: Option<String>,
}

fn saldeo_sync_plan(config: SaldeoSyncPlanConfig<'_>) -> Result<SaldeoSyncPlan> {
    let report = if let Some(path) = config.tri_report {
        read_tri_report(path)?
    } else {
        let mail = config
            .mail
            .ok_or_else(|| anyhow!("podaj --tri-report albo komplet --mail --ksef --saldeo"))?;
        let ksef = config
            .ksef
            .ok_or_else(|| anyhow!("podaj --tri-report albo komplet --mail --ksef --saldeo"))?;
        let saldeo = config
            .saldeo
            .ok_or_else(|| anyhow!("podaj --tri-report albo komplet --mail --ksef --saldeo"))?;
        tri_reconcile(
            load_records(SourceKind::Mail, mail)?,
            load_records(SourceKind::Ksef, ksef)?,
            load_saldeo_records(saldeo)?,
            config.review_score,
        )
    };
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for row in &report.rows {
        if row.saldeo.is_some() {
            continue;
        }
        let related_sources = [("mail", row.mail.as_ref()), ("ksef", row.ksef.as_ref())]
            .into_iter()
            .filter_map(|(name, record)| record.map(|_| name.to_string()))
            .collect::<Vec<_>>();
        let selected = row.mail.as_ref().or(row.ksef.as_ref());
        if let Some(record) = selected {
            let key = record
                .source_path
                .clone()
                .or_else(|| record.ksef_reference.clone())
                .or_else(|| record.invoice_number.clone())
                .unwrap_or_else(|| tri_row_key(row));
            if !seen.insert(key) {
                continue;
            }
            items.push(saldeo_sync_item_from_record(
                &row.status,
                record,
                related_sources,
            ));
        }
    }
    let summary = saldeo_sync_summary(&items);
    Ok(SaldeoSyncPlan {
        generated_at: Utc::now(),
        year: config.year,
        confirm: config.confirm,
        upload_url: config
            .upload_url
            .or_else(|| Some(DEFAULT_SALDEO_UPLOAD_URL.to_string())),
        summary,
        items,
    })
}

fn saldeo_sync_item_from_record(
    status: &str,
    record: &InvoiceRecord,
    related_sources: Vec<String>,
) -> SaldeoSyncItem {
    let source_path = record.source_path.clone();
    let can_upload = source_path
        .as_deref()
        .map(|path| Path::new(path).is_file())
        .unwrap_or(false);
    SaldeoSyncItem {
        status: status.to_string(),
        source: source_as_str(record.source).to_string(),
        related_sources,
        invoice_number: record.invoice_number.clone(),
        issue_date: record.issue_date,
        gross_amount_minor: record.gross_amount_minor,
        currency: record.currency.clone(),
        contractor: record
            .seller_name
            .clone()
            .or_else(|| record.buyer_name.clone()),
        source_path,
        can_upload,
        upload_status: if can_upload {
            "planned"
        } else {
            "missing_local_file"
        }
        .to_string(),
        saldeo_response_status: None,
        saldeo_response_body: None,
        error: None,
    }
}

fn saldeo_sync_summary(items: &[SaldeoSyncItem]) -> SaldeoSyncSummary {
    SaldeoSyncSummary {
        total_missing_saldeo: items.len(),
        uploadable_count: items.iter().filter(|i| i.can_upload).count(),
        missing_file_count: items.iter().filter(|i| !i.can_upload).count(),
        uploaded_count: items
            .iter()
            .filter(|i| i.upload_status == "uploaded")
            .count(),
        failed_count: items.iter().filter(|i| i.upload_status == "failed").count(),
    }
}

const DEFAULT_SALDEO_UPLOAD_URL: &str =
    "https://saldeo.brainshare.pl/rest/client/document/generate-urls-for-upload";

fn saldeo_upload_plan(
    plan: &mut SaldeoSyncPlan,
    storage_state: &Path,
    upload_url: &str,
    _file_field: &str,
) -> Result<()> {
    let session = read_saldeo_session(storage_state)?;
    let client = Client::builder().build()?;
    for item in &mut plan.items {
        if !item.can_upload {
            continue;
        }
        let Some(source_path) = &item.source_path else {
            continue;
        };
        let upload_year = item.issue_date.map(|d| d.year()).unwrap_or(plan.year);
        let upload_month = item
            .issue_date
            .map(|d| d.month())
            .unwrap_or_else(|| Utc::now().month());
        match saldeo_upload_file(
            &client,
            &session,
            upload_url,
            Path::new(source_path),
            upload_year,
            upload_month,
        ) {
            Ok((status, body)) => {
                item.upload_status = "uploaded".to_string();
                item.saldeo_response_status = Some(status);
                item.saldeo_response_body = Some(body);
            }
            Err(err) => {
                item.upload_status = "failed".to_string();
                item.error = Some(err.to_string());
            }
        }
    }
    plan.summary = saldeo_sync_summary(&plan.items);
    Ok(())
}

struct SaldeoSession {
    cookie_header: String,
    xsrf: String,
}

fn read_saldeo_session(storage_state: &Path) -> Result<SaldeoSession> {
    let storage: Value = serde_json::from_str(
        &fs::read_to_string(storage_state)
            .with_context(|| format!("odczyt sesji Saldeo {}", storage_state.display()))?,
    )?;
    let cookies = storage
        .get("cookies")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("storage-state Saldeo nie zawiera cookies"))?;
    let cookie_header = cookies
        .iter()
        .filter_map(|cookie| {
            let name = cookie.get("name")?.as_str()?;
            let value = cookie.get("value")?.as_str()?;
            Some(format!("{name}={value}"))
        })
        .collect::<Vec<_>>()
        .join("; ");
    let xsrf = cookies
        .iter()
        .find(|cookie| cookie.get("name").and_then(|v| v.as_str()) == Some("X-SALDEO-XSRF-C-TOKEN"))
        .and_then(|cookie| cookie.get("value"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("brak X-SALDEO-XSRF-C-TOKEN w storage-state; odśwież sesję Saldeo"))?
        .to_string();
    Ok(SaldeoSession {
        cookie_header,
        xsrf,
    })
}

fn saldeo_upload_file(
    client: &Client,
    session: &SaldeoSession,
    upload_url: &str,
    path: &Path,
    year: i32,
    month: u32,
) -> Result<(u16, String)> {
    let file_name = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| anyhow!("brak nazwy pliku: {}", path.display()))?;
    let bytes = fs::read(path).with_context(|| format!("odczyt {}", path.display()))?;
    let content_type = content_type_for_path(path);
    let body = serde_json::json!({
        "year": year,
        "month": month,
        "documentTypeId": -1,
        "files": [{
            "filename": file_name,
            "contentType": content_type,
            "size": bytes.len(),
        }],
        "clientId": null,
    });
    let response: Value = client
        .post(upload_url)
        .header("Cookie", &session.cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", &session.xsrf)
        .header("saldeoApp", "angularApp")
        .header("timeout", "60000")
        .json(&body)
        .send()
        .with_context(|| format!("Saldeo generate upload URL {}", path.display()))?
        .error_for_status()?
        .json()?;
    if response.get("status").and_then(|v| v.as_str()) != Some("SUCCESS") {
        return Err(anyhow!("Saldeo generate upload URL failed: {}", response));
    }
    let upload = response
        .get("data")
        .and_then(|v| v.get(file_name))
        .ok_or_else(|| anyhow!("Saldeo response missing file entry for {file_name}: {response}"))?;
    let doc_upload_id = upload
        .get("docUploadId")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("Saldeo response missing docUploadId: {upload}"))?;
    let signed_url = upload
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Saldeo response missing upload url: {upload}"))?;
    let download_filename = upload
        .get("downloadFilename")
        .and_then(|v| v.as_str())
        .unwrap_or(file_name);
    let local_storage = upload
        .get("localStorage")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let upload_result = if local_storage {
        let part =
            reqwest::blocking::multipart::Part::bytes(bytes).file_name(file_name.to_string());
        let form = reqwest::blocking::multipart::Form::new().part("file", part);
        client.put(signed_url).multipart(form).send()
    } else {
        client
            .put(signed_url)
            .header("Content-Type", content_type)
            .header(
                "Content-Disposition",
                format!("attachment; filename=\"{download_filename}\""),
            )
            .body(bytes)
            .send()
    };

    if let Err(err) = upload_result.and_then(|r| r.error_for_status()) {
        let _ = saldeo_reject_upload(client, session, doc_upload_id, &err.to_string());
        return Err(anyhow!("Saldeo signed upload failed: {err}"));
    }

    let confirm_url =
        format!("https://saldeo.brainshare.pl/rest/doc-upload/{doc_upload_id}/confirm");
    let confirm = client
        .post(&confirm_url)
        .header("Cookie", &session.cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", &session.xsrf)
        .header("saldeoApp", "angularApp")
        .header("timeout", "60000")
        .json(&serde_json::json!({}))
        .send()
        .with_context(|| format!("Saldeo confirm upload {doc_upload_id}"))?;
    let status = confirm.status().as_u16();
    let text = confirm.text().unwrap_or_default();
    if !(200..300).contains(&status) {
        let _ = saldeo_reject_upload(client, session, doc_upload_id, &text);
        return Err(anyhow!("Saldeo confirm failed HTTP {status}: {text}"));
    }
    Ok((status, text.chars().take(2000).collect()))
}

fn saldeo_reject_upload(
    client: &Client,
    session: &SaldeoSession,
    doc_upload_id: i64,
    reason: &str,
) -> Result<()> {
    let url = format!("https://saldeo.brainshare.pl/rest/doc-upload/{doc_upload_id}/reject");
    client
        .post(url)
        .header("Cookie", &session.cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", &session.xsrf)
        .header("saldeoApp", "angularApp")
        .header("timeout", "60000")
        .body(reason.to_string())
        .send()?
        .error_for_status()?;
    Ok(())
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("xml") => "application/xml",
        Some("json") => "application/json",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    }
}

fn write_saldeo_sync_csv(plan: &SaldeoSyncPlan, path: &Path) -> Result<()> {
    let mut writer =
        csv::Writer::from_path(path).with_context(|| format!("zapis CSV {}", path.display()))?;
    writer.write_record([
        "upload_status",
        "status",
        "source",
        "related_sources",
        "invoice_number",
        "issue_date",
        "gross_amount_minor",
        "currency",
        "contractor",
        "source_path",
        "can_upload",
        "saldeo_response_status",
        "error",
    ])?;
    for item in &plan.items {
        writer.write_record([
            item.upload_status.clone(),
            item.status.clone(),
            item.source.clone(),
            item.related_sources.join("+"),
            item.invoice_number.clone().unwrap_or_default(),
            item.issue_date.map(|d| d.to_string()).unwrap_or_default(),
            item.gross_amount_minor
                .map(|v| v.to_string())
                .unwrap_or_default(),
            item.currency.clone().unwrap_or_default(),
            item.contractor.clone().unwrap_or_default(),
            item.source_path.clone().unwrap_or_default(),
            item.can_upload.to_string(),
            item.saldeo_response_status
                .map(|v| v.to_string())
                .unwrap_or_default(),
            item.error.clone().unwrap_or_default(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn default_saldeo_storage_state_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let lab_path = home
        .join(".config")
        .join("lab")
        .join("saldeo-storage-state.json");
    if lab_path.exists() {
        return lab_path;
    }
    home.join(".config")
        .join("ksef-mail-reconcile")
        .join("saldeo-storage-state.json")
}

fn saldeo_fetch(year: i32, storage_state: &Path, out_dir: &Path) -> Result<SaldeoFetchResult> {
    fs::create_dir_all(out_dir).with_context(|| format!("mkdir {}", out_dir.display()))?;
    let storage: Value = serde_json::from_str(
        &fs::read_to_string(storage_state)
            .with_context(|| format!("odczyt sesji Saldeo {}", storage_state.display()))?,
    )?;
    let cookies = storage
        .get("cookies")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("storage-state Saldeo nie zawiera cookies"))?;
    let cookie_header = cookies
        .iter()
        .filter_map(|cookie| {
            let name = cookie.get("name")?.as_str()?;
            let value = cookie.get("value")?.as_str()?;
            Some(format!("{name}={value}"))
        })
        .collect::<Vec<_>>()
        .join("; ");
    let xsrf = cookies
        .iter()
        .find(|cookie| cookie.get("name").and_then(|v| v.as_str()) == Some("X-SALDEO-XSRF-C-TOKEN"))
        .and_then(|cookie| cookie.get("value"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow!("brak X-SALDEO-XSRF-C-TOKEN w storage-state; odśwież sesję Saldeo")
        })?;

    let client = Client::builder().build()?;
    let mut documents = Vec::new();
    for month in 1..=12 {
        let body = serde_json::json!({
            "pagination": {
                "pageNumber": 0,
                "pageSize": -1,
                "totalCount": 0,
                "columnSorted": { "sortColumn": "DOCUMENT_CREATE_DATE", "sortDirection": "ASC" }
            },
            "filter": {
                "period": { "partOfYear": month, "year": year, "selectionType": "selectedMonth" },
                "duplicatesEnable": false,
                "duplicates": false,
                "splitPayment": false,
                "types": [],
                "contractors": [],
                "stages": [],
                "categories": [],
                "registers": [],
                "tags": [],
                "assignUsers": [],
                "addedBy": [],
                "added": [],
                "paymentStatuses": [],
                "accountingPaymentTypes": [],
                "searchQuery": "",
                "selectKsefDocumentsYesCheckbox": false,
                "selectKsefDocumentsNoCheckbox": false,
                "ksefNumber": "",
                "ksefMiniWorkflowStatus": null,
                "ksefBoId": null,
                "dimensionReportDocumentIds": [],
                "dimensions": null
            }
        });
        let value: Value = client
            .post("https://saldeo.brainshare.pl/rest/client/document/list/search")
            .header("Cookie", &cookie_header)
            .header("X-SALDEO-XSRF-H-TOKEN", xsrf)
            .header("saldeoApp", "angularApp")
            .header("timeout", "60000")
            .json(&body)
            .send()?
            .error_for_status()
            .with_context(|| format!("Saldeo document/list/search month={month}"))?
            .json()?;
        let items = value
            .get("data")
            .and_then(|d| d.get("resultCollection"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for mut item in items {
            if let Value::Object(ref mut map) = item {
                map.insert("saldeoMonth".to_string(), serde_json::json!(month));
            }
            documents.push(item);
        }
    }

    let records = saldeo_documents_to_records(&documents);
    let raw_output = out_dir.join("documents.json");
    let records_output = out_dir.join("records.jsonl");
    fs::write(&raw_output, serde_json::to_vec_pretty(&documents)?)?;
    let mut jsonl = Vec::new();
    for record in &records {
        serde_json::to_writer(&mut jsonl, record)?;
        jsonl.push(b'\n');
    }
    fs::write(&records_output, jsonl)?;

    Ok(SaldeoFetchResult {
        summary: SaldeoFetchSummary {
            year,
            documents_count: documents.len(),
            records_count: records.len(),
            raw_output: raw_output.display().to_string(),
            records_output: records_output.display().to_string(),
        },
        records,
    })
}

fn load_saldeo_records(input: &Path) -> Result<Vec<InvoiceRecord>> {
    if input.extension().and_then(|e| e.to_str()) == Some("jsonl") {
        return load_records(SourceKind::Saldeo, input);
    }
    let text =
        fs::read_to_string(input).with_context(|| format!("odczyt Saldeo {}", input.display()))?;
    if let Ok(mut records) = serde_json::from_str::<Vec<InvoiceRecord>>(&text) {
        for record in &mut records {
            record.source = SourceKind::Saldeo;
        }
        return Ok(records);
    }
    let value: Value = serde_json::from_str(&text)?;
    let docs = value
        .as_array()
        .ok_or_else(|| anyhow!("Saldeo input musi być tablicą documents albo InvoiceRecord[]"))?;
    Ok(saldeo_documents_to_records(docs))
}

fn saldeo_documents_to_records(documents: &[Value]) -> Vec<InvoiceRecord> {
    documents
        .iter()
        .filter_map(saldeo_document_to_record)
        .collect()
}

fn saldeo_document_to_record(doc: &Value) -> Option<InvoiceRecord> {
    let invoice_number = json_string(doc, "number").or_else(|| json_string(doc, "name"));
    let ksef_reference = json_string(doc, "ksefNumber");
    if invoice_number.is_none() && ksef_reference.is_none() {
        return None;
    }
    let mut record = empty_record(SourceKind::Saldeo);
    record.invoice_number = invoice_number.map(|v| clean_invoice_number(&v));
    record.issue_date =
        json_string(doc, "issueDate").and_then(|v| parse_date(v.get(0..10).unwrap_or(&v)));
    record.sale_date =
        json_string(doc, "saleDate").and_then(|v| parse_date(v.get(0..10).unwrap_or(&v)));
    record.due_date =
        json_string(doc, "paymentDate").and_then(|v| parse_date(v.get(0..10).unwrap_or(&v)));
    record.gross_amount_minor = money_value_to_minor(doc.get("grossPrice"));
    record.net_amount_minor = money_value_to_minor(doc.get("netPrice"));
    record.vat_amount_minor = money_value_to_minor(doc.get("vatPrice"));
    record.currency = json_string(doc, "currency").or_else(|| {
        doc.get("grossPrice")
            .and_then(|v| v.get("currency"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });
    record.ksef_reference = ksef_reference;
    record.seller_name = json_string(doc, "contractorDescription")
        .or_else(|| json_string(doc, "contractorName"))
        .and_then(|v| clean_name(&v));
    record.source_path = json_string(doc, "downloadUrl").or_else(|| json_string(doc, "filename"));
    record.content_hash = json_string(doc, "documentId")
        .map(|id| format!("saldeo:{id}"))
        .or_else(|| record.ksef_reference.clone())
        .unwrap_or_else(|| {
            let raw = serde_json::to_string(doc).unwrap_or_default();
            hex::encode(Sha256::digest(raw.as_bytes()))
        });
    Some(record)
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn money_value_to_minor(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    let amount = value
        .get("value")
        .and_then(|v| v.as_f64())
        .or_else(|| value.as_f64())?;
    Some((amount * 100.0).round() as i64)
}

fn tri_reconcile(
    mail_records: Vec<InvoiceRecord>,
    ksef_records: Vec<InvoiceRecord>,
    saldeo_records: Vec<InvoiceRecord>,
    review_score: u8,
) -> TriReconcileReport {
    let mut rows = Vec::new();
    let mut used_ksef = HashSet::new();
    let mut used_saldeo = HashSet::new();

    for mail in &mail_records {
        let best_ksef = best_match(mail, &ksef_records, &used_ksef, review_score);
        let best_saldeo = best_match(mail, &saldeo_records, &used_saldeo, review_score);
        if let Some((idx, _)) = best_ksef {
            used_ksef.insert(idx);
        }
        if let Some((idx, _)) = best_saldeo {
            used_saldeo.insert(idx);
        }
        let ksef = best_ksef.map(|(idx, _)| ksef_records[idx].clone());
        let saldeo = best_saldeo.map(|(idx, _)| saldeo_records[idx].clone());
        let ksef_score_to_saldeo = match (&ksef, &saldeo) {
            (Some(k), Some(s)) => Some(score_pair(k, s).0),
            _ => None,
        };
        rows.push(TriRow {
            status: tri_status(true, ksef.is_some(), saldeo.is_some()).to_string(),
            mail_score_to_ksef: best_ksef.map(|(_, score)| score),
            mail_score_to_saldeo: best_saldeo.map(|(_, score)| score),
            ksef_score_to_saldeo,
            mail: Some(mail.clone()),
            ksef,
            saldeo,
        });
    }

    let mut used_ksef_extra = HashSet::new();
    for (ksef_idx, ksef) in ksef_records.iter().enumerate() {
        if used_ksef.contains(&ksef_idx) {
            continue;
        }
        if let Some((saldeo_idx, score)) =
            best_match(ksef, &saldeo_records, &used_saldeo, review_score)
        {
            used_ksef.insert(ksef_idx);
            used_ksef_extra.insert(ksef_idx);
            used_saldeo.insert(saldeo_idx);
            rows.push(TriRow {
                status: tri_status(false, true, true).to_string(),
                mail_score_to_ksef: None,
                mail_score_to_saldeo: None,
                ksef_score_to_saldeo: Some(score),
                mail: None,
                ksef: Some(ksef.clone()),
                saldeo: Some(saldeo_records[saldeo_idx].clone()),
            });
        }
    }

    for (idx, ksef) in ksef_records.iter().enumerate() {
        if !used_ksef.contains(&idx) && !used_ksef_extra.contains(&idx) {
            rows.push(TriRow {
                status: tri_status(false, true, false).to_string(),
                mail_score_to_ksef: None,
                mail_score_to_saldeo: None,
                ksef_score_to_saldeo: None,
                mail: None,
                ksef: Some(ksef.clone()),
                saldeo: None,
            });
        }
    }
    for (idx, saldeo) in saldeo_records.iter().enumerate() {
        if !used_saldeo.contains(&idx) {
            rows.push(TriRow {
                status: tri_status(false, false, true).to_string(),
                mail_score_to_ksef: None,
                mail_score_to_saldeo: None,
                ksef_score_to_saldeo: None,
                mail: None,
                ksef: None,
                saldeo: Some(saldeo.clone()),
            });
        }
    }

    let summary = TriSummary {
        mail_count: mail_records.len(),
        ksef_count: ksef_records.len(),
        saldeo_count: saldeo_records.len(),
        in_all_three: rows.iter().filter(|r| r.status == "in_all_three").count(),
        gmail_ksef_missing_saldeo: rows
            .iter()
            .filter(|r| r.status == "gmail_ksef_missing_saldeo")
            .count(),
        gmail_saldeo_missing_ksef: rows
            .iter()
            .filter(|r| r.status == "gmail_saldeo_missing_ksef")
            .count(),
        gmail_only: rows.iter().filter(|r| r.status == "gmail_only").count(),
        ksef_saldeo_missing_gmail: rows
            .iter()
            .filter(|r| r.status == "ksef_saldeo_missing_gmail")
            .count(),
        ksef_only: rows.iter().filter(|r| r.status == "ksef_only").count(),
        saldeo_only: rows.iter().filter(|r| r.status == "saldeo_only").count(),
    };
    TriReconcileReport {
        generated_at: Utc::now(),
        review_score,
        summary,
        rows,
    }
}

fn best_match(
    needle: &InvoiceRecord,
    haystack: &[InvoiceRecord],
    used: &HashSet<usize>,
    min_score: u8,
) -> Option<(usize, u8)> {
    haystack
        .iter()
        .enumerate()
        .filter(|(idx, _)| !used.contains(idx))
        .map(|(idx, candidate)| (idx, score_pair(needle, candidate).0))
        .filter(|(_, score)| *score >= min_score)
        .max_by_key(|(_, score)| *score)
}

fn tri_status(has_mail: bool, has_ksef: bool, has_saldeo: bool) -> &'static str {
    match (has_mail, has_ksef, has_saldeo) {
        (true, true, true) => "in_all_three",
        (true, true, false) => "gmail_ksef_missing_saldeo",
        (true, false, true) => "gmail_saldeo_missing_ksef",
        (true, false, false) => "gmail_only",
        (false, true, true) => "ksef_saldeo_missing_gmail",
        (false, true, false) => "ksef_only",
        (false, false, true) => "saldeo_only",
        (false, false, false) => "empty",
    }
}

fn write_tri_csv(report: &TriReconcileReport, path: &Path) -> Result<()> {
    let mut writer =
        csv::Writer::from_path(path).with_context(|| format!("zapis CSV {}", path.display()))?;
    writer.write_record([
        "status",
        "mail_invoice_number",
        "ksef_invoice_number",
        "saldeo_invoice_number",
        "ksef_number",
        "saldeo_ksef_number",
        "issue_date",
        "gross_amount_minor",
        "currency",
        "mail_score_to_ksef",
        "mail_score_to_saldeo",
        "ksef_score_to_saldeo",
    ])?;
    for row in &report.rows {
        let primary = row
            .mail
            .as_ref()
            .or(row.ksef.as_ref())
            .or(row.saldeo.as_ref());
        writer.write_record([
            row.status.clone(),
            row.mail
                .as_ref()
                .and_then(|r| r.invoice_number.clone())
                .unwrap_or_default(),
            row.ksef
                .as_ref()
                .and_then(|r| r.invoice_number.clone())
                .unwrap_or_default(),
            row.saldeo
                .as_ref()
                .and_then(|r| r.invoice_number.clone())
                .unwrap_or_default(),
            row.ksef
                .as_ref()
                .and_then(|r| r.ksef_reference.clone())
                .unwrap_or_default(),
            row.saldeo
                .as_ref()
                .and_then(|r| r.ksef_reference.clone())
                .unwrap_or_default(),
            primary
                .and_then(|r| r.issue_date)
                .map(|d| d.to_string())
                .unwrap_or_default(),
            primary
                .and_then(|r| r.gross_amount_minor)
                .map(|v| v.to_string())
                .unwrap_or_default(),
            primary.and_then(|r| r.currency.clone()).unwrap_or_default(),
            row.mail_score_to_ksef
                .map(|v| v.to_string())
                .unwrap_or_default(),
            row.mail_score_to_saldeo
                .map(|v| v.to_string())
                .unwrap_or_default(),
            row.ksef_score_to_saldeo
                .map(|v| v.to_string())
                .unwrap_or_default(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn doctor(token_env: &str) -> Result<()> {
    let token_ok = std::env::var(token_env)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let pdftotext_ok = Command::new("pdftotext").arg("-v").output().is_ok();
    let status = serde_json::json!({
        "gmail_token_env": token_env,
        "gmail_token_present": token_ok,
        "pdftotext_present": pdftotext_ok,
        "notes": [
            "GmailFetch wymaga tokenu OAuth z zakresem gmail.readonly.",
            "PDF-y są parsowane przez pdftotext; bez niego program nadal obsłuży XML/JSON/TXT/EML."
        ]
    });
    write_json(&status, None)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let text = "Subject: Faktura FV/9/2026\nFrom: billing@example.com\nNIP 521-000-00-01\nData: 01.05.2026\nRazem do zapłaty: 99,90 PLN";
        let record = parse_text_invoice(SourceKind::Mail, text);
        assert_eq!(record.invoice_number.as_deref(), Some("FV/9/2026"));
        assert_eq!(record.seller_tax_id.as_deref(), Some("5210000001"));
        assert_eq!(record.gross_amount_minor, Some(9990));
        assert_eq!(record.currency.as_deref(), Some("PLN"));
    }
}
