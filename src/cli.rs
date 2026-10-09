use chrono::Datelike;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::{DEFAULT_PRODUCTMESH_NIP, SourceKind};

/// Domyślny minimalny score dopasowania — wspólny dla CLI i MCP.
pub(crate) const DEFAULT_REVIEW_SCORE: u8 = 70;
/// Dolna granica akceptowanego `--review-score` / `review_score`.
pub(crate) const MIN_REVIEW_SCORE: u8 = 50;
/// Górna granica akceptowanego `--review-score` / `review_score`.
pub(crate) const MAX_REVIEW_SCORE: u8 = 100;
/// Domyślny limit dokumentów KSeF zatwierdzanych w jednym przebiegu (`0` = bez limitu).
pub(crate) const DEFAULT_MAX_APPROVE: usize = 50;
/// Env var, który wymusza `--require-mail` / `require_mail` (np. `1`).
pub(crate) const APPROVE_REQUIRE_MAIL_ENV: &str = "LAB_APPROVE_REQUIRE_MAIL";

/// Czy `LAB_APPROVE_REQUIRE_MAIL` włącza filtr `require_mail`. Env tylko włącza:
/// ani flaga CLI, ani argument MCP nie mogą go wyłączyć.
pub(crate) fn approve_require_mail_from_env() -> bool {
    env_flag_enabled(std::env::var(APPROVE_REQUIRE_MAIL_ENV).ok().as_deref())
}

/// `1`, `true`, `yes`, `on` (bez względu na wielkość liter) włączają flagę.
pub(crate) fn env_flag_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Domyślny rok rozliczeniowy: bieżący rok wg czasu lokalnego.
pub(crate) fn default_year() -> i32 {
    chrono::Local::now().year()
}

#[derive(Parser, Debug)]
#[command(name = "lab-cli")]
#[command(about = "LAB — Lazy Accounting Buddy", long_about = None)]
pub(crate) struct Cli {
    /// Dedykowana baza SQLite na rekordy, przebiegi i dopasowania.
    /// Domyślnie lab.sqlite w katalogu LAB (LAB_ROOT, zob. lab doctor).
    #[arg(long, global = true)]
    pub(crate) db: Option<PathBuf>,
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
        /// Tylko Gmail/PDF z filtrem Amazon.it / Amazon.es.
        #[arg(long, conflicts_with = "mail")]
        amazon_mail: bool,
        /// Tylko Saldeo.
        #[arg(long)]
        saldeo: bool,
        /// Rok rozliczeniowy (domyślnie bieżący).
        #[arg(long, default_value_t = default_year())]
        year: i32,
        /// Katalog/plik z lokalnym eksportem KSeF (XML, JSON, JSONL). Bez tej flagi KSeF jest pobierany online.
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
        /// Minimalny score dopasowania (50–100).
        #[arg(
            long,
            default_value_t = DEFAULT_REVIEW_SCORE,
            value_parser = clap::value_parser!(u8).range(MIN_REVIEW_SCORE as i64..=MAX_REVIEW_SCORE as i64)
        )]
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
        /// Rok przy --store i --status (domyślnie bieżący).
        #[arg(long, default_value_t = default_year())]
        year: i32,
    },
    /// Wysyła brakujące faktury do SaldeoSMART.
    Upload {
        /// Rok rozliczeniowy (domyślnie bieżący).
        #[arg(long, default_value_t = default_year())]
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
        /// Minimalny score dopasowania (50–100), gdy raport jest liczony z wejść.
        #[arg(
            long,
            default_value_t = DEFAULT_REVIEW_SCORE,
            value_parser = clap::value_parser!(u8).range(MIN_REVIEW_SCORE as i64..=MAX_REVIEW_SCORE as i64)
        )]
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
        /// Po uploadzie zatwierdź w Saldeo nieoznaczone dokumenty KSeF (z limitem --max-approve).
        #[arg(long)]
        approve: bool,
        /// Przy --approve: gdy dokumentów do zatwierdzenia jest więcej, nie zatwierdzaj żadnego (0 = bez limitu).
        #[arg(long, default_value_t = DEFAULT_MAX_APPROVE)]
        max_approve: usize,
        /// Przy --approve: zatwierdzaj tylko dokumenty z fakturą z Gmaila (też LAB_APPROVE_REQUIRE_MAIL=1).
        #[arg(long)]
        require_mail: bool,
    },
    /// Uzupełnia lokalne dane Saldeo z KSeF/Gmail i zgłasza duplikaty.
    Repair {
        /// Rok rozliczeniowy (domyślnie bieżący).
        #[arg(long, default_value_t = default_year())]
        year: i32,
        /// Minimalny score dopasowania (50–100).
        #[arg(
            long,
            default_value_t = DEFAULT_REVIEW_SCORE,
            value_parser = clap::value_parser!(u8).range(MIN_REVIEW_SCORE as i64..=MAX_REVIEW_SCORE as i64)
        )]
        review_score: u8,
        /// Dodatkowo odczytaj brakujące PDF-y przez LLM/OpenRouter.
        #[arg(long)]
        llm: bool,
        /// Zapisz poprawki Saldeo w SQLite. Bez tej flagi zwraca tylko plan.
        #[arg(long)]
        confirm: bool,
        /// Plik JSON z wynikiem.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Zatwierdza w Saldeo nieoznaczone dokumenty KSeF (domyślnie najwyżej 50 naraz).
    Approve {
        /// Rok rozliczeniowy (domyślnie bieżący).
        #[arg(long, default_value_t = default_year())]
        year: i32,
        /// Minimalny score dopasowania (50–100) przy budowie listy dokumentów.
        #[arg(
            long,
            default_value_t = DEFAULT_REVIEW_SCORE,
            value_parser = clap::value_parser!(u8).range(MIN_REVIEW_SCORE as i64..=MAX_REVIEW_SCORE as i64)
        )]
        review_score: u8,
        /// Wykonaj zatwierdzenie w Saldeo. Bez tej flagi zwraca tylko plan.
        #[arg(long)]
        confirm: bool,
        /// Gdy dokumentów do zatwierdzenia jest więcej, nie zatwierdzaj żadnego (0 = bez limitu).
        #[arg(long, default_value_t = DEFAULT_MAX_APPROVE)]
        max_approve: usize,
        /// Zatwierdzaj tylko dokumenty z fakturą z Gmaila (też LAB_APPROVE_REQUIRE_MAIL=1).
        #[arg(long)]
        require_mail: bool,
        /// Plik JSON z wynikiem.
        #[arg(long)]
        output: Option<PathBuf>,
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
