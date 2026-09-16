use anyhow::{Context, Result, anyhow};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use clap::{Parser, ValueEnum};
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
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;
use walkdir::WalkDir;

mod cli;
mod document_text;
use document_text::extract_document_text;
mod hardening;
pub(crate) use hardening::{
    MAX_OCR_PDF_BYTES, apply_isolated_env, chmod_sqlite_files, local_llm_base_url, local_tool,
    require_existing_file, write_private_file,
};
mod keychain;
pub(crate) use keychain::{keychain_get_secret, keychain_set_secret};
mod credentials;
pub(crate) use credentials::*;
#[cfg(test)]
mod credentials_tests;
mod invoice_validation;
use invoice_validation::counterparty_name_is_placeholder;
mod openrouter;
pub(crate) use openrouter::*;
#[cfg(test)]
mod tests;

use cli::{Cli, Commands, DbCommands};

mod ksef;
mod mcp;
mod onboard;
mod reconcile;
mod saldeo;
mod tui;

pub(crate) use ksef::*;
pub(crate) use mcp::*;
pub(crate) use onboard::*;
pub(crate) use reconcile::*;
pub(crate) use saldeo::*;
pub(crate) use tui::*;

pub(crate) const KEYCHAIN_SERVICE: &str = "lab-cli";
const DEFAULT_PRODUCTMESH_NIP: &str = "5242920020";

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

#[derive(Debug, Serialize)]
struct SyncRunSummary {
    synced: Vec<String>,
    year: i32,
    records_count: usize,
    stored: bool,
}

#[allow(clippy::too_many_arguments)]
fn run_sync_sources(
    year: i32,
    ksef: bool,
    mail: bool,
    amazon_mail: bool,
    saldeo: bool,
    ksef_input: Option<&Path>,
    gmail_client_secret: Option<&Path>,
    gmail_token_file: Option<&Path>,
    productmesh_nip: &str,
    db_path: Option<&Path>,
    conn: Option<&Connection>,
) -> Result<SyncRunSummary> {
    run_sync_sources_with_progress(
        year,
        ksef,
        mail,
        amazon_mail,
        saldeo,
        ksef_input,
        gmail_client_secret,
        gmail_token_file,
        productmesh_nip,
        db_path,
        conn,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_sync_sources_with_progress(
    year: i32,
    ksef: bool,
    mail: bool,
    amazon_mail: bool,
    saldeo: bool,
    ksef_input: Option<&Path>,
    gmail_client_secret: Option<&Path>,
    gmail_token_file: Option<&Path>,
    productmesh_nip: &str,
    db_path: Option<&Path>,
    conn: Option<&Connection>,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<SyncRunSummary> {
    let all = !ksef && !mail && !amazon_mail && !saldeo;
    if all {
        eprintln!("Sync: wszystkie źródła (KSeF + Gmail/PDF + Saldeo)");
    }
    let stored = conn.is_some();
    let mut synced: Vec<String> = Vec::new();
    let mut records_count = 0usize;

    if ksef || all {
        let result = if let Some(input) = ksef_input {
            eprintln!("  [KSeF] synchronizacja z lokalnego eksportu...");
            if let Some(progress) = &progress {
                set_progress(progress, "KSeF: synchronizacja z lokalnego eksportu...");
            }
            ksef_sync(year, input, None)?
        } else {
            eprintln!("  [KSeF] synchronizacja online...");
            if progress.is_some() {
                ksef_online_sync_cached_with_progress(year, None, progress.clone())?
            } else {
                ksef_online_sync_with_progress(year, None, progress.clone())?
            }
        };
        records_count += result.records.len();
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!("KSeF: zapis do bazy ({} rekordów)...", result.records.len()),
            );
        }
        if let Some(conn) = conn {
            store_records(conn, &result.records)?;
        }
        eprintln!("  [KSeF] gotowe: {} rekordów", result.summary.records_count);
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!("KSeF: gotowe — {} rekordów", result.summary.records_count),
            );
        }
        synced.push(format!("ksef ({})", result.summary.records_count));
    }

    if mail || amazon_mail || all {
        let mail_source_label = if amazon_mail { "Amazon" } else { "Gmail" };
        let mail_cache = if amazon_mail {
            default_amazon_mail_out_path(year)
        } else {
            default_mail_out_path(year)
        };
        let mail_query = if amazon_mail {
            amazon_gmail_query(year)
        } else {
            default_gmail_query(year)
        };
        let mail_candidates_path = mail_cache.join("candidates.jsonl");
        eprintln!("  [{mail_source_label}] sprawdzanie wiadomości i cache załączników...");
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!("{mail_source_label}: sprawdzanie tokena..."),
            );
        }
        let token_path = gmail_token_file
            .map(PathBuf::from)
            .unwrap_or_else(default_gmail_token_path);
        let token = gmail_access_token("GMAIL_ACCESS_TOKEN", &token_path, gmail_client_secret)?;
        let gmail_result = gmail_fetch(
            &token,
            "me",
            &mail_query,
            &mail_cache,
            usize::MAX,
            &["pdf".to_string()],
            progress.clone(),
        )?;
        eprintln!(
            "  [{mail_source_label}] wiadomości: {} znalezionych, {} z cache, {} pobranych z API; nowe pliki: {} metadane, {} załączniki",
            gmail_result.messages_seen,
            gmail_result.messages_cached,
            gmail_result.messages_fetched,
            gmail_result.metadata_saved,
            gmail_result.attachments_saved
        );
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "{mail_source_label}: {} wiadomości, cache {}, API {}, załączniki {}",
                    gmail_result.messages_seen,
                    gmail_result.messages_cached,
                    gmail_result.messages_fetched,
                    gmail_result.attachments_saved
                ),
            );
        }
        eprintln!("  [{mail_source_label}] skanowanie nowych PDF...");
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!("{mail_source_label}: skanowanie nowych PDF..."),
            );
        }
        let (mail_records, parsed_count) =
            sync_mail_records(&mail_cache, &gmail_result.saved_files)?;
        eprintln!(
            "  [{mail_source_label}] sparsowano {} nowych PDF",
            parsed_count
        );
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "{mail_source_label}: sparsowano {parsed_count} nowych PDF, razem {}",
                    mail_records.len()
                ),
            );
        }
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!("{mail_source_label}: wybór kandydatów ProductMesh..."),
            );
        }
        let mut candidates = productmesh_invoice_candidates(&mail_records, productmesh_nip);
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "{mail_source_label}: cache kandydatów ({} faktur)...",
                    candidates.len()
                ),
            );
        }
        let cached_candidates =
            apply_cached_mail_candidates(&mail_candidates_path, &mut candidates)?;
        enrich_candidates_with_gemma(&mut candidates, &cached_candidates, progress.clone())?;
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "{mail_source_label}: zapis {} kandydatów...",
                    candidates.len()
                ),
            );
        }
        write_records(
            &candidates,
            OutputFormat::Jsonl,
            Some(&mail_candidates_path),
        )?;
        records_count += candidates.len();
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "{mail_source_label}: zapis do bazy ({} rekordów)...",
                    candidates.len()
                ),
            );
        }
        if let Some(conn) = conn {
            store_records(conn, &candidates)?;
        }
        eprintln!(
            "  [{mail_source_label}] gotowe: {} PDF, {} faktur",
            mail_records.len(),
            candidates.len()
        );
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "{mail_source_label}: gotowe — {} PDF, {} faktur",
                    mail_records.len(),
                    candidates.len()
                ),
            );
        }
        synced.push(format!(
            "{} ({} new attachments, {} pdfs, {} candidates)",
            if amazon_mail { "amazon-mail" } else { "mail" },
            gmail_result.attachments_saved,
            mail_records.len(),
            candidates.len()
        ));
    }

    if saldeo || all {
        eprintln!("  [Saldeo] pobieranie dokumentów...");
        let result = saldeo_fetch_with_progress(
            year,
            &default_saldeo_storage_state_path(),
            &default_saldeo_out_path(year),
            db_path,
            progress.clone(),
        )?;
        records_count += result.records.len();
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Saldeo: zapis do bazy ({} rekordów)...",
                    result.records.len()
                ),
            );
        }
        if let Some(conn) = conn {
            store_records(conn, &result.records)?;
        }
        eprintln!(
            "  [Saldeo] gotowe: {} dokumentów",
            result.summary.documents_count
        );
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Saldeo: gotowe — {} dokumentów",
                    result.summary.documents_count
                ),
            );
        }
        synced.push(format!("saldeo ({})", result.summary.documents_count));
    }

    Ok(SyncRunSummary {
        synced,
        year,
        records_count,
        stored,
    })
}

