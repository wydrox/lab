use anyhow::{Context, Result, anyhow};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use dialoguer::{Confirm, Input, Password, Select, theme::ColorfulTheme};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};
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
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;
use walkdir::WalkDir;

const KEYCHAIN_SERVICE: &str = "lab-cli";
const KEYCHAIN_ACCOUNT_GMAIL_TOKEN: &str = "gmail_token";
const KEYCHAIN_ACCOUNT_SALDEO_STORAGE_STATE: &str = "saldeo_storage_state";

#[derive(Parser, Debug)]
#[command(name = "lab-cli")]
#[command(about = "LAB — Lazy Accounting Buddy", long_about = None)]
struct Cli {
    /// Dedykowana baza SQLite na rekordy, przebiegi i dopasowania.
    #[arg(long, global = true, default_value = "lab.sqlite")]
    db: PathBuf,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Konfiguruje środowisko: sprawdza Gmail, Saldeo, bazę danych.
    Onboard {
        /// Tylko sprawdź status, nie uruchamiaj kreatora.
        #[arg(long)]
        check: bool,
        /// Google OAuth Desktop Client JSON do autoryzacji Gmail.
        #[arg(long)]
        gmail_client_secret: Option<PathBuf>,
    },
    /// Synchronizuje dane z Gmaila/PDF, KSeF i/lub Saldeo.
    /// Bez flag synchronizuje wszystkie trzy źródła.
    Sync {
        /// Tylko KSeF.
        #[arg(long)]
        ksef: bool,
        /// Tylko Gmail/PDF (pobiera załączniki, parsuje, filtruje).
        #[arg(long)]
        mail: bool,
        /// Tylko Saldeo.
        #[arg(long)]
        saldeo: bool,
        /// Rok rozliczeniowy.
        #[arg(long, default_value_t = 2026)]
        year: i32,
        /// Katalog/plik z eksportem KSeF (XML, JSON, JSONL). Domyślnie data/ksef-<year>.
        #[arg(long)]
        ksef_input: Option<PathBuf>,
        /// Google OAuth Desktop Client JSON do odświeżenia tokenu Gmail.
        #[arg(long)]
        gmail_client_secret: Option<PathBuf>,
        /// Plik tokenu Gmail; domyślnie ~/.config/lab/gmail_token.json.
        #[arg(long)]
        gmail_token_file: Option<PathBuf>,
        /// NIP do filtrowania PDF-ów z Gmaila.
        #[arg(long, default_value = "5242920020")]
        productmesh_nip: String,
        /// Zapisz rekordy do SQLite.
        #[arg(long)]
        store: bool,
    },
    /// Porównuje rekordy z Gmaila/PDF, KSeF i Saldeo.
    /// Z --status pokazuje ostatni raport z bazy.
    Reconcile {
        /// Pokaż ostatni raport uzgodnienia z bazy zamiast liczyć na nowo.
        #[arg(long)]
        status: bool,
        /// JSON/JSONL z rekordami Gmail/PDF.
        #[arg(long)]
        mail: Option<PathBuf>,
        /// JSON/JSONL z rekordami KSeF.
        #[arg(long)]
        ksef: Option<PathBuf>,
        /// Raw documents.json z Saldeo albo JSON/JSONL z rekordami Saldeo.
        #[arg(long)]
        saldeo: Option<PathBuf>,
        /// Minimalny score dopasowania.
        #[arg(long, default_value_t = 45)]
        review_score: u8,
        /// Plik JSON z raportem.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Wypisz pełny JSON zamiast czytelnego podsumowania.
        #[arg(long)]
        raw: bool,
        /// Opcjonalny CSV z raportem.
        #[arg(long)]
        csv: Option<PathBuf>,
        /// Zapisz temporalny snapshot tri-reconcile w SQLite.
        #[arg(long)]
        store: bool,
        /// Rok przy --store i --status.
        #[arg(long, default_value_t = 2026)]
        year: i32,
    },
    /// Wysyła brakujące faktury do SaldeoSMART.
    Upload {
        /// Rok rozliczeniowy.
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
        /// Minimalny score dopasowania, gdy raport jest liczony z wejść.
        #[arg(long, default_value_t = 70)]
        review_score: u8,
        /// Plik JSON z wynikiem.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Opcjonalny CSV z wynikiem.
        #[arg(long)]
        csv: Option<PathBuf>,
        /// Wykonaj upload do Saldeo. Bez tej flagi zwraca tylko plan.
        #[arg(long)]
        confirm: bool,
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

#[derive(Debug, Clone, Serialize)]
struct TemporalDiffSummary {
    run_id: i64,
    previous_run_id: Option<i64>,
    added_count: usize,
    removed_count: usize,
    changed_count: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let db_path = cli.db;
    let Some(command) = cli.command else {
        return interactive_tui(&db_path);
    };
    match command {
        Commands::Onboard {
            check,
            gmail_client_secret,
        } => {
            onboard(&db_path, check, gmail_client_secret.as_deref())?;
        }
        Commands::Sync {
            ksef,
            mail,
            saldeo,
            year,
            ksef_input,
            gmail_client_secret,
            gmail_token_file,
            productmesh_nip,
            store,
        } => {
            let all = !ksef && !mail && !saldeo;
            if all {
                eprintln!("Sync: wszystkie źródła (KSeF + Gmail/PDF + Saldeo)");
            }
            let conn = if store {
                Some(open_db(&db_path)?)
            } else {
                None
            };
            let mut synced: Vec<String> = Vec::new();
            if ksef || all {
                eprintln!("  [KSeF] synchronizacja...");
                let input = ksef_input
                    .clone()
                    .unwrap_or_else(|| configured_ksef_out_path(year));
                let result = ksef_sync(year, &input, None)?;
                if let Some(ref conn) = conn {
                    store_records(conn, &result.records)?;
                }
                eprintln!("  [KSeF] gotowe: {} rekordów", result.summary.records_count);
                synced.push(format!("ksef ({})", result.summary.records_count));
            }
            if mail || all {
                eprintln!("  [Gmail] sprawdzanie wiadomości i cache załączników...");
                let token_path = gmail_token_file
                    .clone()
                    .unwrap_or_else(default_gmail_token_path);
                let token = gmail_access_token(
                    "GMAIL_ACCESS_TOKEN",
                    &token_path,
                    gmail_client_secret.as_deref(),
                )?;
                let mail_out = default_mail_out_path(year);
                let gmail_result = gmail_fetch(
                    &token,
                    "me",
                    &default_gmail_query(year),
                    &mail_out,
                    500,
                    &["pdf".to_string()],
                )?;
                eprintln!(
                    "  [Gmail] wiadomości: {} znalezionych, {} z cache, {} pobranych z API; nowe pliki: {} metadane, {} załączniki",
                    gmail_result.messages_seen,
                    gmail_result.messages_cached,
                    gmail_result.messages_fetched,
                    gmail_result.metadata_saved,
                    gmail_result.attachments_saved
                );
                eprintln!("  [Gmail] skanowanie nowych PDF...");
                let (mail_records, parsed_count) =
                    sync_mail_records(&mail_out, &gmail_result.saved_files)?;
                eprintln!("  [Gmail] sparsowano {} nowych PDF", parsed_count);
                let mut candidates =
                    productmesh_invoice_candidates(&mail_records, &productmesh_nip);
                let cached_candidates = apply_cached_mail_candidates(year, &mut candidates)?;
                enrich_candidates_with_gemma(&mut candidates, &cached_candidates)?;
                write_records(
                    &candidates,
                    OutputFormat::Jsonl,
                    Some(&default_mail_candidates_path(year)),
                )?;
                if let Some(ref conn) = conn {
                    store_records(conn, &candidates)?;
                }
                eprintln!(
                    "  [Gmail] gotowe: {} PDF, {} faktur",
                    mail_records.len(),
                    candidates.len()
                );
                synced.push(format!(
                    "mail ({} new attachments, {} pdfs, {} candidates)",
                    gmail_result.attachments_saved,
                    mail_records.len(),
                    candidates.len()
                ));
            }
            if saldeo || all {
                eprintln!("  [Saldeo] pobieranie dokumentów...");
                let result = saldeo_fetch(
                    year,
                    &default_saldeo_storage_state_path(),
                    &default_saldeo_out_path(year),
                )?;
                if let Some(ref conn) = conn {
                    store_records(conn, &result.records)?;
                }
                eprintln!(
                    "  [Saldeo] gotowe: {} dokumentów",
                    result.summary.documents_count
                );
                synced.push(format!("saldeo ({})", result.summary.documents_count));
            }
            write_json(
                &serde_json::json!({"synced": synced, "year": year, "stored": store}),
                None,
            )?;
        }
        Commands::Reconcile {
            status,
            mail,
            ksef,
            saldeo,
            review_score,
            output,
            raw,
            csv,
            store,
            year,
        } => {
            if status {
                let conn = open_db(&db_path)?;
                let report = load_last_tri_report(&conn, year)?;
                if raw || output.is_some() {
                    return write_json(&report, output.as_deref());
                }
                return write_reconcile_human(&report, None);
            }
            let mail_path = mail.unwrap_or_else(|| default_mail_candidates_path(year));
            let ksef_path = ksef.unwrap_or_else(|| configured_ksef_out_path(year));
            let saldeo_path = saldeo.unwrap_or_else(|| default_saldeo_records_path(year));
            let mail_records = load_records(SourceKind::Mail, &mail_path)?;
            let ksef_records = load_records(SourceKind::Ksef, &ksef_path)?;
            let saldeo_records = load_saldeo_records(&saldeo_path)?;
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
            if output.is_some() {
                write_json(&report, output.as_deref())?;
            } else if raw {
                if temporal_diff.is_some() {
                    write_json(
                        &serde_json::json!({"report": report, "temporal_diff": temporal_diff}),
                        None,
                    )?;
                } else {
                    write_json(&report, None)?;
                }
            } else {
                write_reconcile_human(&report, temporal_diff.as_ref())?;
            }
        }
        Commands::Upload {
            year,
            tri_report,
            mail,
            ksef,
            saldeo,
            review_score,
            output,
            csv,
            confirm,
        } => {
            let mut plan = saldeo_sync_plan(SaldeoSyncPlanConfig {
                year,
                tri_report: tri_report.as_deref(),
                mail: mail.as_deref(),
                ksef: ksef.as_deref(),
                saldeo: saldeo.as_deref(),
                review_score,
                confirm,
                upload_url: None,
            })?;
            if confirm {
                let storage_state = default_saldeo_storage_state_path();
                saldeo_upload_plan(&mut plan, &storage_state, DEFAULT_SALDEO_UPLOAD_URL, "file")?;
            }
            if let Some(csv_path) = csv {
                write_saldeo_sync_csv(&plan, &csv_path)?;
            }
            write_json(&plan, output.as_deref())?;
        }
        Commands::Mcp => run_mcp_server(&db_path)?,
        Commands::Db { command } => handle_db_command(&db_path, command)?,
        Commands::Doctor { token_env } => doctor(&token_env)?,
    }
    Ok(())
}

fn interactive_tui(_db_path: &Path) -> Result<()> {
    interactive_reconcile_actions()
}

fn interactive_reconcile_actions() -> Result<()> {
    let theme = ColorfulTheme::default();
    let mut year: i32 = 2026;
    let mut review_score: u8 = 70;
    let mut rows = build_invoice_table_rows(year, review_score)?;

    loop {
        match run_invoice_table_tui(&mut rows, &mut year, &mut review_score)? {
            TuiResult::Commit => break,
            TuiResult::Cancel => return Ok(()),
            TuiResult::Doctor => {
                eprintln!("  [LAB] diagnostyka...");
                doctor("GMAIL_ACCESS_TOKEN")?;
            }
            TuiResult::Onboard => {
                eprintln!("  [LAB] konfiguracja...");
                onboard(&PathBuf::from("lab.sqlite"), false, None)?;
            }
            TuiResult::SaldeoAuth => {
                eprintln!("  [LAB] odświeżanie sesji Saldeo...");
                run_saldeo_auth_script()?;
            }
            _ => {}
        }
    }

    let selected_upload_items = rows
        .iter()
        .filter(|row| row.action == InvoiceTableAction::Upload)
        .filter_map(|row| row.upload_item.clone())
        .collect::<Vec<_>>();
    let selected_approve_ids = rows
        .iter()
        .filter(|row| row.action == InvoiceTableAction::ApproveKsef)
        .filter_map(|row| row.ksef_document_id)
        .collect::<Vec<_>>();
    let selected_reject_ids = rows
        .iter()
        .filter(|row| row.action == InvoiceTableAction::RejectKsef)
        .filter_map(|row| row.ksef_document_id)
        .collect::<Vec<_>>();

    eprintln!(
        "  [LAB] wybrano: {} upload, {} zatwierdź KSeF, {} odrzuć KSeF",
        selected_upload_items.len(),
        selected_approve_ids.len(),
        selected_reject_ids.len()
    );
    if selected_upload_items.is_empty()
        && selected_approve_ids.is_empty()
        && selected_reject_ids.is_empty()
    {
        eprintln!("  [LAB] nic nie wybrano — bez zmian");
        return Ok(());
    }
    if !Confirm::with_theme(&theme)
        .with_prompt("Wykonać wybrane operacje w Saldeo?")
        .default(false)
        .interact()?
    {
        eprintln!("  [LAB] anulowano — bez zmian");
        return Ok(());
    }

    let session = read_saldeo_session(&default_saldeo_storage_state_path())?;
    let mut result = serde_json::json!({
        "year": year,
        "uploaded": null,
        "approved_ksef_document_ids": selected_approve_ids,
        "rejected_ksef_document_ids": selected_reject_ids,
    });
    if !selected_upload_items.is_empty() {
        let mut upload_plan = SaldeoSyncPlan {
            generated_at: Utc::now(),
            year,
            confirm: true,
            upload_url: Some(DEFAULT_SALDEO_UPLOAD_URL.to_string()),
            summary: saldeo_sync_summary(&selected_upload_items),
            items: selected_upload_items,
        };
        saldeo_upload_plan(
            &mut upload_plan,
            &default_saldeo_storage_state_path(),
            DEFAULT_SALDEO_UPLOAD_URL,
            "file",
        )?;
        result["uploaded"] = serde_json::to_value(&upload_plan)?;
    }
    if !selected_approve_ids.is_empty() {
        result["approve_response"] =
            saldeo_mark_ksef_documents(&session, &selected_approve_ids, true)?;
    }
    if !selected_reject_ids.is_empty() {
        result["reject_response"] =
            saldeo_mark_ksef_documents(&session, &selected_reject_ids, false)?;
    }
    write_json(&result, None)
}

enum TuiResult {
    Commit,
    Cancel,
    Rebuild { year: i32, threshold: u8 },
    Sync { year: i32 },
    Llm { year: i32 },
    Doctor,
    Onboard,
    SaldeoAuth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvoiceTableAction {
    None,
    Upload,
    ApproveKsef,
    RejectKsef,
}

#[derive(Debug, Clone)]
struct InvoiceTableRow {
    selected: bool,
    sources: String,
    record: InvoiceRecord,
    upload_item: Option<SaldeoSyncItem>,
    ksef_document_id: Option<i64>,
    ksef_accounting: Option<bool>,
    action: InvoiceTableAction,
}

impl InvoiceTableRow {
    fn is_actionable(&self) -> bool {
        self.upload_item.is_some() || self.ksef_document_id.is_some()
    }

    fn can_upload(&self) -> bool {
        self.upload_item.is_some()
    }

    fn can_mark_ksef(&self) -> bool {
        self.ksef_document_id.is_some()
    }
}

fn build_invoice_table_rows(year: i32, review_score: u8) -> Result<Vec<InvoiceTableRow>> {
    let mail = default_mail_candidates_path(year);
    let ksef = configured_ksef_out_path(year);
    let saldeo = default_saldeo_records_path(year);
    let mail_records = load_records(SourceKind::Mail, &mail)?;
    let ksef_records = load_records(SourceKind::Ksef, &ksef)?;
    let saldeo_records = load_saldeo_records(&saldeo)?;
    let report = tri_reconcile(
        mail_records,
        ksef_records,
        saldeo_records.clone(),
        review_score,
    );

    let ksef_ids = saldeo_ksef_accounting_candidates(&saldeo_records)
        .into_iter()
        .map(|candidate| candidate.document_id)
        .collect::<Vec<_>>();
    let ksef_statuses = if ksef_ids.is_empty() {
        HashMap::new()
    } else {
        let session = read_saldeo_session(&default_saldeo_storage_state_path())?;
        saldeo_fetch_ksef_accounting_statuses(&session, &ksef_ids)?
    };

    let rows = report
        .rows
        .iter()
        .filter_map(|row| invoice_table_row_from_reconcile_row(row, &ksef_statuses))
        .collect::<Vec<_>>();
    Ok(rows)
}

fn invoice_table_row_from_reconcile_row(
    row: &TriRow,
    ksef_statuses: &HashMap<i64, Option<bool>>,
) -> Option<InvoiceTableRow> {
    let record = row
        .mail
        .as_ref()
        .or(row.ksef.as_ref())
        .or(row.saldeo.as_ref())?
        .clone();
    let upload_item = row.mail.as_ref().and_then(|mail| {
        if row.saldeo.is_some() {
            return None;
        }
        let related_sources = [("mail", row.mail.as_ref()), ("ksef", row.ksef.as_ref())]
            .into_iter()
            .filter_map(|(name, record)| record.map(|_| name.to_string()))
            .collect::<Vec<_>>();
        let item = saldeo_sync_item_from_record(&row.status, mail, related_sources);
        item.can_upload.then_some(item)
    });
    let (ksef_document_id, ksef_accounting) = row
        .saldeo
        .as_ref()
        .and_then(saldeo_document_id)
        .map(|document_id| {
            let accounting = ksef_statuses.get(&document_id).copied().flatten();
            let actionable_id = if accounting.is_none() {
                Some(document_id)
            } else {
                None
            };
            (actionable_id, accounting)
        })
        .unwrap_or((None, None));
    Some(InvoiceTableRow {
        selected: false,
        sources: row_source_mask(row),
        record,
        upload_item,
        ksef_document_id,
        ksef_accounting,
        action: InvoiceTableAction::None,
    })
}

fn invoice_table_counts(rows: &[InvoiceTableRow]) -> (usize, usize, usize, usize) {
    let mut u = 0;
    let mut a = 0;
    let mut r = 0;
    let mut s = 0;
    for row in rows {
        if row.selected {
            s += 1;
        }
        match row.action {
            InvoiceTableAction::Upload => u += 1,
            InvoiceTableAction::ApproveKsef => a += 1,
            InvoiceTableAction::RejectKsef => r += 1,
            InvoiceTableAction::None => {}
        }
    }
    (u, a, r, s)
}

fn run_invoice_table_tui(
    rows: &mut Vec<InvoiceTableRow>,
    year: &mut i32,
    review_score: &mut u8,
) -> Result<TuiResult> {
    let mut terminal = ratatui::init();
    let mut table_sel = 0usize;
    let mut menu_sel = 0usize;
    let mut actionable_only = false;
    let mut editing_year: Option<String> = None;
    let mut editing_threshold: Option<String> = None;
    let mut paint_mode: bool = false;
    let mut menu_open: bool = false;
    let mut spinner_frame: usize = 0;
    let mut status_message: String = String::new();
    let mut pending_action: Option<TuiResult> = None;
    let mut loop_result: Result<TuiResult> = Ok(TuiResult::Cancel);

    // Main menu
    const MI_SYNC: usize = 0;
    const MI_REFRESH: usize = 1;
    const MI_RECONCILE: usize = 2;
    const MI_LLM: usize = 3;
    const MI_UPLOAD: usize = 4;
    const MI_APPROVE: usize = 5;
    const MI_REJECT: usize = 6;
    const MI_CLEAR: usize = 7;
    const MI_MENU: usize = 8;
    const MAIN_COUNT: usize = 9;
    // Submenu (when menu_open)
    const SM_SALDEO: usize = 0;
    const SM_YEAR: usize = 1;
    const SM_THRESHOLD: usize = 2;
    const SM_DOCTOR: usize = 3;
    const SM_ONBOARD: usize = 4;
    const SM_BACK: usize = 5;
    const SUB_COUNT: usize = 6;

    loop {
        spinner_frame = spinner_frame.wrapping_add(1);

        // Execute pending action if any
        if let Some(action) = pending_action.take() {
            match action {
                TuiResult::Sync { year: y } => {
                    status_message = format!("Sync Gmail dla roku {y}...");
                    terminal
                        .draw(|f| {
                            f.render_widget(Paragraph::new(status_message.as_str()), f.area());
                        })
                        .ok();
                    match (|| -> Result<Vec<InvoiceTableRow>> {
                        let token_path = default_gmail_token_path();
                        let token = gmail_access_token("GMAIL_ACCESS_TOKEN", &token_path, None)?;
                        let mail_out = default_mail_out_path(y);
                        gmail_fetch(
                            &token,
                            "me",
                            &default_gmail_query(y),
                            &mail_out,
                            500,
                            &["pdf".to_string()],
                        )?;
                        sync_mail_records(&mail_out, &[])?;
                        build_invoice_table_rows(y, *review_score)
                    })() {
                        Ok(new_rows) => {
                            *year = y;
                            *rows = new_rows;
                            status_message = format!("Sync zakończony: {} faktur", rows.len());
                        }
                        Err(e) => status_message = format!("Błąd sync: {e}"),
                    }
                }
                TuiResult::Llm { year: y } => {
                    status_message = format!("LLM wzbogacanie dla roku {y}...");
                    terminal
                        .draw(|f| {
                            f.render_widget(Paragraph::new(status_message.as_str()), f.area());
                        })
                        .ok();
                    match (|| -> Result<Vec<InvoiceTableRow>> {
                        let mail_path = default_mail_candidates_path(y);
                        if mail_path.exists() {
                            let mut candidates = load_records(SourceKind::Mail, &mail_path)?;
                            let cached = apply_cached_mail_candidates(y, &mut candidates)?;
                            enrich_candidates_with_gemma(&mut candidates, &cached)?;
                            write_records(
                                &candidates,
                                OutputFormat::Jsonl,
                                Some(&default_mail_candidates_path(y)),
                            )?;
                        }
                        build_invoice_table_rows(y, *review_score)
                    })() {
                        Ok(new_rows) => {
                            *rows = new_rows;
                            status_message = format!("LLM zakończony: {} faktur", rows.len());
                        }
                        Err(e) => status_message = format!("Błąd LLM: {e}"),
                    }
                }
                TuiResult::Rebuild {
                    year: y,
                    threshold: t,
                } => {
                    status_message = format!("Przebudowa tabeli (rok {y}, próg {t})...");
                    terminal
                        .draw(|f| {
                            f.render_widget(Paragraph::new(status_message.as_str()), f.area());
                        })
                        .ok();
                    match build_invoice_table_rows(y, t) {
                        Ok(new_rows) => {
                            *year = y;
                            *review_score = t;
                            *rows = new_rows;
                            status_message = format!("Tabela odświeżona: {} faktur", rows.len());
                        }
                        Err(e) => status_message = format!("Błąd: {e}"),
                    }
                }
                _ => {}
            }
            table_sel = 0;
        }

        let visible = rows
            .iter()
            .enumerate()
            .filter_map(|(idx, row)| (!actionable_only || row.is_actionable()).then_some(idx))
            .collect::<Vec<_>>();
        if visible.is_empty() {
            table_sel = 0;
        } else if table_sel >= visible.len() {
            table_sel = visible.len().saturating_sub(1);
        }
        let menu_items = if menu_open { SUB_COUNT } else { MAIN_COUNT };
        if menu_sel >= menu_items {
            menu_sel = menu_items.saturating_sub(1);
        }

        if let Err(err) = terminal.draw(|frame| {
            let area = frame.area();
            let chunks = Layout::vertical([
                Constraint::Min(5),
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

            // Table
            let header = Row::new(vec![
                Cell::from("sel"),
                Cell::from("akcja / KSeF"),
                Cell::from("G/K/S"),
                Cell::from("faktura"),
                Cell::from("kontrahent"),
                Cell::from("data"),
                Cell::from("brutto"),
            ])
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
            let table_rows = visible.iter().map(|vidx| {
                let row = &rows[*vidx];
                let sel_mark = if row.selected { "[*]" } else { "[ ]" };
                let style = match row.action {
                    InvoiceTableAction::Upload => Style::default().fg(Color::Cyan),
                    InvoiceTableAction::ApproveKsef => Style::default().fg(Color::Green),
                    InvoiceTableAction::RejectKsef => Style::default().fg(Color::Red),
                    InvoiceTableAction::None if row.is_actionable() => {
                        Style::default().fg(Color::White)
                    }
                    InvoiceTableAction::None => Style::default().fg(Color::DarkGray),
                };
                Row::new(vec![
                    Cell::from(sel_mark),
                    Cell::from(invoice_table_action_ksef_label(row)),
                    Cell::from(row.sources.clone()),
                    Cell::from(truncate(
                        row.record.invoice_number.as_deref().unwrap_or("-"),
                        24,
                    )),
                    Cell::from(truncate(&counterparty_name(Some(&row.record)), 26)),
                    Cell::from(
                        row.record
                            .issue_date
                            .map(|date| date.to_string())
                            .unwrap_or_else(|| "-".to_string()),
                    ),
                    Cell::from(
                        row.record
                            .gross_amount_minor
                            .map(format_minor_money)
                            .unwrap_or_else(|| "-".to_string()),
                    ),
                ])
                .style(style)
            });
            let table = Table::new(
                table_rows,
                [
                    Constraint::Percentage(4),
                    Constraint::Percentage(14),
                    Constraint::Percentage(7),
                    Constraint::Percentage(22),
                    Constraint::Percentage(23),
                    Constraint::Percentage(10),
                    Constraint::Percentage(20),
                ],
            )
            .header(header)
            .block(
                Block::default()
                    .title(format!(" LAB faktury {year} · próg {review_score} "))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .row_highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );
            let mut table_state = TableState::default();
            if !visible.is_empty() {
                table_state.select(Some(table_sel));
            }
            frame.render_stateful_widget(table, chunks[0], &mut table_state);

            // Menu bar
            let (u, a, r, sel) = invoice_table_counts(rows);
            let year_text = editing_year.clone().unwrap_or_else(|| year.to_string());
            let threshold_text = editing_threshold
                .clone()
                .unwrap_or_else(|| review_score.to_string());
            let mkbtn = |label: &str, idx: usize, editing: bool| {
                let text = if editing {
                    format!("[{}_]", label)
                } else if idx == menu_sel {
                    format!("[ {} ]", label)
                } else {
                    format!("  {}  ", label)
                };
                let style = if idx == menu_sel || editing {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                ratatui::text::Line::styled(text, style)
            };
            if menu_open {
                let items = [
                    mkbtn("Saldeo", SM_SALDEO, false),
                    mkbtn(
                        &format!("Rok:{}", year_text),
                        SM_YEAR,
                        editing_year.is_some(),
                    ),
                    mkbtn(
                        &format!("Próg:{}", threshold_text),
                        SM_THRESHOLD,
                        editing_threshold.is_some(),
                    ),
                    mkbtn("Doctor", SM_DOCTOR, false),
                    mkbtn("Onboard", SM_ONBOARD, false),
                    mkbtn("◀ Wróć", SM_BACK, false),
                ];
                let menu_area = Layout::horizontal([
                    Constraint::Fill(1),
                    Constraint::Length(12),
                    Constraint::Length(1),
                    Constraint::Length(12),
                    Constraint::Length(1),
                    Constraint::Length(10),
                    Constraint::Length(1),
                    Constraint::Length(10),
                    Constraint::Length(1),
                    Constraint::Length(12),
                    Constraint::Length(1),
                    Constraint::Length(13),
                ])
                .split(chunks[1]);
                for (i, line) in items.into_iter().enumerate() {
                    frame.render_widget(Paragraph::new(line), menu_area[i * 2 + 1]);
                }
            } else {
                let items = [
                    mkbtn("Sync", MI_SYNC, false),
                    mkbtn("Refresh", MI_REFRESH, false),
                    mkbtn("Reconcile", MI_RECONCILE, false),
                    mkbtn("LLM", MI_LLM, false),
                    mkbtn("Upload", MI_UPLOAD, false),
                    mkbtn("Zatwierdź", MI_APPROVE, false),
                    mkbtn("Odrzuć", MI_REJECT, false),
                    mkbtn("Wyczyść", MI_CLEAR, false),
                    mkbtn("☰ Menu", MI_MENU, false),
                ];
                let menu_area = Layout::horizontal([
                    Constraint::Length(11),
                    Constraint::Length(1),
                    Constraint::Length(13),
                    Constraint::Length(1),
                    Constraint::Length(14),
                    Constraint::Length(1),
                    Constraint::Length(10),
                    Constraint::Length(1),
                    Constraint::Length(12),
                    Constraint::Length(1),
                    Constraint::Length(14),
                    Constraint::Length(1),
                    Constraint::Length(12),
                    Constraint::Length(1),
                    Constraint::Length(12),
                    Constraint::Fill(1),
                    Constraint::Length(12),
                ])
                .split(chunks[1]);
                for (i, line) in items.into_iter().enumerate() {
                    frame.render_widget(Paragraph::new(line), menu_area[i * 2]);
                }
            }
            // Status bar
            if !status_message.is_empty() {
                let spinner_chars = ['|', '/', '-', '\\'];
                let spin = spinner_chars[spinner_frame % spinner_chars.len()];
                let line = ratatui::text::Line::styled(
                    format!(" {} {}", spin, status_message),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                );
                frame.render_widget(Paragraph::new(line), chunks[2]);
            }

            // Stats bar — stats left, help right
            let stats_text = format!(
                "sel:{} | up:{} | zatw:{} | odrz:{} | {}/{}",
                sel,
                u,
                a,
                r,
                visible.len(),
                rows.len()
            );
            let filter = if actionable_only {
                "[f] akcje"
            } else {
                "[f] wszystko"
            };
            let help = format!(
                "{} | f=filtr  spc=toggle  ⏎=select  ⌘c=commit  q=wyjdź",
                filter
            );
            let stats_span = ratatui::text::Span::styled(
                stats_text,
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::ITALIC),
            );
            let help_span = ratatui::text::Span::styled(help, Style::default().fg(Color::DarkGray));
            let stats_area = Layout::horizontal([
                Constraint::Fill(1),
                Constraint::Length(help_span.width() as u16),
            ])
            .split(chunks[3]);
            frame.render_widget(
                Paragraph::new(ratatui::text::Line::from(stats_span)),
                stats_area[0],
            );
            frame.render_widget(
                Paragraph::new(ratatui::text::Line::from(help_span)),
                stats_area[1],
            );
        }) {
            loop_result = Err(anyhow!("terminal draw: {err}"));
            break;
        }

        let event = match event::read() {
            Ok(event) => event,
            Err(err) => {
                loop_result = Err(anyhow!("terminal event: {err}"));
                break;
            }
        };
        let Event::Key(key) = event else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let cmd = key
            .modifiers
            .intersects(crossterm::event::KeyModifiers::SUPER)
            || key
                .modifiers
                .intersects(crossterm::event::KeyModifiers::META);
        let shift = key
            .modifiers
            .intersects(crossterm::event::KeyModifiers::SHIFT);

        // Handle field editing
        let editing = editing_year.is_some() || editing_threshold.is_some();
        if editing {
            let field = if editing_year.is_some() {
                editing_year.as_mut()
            } else {
                editing_threshold.as_mut()
            };
            match key.code {
                KeyCode::Esc => {
                    editing_year = None;
                    editing_threshold = None;
                }
                KeyCode::Enter => {
                    if let Some(val) = editing_year.take() {
                        if let Ok(y) = val.parse::<i32>() {
                            pending_action = Some(TuiResult::Rebuild {
                                year: y,
                                threshold: *review_score,
                            });
                        }
                    } else if let Some(val) = editing_threshold.take()
                        && let Ok(t) = val.parse::<u8>()
                    {
                        pending_action = Some(TuiResult::Rebuild {
                            year: *year,
                            threshold: t,
                        });
                    }
                }
                KeyCode::Backspace => {
                    if let Some(s) = field {
                        s.pop();
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    if let Some(s) = field {
                        s.push(c);
                    }
                }
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') => break,
            KeyCode::Esc => {
                if menu_open {
                    menu_open = false;
                } else {
                    break;
                }
            }
            KeyCode::Char('c') if cmd => {
                loop_result = Ok(TuiResult::Commit);
                break;
            }
            KeyCode::Enter if cmd => {
                loop_result = Ok(TuiResult::Commit);
                break;
            }
            KeyCode::Enter => {
                if menu_open {
                    match menu_sel {
                        SM_SALDEO => {
                            loop_result = Ok(TuiResult::SaldeoAuth);
                            break;
                        }
                        SM_YEAR => editing_year = Some(year.to_string()),
                        SM_THRESHOLD => editing_threshold = Some(review_score.to_string()),
                        SM_DOCTOR => {
                            loop_result = Ok(TuiResult::Doctor);
                            break;
                        }
                        SM_ONBOARD => {
                            loop_result = Ok(TuiResult::Onboard);
                            break;
                        }
                        SM_BACK => menu_open = false,
                        _ => {}
                    }
                } else {
                    match menu_sel {
                        MI_SYNC => {
                            pending_action = Some(TuiResult::Sync { year: *year });
                        }
                        MI_REFRESH => {
                            pending_action = Some(TuiResult::Rebuild {
                                year: *year,
                                threshold: *review_score,
                            });
                        }
                        MI_RECONCILE => {
                            pending_action = Some(TuiResult::Rebuild {
                                year: *year,
                                threshold: *review_score,
                            });
                        }
                        MI_LLM => {
                            pending_action = Some(TuiResult::Llm { year: *year });
                        }
                        MI_UPLOAD => {
                            for row in rows.iter_mut().filter(|r| r.selected) {
                                if row.can_upload() {
                                    row.action = InvoiceTableAction::Upload;
                                }
                            }
                        }
                        MI_APPROVE => {
                            for row in rows.iter_mut().filter(|r| r.selected) {
                                if row.can_mark_ksef() {
                                    row.action = InvoiceTableAction::ApproveKsef;
                                }
                            }
                        }
                        MI_REJECT => {
                            for row in rows.iter_mut().filter(|r| r.selected) {
                                if row.can_mark_ksef() {
                                    row.action = InvoiceTableAction::RejectKsef;
                                }
                            }
                        }
                        MI_CLEAR => {
                            for row in rows.iter_mut().filter(|r| r.selected) {
                                row.action = InvoiceTableAction::None;
                                row.selected = false;
                            }
                        }
                        MI_MENU => {
                            menu_open = true;
                            menu_sel = SM_BACK;
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Down => {
                if table_sel + 1 < visible.len() {
                    table_sel += 1;
                    if (paint_mode || shift)
                        && let Some(row_idx) = visible.get(table_sel).copied()
                    {
                        rows[row_idx].selected = !rows[row_idx].selected;
                    }
                }
            }
            KeyCode::Up => {
                if table_sel > 0 {
                    table_sel -= 1;
                    if (paint_mode || shift)
                        && let Some(row_idx) = visible.get(table_sel).copied()
                    {
                        rows[row_idx].selected = !rows[row_idx].selected;
                    }
                }
            }
            KeyCode::Right => {
                let max = if menu_open { SUB_COUNT } else { MAIN_COUNT };
                menu_sel = (menu_sel + 1).min(max.saturating_sub(1));
            }
            KeyCode::Left => {
                menu_sel = menu_sel.saturating_sub(1);
            }
            KeyCode::Home => {
                table_sel = 0;
                if paint_mode && let Some(row_idx) = visible.get(table_sel).copied() {
                    rows[row_idx].selected = !rows[row_idx].selected;
                }
            }
            KeyCode::End => {
                table_sel = visible.len().saturating_sub(1);
                if paint_mode && let Some(row_idx) = visible.get(table_sel).copied() {
                    rows[row_idx].selected = !rows[row_idx].selected;
                }
            }
            KeyCode::Char('f') => {
                actionable_only = !actionable_only;
                table_sel = 0;
            }
            KeyCode::Char('v') => {
                paint_mode = !paint_mode;
            }
            KeyCode::Char(' ') => {
                if let Some(row_idx) = visible.get(table_sel).copied() {
                    rows[row_idx].selected = !rows[row_idx].selected;
                }
            }
            _ => {}
        }
    }

    ratatui::restore();
    loop_result
}

fn invoice_table_action_ksef_label(row: &InvoiceTableRow) -> String {
    match row.action {
        InvoiceTableAction::Upload => "UPLOAD".to_string(),
        InvoiceTableAction::ApproveKsef => "ZATWIERDŹ".to_string(),
        InvoiceTableAction::RejectKsef => "ODRZUĆ".to_string(),
        InvoiceTableAction::None => invoice_table_ksef_status(row),
    }
}

fn invoice_table_ksef_status(row: &InvoiceTableRow) -> String {
    match (row.ksef_document_id, row.ksef_accounting) {
        (Some(_), None) => "nieozn.".to_string(),
        (_, Some(true)) => "zatw.".to_string(),
        (_, Some(false)) => "odrz.".to_string(),
        _ => "—".to_string(),
    }
}

fn row_source_mask(row: &TriRow) -> String {
    format!(
        "{}/{}/{}",
        if row.mail.is_some() { "G" } else { "-" },
        if row.ksef.is_some() { "K" } else { "-" },
        if row.saldeo.is_some() { "S" } else { "-" }
    )
}

#[derive(Debug, Clone)]
struct SaldeoKsefAccountingCandidate {
    document_id: i64,
}

fn saldeo_ksef_accounting_candidates(
    records: &[InvoiceRecord],
) -> Vec<SaldeoKsefAccountingCandidate> {
    let mut seen = HashSet::new();
    records
        .iter()
        .filter(|record| record.ksef_reference.is_some())
        .filter_map(|record| {
            let document_id = saldeo_document_id(record)?;
            if !seen.insert(document_id) {
                return None;
            }
            Some(SaldeoKsefAccountingCandidate { document_id })
        })
        .collect()
}

fn saldeo_document_id(record: &InvoiceRecord) -> Option<i64> {
    record.content_hash.strip_prefix("saldeo:")?.parse().ok()
}

fn saldeo_fetch_ksef_accounting_statuses(
    session: &SaldeoSession,
    document_ids: &[i64],
) -> Result<HashMap<i64, Option<bool>>> {
    if document_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let client = Client::builder().build()?;
    let body = serde_json::json!({"clientId": 0, "documentIds": document_ids});
    let response: Value = client
        .post("https://saldeo.brainshare.pl/rest/client/document/ksef/accounting")
        .header("Cookie", &session.cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", &session.xsrf)
        .header("saldeoApp", "angularApp")
        .json(&body)
        .send()?
        .error_for_status()?
        .json()?;
    if response.get("status").and_then(|v| v.as_str()) != Some("SUCCESS") {
        return Err(anyhow!("Saldeo KSeF accounting status failed: {response}"));
    }
    let mut out = HashMap::new();
    for item in response
        .get("data")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        if let Some(document_id) = item.get("documentId").and_then(|v| v.as_i64()) {
            out.insert(
                document_id,
                item.get("accounting").and_then(|v| v.as_bool()),
            );
        }
    }
    Ok(out)
}

fn saldeo_mark_ksef_documents(
    session: &SaldeoSession,
    document_ids: &[i64],
    mark_is_accounting: bool,
) -> Result<Value> {
    let client = Client::builder().build()?;
    let body = serde_json::json!({
        "ids": document_ids,
        "markIsAccounting": mark_is_accounting,
        "skipPreviousSetup": true,
    });
    let response: Value = client
        .post("https://saldeo.brainshare.pl/rest/client/document/list/bulkupdate/markAccounting")
        .header("Cookie", &session.cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", &session.xsrf)
        .header("saldeoApp", "angularApp")
        .json(&body)
        .send()?
        .error_for_status()?
        .json()?;
    if response.get("status").and_then(|v| v.as_str()) != Some("SUCCESS") {
        return Err(anyhow!("Saldeo markAccounting failed: {response}"));
    }
    Ok(response)
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

fn keychain_get_secret(account: &str) -> Result<Option<String>> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                account,
                "-w",
            ])
            .output()
            .with_context(|| "uruchomienie macOS security find-generic-password")?;
        if output.status.success() {
            let raw = String::from_utf8_lossy(&output.stdout)
                .trim_end_matches(['\r', '\n'])
                .to_string();
            Ok(Some(decode_keychain_secret(&raw).unwrap_or(raw)))
        } else {
            Ok(None)
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = account;
        Ok(None)
    }
}

fn decode_keychain_secret(raw: &str) -> Option<String> {
    if let Some(encoded) = raw.strip_prefix("base64:") {
        return STANDARD
            .decode(encoded)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
    }

    if raw.len().is_multiple_of(2) && raw.chars().all(|c| c.is_ascii_hexdigit()) {
        return hex::decode(raw)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
    }

    None
}

fn keychain_set_secret(account: &str, secret: &str) -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("security")
            .args([
                "add-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                account,
                "-U",
                "-w",
                &format!("base64:{}", STANDARD.encode(secret)),
            ])
            .status()
            .with_context(|| "uruchomienie macOS security add-generic-password")?;
        Ok(status.success())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (account, secret);
        Ok(false)
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

fn load_last_tri_report(conn: &Connection, year: i32) -> Result<TriReconcileReport> {
    let run_id: i64 = conn
        .query_row(
            "SELECT id FROM tri_reconcile_runs WHERE year = ?1 ORDER BY id DESC LIMIT 1",
            params![year],
            |row| row.get(0),
        )
        .with_context(|| format!("brak przebiegu tri-reconcile dla roku {year}"))?;
    let mut stmt = conn.prepare(
        "SELECT status, mail_invoice_number, ksef_invoice_number, saldeo_invoice_number,
                issue_date, gross_amount_minor, currency, row_json
         FROM tri_reconcile_rows WHERE run_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![run_id], |row| {
        let status: String = row.get(0)?;
        let row_json: String = row.get(7)?;
        let tri_row: TriRow = serde_json::from_str(&row_json).unwrap_or(TriRow {
            status,
            mail_score_to_ksef: None,
            mail_score_to_saldeo: None,
            ksef_score_to_saldeo: None,
            mail: None,
            ksef: None,
            saldeo: None,
        });
        Ok(tri_row)
    })?;
    let mut tri_rows = Vec::new();
    for row in rows {
        tri_rows.push(row?);
    }
    let summary = tri_summary_from_rows(&tri_rows);
    Ok(TriReconcileReport {
        generated_at: Utc::now(),
        review_score: 0,
        summary,
        rows: tri_rows,
    })
}

fn tri_summary_from_rows(rows: &[TriRow]) -> TriSummary {
    TriSummary {
        mail_count: rows.iter().filter(|r| r.mail.is_some()).count(),
        ksef_count: rows.iter().filter(|r| r.ksef.is_some()).count(),
        saldeo_count: rows.iter().filter(|r| r.saldeo.is_some()).count(),
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
    }
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
        for file_name in [
            "ksef_records.jsonl",
            "records.jsonl",
            "ksef_records.json",
            "records.json",
        ] {
            let candidate = input.join(file_name);
            if candidate.is_file() {
                return load_records(source, &candidate);
            }
        }
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

fn sync_mail_records(
    mail_out: &Path,
    saved_files: &[String],
) -> Result<(Vec<InvoiceRecord>, usize)> {
    let cache_path = mail_out.join("records.jsonl");
    if !cache_path.exists() {
        let records = scan_mail_input(mail_out)?;
        let parsed_count = records.len();
        write_records(&records, OutputFormat::Jsonl, Some(&cache_path))?;
        return Ok((records, parsed_count));
    }

    let mut records = load_records(SourceKind::Mail, &cache_path)?;
    let mut seen = records
        .iter()
        .map(|record| record.content_hash.clone())
        .collect::<HashSet<_>>();
    let mut parsed_count = 0usize;
    for path in saved_files.iter().map(PathBuf::from) {
        if !is_mail_candidate_file(&path) {
            continue;
        }
        let record = parse_file(SourceKind::Mail, &path)?;
        if seen.insert(record.content_hash.clone()) {
            records.push(record);
            parsed_count += 1;
        }
    }
    if parsed_count > 0 {
        write_records(&records, OutputFormat::Jsonl, Some(&cache_path))?;
    }
    Ok((records, parsed_count))
}

fn scan_mail_input(input: &Path) -> Result<Vec<InvoiceRecord>> {
    let mut files = Vec::new();
    if input.is_dir() {
        for entry in WalkDir::new(input).into_iter().filter_map(Result::ok) {
            let path = entry.path();
            if entry.file_type().is_file() && is_mail_candidate_file(path) {
                files.push(path.to_path_buf());
            }
        }
    } else if input.is_file() && is_mail_candidate_file(input) {
        files.push(input.to_path_buf());
    } else {
        return Err(anyhow!(
            "input nie istnieje albo nie jest wspieranym plikiem mail: {}",
            input.display()
        ));
    }

    files.sort();
    files
        .iter()
        .map(|path| parse_file(SourceKind::Mail, path))
        .collect::<Result<Vec<_>>>()
}

fn is_mail_candidate_file(path: &Path) -> bool {
    is_supported_file(path) && !is_gmail_metadata_path(path)
}

fn is_gmail_metadata_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.ends_with("_message.json") || name == "records.json")
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

fn record_missing_core_fields(record: &InvoiceRecord) -> bool {
    record.invoice_number.is_none()
        || record.issue_date.is_none()
        || record.gross_amount_minor.is_none()
        || record.currency.is_none()
        || (record.seller_name.is_none() && record.buyer_name.is_none())
}

fn json_first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|v| match v {
            Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    })
}

fn json_first_money_minor(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        let raw = value.get(*key)?;
        match raw {
            Value::Number(n) => parse_money_minor(&n.to_string()),
            Value::String(s) => parse_money_minor(s),
            _ => None,
        }
    })
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

    Err(anyhow!(
        "ekstrakcja PDF nie powiodła się; zainstaluj poppler (`brew install poppler`)"
    ))
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
        let comparable_a = comparable_invoice_number(a);
        let comparable_b = comparable_invoice_number(b);
        if comparable_a == comparable_b {
            score += 45;
            reasons.push("invoice_number exact".to_string());
        } else if invoice_number_strong_contains(&comparable_a, &comparable_b) {
            score += 45;
            reasons.push("invoice_number embedded exact".to_string());
        } else if comparable_a.contains(&comparable_b) || comparable_b.contains(&comparable_a) {
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

fn invoice_number_strong_contains(a: &str, b: &str) -> bool {
    let min_len = a.len().min(b.len());
    min_len >= 8 && (a.contains(b) || b.contains(a))
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
    let bytes = serde_json::to_vec_pretty(token)?;
    if uses_default_gmail_token_path(path)
        && keychain_set_secret(
            KEYCHAIN_ACCOUNT_GMAIL_TOKEN,
            std::str::from_utf8(&bytes).context("token Gmail nie jest UTF-8")?,
        )?
    {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
        return Ok(());
    }

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("zapis tokenu Gmail {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn read_gmail_token(path: &Path) -> Result<GmailTokenFile> {
    if uses_default_gmail_token_path(path)
        && let Some(text) = keychain_get_secret(KEYCHAIN_ACCOUNT_GMAIL_TOKEN)?
    {
        return serde_json::from_str(&text).context("niepoprawny token Gmail w macOS Keychain");
    }

    let text = fs::read_to_string(path)
        .with_context(|| format!("odczyt tokenu Gmail {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("niepoprawny token Gmail {}", path.display()))
}

fn uses_default_gmail_token_path(path: &Path) -> bool {
    path == default_gmail_token_path().as_path()
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
        .or_else(|| lab_config_var("GOOGLE_CLIENT_SECRET_PATH").map(PathBuf::from))
        .ok_or_else(|| anyhow!("token Gmail wygasł; podaj --gmail-client-secret albo ustaw GOOGLE_CLIENT_SECRET_PATH w lab onboard, żeby go odświeżyć"))?;
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
    messages_cached: usize,
    messages_fetched: usize,
    metadata_saved: usize,
    attachments_saved: usize,
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
    let mut messages_cached = 0usize;
    let mut messages_fetched = 0usize;
    let mut metadata_saved = 0usize;
    let mut attachments_saved = 0usize;
    for id in &message_ids {
        let metadata_path = out_dir.join(format!("{}_message.json", sanitize_filename(id)));
        if gmail_message_cached(out_dir, id, &allowed_exts) {
            messages_cached += 1;
            continue;
        }

        messages_fetched += 1;
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
        fs::write(&metadata_path, serde_json::to_vec_pretty(&msg)?)?;
        saved_files.push(metadata_path.display().to_string());
        metadata_saved += 1;

        let before_parts = saved_files.len();
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
        attachments_saved += saved_files.len().saturating_sub(before_parts);
    }

    Ok(GmailFetchResult {
        query: query.to_string(),
        messages_seen: message_ids.len(),
        messages_cached,
        messages_fetched,
        metadata_saved,
        attachments_saved,
        files_saved: saved_files.len(),
        out_dir: out_dir.display().to_string(),
        saved_files,
    })
}

fn gmail_message_cached(out_dir: &Path, message_id: &str, allowed_exts: &HashSet<String>) -> bool {
    let sanitized_id = sanitize_filename(message_id);
    let metadata_path = out_dir.join(format!("{sanitized_id}_message.json"));
    if !metadata_path.is_file() {
        return false;
    }
    let Ok(entries) = fs::read_dir(out_dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let path = entry.path();
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        file_name.starts_with(&format!("{sanitized_id}_")) && allowed_exts.contains(&ext)
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
    if path.exists() {
        return Ok(());
    }

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
        .unwrap_or_else(|| configured_ksef_out_path(year));
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

fn default_gmail_query(year: i32) -> String {
    format!(
        "after:{year}/01/01 before:{}/01/01 has:attachment filename:pdf",
        year + 1
    )
}

fn default_mail_out_path(year: i32) -> PathBuf {
    PathBuf::from(format!("data/mail-all-pdf-{year}-pdfs"))
}

fn default_mail_candidates_path(year: i32) -> PathBuf {
    default_mail_out_path(year).join("candidates.jsonl")
}

fn default_saldeo_out_path(year: i32) -> PathBuf {
    PathBuf::from(format!("data/saldeo-{year}"))
}

fn default_saldeo_records_path(year: i32) -> PathBuf {
    default_saldeo_out_path(year).join("records.jsonl")
}

fn default_ksef_out_path(year: i32) -> PathBuf {
    PathBuf::from(format!("data/ksef-{year}"))
}

fn configured_ksef_out_path(year: i32) -> PathBuf {
    lab_config_var("KSEF_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_ksef_out_path(year))
}

fn productmesh_invoice_candidates(
    records: &[InvoiceRecord],
    productmesh_nip: &str,
) -> Vec<InvoiceRecord> {
    let productmesh_nip =
        normalize_tax_id(productmesh_nip).unwrap_or_else(|| productmesh_nip.to_string());
    let excluded = Regex::new(r"(?i)(receipt|statement|regulamin|warunki|informacje|upowa|oferta|umowa|order|label|bilet|dr_skan|wypowiedzenie|grafklient|cennik|polityka|pasek|wishlist|terms|portfolio|kosztorys|formularz|prawo_jazdy|zalacznik)").unwrap();
    let mut by_invoice: HashMap<String, InvoiceRecord> = HashMap::new();
    let mut fallback_seen = HashSet::new();
    let mut fallback_out = Vec::new();
    for record in records {
        let names = format!(
            "{} {}",
            record.seller_name.clone().unwrap_or_default(),
            record.buyer_name.clone().unwrap_or_default()
        );
        let related = record.seller_tax_id.as_deref() == Some(productmesh_nip.as_str())
            || record.buyer_tax_id.as_deref() == Some(productmesh_nip.as_str())
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
        if let Some(invoice_number) = &record.invoice_number {
            let key = comparable_invoice_number(invoice_number);
            if !key.is_empty() {
                match by_invoice.get(&key) {
                    Some(existing)
                        if record_quality_score(existing) >= record_quality_score(record) => {}
                    _ => {
                        by_invoice.insert(key, record.clone());
                    }
                }
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
        if fallback_seen.insert(key) {
            fallback_out.push(record.clone());
        }
    }
    let mut out = by_invoice.into_values().collect::<Vec<_>>();
    out.extend(fallback_out);
    out.sort_by(|a, b| a.source_path.cmp(&b.source_path));
    out
}

fn record_quality_score(record: &InvoiceRecord) -> usize {
    [
        record.invoice_number.is_some(),
        record.issue_date.is_some(),
        record.gross_amount_minor.is_some(),
        record.currency.is_some(),
        record.seller_tax_id.is_some(),
        record.buyer_tax_id.is_some(),
        record.seller_name.is_some(),
        record.buyer_name.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count()
}

fn apply_cached_mail_candidates(
    year: i32,
    candidates: &mut [InvoiceRecord],
) -> Result<HashSet<String>> {
    let path = default_mail_candidates_path(year);
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let cached = load_records(SourceKind::Mail, &path)?;
    let by_hash = cached
        .into_iter()
        .map(|record| (record.content_hash.clone(), record))
        .collect::<HashMap<_, _>>();
    let mut cached_hashes = HashSet::new();
    for candidate in candidates {
        if let Some(cached) = by_hash.get(&candidate.content_hash) {
            *candidate = cached.clone();
            cached_hashes.insert(candidate.content_hash.clone());
        }
    }
    Ok(cached_hashes)
}

fn enrich_candidates_with_gemma(
    records: &mut [InvoiceRecord],
    skip_hashes: &HashSet<String>,
) -> Result<()> {
    let todo = records
        .iter()
        .filter(|record| !skip_hashes.contains(&record.content_hash))
        .filter(|record| record_missing_core_fields(record))
        .filter(|record| {
            record.source_path.as_ref().is_some_and(|path| {
                Path::new(path).extension().and_then(|e| e.to_str()) == Some("pdf")
            })
        })
        .count();
    if todo == 0 {
        return Ok(());
    }
    eprintln!(
        "  [Gmail/Gemma] wzbogacanie {} kandydatów przez {}...",
        todo,
        llm_model()
    );
    ensure_ppmlx_server()?;
    let mut processed = 0usize;
    let mut consecutive_errors = 0usize;
    for record in records.iter_mut() {
        if skip_hashes.contains(&record.content_hash) || !record_missing_core_fields(record) {
            continue;
        }
        let Some(source_path) = record.source_path.clone() else {
            continue;
        };
        let path = Path::new(&source_path);
        if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        processed += 1;
        eprintln!(
            "  [Gmail/Gemma] {}/{} {}",
            processed,
            todo,
            path.file_name().and_then(|n| n.to_str()).unwrap_or("PDF")
        );
        match gemma_extract_invoice_fields(record, path) {
            Ok(true) => {
                consecutive_errors = 0;
                record
                    .warnings
                    .push("gemma-4-e4b enrichment applied".to_string());
            }
            Ok(false) => {
                consecutive_errors = 0;
            }
            Err(err) => {
                consecutive_errors += 1;
                record.warnings.push(format!("gemma-4-e4b: {err}"));
                eprintln!("  [Gmail/Gemma] błąd: {err}");
                if consecutive_errors >= 2 {
                    eprintln!("  [Gmail/Gemma] pomijam dalsze wzbogacanie po 2 kolejnych błędach");
                    break;
                }
            }
        }
    }
    eprintln!("  [Gmail/Gemma] gotowe");
    Ok(())
}

fn ppmlx_base_url() -> String {
    std::env::var("PPMLX_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:6767".to_string())
}

fn llm_model() -> String {
    std::env::var("LAB_LLM_MODEL").unwrap_or_else(|_| "gemma-4-e4b".to_string())
}

fn llm_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("LAB_LLM_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(45),
    )
}

fn ensure_ppmlx_server() -> Result<()> {
    let base = ppmlx_base_url();
    let model = llm_model();
    let client = Client::builder().timeout(Duration::from_secs(2)).build()?;
    if client
        .get(format!("{base}/v1/models"))
        .send()
        .is_ok_and(|r| r.status().is_success())
    {
        return Ok(());
    }
    if !(base.contains("127.0.0.1") || base.contains("localhost")) {
        return Err(anyhow!("ppmlx server niedostępny: {base}"));
    }
    Command::new("ppmlx")
        .args(["serve", "--model", &model])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("uruchomienie `ppmlx serve`")?;
    for _ in 0..30 {
        sleep(Duration::from_secs(1));
        if client
            .get(format!("{base}/v1/models"))
            .send()
            .is_ok_and(|r| r.status().is_success())
        {
            return Ok(());
        }
    }
    Err(anyhow!("ppmlx server nie wystartował na {base}"))
}

fn gemma_extract_invoice_fields(record: &mut InvoiceRecord, path: &Path) -> Result<bool> {
    let text = extract_pdf_text(path).unwrap_or_default();
    if text.trim().is_empty() {
        return Ok(false);
    }
    let prompt = format!(
        r#"Wyciągnij dane faktury z tekstu PDF.

Zwróć dokładnie jeden poprawny obiekt JSON: bez markdown, bez komentarzy, bez analizy, bez <|channel>thought.
Odpowiedź musi zaczynać się znakiem {{ i kończyć znakiem }}.

Użyj dokładnie tych kluczy. Jeśli brak pewności, wpisz null:
{{
  "invoice_number": null,
  "issue_date": null,
  "sale_date": null,
  "due_date": null,
  "gross_amount": null,
  "net_amount": null,
  "vat_amount": null,
  "currency": null,
  "seller_tax_id": null,
  "buyer_tax_id": null,
  "seller_name": null,
  "buyer_name": null
}}

Formaty wartości:
- Daty: string "YYYY-MM-DD" albo null.
- Kwoty: string z kropką i 2 miejscami, bez spacji i waluty, np. "1234.56", albo null.
- Waluta: "PLN", "EUR", "USD", "GBP" albo null. zł/PLN/zloty traktuj jako PLN.
- NIP/VAT PL: string z samymi 10 cyframi, bez PL/spacji/myślników; inaczej null.
- NIP/VAT widoczny w sekcji Bill to/Nabywca/Buyer przypisz do buyer_tax_id, nie do seller_tax_id.
- seller_tax_id to tylko identyfikator sprzedawcy/vendor/seller, jeśli jasno występuje przy sprzedawcy.
- Nazwy: pełna nazwa sprzedawcy/nabywcy z faktury albo null.
- Dla faktur zakupowych Productmesh zwykle buyer_name/buyer_tax_id to Productmesh; sprzedawca to kontrahent.
- Nie zgaduj i nie wyliczaj pól, jeśli nie wynikają jasno z tekstu.

Tekst PDF:
---
{}
---"#,
        text.chars().take(12000).collect::<String>()
    );
    let value = ppmlx_extract_json(&prompt)?;
    let before = serde_json::to_string(record)?;
    apply_extracted_invoice_json(record, &value);
    Ok(serde_json::to_string(record)? != before)
}

fn ppmlx_extract_json(prompt: &str) -> Result<Value> {
    let base = ppmlx_base_url();
    let model = llm_model();
    let client = Client::builder().timeout(llm_timeout()).build()?;
    let response: Value = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": model,
            "temperature": 0,
            "max_tokens": 1200,
            "messages": [
                {"role": "system", "content": "Jesteś ekstraktorem danych z faktur. Odpowiadasz tylko poprawnym JSON."},
                {"role": "user", "content": prompt}
            ]
        }))
        .send()?
        .error_for_status()?
        .json()?;
    let content = response
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("ppmlx response bez choices[0].message.content"))?;
    parse_json_from_llm(content)
}

fn parse_json_from_llm(content: &str) -> Result<Value> {
    let sanitized = sanitize_llm_content(content);
    let content = sanitized.trim();

    if content.is_empty() {
        return Err(anyhow!("LLM nie zwrócił JSON"));
    }
    if let Ok(value) = serde_json::from_str(content) {
        return Ok(value);
    }

    let candidates = json_object_candidates(content);
    if candidates.is_empty() {
        return Err(anyhow!("LLM nie zwrócił JSON"));
    }

    // After deterministic channel sanitization, prefer the last syntactically valid
    // JSON object to tolerate markdown fences or short explanatory prefixes.
    let mut last_err = None;
    for candidate in candidates.iter().rev() {
        match serde_json::from_str::<Value>(candidate) {
            Ok(value) => return Ok(value),
            Err(err) => last_err = Some(err),
        }
    }

    Err(last_err
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow!("LLM nie zwrócił JSON")))
    .context("niepoprawny JSON z LLM")
}

fn sanitize_llm_content(content: &str) -> String {
    if let Some(final_payload) = channel_payload(content, "final") {
        return strip_channel_tokens(final_payload).trim().to_string();
    }

    strip_channel_tokens(&remove_reasoning_channels(content))
        .trim()
        .to_string()
}

fn channel_payload<'a>(content: &'a str, channel: &str) -> Option<&'a str> {
    let marker = format!("<|channel>{channel}");
    let start = content.find(&marker)? + marker.len();
    let end = content[start..]
        .find("<|channel>")
        .map(|idx| start + idx)
        .unwrap_or(content.len());
    Some(&content[start..end])
}

fn remove_reasoning_channels(content: &str) -> String {
    let marker = "<|channel>";
    let mut output = String::new();
    let mut pos = 0usize;

    while let Some(rel_start) = content[pos..].find(marker) {
        let start = pos + rel_start;
        output.push_str(&content[pos..start]);

        let name_start = start + marker.len();
        let name_len = content[name_start..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
            .map(char::len_utf8)
            .sum::<usize>();
        let name = &content[name_start..name_start + name_len];
        let next = content[name_start + name_len..]
            .find(marker)
            .map(|idx| name_start + name_len + idx)
            .unwrap_or(content.len());

        if !matches!(name, "thought" | "analysis" | "reasoning") {
            output.push_str(&content[start..next]);
        }
        pos = next;
    }

    output.push_str(&content[pos..]);
    output
}

fn strip_channel_tokens(content: &str) -> String {
    let marker = "<|channel>";
    let mut output = String::new();
    let mut pos = 0usize;

    while let Some(rel_start) = content[pos..].find(marker) {
        let start = pos + rel_start;
        output.push_str(&content[pos..start]);
        let name_start = start + marker.len();
        let skip_len = content[name_start..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
            .map(char::len_utf8)
            .sum::<usize>();
        pos = name_start + skip_len;
    }

    output.push_str(&content[pos..]);
    output
}

fn json_object_candidates(content: &str) -> Vec<&str> {
    let mut candidates = Vec::new();
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in content.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' if depth > 0 => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0
                    && let Some(start_idx) = start.take()
                {
                    candidates.push(&content[start_idx..idx + ch.len_utf8()]);
                }
            }
            _ => {}
        }
    }

    candidates
}

fn apply_extracted_invoice_json(record: &mut InvoiceRecord, value: &Value) {
    if record.invoice_number.is_none() {
        record.invoice_number = json_first_string(value, &["invoice_number", "number"])
            .map(|v| clean_invoice_number(&v));
    }
    if record.issue_date.is_none() {
        record.issue_date =
            json_first_string(value, &["issue_date", "date"]).and_then(|v| parse_date(&v));
    }
    if record.sale_date.is_none() {
        record.sale_date = json_first_string(value, &["sale_date"]).and_then(|v| parse_date(&v));
    }
    if record.due_date.is_none() {
        record.due_date =
            json_first_string(value, &["due_date", "date_due"]).and_then(|v| parse_date(&v));
    }
    if record.gross_amount_minor.is_none() {
        record.gross_amount_minor =
            json_first_money_minor(value, &["gross_amount", "amount", "total"]);
    }
    if record.net_amount_minor.is_none() {
        record.net_amount_minor = json_first_money_minor(value, &["net_amount", "amount_net"]);
    }
    if record.vat_amount_minor.is_none() {
        record.vat_amount_minor = json_first_money_minor(value, &["vat_amount", "amount_vat"]);
    }
    if record.currency.is_none() {
        record.currency =
            json_first_string(value, &["currency"]).map(|v| v.trim().to_ascii_uppercase());
    }
    if record.seller_tax_id.is_none() {
        record.seller_tax_id =
            json_first_string(value, &["seller_tax_id"]).and_then(|v| normalize_tax_id(&v));
    }
    if record.buyer_tax_id.is_none() {
        record.buyer_tax_id =
            json_first_string(value, &["buyer_tax_id"]).and_then(|v| normalize_tax_id(&v));
    }
    if record.seller_name.is_none() {
        record.seller_name =
            json_first_string(value, &["seller_name"]).and_then(|v| clean_name(&v));
    }
    if record.buyer_name.is_none() {
        record.buyer_name = json_first_string(value, &["buyer_name"]).and_then(|v| clean_name(&v));
    }
}

fn read_tri_report(path: &Path) -> Result<TriReconcileReport> {
    let text = fs::read_to_string(path).with_context(|| format!("odczyt {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("niepoprawny tri report {}", path.display()))
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
            "name": "sync",
            "description": "Sync invoice data from Gmail/PDF, KSeF, and/or Saldeo. Without source flags, syncs all three sources.",
            "inputSchema": {"type":"object","properties":{
                "ksef":{"type":"boolean","description":"Sync only KSeF"},
                "mail":{"type":"boolean","description":"Sync only Gmail/PDF (fetch attachments, parse, filter, store)"},
                "saldeo":{"type":"boolean","description":"Sync only Saldeo"},
                "year":{"type":"integer","default":2026},
                "ksef_input":{"type":"string","description":"Path to KSeF export directory/file"},
                "gmail_client_secret":{"type":"string","description":"Google OAuth Desktop Client JSON for token refresh"},
                "gmail_token_file":{"type":"string","description":"Path to Gmail token file"},
                "productmesh_nip":{"type":"string","default":"5242920020","description":"NIP filter for mail scanning"},
                "store":{"type":"boolean","default":false,"description":"Store records in SQLite"}
            }}
        },
        {
            "name": "reconcile",
            "description": "Compare Gmail/PDF, KSeF, and Saldeo records (tri-reconcile).",
            "inputSchema": {"type":"object","required":["mail","ksef","saldeo"],"properties":{
                "mail":{"type":"string","description":"Path to Gmail/PDF records JSON/JSONL"},
                "ksef":{"type":"string","description":"Path to KSeF records JSON/JSONL"},
                "saldeo":{"type":"string","description":"Path to Saldeo records JSON/JSONL or raw documents.json"},
                "review_score":{"type":"integer","default":45,"description":"Minimum match score"},
                "store":{"type":"boolean","default":false,"description":"Store temporal snapshot in SQLite"},
                "year":{"type":"integer","default":2026}
            }}
        },
        {
            "name": "reconcile_status",
            "description": "Show the last tri-reconcile report from the database for a given year.",
            "inputSchema": {"type":"object","properties":{
                "year":{"type":"integer","default":2026}
            }}
        },
        {
            "name": "upload",
            "description": "Upload invoices missing in Saldeo. Requires tri_report path or mail+ksef+saldeo paths.",
            "inputSchema": {"type":"object","properties":{
                "year":{"type":"integer","default":2026},
                "tri_report":{"type":"string","description":"Path to tri-reconcile report JSON"},
                "mail":{"type":"string","description":"Path to Gmail/PDF records JSON/JSONL"},
                "ksef":{"type":"string","description":"Path to KSeF records JSON/JSONL"},
                "saldeo":{"type":"string","description":"Path to Saldeo records JSON/JSONL"},
                "review_score":{"type":"integer","default":70,"description":"Minimum match score when computing from sources"}
            }}
        },
        {
            "name": "db_stats",
            "description": "Return SQLite record counts.",
            "inputSchema": {"type":"object","properties":{}}
        },
        {
            "name": "tri_runs",
            "description": "List temporal tri-reconcile runs and diff counters.",
            "inputSchema": {"type":"object","properties":{
                "limit":{"type":"integer","default":20}
            }}
        }
    ])
}

fn call_mcp_tool(db_path: &Path, name: &str, args: &Value) -> Result<Value> {
    match name {
        "sync" => {
            let year = json_i32(args, "year", 2026);
            let all = !json_bool(args, "ksef", false)
                && !json_bool(args, "mail", false)
                && !json_bool(args, "saldeo", false);
            if all {
                eprintln!("Sync: wszystkie źródła (KSeF + Gmail/PDF + Saldeo)");
            }
            let conn = if json_bool(args, "store", false) {
                Some(open_db(db_path)?)
            } else {
                None
            };
            let mut synced: Vec<String> = Vec::new();
            let mut records_count = 0usize;
            if json_bool(args, "ksef", false) || all {
                eprintln!("  [KSeF] synchronizacja...");
                let input = json_path_arg(args, "ksef_input")
                    .unwrap_or_else(|| configured_ksef_out_path(year));
                let result = ksef_sync(year, &input, None)?;
                records_count += result.summary.records_count;
                if let Some(ref conn) = conn {
                    store_records(conn, &result.records)?;
                }
                eprintln!("  [KSeF] gotowe: {} rekordów", result.summary.records_count);
                synced.push(format!("ksef ({})", result.summary.records_count));
            }
            if json_bool(args, "mail", false) || all {
                eprintln!("  [Gmail] sprawdzanie wiadomości i cache załączników...");
                let token_path = json_path_arg(args, "gmail_token_file")
                    .unwrap_or_else(default_gmail_token_path);
                let token = gmail_access_token(
                    "GMAIL_ACCESS_TOKEN",
                    &token_path,
                    json_path_arg(args, "gmail_client_secret").as_deref(),
                )?;
                let mail_out = default_mail_out_path(year);
                let gmail_result = gmail_fetch(
                    &token,
                    "me",
                    &default_gmail_query(year),
                    &mail_out,
                    500,
                    &["pdf".to_string()],
                )?;
                eprintln!(
                    "  [Gmail] wiadomości: {} znalezionych, {} z cache, {} pobranych z API; nowe pliki: {} metadane, {} załączniki",
                    gmail_result.messages_seen,
                    gmail_result.messages_cached,
                    gmail_result.messages_fetched,
                    gmail_result.metadata_saved,
                    gmail_result.attachments_saved
                );
                eprintln!("  [Gmail] skanowanie nowych PDF...");
                let (mail_records, parsed_count) =
                    sync_mail_records(&mail_out, &gmail_result.saved_files)?;
                eprintln!("  [Gmail] sparsowano {} nowych PDF", parsed_count);
                let nip = json_string_arg(args, "productmesh_nip")
                    .unwrap_or_else(|| "5242920020".to_string());
                let mut candidates = productmesh_invoice_candidates(&mail_records, &nip);
                let cached_candidates = apply_cached_mail_candidates(year, &mut candidates)?;
                enrich_candidates_with_gemma(&mut candidates, &cached_candidates)?;
                write_records(
                    &candidates,
                    OutputFormat::Jsonl,
                    Some(&default_mail_candidates_path(year)),
                )?;
                records_count += candidates.len();
                if let Some(ref conn) = conn {
                    store_records(conn, &candidates)?;
                }
                eprintln!(
                    "  [Gmail] gotowe: {} PDF, {} faktur",
                    mail_records.len(),
                    candidates.len()
                );
                synced.push(format!(
                    "mail ({} new attachments, {} pdfs, {} candidates)",
                    gmail_result.attachments_saved,
                    mail_records.len(),
                    candidates.len()
                ));
            }
            if json_bool(args, "saldeo", false) || all {
                eprintln!("  [Saldeo] pobieranie dokumentów...");
                let result = saldeo_fetch(
                    year,
                    &default_saldeo_storage_state_path(),
                    &default_saldeo_out_path(year),
                )?;
                records_count += result.summary.records_count;
                if let Some(ref conn) = conn {
                    store_records(conn, &result.records)?;
                }
                eprintln!(
                    "  [Saldeo] gotowe: {} dokumentów",
                    result.summary.documents_count
                );
                synced.push(format!("saldeo ({})", result.summary.documents_count));
            }
            Ok(
                serde_json::json!({"synced": synced, "year": year, "records_count": records_count, "stored": conn.is_some()}),
            )
        }
        "reconcile" => {
            let mail = json_path_arg(args, "mail").ok_or_else(|| anyhow!("missing mail"))?;
            let ksef = json_path_arg(args, "ksef").ok_or_else(|| anyhow!("missing ksef"))?;
            let saldeo = json_path_arg(args, "saldeo").ok_or_else(|| anyhow!("missing saldeo"))?;
            let report = tri_reconcile(
                load_records(SourceKind::Mail, &mail)?,
                load_records(SourceKind::Ksef, &ksef)?,
                load_saldeo_records(&saldeo)?,
                json_u8(args, "review_score", 45),
            );
            if json_bool(args, "store", false) {
                let conn = open_db(db_path)?;
                let diff =
                    store_tri_reconcile_report(&conn, json_i32(args, "year", 2026), &report)?;
                return Ok(serde_json::json!({"report": report, "temporal_diff": diff}));
            }
            Ok(serde_json::to_value(report)?)
        }
        "reconcile_status" => {
            let year = json_i32(args, "year", 2026);
            let conn = open_db(db_path)?;
            let report = load_last_tri_report(&conn, year)?;
            Ok(serde_json::to_value(report)?)
        }
        "upload" => {
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
                confirm: true,
                upload_url: None,
            })?;
            saldeo_upload_plan(
                &mut plan,
                &default_saldeo_storage_state_path(),
                DEFAULT_SALDEO_UPLOAD_URL,
                "file",
            )?;
            Ok(serde_json::to_value(plan)?)
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
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_mail_candidates_path(config.year));
        let ksef = config
            .ksef
            .map(Path::to_path_buf)
            .unwrap_or_else(|| configured_ksef_out_path(config.year));
        let saldeo = config
            .saldeo
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_saldeo_records_path(config.year));
        tri_reconcile(
            load_records(SourceKind::Mail, &mail)?,
            load_records(SourceKind::Ksef, &ksef)?,
            load_saldeo_records(&saldeo)?,
            config.review_score,
        )
    };
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for row in &report.rows {
        if row.saldeo.is_some() {
            continue;
        }
        let Some(record) = row.mail.as_ref() else {
            continue;
        };
        let related_sources = [("mail", row.mail.as_ref()), ("ksef", row.ksef.as_ref())]
            .into_iter()
            .filter_map(|(name, record)| record.map(|_| name.to_string()))
            .collect::<Vec<_>>();
        let key = record
            .source_path
            .clone()
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
    let storage: Value = serde_json::from_str(&read_saldeo_storage_state(storage_state)?)?;
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
    let storage: Value = serde_json::from_str(&read_saldeo_storage_state(storage_state)?)?;
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
            .map_err(|e| {
                if e.status() == Some(reqwest::StatusCode::UNAUTHORIZED) {
                    anyhow!(
                        "Saldeo session expired (401). Refresh Playwright storage state:\n  {}",
                        default_saldeo_storage_state_path().display()
                    )
                } else {
                    anyhow!("Saldeo document/list/search month={month}: {e}")
                }
            })?
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

fn write_reconcile_human(
    report: &TriReconcileReport,
    temporal_diff: Option<&TemporalDiffSummary>,
) -> Result<()> {
    let mut out = String::new();
    out.push_str("LAB reconcile\n");
    out.push_str(&format!(
        "generated: {} | review_score: {}\n\n",
        report.generated_at, report.review_score
    ));
    out.push_str(&format!(
        "Źródła: Gmail {} | KSeF {} | Saldeo {}\n",
        report.summary.mail_count, report.summary.ksef_count, report.summary.saldeo_count
    ));
    out.push_str("Statusy:\n");
    for (label, count) in reconcile_status_counts(&report.summary) {
        out.push_str(&format!("  {:30} {}\n", label, count));
    }
    if let Some(diff) = temporal_diff {
        out.push_str(&format!(
            "\nDiff vs poprzedni run: +{} -{} ~{} (run #{})\n",
            diff.added_count, diff.removed_count, diff.changed_count, diff.run_id
        ));
    }

    let missing_rows = report
        .rows
        .iter()
        .filter(|row| row.status != "in_all_three")
        .collect::<Vec<_>>();
    out.push_str(&format!(
        "\nBraki / do sprawdzenia: {} pozycji",
        missing_rows.len()
    ));
    if !missing_rows.is_empty() {
        out.push('\n');
        out.push_str(&format!(
            "{:<30} {:<28} {:<30} {:<12} {:>12} {:<4} {:<18}\n",
            "status", "faktura", "kontrahent", "data", "brutto", "wal", "źródła"
        ));
        out.push_str(&format!("{}\n", "-".repeat(142)));
        for row in missing_rows {
            let primary = row
                .mail
                .as_ref()
                .or(row.ksef.as_ref())
                .or(row.saldeo.as_ref());
            out.push_str(&format!(
                "{:<30} {:<28} {:<30} {:<12} {:>12} {:<4} {:<18}\n",
                truncate(&row.status, 30),
                truncate(
                    &primary
                        .and_then(|r| r.invoice_number.clone())
                        .unwrap_or_else(|| "-".to_string()),
                    28,
                ),
                truncate(&counterparty_name(primary), 30),
                primary
                    .and_then(|r| r.issue_date)
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                primary
                    .and_then(|r| r.gross_amount_minor)
                    .map(format_minor_money)
                    .unwrap_or_else(|| "-".to_string()),
                primary
                    .and_then(|r| r.currency.clone())
                    .unwrap_or_else(|| "-".to_string()),
                row_sources(row),
            ));
        }
    } else {
        out.push('\n');
    }
    out.push_str("\nPełny JSON: lab reconcile --raw\n");
    print!("{out}");
    Ok(())
}

fn reconcile_status_counts(summary: &TriSummary) -> Vec<(&'static str, usize)> {
    vec![
        ("in_all_three", summary.in_all_three),
        (
            "gmail_ksef_missing_saldeo",
            summary.gmail_ksef_missing_saldeo,
        ),
        (
            "gmail_saldeo_missing_ksef",
            summary.gmail_saldeo_missing_ksef,
        ),
        ("gmail_only", summary.gmail_only),
        (
            "ksef_saldeo_missing_gmail",
            summary.ksef_saldeo_missing_gmail,
        ),
        ("ksef_only", summary.ksef_only),
        ("saldeo_only", summary.saldeo_only),
    ]
}

fn counterparty_name(record: Option<&InvoiceRecord>) -> String {
    record
        .and_then(|r| r.seller_name.clone().or_else(|| r.buyer_name.clone()))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "-".to_string())
}

fn row_sources(row: &TriRow) -> String {
    [
        ("G", row.mail.is_some()),
        ("K", row.ksef.is_some()),
        ("S", row.saldeo.is_some()),
    ]
    .into_iter()
    .filter_map(|(label, present)| present.then_some(label))
    .collect::<Vec<_>>()
    .join("+")
}

fn format_minor_money(value: i64) -> String {
    format!("{}.{:02}", value / 100, value.abs() % 100)
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
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

#[derive(Debug, Clone)]
struct OnboardStatus {
    db_exists: bool,
    token_exists: bool,
    gmail_authed: bool,
    saldeo_exists: bool,
    saldeo_valid: bool,
    pdftotext_ok: bool,
    python_ok: bool,
    year: i32,
    ksef_dir: PathBuf,
    ksef_data_exists: bool,
    ksef_cert: Option<String>,
    ksef_cert_ok: bool,
    ksef_key: Option<String>,
    ksef_key_ok: bool,
    ksef_password_ok: bool,
    ksef_token_ok: bool,
    ksef_api_ok: bool,
}

fn onboard(db_path: &Path, check: bool, gmail_client_secret: Option<&Path>) -> Result<()> {
    let mut gmail_client_secret = gmail_client_secret.map(Path::to_path_buf);
    let mut status = collect_onboard_status(db_path)?;

    eprintln!("LAB — konfiguracja środowiska\n");
    print_onboard_status(&status, db_path);

    if check {
        return write_onboard_check_json(&status);
    }

    let theme = ColorfulTheme::default();
    loop {
        let items = onboard_menu_items(&status, db_path, gmail_client_secret.as_deref());
        let exit_index = items.len() - 1;
        let selection = Select::with_theme(&theme)
            .with_prompt("Wszystkie parametry LAB — wybierz pozycję do edycji")
            .items(&items)
            .default(exit_index)
            .interact()?;

        match selection {
            0 => onboard_configure_gmail(&mut gmail_client_secret, status.gmail_authed)?,
            1 => onboard_configure_gmail(&mut gmail_client_secret, status.gmail_authed)?,
            2 => onboard_configure_saldeo()?,
            3 => onboard_edit_env_path("KSEF_CERT_PATH")?,
            4 => onboard_edit_env_path("KSEF_KEY_PATH")?,
            5 => onboard_edit_env_secret("KSEF_CERT_PASSWORD")?,
            6 => onboard_edit_env_secret("KSEF_TOKEN")?,
            7 => onboard_configure_ksef_data(status.year)?,
            8 => {
                open_db(db_path)?;
                eprintln!("✓ Baza gotowa: {}\n", db_path.display());
            }
            9 => run_saldeo_auth_script()?,
            10 => {}
            11 => break,
            _ => unreachable!(),
        }

        status = collect_onboard_status(db_path)?;
        eprintln!();
        print_onboard_status(&status, db_path);
    }

    write_json(&serde_json::json!({"all_ok": status.all_ok()}), None)
}

impl OnboardStatus {
    fn all_ok(&self) -> bool {
        self.pdftotext_ok
            && self.python_ok
            && self.gmail_authed
            && self.saldeo_valid
            && self.ksef_api_ok
            && self.ksef_data_exists
    }
}

fn onboard_menu_items(
    status: &OnboardStatus,
    db_path: &Path,
    gmail_client_secret: Option<&Path>,
) -> Vec<String> {
    vec![
        format!(
            "GOOGLE_CLIENT_SECRET_PATH — {}",
            gmail_client_secret
                .map(|p| p.display().to_string())
                .or_else(|| lab_config_var("GOOGLE_CLIENT_SECRET_PATH"))
                .unwrap_or_else(|| "(puste; potrzebne do auto-refresh Gmail)".to_string())
        ),
        format!(
            "GMAIL_TOKEN_FILE — {} {}",
            if status.gmail_authed { "✓" } else { "✗" },
            default_gmail_token_path().display()
        ),
        format!(
            "SALDEO_STORAGE_STATE — {} {}",
            if status.saldeo_valid { "✓" } else { "✗" },
            default_saldeo_storage_state_path().display()
        ),
        format!(
            "KSEF_CERT_PATH — {}",
            display_path_value(status.ksef_cert.as_deref(), status.ksef_cert_ok)
        ),
        format!(
            "KSEF_KEY_PATH — {}",
            display_path_value(status.ksef_key.as_deref(), status.ksef_key_ok)
        ),
        format!(
            "KSEF_CERT_PASSWORD — {}",
            display_secret_value(status.ksef_password_ok)
        ),
        format!(
            "KSEF_TOKEN — {}",
            display_secret_value(status.ksef_token_ok)
        ),
        format!(
            "KSEF_DATA_DIR — {} {}",
            if status.ksef_data_exists {
                "✓"
            } else {
                "✗"
            },
            status.ksef_dir.display()
        ),
        format!(
            "DB_PATH — {} {}",
            if status.db_exists {
                "✓"
            } else {
                "✓ (nowa)"
            },
            db_path.display()
        ),
        "SALDEO_AUTH_SCRIPT — uruchom pobieranie auth".to_string(),
        "Odśwież status".to_string(),
        if status.all_ok() {
            "Zakończ — wszystko gotowe".to_string()
        } else {
            "Zakończ — wrócę później".to_string()
        },
    ]
}

fn display_path_value(value: Option<&str>, exists: bool) -> String {
    match value {
        Some(value) if exists => format!("✓ {value}"),
        Some(value) => format!("✗ {value}"),
        None => "✗ (puste)".to_string(),
    }
}

fn display_secret_value(is_set: bool) -> &'static str {
    if is_set {
        "✓ ********"
    } else {
        "✗ (puste)"
    }
}

fn collect_onboard_status(db_path: &Path) -> Result<OnboardStatus> {
    let token_file = default_gmail_token_path();
    let token_exists = token_file.exists()
        || keychain_get_secret(KEYCHAIN_ACCOUNT_GMAIL_TOKEN)
            .map(|v| v.is_some())
            .unwrap_or(false);
    let gmail_authed = token_exists
        && read_gmail_token(&token_file)
            .map(|t| {
                t.expires_at
                    .map(|exp| exp > Utc::now() + chrono::Duration::seconds(60))
                    .unwrap_or(true)
            })
            .unwrap_or(false);

    let saldeo_state = default_saldeo_storage_state_path();
    let saldeo_exists = saldeo_state.exists()
        || keychain_get_secret(KEYCHAIN_ACCOUNT_SALDEO_STORAGE_STATE)
            .map(|v| v.is_some())
            .unwrap_or(false);
    let saldeo_valid = saldeo_exists && saldeo_session_valid(&saldeo_state);

    let pdftotext_ok = Command::new("pdftotext").arg("-v").output().is_ok();
    let python_ok = Command::new("python3")
        .arg("-c")
        .arg("import shutil, subprocess, sys; pp=shutil.which('ppmlx'); sys.exit(1 if not pp else 0)")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let db_exists = db_path.exists();
    if !db_exists {
        open_db(db_path)?;
    }

    let year = Utc::now().year();
    let ksef_dir = configured_ksef_out_path(year);
    let ksef_data_exists = ksef_dir.exists()
        && std::fs::read_dir(&ksef_dir)
            .map(|mut d| d.any(|e| e.is_ok()))
            .unwrap_or(false);

    let ksef_cert = lab_config_var("KSEF_CERT_PATH");
    let ksef_cert_ok = ksef_cert
        .as_ref()
        .map(|p| Path::new(p).exists())
        .unwrap_or(false);
    let ksef_key = lab_config_var("KSEF_KEY_PATH");
    let ksef_key_ok = ksef_key
        .as_ref()
        .map(|p| Path::new(p).exists())
        .unwrap_or(false);
    let ksef_password_ok = lab_config_var("KSEF_CERT_PASSWORD")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let ksef_token_ok = lab_config_var("KSEF_TOKEN")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let ksef_api_ok = ksef_cert_ok && ksef_key_ok && ksef_password_ok && ksef_token_ok;

    Ok(OnboardStatus {
        db_exists,
        token_exists,
        gmail_authed,
        saldeo_exists,
        saldeo_valid,
        pdftotext_ok,
        python_ok,
        year,
        ksef_dir,
        ksef_data_exists,
        ksef_cert,
        ksef_cert_ok,
        ksef_key,
        ksef_key_ok,
        ksef_password_ok,
        ksef_token_ok,
        ksef_api_ok,
    })
}

fn print_onboard_status(status: &OnboardStatus, db_path: &Path) {
    eprintln!(
        "  Baza danych:     {} ({})",
        if status.db_exists {
            "✓"
        } else {
            "✓ (nowa)"
        },
        db_path.display()
    );
    eprintln!(
        "  pdftotext:       {}",
        if status.pdftotext_ok {
            "✓"
        } else {
            "✗ (brew install poppler)"
        }
    );
    eprintln!(
        "  ppmlx/gemma:     {}",
        if status.python_ok {
            "✓"
        } else {
            "✗ (zainstaluj ppmlx i model gemma-4-e4b)"
        }
    );
    eprintln!(
        "  Gmail:           {}",
        if status.gmail_authed {
            "✓"
        } else if status.token_exists {
            "✗ (token wygasł)"
        } else {
            "✗"
        }
    );
    eprintln!(
        "  Saldeo:          {}",
        if status.saldeo_valid {
            "✓"
        } else if status.saldeo_exists {
            "✗ (sesja wygasła)"
        } else {
            "✗"
        }
    );
    eprintln!(
        "  KSeF certyfikat: {}",
        if status.ksef_cert_ok {
            format!("✓ ({})", status.ksef_cert.as_deref().unwrap_or(""))
        } else {
            "✗".to_string()
        }
    );
    eprintln!(
        "  KSeF klucz:      {}",
        if status.ksef_key_ok {
            format!("✓ ({})", status.ksef_key.as_deref().unwrap_or(""))
        } else {
            "✗".to_string()
        }
    );
    eprintln!(
        "  KSeF hasło:      {}",
        if status.ksef_password_ok {
            "✓"
        } else {
            "✗"
        }
    );
    eprintln!(
        "  KSeF token:      {}",
        if status.ksef_token_ok { "✓" } else { "✗" }
    );
    eprintln!(
        "  KSeF dane:       {}",
        if status.ksef_data_exists {
            format!("✓ ({})", status.ksef_dir.display())
        } else {
            format!("✗ ({})", status.ksef_dir.display())
        }
    );
    eprintln!();
}

fn write_onboard_check_json(status: &OnboardStatus) -> Result<()> {
    let mut steps: Vec<&str> = Vec::new();
    if !status.pdftotext_ok {
        steps.push("brew install poppler");
    }
    if !status.python_ok {
        steps.push("Zainstaluj ppmlx i pobierz model: ppmlx pull gemma-4-e4b");
    }
    if !status.gmail_authed {
        steps.push("lab onboard --gmail-client-secret <ścieżka>");
    }
    if !status.saldeo_valid {
        steps.push("Odśwież sesję Saldeo (~/.config/lab/saldeo-storage-state.json)");
    }
    if !status.ksef_api_ok {
        steps.push("Ustaw KSEF_CERT_PATH, KSEF_KEY_PATH, KSEF_CERT_PASSWORD, KSEF_TOKEN");
    }
    if !status.ksef_data_exists {
        steps.push("Umieść eksport KSeF w data/ksef-<rok>");
    }
    if steps.is_empty() {
        steps.push("Wszystko gotowe. Uruchom: lab sync");
    }
    let status_json = serde_json::json!({
        "prerequisites": { "pdftotext": status.pdftotext_ok, "ppmlx_gemma": status.python_ok },
        "gmail": { "token_valid": status.gmail_authed },
        "saldeo": { "session_valid": status.saldeo_valid },
        "ksef": { "api_ok": status.ksef_api_ok, "data_exists": status.ksef_data_exists },
        "database": { "exists": status.db_exists },
        "next_steps": steps
    });
    write_json(&status_json, None)
}

fn onboard_configure_gmail(
    gmail_client_secret: &mut Option<PathBuf>,
    gmail_authed: bool,
) -> Result<()> {
    eprintln!("── Gmail ──");
    if gmail_authed
        && !Confirm::new()
            .with_prompt("Token wygląda poprawnie. Odświeżyć/autoryzować ponownie?")
            .default(false)
            .interact()?
    {
        eprintln!("⏭ Pominięto.\n");
        return Ok(());
    }

    eprintln!("Potrzebny plik Google OAuth Desktop Client JSON.");
    eprintln!("Pobierz go z Google Cloud Console → APIs & Services → Credentials.\n");
    let default = gmail_client_secret
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let path: String = Input::new()
        .with_prompt("Ścieżka do client_secret JSON")
        .default(default)
        .allow_empty(true)
        .interact_text()?;
    if path.trim().is_empty() {
        eprintln!("⏭ Pominięto.\n");
        return Ok(());
    }
    let secret = PathBuf::from(path.trim());
    if !secret.exists() {
        eprintln!("✗ Plik nie istnieje: {}\n", secret.display());
        return Ok(());
    }

    *gmail_client_secret = Some(secret.clone());
    let mut vars = read_lab_env_file().unwrap_or_default();
    vars.insert(
        "GOOGLE_CLIENT_SECRET_PATH".to_string(),
        secret.display().to_string(),
    );
    write_lab_env_file(&vars)?;
    match gmail_auth(&secret, &default_gmail_token_path(), false) {
        Ok(result) => eprintln!(
            "✓ Gmail skonfigurowany. Token: {}\n✓ GOOGLE_CLIENT_SECRET_PATH zapisany w {}\n",
            result.token_file,
            lab_env_file_path().display()
        ),
        Err(err) => eprintln!("✗ Błąd autoryzacji: {err}\n"),
    }
    Ok(())
}

fn onboard_configure_saldeo() -> Result<()> {
    eprintln!("── Saldeo ──");
    let target = preferred_saldeo_storage_state_path();
    eprintln!("Domyślny plik sesji: {}", target.display());
    eprintln!("Podaj plik Playwright storage state; zostanie skopiowany do domyślnej lokalizacji.");
    let path: String = Input::new()
        .with_prompt("Ścieżka storage state JSON")
        .default(target.display().to_string())
        .allow_empty(true)
        .interact_text()?;
    if path.trim().is_empty() {
        eprintln!("⏭ Pominięto.\n");
        return Ok(());
    }
    let source = PathBuf::from(path.trim());
    if !source.exists() {
        eprintln!("✗ Plik nie istnieje: {}\n", source.display());
        return Ok(());
    }
    if source != target {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
        }
        fs::copy(&source, &target)
            .with_context(|| format!("kopiowanie {} → {}", source.display(), target.display()))?;
    }
    save_saldeo_storage_state_secret(&target)?;
    eprintln!(
        "✓ Saldeo storage state zapisany: {}\n✓ Saldeo storage state zapisany w macOS Keychain (jeśli dostępny)\n",
        target.display()
    );
    Ok(())
}

fn onboard_edit_env_path(name: &str) -> Result<()> {
    if let Some(value) = prompt_env_path(name, lab_config_var(name))? {
        let mut vars = read_lab_env_file().unwrap_or_default();
        vars.insert(name.to_string(), value);
        write_lab_env_file(&vars)?;
        eprintln!("✓ Zapisano {name} w {}\n", lab_env_file_path().display());
    } else {
        eprintln!("⏭ Bez zmian.\n");
    }
    Ok(())
}

fn onboard_edit_env_secret(name: &str) -> Result<()> {
    let current = lab_config_var(name).is_some();
    let prompt = if current {
        format!("{name} (ustawione; puste = bez zmian)")
    } else {
        format!("{name} (puste = bez zmian)")
    };
    let value = Password::new()
        .with_prompt(prompt)
        .allow_empty_password(true)
        .interact()?;
    if value.is_empty() {
        eprintln!("⏭ Bez zmian.\n");
        return Ok(());
    }
    let mut vars = read_lab_env_file().unwrap_or_default();
    vars.insert(name.to_string(), value);
    write_lab_env_file(&vars)?;
    eprintln!("✓ Zapisano {name} w {}\n", lab_env_file_path().display());
    Ok(())
}

fn run_saldeo_auth_script() -> Result<()> {
    eprintln!("── Saldeo auth ──");
    let target = preferred_saldeo_storage_state_path();
    if !Confirm::new()
        .with_prompt(format!(
            "Uruchomić Playwright i zapisać auth do {}?",
            target.display()
        ))
        .default(true)
        .interact()?
    {
        eprintln!("⏭ Pominięto.\n");
        return Ok(());
    }

    if let Some(script) = find_saldeo_auth_script() {
        let status = Command::new(&script)
            .arg(&target)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("uruchomienie {}", script.display()))?;
        if !status.success() {
            return Err(anyhow!("skrypt Saldeo auth zakończył się błędem: {status}"));
        }
        save_saldeo_storage_state_secret(&target)?;
        return Ok(());
    }

    eprintln!(
        "Nie znalazłem scripts/saldeo-auth.sh — uruchamiam fallback przez npx playwright + Helium."
    );
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let url =
        std::env::var("SALDEO_URL").unwrap_or_else(|_| "https://saldeo.brainshare.pl/".to_string());
    let helium = std::env::var("HELIUM_EXECUTABLE")
        .unwrap_or_else(|_| "/Applications/Helium.app/Contents/MacOS/Helium".to_string());
    if !Path::new(&helium).is_file() {
        return Err(anyhow!("nie znalazłem Helium executable: {helium}"));
    }
    let node_script = r#"
const { chromium } = require('playwright');
const fs = require('fs');
const os = require('os');
const path = require('path');
const readline = require('readline');
(async () => {
  const out = process.env.LAB_SALDEO_STORAGE_STATE;
  const url = process.env.SALDEO_URL || 'https://saldeo.brainshare.pl/';
  const executablePath = process.env.HELIUM_EXECUTABLE;
  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lab-helium-profile-'));
  const context = await chromium.launchPersistentContext(userDataDir, { executablePath, headless: false, viewport: { width: 1400, height: 1000 } });
  const page = context.pages()[0] || await context.newPage();
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  await new Promise(resolve => rl.question('\nPo zalogowaniu naciśnij Enter, żeby zapisać auth... ', resolve));
  rl.close();
  await context.storageState({ path: out });
  await context.close();
  fs.rmSync(userDataDir, { recursive: true, force: true });
})().catch(err => { console.error(err && err.stack ? err.stack : err); process.exit(1); });
"#;
    let status = Command::new("npx")
        .arg("--yes")
        .arg("-p")
        .arg("playwright")
        .arg("node")
        .arg("-e")
        .arg(node_script)
        .env("LAB_SALDEO_STORAGE_STATE", &target)
        .env("SALDEO_URL", &url)
        .env("HELIUM_EXECUTABLE", &helium)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("uruchomienie npx playwright + Helium")?;
    if !status.success() {
        return Err(anyhow!("Playwright auth zakończył się błędem: {status}"));
    }
    if !target.exists() {
        return Err(anyhow!(
            "storage state nie został zapisany: {}",
            target.display()
        ));
    }
    save_saldeo_storage_state_secret(&target)?;
    eprintln!("✓ Zapisano Saldeo auth: {}\n", target.display());
    Ok(())
}

fn find_saldeo_auth_script() -> Option<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        roots.extend(parent.ancestors().map(Path::to_path_buf));
    }

    for root in roots {
        let script = root.join("scripts").join("saldeo-auth.sh");
        if script.is_file() {
            return Some(script);
        }
    }
    None
}

fn prompt_env_path(name: &str, current: Option<String>) -> Result<Option<String>> {
    let default = current.unwrap_or_default();
    let value: String = Input::new()
        .with_prompt(name)
        .default(default)
        .allow_empty(true)
        .interact_text()?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return Ok(None);
    }
    if !Path::new(&value).exists() {
        eprintln!("⚠ Plik nie istnieje: {value}");
        if !Confirm::new()
            .with_prompt("Zapisać mimo to?")
            .default(false)
            .interact()?
        {
            return Ok(None);
        }
    }
    Ok(Some(value))
}

