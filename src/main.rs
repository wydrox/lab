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
mod lab_root;
mod own_nip;
use invoice_validation::counterparty_name_is_placeholder;
pub(crate) use lab_root::*;
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

mod commands;
mod db;
mod gmail;
mod llm;
mod mail_cache;
mod model;
mod parser;
mod scoring;
mod sync;

pub(crate) use commands::*;
pub(crate) use db::*;
pub(crate) use gmail::*;
pub(crate) use llm::*;
pub(crate) use mail_cache::*;
pub(crate) use model::*;
pub(crate) use parser::*;
pub(crate) use scoring::*;
pub(crate) use sync::*;

pub(crate) const KEYCHAIN_SERVICE: &str = "lab-cli";
const DEFAULT_PRODUCTMESH_NIP: &str = "5242920020";

fn main() -> Result<()> {
    let cli = Cli::parse();
    prepare_secret_store();
    // Katalog LAB wybierany raz, przed TUI: komunikat i zapamiętanie LAB_ROOT idą na stderr.
    let _ = lab_root_info();
    let db_path = cli.db.unwrap_or_else(default_db_path);
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
            approve,
            max_approve,
            require_mail,
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
            approve,
            ApprovePolicy::from_args(max_approve, require_mail),
        ),
        Commands::Repair {
            year,
            review_score,
            llm,
            confirm,
            output,
        } => handle_repair_command(db_path, year, review_score, llm, confirm, output),
        Commands::Approve {
            year,
            review_score,
            confirm,
            max_approve,
            require_mail,
            output,
        } => handle_approve_command(
            db_path,
            year,
            review_score,
            confirm,
            ApprovePolicy::from_args(max_approve, require_mail),
            output,
        ),
        Commands::Mcp => run_mcp_server(db_path),
        Commands::Db { command } => handle_db_command(db_path, command),
        Commands::Doctor { token_env } => doctor(db_path, &token_env),
    }
}