fn sync_reconcile_metadata(year: i32, ksef: bool, saldeo: bool, db_path: &Path) -> Result<()> {
    sync_reconcile_metadata_with_progress(year, ksef, saldeo, db_path, None)
}

fn sync_reconcile_metadata_with_progress(
    year: i32,
    ksef: bool,
    saldeo: bool,
    db_path: &Path,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<()> {
    if let Some(progress) = &progress {
        set_progress(progress, "Reconcile: otwieranie lokalnej bazy...");
    }
    let conn = open_db(db_path)?;
    if ksef {
        eprintln!("  [KSeF] pobieranie metadanych online do lokalnej bazy...");
        let result = if progress.is_some() {
            ksef_online_sync_cached_with_progress(year, None, progress.clone())?
        } else {
            ksef_online_sync_with_progress(year, None, progress.clone())?
        };
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Reconcile/KSeF: zapis do bazy ({} rekordów)...",
                    result.records.len()
                ),
            );
        }
        store_records(&conn, &result.records)?;
    }

    if saldeo {
        eprintln!("  [Saldeo] pobieranie metadanych reconcile do lokalnej bazy...");
        let result = saldeo_fetch_with_progress(
            year,
            &default_saldeo_storage_state_path(),
            &default_saldeo_out_path(year),
            Some(db_path),
            progress.clone(),
        )?;
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Reconcile/Saldeo: zapis do bazy ({} rekordów)...",
                    result.records.len()
                ),
            );
        }
        store_records(&conn, &result.records)?;
    }
    if let Some(progress) = &progress {
        set_progress(progress, "Reconcile: metadane odświeżone");
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let db_path = cli.db;
    match cli.command {
        Some(command) => handle_command(&db_path, command),
        None => interactive_tui(&db_path),
    }
}

fn handle_command(db_path: &Path, command: Commands) -> Result<()> {
    match command {
        Commands::Onboard {
            check,
            gmail_client_secret,
        } => onboard(db_path, check, gmail_client_secret.as_deref()),
        Commands::Sync {
            ksef,
            mail,
            amazon_mail,
            saldeo,
            year,
            ksef_input,
            gmail_client_secret,
            gmail_token_file,
            productmesh_nip,
            store,
        } => handle_sync_command(
            db_path,
            year,
            ksef,
            mail,
            amazon_mail,
            saldeo,
            ksef_input,
            gmail_client_secret,
            gmail_token_file,
            productmesh_nip,
            store,
        ),
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
        } => handle_reconcile_command(
            db_path,
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
        ),
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
        } => handle_upload_command(
            db_path,
            year,
            tri_report,
            mail,
            ksef,
            saldeo,
            review_score,
            output,
            csv,
            confirm,
        ),
        Commands::Mcp => run_mcp_server(db_path),
        Commands::Db { command } => handle_db_command(db_path, command),
        Commands::Doctor { token_env } => doctor(db_path, &token_env),
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_sync_command(
    db_path: &Path,
    year: i32,
    ksef: bool,
    mail: bool,
    amazon_mail: bool,
    saldeo: bool,
    ksef_input: Option<PathBuf>,
    gmail_client_secret: Option<PathBuf>,
    gmail_token_file: Option<PathBuf>,
    productmesh_nip: String,
    store: bool,
) -> Result<()> {
    let auto_store_online_ksef =
        ksef_input.is_none() && (ksef || (!ksef && !mail && !amazon_mail && !saldeo));
    let conn = if store || auto_store_online_ksef {
        Some(open_db(db_path)?)
    } else {
        None
    };
    let summary = run_sync_sources(
        year,
        ksef,
        mail,
        amazon_mail,
        saldeo,
        ksef_input.as_deref(),
        gmail_client_secret.as_deref(),
        gmail_token_file.as_deref(),
        &productmesh_nip,
        Some(db_path),
        conn.as_ref(),
    )?;
    write_json(&summary, None)
}

