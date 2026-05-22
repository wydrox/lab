use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::{DEFAULT_PRODUCTMESH_NIP, SourceKind};

#[derive(Parser, Debug)]
#[command(name = "lab-cli")]
#[command(about = "LAB — Lazy Accounting Buddy", long_about = None)]
pub(crate) struct Cli {
    /// Dedykowana baza SQLite na rekordy, przebiegi i dopasowania.
    #[arg(long, global = true, default_value = "lab.sqlite")]
    pub(crate) db: PathBuf,
    #[command(subcommand)]
    pub(crate) command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
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
        #[arg(long, default_value = DEFAULT_PRODUCTMESH_NIP)]
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
pub(crate) enum DbCommands {
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