fn onboard_configure_ksef_data(current_year: i32) -> Result<()> {
    eprintln!("── KSeF dane ──");
    let year_text: String = Input::new()
        .with_prompt("Rok eksportu KSeF")
        .default(current_year.to_string())
        .interact_text()?;
    let year = year_text.trim().parse::<i32>().unwrap_or(current_year);
    let default_dir = configured_ksef_out_path(year);
    let dir_text: String = Input::new()
        .with_prompt("Katalog eksportu KSeF XML/JSON")
        .default(default_dir.display().to_string())
        .interact_text()?;
    let dir = PathBuf::from(dir_text.trim());
    if !dir.exists()
        && Confirm::new()
            .with_prompt("Katalog nie istnieje. Utworzyć?")
            .default(true)
            .interact()?
    {
        fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
        eprintln!("✓ Utworzono: {}", dir.display());
    }
    let mut vars = read_lab_env_file().unwrap_or_default();
    vars.insert("KSEF_DATA_DIR".to_string(), dir.display().to_string());
    write_lab_env_file(&vars)?;
    eprintln!(
        "✓ Zapisano KSEF_DATA_DIR w {}",
        lab_env_file_path().display()
    );
    eprintln!("Umieść eksport KSeF w: {}", dir.display());
    eprintln!(
        "Synchronizacja: lab sync --ksef --year {year} --ksef-input {}\n",
        dir.display()
    );
    Ok(())
}