#[allow(clippy::too_many_arguments)]
fn handle_reconcile_command(
    db_path: &Path,
    status: bool,
    mail: Option<PathBuf>,
    ksef: Option<PathBuf>,
    saldeo: Option<PathBuf>,
    review_score: u8,
    output: Option<PathBuf>,
    raw: bool,
    csv: Option<PathBuf>,
    store: bool,
    year: i32,
) -> Result<()> {
    if status {
        let conn = open_db(db_path)?;
        let report = load_last_tri_report(&conn, year)?;
        if raw || output.is_some() {
            return write_json(&report, output.as_deref());
        }
        return write_reconcile_human(&report, None);
    }

    let refresh_ksef = ksef.is_none();
    let refresh_saldeo = saldeo.is_none();
    sync_reconcile_metadata(year, refresh_ksef, refresh_saldeo, db_path)?;

    let mail_path = mail.unwrap_or_else(|| default_mail_candidates_path(year));
    let ksef_path = ksef.unwrap_or_else(|| configured_ksef_out_path(year));
    let saldeo_path = saldeo.unwrap_or_else(|| default_saldeo_records_path(year));
    let mail_records = load_records(SourceKind::Mail, &mail_path)?;
    let ksef_records = load_records(SourceKind::Ksef, &ksef_path)?;
    let saldeo_records = load_saldeo_records(&saldeo_path, Some(db_path))?;
    let report = tri_reconcile(mail_records, ksef_records, saldeo_records, review_score);

    let temporal_diff = if store {
        let conn = open_db(db_path)?;
        Some(store_tri_reconcile_report(&conn, year, &report)?)
    } else {
        None
    };
    if let Some(csv_path) = csv {
        write_tri_csv(&report, &csv_path)?;
    }
    if output.is_some() {
        write_json(&report, output.as_deref())
    } else if raw {
        if temporal_diff.is_some() {
            write_json(
                &serde_json::json!({"report": report, "temporal_diff": temporal_diff}),
                None,
            )
        } else {
            write_json(&report, None)
        }
    } else {
        write_reconcile_human(&report, temporal_diff.as_ref())
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_upload_command(
    db_path: &Path,
    year: i32,
    tri_report: Option<PathBuf>,
    mail: Option<PathBuf>,
    ksef: Option<PathBuf>,
    saldeo: Option<PathBuf>,
    review_score: u8,
    output: Option<PathBuf>,
    csv: Option<PathBuf>,
    confirm: bool,
) -> Result<()> {
    let mut plan = saldeo_sync_plan(SaldeoSyncPlanConfig {
        year,
        tri_report: tri_report.as_deref(),
        mail: mail.as_deref(),
        ksef: ksef.as_deref(),
        saldeo: saldeo.as_deref(),
        db_path: Some(db_path),
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
    if confirm && plan.summary.uploaded_count > 0 {
        if let Err(err) = saldeo_fetch_with_progress(
            year,
            &default_saldeo_storage_state_path(),
            &default_saldeo_out_path(year),
            Some(db_path),
            None,
        ) {
            eprintln!("  [Saldeo] refresh po uploadzie nie powiódł się: {err}");
        }
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
        Some(path) => write_private_file(path, bytes),
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
    chmod_sqlite_files(path)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "trusted_schema", "OFF")?;
    conn.pragma_update(None, "cell_size_check", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    chmod_sqlite_files(path)?;
    conn.execute_batch(
        r#"
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
        CREATE TABLE IF NOT EXISTS saldeo_overrides (
            content_hash TEXT PRIMARY KEY,
            invoice_number TEXT,
            seller_tax_id TEXT,
            buyer_tax_id TEXT,
            seller_name TEXT,
            buyer_name TEXT,
            issue_date TEXT,
            gross_amount_minor INTEGER,
            currency TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_saldeo_overrides_updated_at ON saldeo_overrides(updated_at);
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
    db_path: &Path,
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
    if source.is_none() || matches!(source, Some(SourceKind::Saldeo)) {
        let _ = apply_saldeo_record_overrides(&mut records, Some(db_path))?;
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
        let primary = tri_row_display_record(row);
        let primary = primary.as_ref();
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
    let primary = tri_row_display_record(row);
    if let Some(record) = primary.as_ref() {
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
            let records = load_records_from_db(&conn, path, source, Some(limit))?;
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

const MAIL_PARSER_VERSION: &str = "lab-mail-parser:v3";

#[cfg(test)]
mod mail_cache_tests;

fn mail_parse_needs_retry(record: &InvoiceRecord) -> bool {
    record
        .warnings
        .iter()
        .any(|warning| mail_warning_needs_retry(warning))
}

fn mail_warning_needs_retry(warning: &str) -> bool {
    let warning = warning.to_lowercase();
    warning.starts_with("nie udało się wyciągnąć tekstu pdf")
        || warning.starts_with("pdf bez tekstu")
        || warning.starts_with("pdf nie zawiera tekstu")
        || warning.starts_with("ocr niedostępny")
        || warning.starts_with("ocr nie powiódł się")
        || warning.starts_with("błąd odczytu")
}

// Keep established values; only fill gaps and replace known name placeholders.
fn merge_mail_fields(target: &mut InvoiceRecord, incoming: &InvoiceRecord) {
    macro_rules! fill {
        ($($field:ident),* $(,)?) => {$(
            if target.$field.is_none() {
                target.$field = incoming.$field.clone();
            }
        )*};
    }
    fill!(
        invoice_number,
        seller_tax_id,
        buyer_tax_id,
        issue_date,
        sale_date,
        due_date,
        gross_amount_minor,
        net_amount_minor,
        vat_amount_minor,
        currency,
        ksef_reference,
        source_path,
        email_message_id,
        email_subject,
        email_from
    );
    for (target, incoming) in [
        (&mut target.seller_name, &incoming.seller_name),
        (&mut target.buyer_name, &incoming.buyer_name),
    ] {
        if target
            .as_deref()
            .is_none_or(counterparty_name_is_placeholder)
            && incoming
                .as_deref()
                .is_some_and(|name| !counterparty_name_is_placeholder(name))
        {
            *target = incoming.clone();
        }
    }
}

fn sync_mail_records(
    mail_out: &Path,
    saved_files: &[String],
) -> Result<(Vec<InvoiceRecord>, usize)> {
    sync_mail_records_with_parser(mail_out, saved_files, |path| {
        parse_file(SourceKind::Mail, path)
    })
}

fn sync_mail_records_with_parser(
    mail_out: &Path,
    saved_files: &[String],
    mut parse: impl FnMut(&Path) -> Result<InvoiceRecord>,
) -> Result<(Vec<InvoiceRecord>, usize)> {
    let cache_path = mail_out.join("records.jsonl");
    if !cache_path.exists() {
        let records = scan_mail_input_with_parser(mail_out, &mut parse)?;
        let parsed_count = records.len();
        write_records(&records, OutputFormat::Jsonl, Some(&cache_path))?;
        return Ok((records, parsed_count));
    }
    let mut records = load_records(SourceKind::Mail, &cache_path)?;
    let mut parsed_count = 0usize;
    for record in &mut records {
        if record
            .warnings
            .iter()
            .any(|warning| warning == MAIL_PARSER_VERSION)
            && !mail_parse_needs_retry(record)
        {
            continue;
        }
        let Some(path) = record.source_path.as_deref().map(Path::new) else {
            continue;
        };
        if !path.is_file()
            || !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
        {
            continue;
        }
        // A failed extraction can contain filename guesses. Preserve the original
        // record, including its parser version, so the next sync can try again.
        let Ok(parsed) = parse(path) else { continue };
        if mail_parse_needs_retry(&parsed) {
            continue;
        }
        merge_mail_fields(record, &parsed);
        record.content_hash = parsed.content_hash;
        record.warnings.retain(|warning| {
            !warning.starts_with("lab-mail-parser:") && !mail_warning_needs_retry(warning)
        });
        for warning in parsed.warnings {
            if !warning.starts_with("lab-mail-parser:") && !record.warnings.contains(&warning) {
                record.warnings.push(warning);
            }
        }
        record.warnings.push(MAIL_PARSER_VERSION.to_string());
        parsed_count += 1;
    }
    let mut seen = records
        .iter()
        .map(|record| record.content_hash.clone())
        .collect::<HashSet<_>>();
    let mut paths = saved_files.iter().map(PathBuf::from).collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    for path in paths {
        if !path.is_file() || !is_mail_candidate_file(&path) {
            continue;
        }
        let mut record = parse(&path)?;
        if !mail_parse_needs_retry(&record) {
            record.warnings.push(MAIL_PARSER_VERSION.to_string());
        }
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

fn scan_mail_input_with_parser(
    input: &Path,
    parse: &mut impl FnMut(&Path) -> Result<InvoiceRecord>,
) -> Result<Vec<InvoiceRecord>> {
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
        .map(|path| {
            let mut record = parse(path)?;
            if !mail_parse_needs_retry(&record) {
                record.warnings.push(MAIL_PARSER_VERSION.to_string());
            }
            Ok(record)
        })
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
        "pdf" => match extract_document_text(path) {
            Ok((text, extraction_warnings)) => {
                warnings.extend(extraction_warnings);
                text
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

fn record_amounts_inconsistent(record: &InvoiceRecord) -> bool {
    match (
        record.net_amount_minor,
        record.vat_amount_minor,
        record.gross_amount_minor,
    ) {
        (Some(net), Some(vat), Some(gross)) => net.checked_add(vat) != Some(gross),
        (Some(net), None, Some(gross)) if net >= 0 && gross >= 0 => net > gross,
        (None, Some(vat), Some(gross)) if vat >= 0 && gross >= 0 => vat > gross,
        _ => false,
    }
}

fn record_missing_core_fields(record: &InvoiceRecord) -> bool {
    record_amounts_inconsistent(record)
        || record.invoice_number.is_none()
        || record.issue_date.is_none()
        || record.gross_amount_minor.is_none()
        || record.currency.is_none()
        || (record
            .seller_name
            .as_deref()
            .is_none_or(counterparty_name_is_placeholder)
            && record
                .buyer_name
                .as_deref()
                .is_none_or(counterparty_name_is_placeholder))
        || record
            .seller_name
            .as_deref()
            .is_some_and(counterparty_name_is_placeholder)
        || record
            .buyer_name
            .as_deref()
            .is_some_and(counterparty_name_is_placeholder)
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
        .and_then(|v| normalize_currency(&v));
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
    record.currency = get(&["currency", "kodWaluty"]).and_then(|v| normalize_currency(&v));
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
            "date due",
            "payment due",
        ],
    );
    let tax_ids = tax_ids_from_text(text);
    let (seller_name, buyer_name) = counterparty_names_from_text(text);
    record.seller_name = seller_name;
    record.buyer_name = buyer_name;
    if tax_ids.len() == 1 && sole_tax_id_looks_like_buyer(text) {
        record.buyer_tax_id = tax_ids.first().cloned();
    } else {
        record.seller_tax_id = tax_ids.first().cloned();
        record.buyer_tax_id = tax_ids.get(1).cloned();
    }
    record.gross_amount_minor = amount_from_text(
        text,
        &[
            "łączna kwota brutto",
            "laczna kwota brutto",
            "wartość brutto",
            "wartosc brutto",
            "kwota brutto",
            "razem brutto",
            "total gross",
            "total",
            "brutto",
            "razem do zapłaty",
            "do zapłaty",
            "amount due",
        ],
    );
    record.net_amount_minor = amount_from_text(
        text,
        &[
            "total net",
            "wartość netto",
            "wartosc netto",
            "kwota netto",
            "netto",
            "net",
        ],
    );
    record.vat_amount_minor = amount_from_text(
        text,
        &[
            "total vat",
            "wartość vat",
            "wartosc vat",
            "kwota vat",
            "podatek vat",
            "vat amount",
            "tax amount",
            "podatek",
            "vat",
        ],
    );
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
        r"(?is)numer\s+faktury[\s:#\-]*([0-9A-Z][A-Z0-9/_.\-]{2,})",
        r"(?im)^\s*invoice[ \t]*(?:no\.?|number)[ \t:#\-]*([A-Z0-9][A-Z0-9/_.\-]{2,})\s*$",
        r"(?i)(?:obraz\s+)?faktur(?:a|y)?\s*(?:vat)?\s*(?:nr|numer)?[\s:#\-\n]*([A-Z0-9][A-Z0-9/_.\-]{2,})",
        r"(?i)(?:nr\s*faktury|invoice\s*(?:no\.?|number)?)[\s:#\-\n]*([A-Z0-9][A-Z0-9/_.\-]{2,})",
        r"(?i)\bFV[\s:#\-]*([A-Z0-9][A-Z0-9/_.\-]{2,})",
    ];
    for pattern in patterns {
        let re = Regex::new(pattern).unwrap();
        for caps in re.captures_iter(text) {
            if let Some(value) = caps.get(1) {
                let cleaned = clean_invoice_number(value.as_str());
                if is_valid_invoice_number_candidate(&cleaned) {
                    return Some(cleaned);
                }
            }
        }
    }
    None
}

fn is_valid_invoice_number_candidate(value: &str) -> bool {
    let value = value.trim();
    if value.chars().count() < 3 || !value.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    !matches!(
        value,
        "ZOSTA"
            | "ZOSTAŁA"
            | "VAT"
            | "FOR"
            | "INVOICE"
            | "NUMBER"
            | "DATE"
            | "DUE"
            | "FAKTURY"
            | "FAKTURA"
            | "NUMER"
            | "PODSTAWOWA"
            | "SYSTEM"
            | "KSEF"
    )
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
    let patterns = [
        r"(?i)\b(?:NIP[ \t:]*|PL[ \t]+VAT[ \t:]*)(?:PL[ \t]*)?([0-9][0-9\- \t]{8,20}[0-9])\b",
        r"(?i)\bPL[ \t]*([0-9]{10})\b",
    ];
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for pattern in patterns {
        let re = Regex::new(pattern).unwrap();
        for caps in re.captures_iter(text) {
            if let Some(id) = caps.get(1).and_then(|m| normalize_tax_id(m.as_str()))
                && seen.insert(id.clone())
            {
                ids.push(id);
            }
        }
    }
    ids
}

fn sole_tax_id_looks_like_buyer(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "bill to",
        "nabywca",
        "odbiorca",
        "kupujący",
        "kupujacy",
        "buyer",
        "customer",
    ]
    .iter()
    .any(|label| lower.contains(label))
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
        "%Y-%m-%d",
        "%d.%m.%Y",
        "%d-%m-%Y",
        "%Y/%m/%d",
        "%d/%m/%Y",
        "%d %m %Y",
        "%B %-d, %Y",
        "%B %d, %Y",
        "%b %-d, %Y",
        "%b %d, %Y",
    ] {
        if let Ok(date) = NaiveDate::parse_from_str(value, fmt) {
            return Some(date);
        }
    }
    None
}

fn date_from_text(text: &str) -> Option<NaiveDate> {
    let patterns = [
        r"\b\d{4}-\d{2}-\d{2}\b",
        r"\b\d{2}[./-]\d{2}[./-]\d{4}\b",
        r"\b[A-Za-z]{3,9}\s+\d{1,2},\s+\d{4}\b",
    ];
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
            r"(?i){}[^0-9A-Z]{{0,30}}(\d{{4}}-\d{{2}}-\d{{2}}|\d{{2}}[./-]\d{{2}}[./-]\d{{4}}|[A-Z]{{3,9}}\s+\d{{1,2}},\s+\d{{4}})",
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
    let seller_labels = ["sprzedawca", "wystawca", "seller", "supplier"];
    let seller = name_after_label(text, &seller_labels).or_else(|| {
        if text
            .lines()
            .any(|line| name_label_match(line, &seller_labels).is_some())
        {
            None
        } else {
            name_before_label(text, &["bill to", "buyer", "customer"])
                .or_else(|| name_before_first_nip(text))
        }
    });
    let buyer_labels = [
        "nabywca",
        "odbiorca",
        "kupujący",
        "kupujacy",
        "buyer",
        "customer",
        "bill to",
    ];
    let buyer = name_after_label(text, &buyer_labels).or_else(|| {
        if text
            .lines()
            .any(|line| name_label_match(line, &buyer_labels).is_some())
        {
            None
        } else {
            name_before_nth_nip(text, 2)
        }
    });
    (seller, buyer)
}

fn name_label_match<'a>(line: &'a str, labels: &[&str]) -> Option<regex::Match<'a>> {
    let alternatives = labels
        .iter()
        .map(|label| regex::escape(label))
        .collect::<Vec<_>>()
        .join("|");
    Regex::new(&format!(r"(?i)\b(?:{alternatives})\b(?:[ \t]+details\b)?"))
        .ok()?
        .find(line)
}

fn any_name_label(line: &str) -> Option<regex::Match<'_>> {
    name_label_match(
        line,
        &[
            "sprzedawca",
            "wystawca",
            "seller",
            "supplier",
            "nabywca",
            "odbiorca",
            "kupujący",
            "kupujacy",
            "buyer",
            "customer",
            "bill to",
        ],
    )
}

fn name_after_label(text: &str, labels: &[&str]) -> Option<String> {
    let lines = raw_nonempty_lines(text);
    for (idx, line) in lines.iter().enumerate() {
        let Some(label) = name_label_match(line, labels) else {
            continue;
        };
        // Regex offsets refer to the original UTF-8 string, not its lowercase copy.
        let before = &line[..label.start()];
        let after = &line[label.end()..];
        let next_label = any_name_label(after);
        let right_column = !before.trim().is_empty();
        let paired = any_name_label(before).is_some() || next_label.is_some();
        let inline = &after[..next_label.map_or(after.len(), |m| m.start())];
        if let Some(name) = clean_name(inline).filter(|name| is_probable_name_line(name)) {
            return Some(name);
        }
        let column_start = line[..label.start()].chars().count();
        for candidate in lines.iter().skip(idx + 1).take(6) {
            if any_name_label(candidate).is_some() {
                break;
            }
            let segment = if paired || right_column {
                name_column_segment(candidate, right_column, column_start)
            } else {
                Some(candidate.as_str())
            };
            if let Some(name) = segment
                .and_then(clean_name)
                .filter(|name| is_probable_name_line(name))
            {
                return Some(name);
            }
        }
    }
    None
}

fn name_before_label(text: &str, labels: &[&str]) -> Option<String> {
    let lines = raw_nonempty_lines(text);
    for (idx, line) in lines.iter().enumerate() {
        let Some(label) = name_label_match(line, labels) else {
            continue;
        };
        if let Some(before_label) = clean_name(&line[..label.start()])
            && is_probable_name_line(&before_label)
        {
            return Some(before_label);
        }
        for candidate in lines[..idx].iter().rev().take(8) {
            let cleaned = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
            if is_probable_name_line(&cleaned) {
                return clean_name(&cleaned);
            }
        }
    }
    None
}

fn raw_nonempty_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim_end().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn name_column_segment(line: &str, right: bool, column_start: usize) -> Option<&str> {
    let separator = Regex::new(r" {2,}|\t+").ok()?;
    let trimmed = line.trim();
    if let Some(gap) = separator.find(trimmed) {
        return Some(if right {
            &trimmed[gap.end()..]
        } else {
            &trimmed[..gap.start()]
        });
    }
    // A row can contain only one populated column. Keep its indentation.
    let start = line.chars().take_while(|c| c.is_whitespace()).count();
    let leading_tab = line
        .chars()
        .take_while(|c| c.is_whitespace())
        .any(|c| c == '\t');
    if right {
        (leading_tab || start.saturating_add(2) >= column_start).then_some(trimmed)
    } else {
        (!leading_tab && start <= column_start.saturating_add(2)).then_some(trimmed)
    }
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
    line.chars().count() >= 3
        && !counterparty_name_is_placeholder(line)
        && !matches!(lower.trim(), "name" | "name:" | "nazwa" | "nazwa:")
        && !lower
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| word == "vat")
        && line.chars().count() <= 140
        && !lower.contains("nip")
        && !lower.contains("regon")
        && !lower.contains("adres")
        && !lower.contains("siedziba")
        && !lower.starts_with("ul.")
        && !lower.starts_with("ul ")
        && !lower.contains("data")
        && !lower.contains('@')
        && !lower.contains("http")
        && !line.chars().next().is_some_and(|c| c.is_ascii_digit())
        && !lower.contains("faktura")
        && !lower.contains("invoice")
        && !lower.contains("date")
        && !lower.contains("street")
        && !lower.contains("united states")
        && !lower.contains("poland")
        && !lower.contains("california")
        && !lower.contains("warszawa")
        && !lower.contains("pasadena")
        && !lower.contains("p.o. box")
        && !lower.contains("pmb ")
        && !lower.contains("razem")
        && !lower.contains("zapłaty")
        && !lower.contains("zapl")
        && line.chars().any(|c| c.is_alphabetic())
}

#[cfg(test)]
mod parser_names_tests;

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
        units
            .checked_mul(sign)?
            .checked_mul(100)?
            .checked_add(sign * cents)
    } else {
        s.retain(|c| c.is_ascii_digit() || c == '-');
        s.parse::<i64>().ok().and_then(|v| v.checked_mul(100))
    }
}

fn amount_from_text(text: &str, labels: &[&str]) -> Option<i64> {
    // Separatory grup wymagają pełnej części dziesiętnej. Nie łączymy kolumn ani linii.
    let decimal = r"-?(?:[0-9]{1,3}(?:[ \u{00a0}\u{202f}][0-9]{3})+[.,][0-9]{2}|[0-9]{1,3}(?:\.[0-9]{3})+,[0-9]{2}|[0-9]{1,3}(?:,[0-9]{3})+\.[0-9]{2}|[0-9]+[.,][0-9]{2})";
    let number = format!(r"(?:{decimal}|-?[0-9]+)");
    let currency = r"(?:PLN|EUR|USD|GBP|CHF|CZK|SEK|NOK|DKK|zł|€|\$|£)";
    let unit = format!(r"(?:\({currency}\)|{currency})");
    let h = r"[\t \u{00a0}\u{202f}]*";
    let value_re = Regex::new(&format!(
        r"(?i)^{h}(?:{unit}{h})?({number})(?:{h}{currency})?{h}(?:$|[ \t]+(?:NET|VAT|TOTAL)\b)"
    ))
    .unwrap();
    let parse_value = |value: &str| {
        // Limit chroni również starszy parse_money_minor przed przepełnieniem.
        if value.bytes().filter(u8::is_ascii_digit).count() > 16 {
            return None;
        }
        parse_money_minor(value)
    };

    // Układ kolumn musi wynikać z nagłówka, nigdy z arytmetyki kwot.
    // OCR może umieścić wartości w nagłówku, Poppler zachowuje osobny wiersz.
    let header_re = Regex::new(&format!(
        r"(?i)\bNET\b{h}(?:{unit}{h})?(?:{decimal}{h})?VAT\b{h}(?:{unit}{h})?(?:{decimal}{h})?TOTAL\b"
    )).unwrap();
    let row_re = Regex::new(&format!(
        r"(?im)^[ \t]*TOTAL\b[ \t]*:?[ \t\r\n]+({decimal})[ \t\r\n]+({decimal})[ \t\r\n]+({decimal})[ \t]*\r?$"
    )).unwrap();
    let table = header_re.find_iter(text).find_map(|header| {
        row_re.captures(&text[header.end()..]).map(|caps| {
            [1, 2, 3].map(|column| caps.get(column).and_then(|m| parse_value(m.as_str())))
        })
    });

    for label in labels {
        let column = match *label {
            "net" => Some(0),
            "vat" => Some(1),
            "total" => Some(2),
            _ => None,
        };
        if let Some(value) = column.and_then(|column| table.and_then(|row| row[column])) {
            return Some(value);
        }
        let label_re = Regex::new(&format!(
            r"(?i)\b{}\b{h}(?:{unit}{h})?[:=]?{h}",
            regex::escape(label)
        ))
        .unwrap();
        let lines: Vec<_> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            for label_match in label_re.find_iter(line) {
                let tail = &line[label_match.end()..];
                let candidate = if tail.is_empty() && line[..label_match.start()].trim().is_empty()
                {
                    // Tylko bezpośrednio następna linia; bez przeskakiwania nagłówków.
                    lines.get(index + 1).copied().unwrap_or_default()
                } else {
                    tail
                };
                if let Some(caps) = value_re.captures(candidate)
                    && let Some(value) = caps.get(1).and_then(|m| parse_value(m.as_str()))
                {
                    return Some(value);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod parser_amounts_tests;

fn normalize_currency(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let code_re = Regex::new(r"(?i)\b(PLN|EUR|USD|GBP|CHF|CZK|SEK|NOK|DKK)\b").unwrap();
    if let Some(code) = code_re.captures(value).and_then(|caps| caps.get(1)) {
        return Some(code.as_str().to_ascii_uppercase());
    }

    let lower = value.to_lowercase();
    if lower.contains('€') || Regex::new(r"(?i)\beuro\b").unwrap().is_match(value) {
        return Some("EUR".to_string());
    }
    if lower.contains("zł")
        || Regex::new(r"(?i)\bzl\b|\bzlot(?:y|ych|e)?\b|\bzłot(?:y|ych|e)?\b")
            .unwrap()
            .is_match(value)
    {
        return Some("PLN".to_string());
    }
    if value.contains('$')
        || Regex::new(r"(?i)\bdol(?:ar|lar|lars?)\b")
            .unwrap()
            .is_match(value)
    {
        return Some("USD".to_string());
    }
    if value.contains('£')
        || Regex::new(r"(?i)\b(?:gbp|pound|funt)\b")
            .unwrap()
            .is_match(value)
    {
        return Some("GBP".to_string());
    }
    if Regex::new(r"(?i)\bfrank(?:a|ów)?\b")
        .unwrap()
        .is_match(value)
    {
        return Some("CHF".to_string());
    }

    None
}

fn currency_from_text(text: &str) -> Option<String> {
    let labels = [
        "waluta",
        "currency",
        "currency code",
        "kod waluty",
        "kwota",
        "razem",
        "total",
        "amount",
        "do zapłaty",
        "do zaplaty",
    ];
    for label in labels {
        let pattern = format!(r"(?i){}[^\n]{{0,80}}", regex::escape(label));
        let re = Regex::new(&pattern).unwrap();
        for value in re.find_iter(text) {
            if let Some(currency) = normalize_currency(value.as_str()) {
                return Some(currency);
            }
        }
    }
    normalize_currency(text)
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
    let mut command = Command::new(local_tool("pdftotext")?);
    apply_isolated_env(&mut command);
    let pdftotext = command.arg("-layout").arg(path).arg("-").output();
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

    let ksef_ids = scoring_tax_ids(ksef);
    let mail_ids = scoring_tax_ids(mail);
    if !ksef_ids.is_empty() && ksef_ids.iter().any(|id| mail_ids.contains(id)) {
        score += 20;
        reasons.push("tax_id match".to_string());
    }
    if let (Some(a), Some(b)) = (&ksef.seller_tax_id, &mail.seller_tax_id) {
        let a = normalize_tax_id(a);
        let b = normalize_tax_id(b);
        if a.is_some() && a == b && a.as_deref() != Some(DEFAULT_PRODUCTMESH_NIP) {
            score += 5;
            reasons.push("seller_tax_id same position".to_string());
        }
    }

    if let (Some(a), Some(b)) = (ksef.gross_amount_minor, mail.gross_amount_minor)
        && a != 0
        && b != 0
    {
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

fn scoring_tax_ids(record: &InvoiceRecord) -> HashSet<String> {
    [
        record.seller_tax_id.as_deref(),
        record.buyer_tax_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(normalize_tax_id)
    .filter(|id| id != DEFAULT_PRODUCTMESH_NIP)
    .collect()
}

fn invoice_identity_match(left: &InvoiceRecord, right: &InvoiceRecord) -> bool {
    if let (Some(left_ksef), Some(right_ksef)) = (&left.ksef_reference, &right.ksef_reference)
        && !left_ksef.trim().is_empty()
        && left_ksef == right_ksef
    {
        return true;
    }
    let Some(left_number) = left
        .invoice_number
        .as_deref()
        .map(comparable_invoice_number)
        .filter(|number| !number.is_empty())
    else {
        return false;
    };
    let Some(right_number) = right
        .invoice_number
        .as_deref()
        .map(comparable_invoice_number)
        .filter(|number| !number.is_empty())
    else {
        return false;
    };
    if left_number != right_number {
        return false;
    }
    let left_ids = scoring_tax_ids(left);
    let right_ids = scoring_tax_ids(right);
    left_ids.is_empty() || right_ids.is_empty() || !left_ids.is_disjoint(&right_ids)
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
    let (mut stream, peer) = listener.accept().context("oczekiwanie na redirect OAuth")?;
    if !peer.ip().is_loopback() {
        return Err(anyhow!("OAuth: połączenie spoza pętli lokalnej"));
    }
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
    let text = serde_json::to_string_pretty(token)?;
    if uses_default_gmail_token_path(path) {
        save_secret(Secret::GmailToken, &text)?;
        return Ok(());
    }

    write_private_file(path, text.as_bytes())
}

fn read_gmail_token(path: &Path) -> Result<GmailTokenFile> {
    if uses_default_gmail_token_path(path) {
        let text = secret_value(Secret::GmailToken)?.ok_or_else(|| {
            anyhow!(
                "brak tokenu Gmail w Keychain i w {}; uruchom lab onboard",
                path.display()
            )
        })?;
        return serde_json::from_str(&text).context("niepoprawny token Gmail");
    }

    let text = read_secret_file(path, "token Gmail")?;
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
    progress: Option<Arc<Mutex<String>>>,
) -> Result<GmailFetchResult> {
    if let Some(progress) = &progress {
        set_progress(progress, "Gmail: przygotowanie katalogu i klienta...");
    }
    fs::create_dir_all(out_dir).with_context(|| format!("mkdir {}", out_dir.display()))?;
    let client = Client::builder().build()?;
    let allowed_exts = extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut page_token: Option<String> = None;
    let mut message_ids = Vec::new();

    while message_ids.len() < max {
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Gmail: lista wiadomości (znaleziono {})...",
                    message_ids.len()
                ),
            );
        }
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
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Gmail: lista wiadomości: {} znalezionych",
                    message_ids.len()
                ),
            );
        }
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
            if let Some(progress) = &progress {
                set_progress(
                    progress,
                    format!(
                        "Gmail: cache {}/{} wiadomości, API {}, pliki {}",
                        messages_cached,
                        message_ids.len(),
                        messages_fetched,
                        saved_files.len()
                    ),
                );
            }
            continue;
        }

        messages_fetched += 1;
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Gmail: pobieram wiadomość {}/{} (cache {}, API {})...",
                    messages_cached + messages_fetched,
                    message_ids.len(),
                    messages_cached,
                    messages_fetched
                ),
            );
        }
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
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Gmail: zapisano {} metadanych i {} załączników",
                    metadata_saved, attachments_saved
                ),
            );
        }
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

fn default_gmail_query(year: i32) -> String {
    format!(
        "after:{year}/01/01 before:{}/01/01 has:attachment filename:pdf",
        year + 1
    )
}

fn default_mail_out_path(year: i32) -> PathBuf {
    PathBuf::from(format!("data/mail-all-pdf-{year}-pdfs"))
}

fn default_amazon_mail_out_path(year: i32) -> PathBuf {
    PathBuf::from(format!("data/mail-amazon-{year}-pdfs"))
}

fn default_mail_candidates_path(year: i32) -> PathBuf {
    default_mail_out_path(year).join("candidates.jsonl")
}

fn amazon_gmail_query(year: i32) -> String {
    format!(
        "after:{year}/01/01 before:{}/01/01 (from:amazon.it OR from:amazon.es) has:attachment filename:pdf",
        year + 1
    )
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

// A cache entry must cover and agree with every established invoice field.
fn mail_cache_covers_fresh(cached: &InvoiceRecord, fresh: &InvoiceRecord) -> bool {
    macro_rules! covers {
        ($($field:ident),* $(,)?) => { true $(
            && (fresh.$field.is_none() || fresh.$field == cached.$field)
        )* };
    }
    covers!(
        invoice_number,
        seller_tax_id,
        buyer_tax_id,
        issue_date,
        sale_date,
        due_date,
        gross_amount_minor,
        net_amount_minor,
        vat_amount_minor,
        currency,
        ksef_reference
    ) && [
        (&fresh.seller_name, &cached.seller_name),
        (&fresh.buyer_name, &cached.buyer_name),
    ]
    .into_iter()
    .all(|(fresh, cached)| {
        fresh
            .as_deref()
            .is_none_or(counterparty_name_is_placeholder)
            || fresh == cached
    })
}

fn apply_cached_mail_candidates(
    path: &Path,
    candidates: &mut [InvoiceRecord],
) -> Result<HashSet<String>> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let cached = load_records(SourceKind::Mail, path)?;
    let by_hash = cached
        .into_iter()
        .filter(|record| {
            record
                .warnings
                .iter()
                .any(|warning| warning == MAIL_PARSER_VERSION)
        })
        .map(|record| (record.content_hash.clone(), record))
        .collect::<HashMap<_, _>>();
    let mut cached_hashes = HashSet::new();
    for candidate in candidates {
        if let Some(cached) = by_hash.get(&candidate.content_hash) {
            if mail_parse_needs_retry(cached) || !mail_cache_covers_fresh(cached, candidate) {
                continue;
            }
            merge_mail_fields(candidate, cached);
            for warning in &cached.warnings {
                if !candidate.warnings.contains(warning) {
                    candidate.warnings.push(warning.clone());
                }
            }
            if !record_missing_core_fields(cached) && !mail_parse_needs_retry(candidate) {
                cached_hashes.insert(candidate.content_hash.clone());
            }
        }
    }
    Ok(cached_hashes)
}