fn lab_config_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| read_lab_env_file().ok()?.remove(name))
}

fn preferred_saldeo_storage_state_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("lab")
        .join("saldeo-storage-state.json")
}

fn lab_env_file_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("lab")
        .join("env")
}

fn read_lab_env_file() -> Result<HashMap<String, String>> {
    let path = lab_env_file_path();
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("odczyt {}", path.display()))?;
    let mut vars = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        vars.insert(key.trim().to_string(), unquote_env_value(value.trim()));
    }
    Ok(vars)
}

fn write_lab_env_file(vars: &HashMap<String, String>) -> Result<()> {
    let path = lab_env_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let mut keys = vars.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    let mut out = String::from(
        "# LAB local environment. Source it with: set -a; source ~/.config/lab/env; set +a\n",
    );
    for key in keys {
        if let Some(value) = vars.get(&key) {
            out.push_str(&format!("{}={}\n", key, quote_env_value(value)));
        }
    }
    fs::write(&path, out).with_context(|| format!("zapis {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }
    Ok(())
}

fn quote_env_value(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn unquote_env_value(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].replace("'\\''", "'")
    } else if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn save_saldeo_storage_state_secret(storage_state: &Path) -> Result<()> {
    if let Ok(text) = fs::read_to_string(storage_state)
        && keychain_set_secret(KEYCHAIN_ACCOUNT_SALDEO_STORAGE_STATE, &text)?
    {
        return Ok(());
    }
    Ok(())
}

fn read_saldeo_storage_state(storage_state: &Path) -> Result<String> {
    if let Some(text) = keychain_get_secret(KEYCHAIN_ACCOUNT_SALDEO_STORAGE_STATE)? {
        return Ok(text);
    }
    fs::read_to_string(storage_state)
        .with_context(|| format!("odczyt sesji Saldeo {}", storage_state.display()))
}

fn saldeo_session_valid(storage_state: &Path) -> bool {
    let Ok(text) = read_saldeo_storage_state(storage_state) else {
        return false;
    };
    let Ok(storage): Result<Value, _> = serde_json::from_str(&text) else {
        return false;
    };
    let Some(cookies) = storage.get("cookies").and_then(|v| v.as_array()) else {
        return false;
    };
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
        .and_then(|v| v.as_str());
    let Some(xsrf) = xsrf else {
        return false;
    };
    let Ok(client) = Client::builder().build() else {
        return false;
    };
    let body = serde_json::json!({
        "pagination": { "pageNumber": 0, "pageSize": 1, "totalCount": 0,
            "columnSorted": { "sortColumn": "DOCUMENT_CREATE_DATE", "sortDirection": "ASC" } },
        "filter": { "period": { "partOfYear": 1, "year": Utc::now().year(), "selectionType": "selectedMonth" },
            "duplicatesEnable": false, "duplicates": false, "splitPayment": false,
            "types": [], "contractors": [], "stages": [], "categories": [], "registers": [],
            "tags": [], "assignUsers": [], "addedBy": [], "added": [],
            "paymentStatuses": [], "accountingPaymentTypes": [],
            "searchQuery": "", "selectKsefDocumentsYesCheckbox": false,
            "selectKsefDocumentsNoCheckbox": false, "ksefNumber": "",
            "ksefMiniWorkflowStatus": null, "ksefBoId": null,
            "dimensionReportDocumentIds": [], "dimensions": null }
    });
    match client
        .post("https://saldeo.brainshare.pl/rest/client/document/list/search")
        .header("Cookie", &cookie_header)
        .header("X-SALDEO-XSRF-H-TOKEN", xsrf)
        .header("saldeoApp", "angularApp")
        .header("timeout", "60000")
        .json(&body)
        .send()
    {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
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
            "PDF-y są parsowane przez pdftotext, potem PyMuPDF/pdfplumber/pypdf jako fallback."
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

    #[test]
    fn parses_llm_json_from_markdown() {
        let value = parse_json_from_llm("```json\n{\"ok\":true}\n```").unwrap();
        assert_eq!(value["ok"], true);
    }

    #[test]
    fn parses_llm_json_after_reasoning_and_invalid_template() {
        let content = r#"
<|channel>thought
Template:
{
  "currency": "PLN" | "EUR" | null
}
<|channel>final
{"invoice_number":"FV/1/2026","currency":"PLN"}
"#;
        let value = parse_json_from_llm(content).unwrap();
        assert_eq!(value["invoice_number"], "FV/1/2026");
        assert_eq!(value["currency"], "PLN");
    }

    #[test]
    fn final_channel_wins_over_valid_json_in_thought() {
        let content = r#"
<|channel>thought
{"invoice_number":"WRONG","currency":"EUR"}
<|channel>final
{"invoice_number":"RIGHT","currency":"PLN"}
"#;
        let value = parse_json_from_llm(content).unwrap();
        assert_eq!(value["invoice_number"], "RIGHT");
        assert_eq!(value["currency"], "PLN");
    }

    #[test]
    fn rejects_thought_only_json() {
        let content = r#"
<|channel>thought
{"invoice_number":"WRONG","currency":"EUR"}
"#;
        assert!(parse_json_from_llm(content).is_err());
    }

    #[test]
    fn productmesh_filter_normalizes_input_nip() {
        let mut record = empty_record(SourceKind::Mail);
        record.invoice_number = Some("FV/1/2026".into());
        record.buyer_tax_id = Some("5242920020".into());
        let candidates = productmesh_invoice_candidates(&[record], "PL 524-292-00-20");
        assert_eq!(candidates.len(), 1);
    }
}