fn enrich_candidates_with_gemma(
    records: &mut [InvoiceRecord],
    skip_hashes: &HashSet<String>,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<()> {
    enrich_candidates_with_gemma_with_hook(records, skip_hashes, progress, |_, _| Ok(()))
}

fn enrich_candidates_with_gemma_with_hook<F>(
    records: &mut [InvoiceRecord],
    skip_hashes: &HashSet<String>,
    progress: Option<Arc<Mutex<String>>>,
    mut after_record: F,
) -> Result<()>
where
    F: FnMut(&[InvoiceRecord], usize) -> Result<()>,
{
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
    let use_openrouter = openrouter_configured();
    let model = if use_openrouter {
        openrouter_model()
    } else {
        llm_model()
    };
    let extract: fn(&mut InvoiceRecord, &Path) -> Result<bool> = if use_openrouter {
        openrouter_extract_invoice_fields
    } else {
        gemma_extract_invoice_fields
    };
    eprintln!(
        "  [Gmail/LLM] wzbogacanie {} kandydatów przez {}...",
        todo, model
    );
    if !use_openrouter
        && let Err(err) = ensure_ppmlx_server()
    {
        eprintln!("  [Gmail/LLM] pominięto wzbogacanie: {err}");
        for idx in 0..records.len() {
            if !skip_hashes.contains(&records[idx].content_hash)
                && record_missing_core_fields(&records[idx])
            {
                let warning = format!("LLM niedostępny: {err}");
                if !records[idx].warnings.contains(&warning) {
                    records[idx].warnings.push(warning);
                }
                after_record(records, idx)?;
            }
        }
        return Ok(());
    }
    let mut processed = 0usize;
    for idx in 0..records.len() {
        if skip_hashes.contains(&records[idx].content_hash)
            || !record_missing_core_fields(&records[idx])
        {
            continue;
        }
        let Some(source_path) = records[idx].source_path.clone() else {
            continue;
        };
        let path = Path::new(&source_path);
        if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        processed += 1;
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("PDF");
        let status = format!("LLM: {}/{} {}", processed, todo, fname);
        eprintln!("  [Gmail/LLM] {}", status);
        if let Some(ref p) = progress {
            *p.lock().unwrap() = status;
        }
        match extract(&mut records[idx], path) {
            Ok(true) => {
                records[idx].warnings.push(format!(
                    "LLM {model}: zastosowano zweryfikowane uzupełnienie"
                ));
            }
            Ok(false) => {}
            Err(err) => {
                let warning = format!("LLM {model}: {err}");
                if !records[idx].warnings.contains(&warning) {
                    records[idx].warnings.push(warning);
                }
                eprintln!("  [Gmail/LLM] odrzucono uzupełnienie: {err}");
            }
        }
        after_record(records, idx)?;
    }
    eprintln!("  [Gmail/LLM] gotowe");
    Ok(())
}

fn ppmlx_base_url() -> Result<String> {
    local_llm_base_url(
        &lab_config_var("PPMLX_BASE_URL").unwrap_or_else(|| "http://127.0.0.1:6767".to_string()),
    )
}

fn llm_model() -> String {
    lab_config_var("LAB_LLM_MODEL").unwrap_or_else(|| "gemma-4-e4b-it-optiq".to_string())
}

fn llm_timeout() -> Duration {
    Duration::from_secs(
        lab_config_var("LAB_LLM_TIMEOUT_SECS")
            .and_then(|value| value.parse().ok())
            .unwrap_or(45),
    )
}

fn ensure_ppmlx_server() -> Result<()> {
    let base = ppmlx_base_url()?;
    let model = llm_model();
    let client = Client::builder().timeout(Duration::from_secs(2)).build()?;
    if client
        .get(format!("{base}/v1/models"))
        .send()
        .is_ok_and(|r| r.status().is_success())
    {
        return Ok(());
    }
    Err(anyhow!(
        "ppmlx niedostępny: {base}; uruchom w osobnym terminalu: ppmlx serve --model {model}"
    ))
}

fn gemma_extract_invoice_fields(record: &mut InvoiceRecord, path: &Path) -> Result<bool> {
    let (text, extraction_warnings) = extract_document_text(path)?;
    if text.chars().count() > 60_000 {
        return Err(anyhow!(
            "Dokument przekracza limit 60000 znaków LLM; nie obcinam stron"
        ));
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
        text
    );
    let value = ppmlx_extract_json(&prompt)?;
    let changed = invoice_validation::apply_normalized_invoice_json(record, &value)?;
    for warning in extraction_warnings {
        if !record.warnings.contains(&warning) {
            record.warnings.push(warning);
        }
    }
    Ok(changed)
}

fn ppmlx_extract_json(prompt: &str) -> Result<Value> {
    let base = ppmlx_base_url()?;
    let model = llm_model();
    let client = Client::builder().timeout(llm_timeout()).build()?;
    let body = serde_json::json!({
        "model": model,
        "temperature": 0,
        "max_tokens": 4000,
        "messages": [
            {"role": "system", "content": "Jesteś ekstraktorem danych z faktur. Odpowiadasz tylko poprawnym JSON."},
            {"role": "user", "content": prompt}
        ]
    });

    let mut last_err = None;
    for attempt in 0..5 {
        match client
            .post(format!("{base}/v1/chat/completions"))
            .json(&body)
            .send()
        {
            Ok(resp) if resp.status().is_success() => {
                let response: Value = resp.json()?;
                return ppmlx_response_json(&response);
            }
            Ok(resp) if resp.status().as_u16() == 503 => {
                let delay = std::time::Duration::from_secs(2u64.pow(attempt));
                eprintln!(
                    "  [Gmail/Gemma] serwer zajęty (503), czekam {}s...",
                    delay.as_secs()
                );
                std::thread::sleep(delay);
                continue;
            }
            Ok(resp) => {
                return Err(anyhow!(
                    "ppmlx HTTP {}: {}",
                    resp.status(),
                    resp.text().unwrap_or_default()
                ));
            }
            Err(err) => {
                last_err = Some(err);
                let delay = std::time::Duration::from_secs(2u64.pow(attempt));
                std::thread::sleep(delay);
            }
        }
    }
    Err(last_err.map(anyhow::Error::from).unwrap_or_else(|| {
        anyhow!("ppmlx nie odpowiedział po 5 próbach (503 Service Unavailable)")
    }))
}

fn ppmlx_response_json(response: &Value) -> Result<Value> {
    let choice = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
        .ok_or_else(|| anyhow!("ppmlx: brak odpowiedzi modelu"))?;
    if choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .is_some_and(|r| r != "stop")
    {
        return Err(anyhow!(
            "ppmlx: odpowiedź nie została zakończona poprawnie; nie zapisuję częściowego JSON"
        ));
    }
    let content = choice
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("ppmlx: brak treści odpowiedzi"))?;
    parse_json_from_llm(content)
}

#[cfg(test)]
mod llm_flow_tests;

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
            json_first_string(value, &["currency"]).and_then(|v| normalize_currency(&v));
    }
    if record.seller_tax_id.is_none() {
        record.seller_tax_id =
            json_first_string(value, &["seller_tax_id"]).and_then(|v| normalize_tax_id(&v));
    }
    if record.buyer_tax_id.is_none() {
        record.buyer_tax_id =
            json_first_string(value, &["buyer_tax_id"]).and_then(|v| normalize_tax_id(&v));
    }
    if record
        .seller_name
        .as_deref()
        .is_none_or(counterparty_name_is_placeholder)
        && let Some(name) = json_first_string(value, &["seller_name"])
            .and_then(|v| clean_name(&v))
            .filter(|v| !counterparty_name_is_placeholder(v))
    {
        record.seller_name = Some(name);
    }
    if record
        .buyer_name
        .as_deref()
        .is_none_or(counterparty_name_is_placeholder)
        && let Some(name) = json_first_string(value, &["buyer_name"])
            .and_then(|v| clean_name(&v))
            .filter(|v| !counterparty_name_is_placeholder(v))
    {
        record.buyer_name = Some(name);
    }
}
