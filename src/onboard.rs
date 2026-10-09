use crate::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone)]
pub(crate) struct OnboardStatus {
    db_exists: bool,
    token_exists: bool,
    gmail_authed: bool,
    saldeo_exists: bool,
    saldeo_valid: bool,
    pdftotext_ok: bool,
    python_ok: bool,
    openssl_ok: bool,
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
    gmail_token_source: SecretSource,
    saldeo_state_source: SecretSource,
    ksef_token_source: SecretSource,
    ksef_password_source: SecretSource,
    ksef_access_source: SecretSource,
    /// Odrzucone kopie narzędzi i katalogi narzędzi zapisywalne dla grupy.
    tool_warnings: Vec<String>,
}

pub(crate) fn onboard(
    db_path: &Path,
    check: bool,
    gmail_client_secret: Option<&Path>,
) -> Result<()> {
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
            7 => onboard_edit_env_secret("OPENROUTER_API_KEY")?,
            8 => onboard_configure_ksef_data(status.year)?,
            9 => {
                open_db(db_path)?;
                eprintln!("✓ Baza gotowa: {}\n", db_path.display());
            }
            10 => run_saldeo_auth_script()?,
            11 => {}
            12 => break,
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
            && self.openssl_ok
            && self.gmail_authed
            && self.saldeo_valid
            && self.ksef_api_ok
            && self.ksef_data_exists
    }
}

pub(crate) fn onboard_menu_items(
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
            "OPENROUTER_API_KEY — {}",
            display_secret_value(secret_is_set(Secret::OpenRouterApiKey))
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

pub(crate) fn display_path_value(value: Option<&str>, exists: bool) -> String {
    match value {
        Some(value) if exists => format!("✓ {value}"),
        Some(value) => format!("✗ {value}"),
        None => "✗ (puste)".to_string(),
    }
}

pub(crate) fn display_secret_value(is_set: bool) -> &'static str {
    if is_set {
        "✓ ********"
    } else {
        "✗ (puste)"
    }
}

pub(crate) fn collect_onboard_status(db_path: &Path) -> Result<OnboardStatus> {
    let token_file = default_gmail_token_path();
    let gmail_token_source = secret_source(Secret::GmailToken);
    let token_exists = gmail_token_source.is_set();
    let gmail_authed = token_exists
        && read_gmail_token(&token_file)
            .map(|t| {
                t.expires_at
                    .map(|exp| exp > Utc::now() + chrono::Duration::seconds(60))
                    .unwrap_or(true)
            })
            .unwrap_or(false);

    let saldeo_state = default_saldeo_storage_state_path();
    let saldeo_state_source = secret_source(Secret::SaldeoStorageState);
    let saldeo_exists = saldeo_state_source.is_set();
    let saldeo_valid = saldeo_exists && saldeo_session_valid(&saldeo_state);

    let pdftotext_ok = local_tool("pdftotext").is_ok();
    // Tylko informacja o obecności ppmlx; żaden proces (np. Python z cwd) nie jest uruchamiany.
    let python_ok = crate::hardening::tool_present_for_status("ppmlx");
    let openssl_ok = local_tool("openssl").is_ok();
    let tool_warnings = crate::hardening::tool_dir_warnings();

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
    let ksef_password_source = secret_source(Secret::KsefCertPassword);
    let ksef_password_ok = ksef_password_source.is_set();
    let ksef_token_source = secret_source(Secret::KsefToken);
    let ksef_token_ok = ksef_token_source.is_set();
    let ksef_access_source = secret_source(Secret::KsefAccessToken);
    let ksef_api_ok = ksef_token_ok;

    Ok(OnboardStatus {
        db_exists,
        token_exists,
        gmail_authed,
        saldeo_exists,
        saldeo_valid,
        pdftotext_ok,
        python_ok,
        openssl_ok,
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
        gmail_token_source,
        saldeo_state_source,
        ksef_token_source,
        ksef_password_source,
        ksef_access_source,
        tool_warnings,
    })
}

/// Raport o sekretach: czy są ustawione i skąd pochodzą; bez wartości.
pub(crate) fn secrets_status_json(status: &OnboardStatus) -> Value {
    serde_json::json!({
        "gmail_token": secret_status_entry(&status.gmail_token_source),
        "saldeo_storage_state": secret_status_entry(&status.saldeo_state_source),
        "saldeo_username": secret_status_entry(&secret_source(Secret::SaldeoUsername)),
        "saldeo_password": secret_status_entry(&secret_source(Secret::SaldeoPassword)),
        "ksef_token": secret_status_entry(&status.ksef_token_source),
        "ksef_cert_password": secret_status_entry(&status.ksef_password_source),
        "ksef_access_token": secret_status_entry(&status.ksef_access_source),
        "openrouter_api_key": secret_status_entry(&secret_source(Secret::OpenRouterApiKey)),
    })
}

fn secret_status_entry(source: &SecretSource) -> Value {
    let mut entry = serde_json::json!({ "set": source.is_set(), "source": source.as_str() });
    if let Some(problem) = source.problem() {
        entry["problem"] = Value::String(problem);
    }
    entry
}

/// Skąd LAB wziął sekret; do wydruku statusu, nigdy z wartością.
pub(crate) fn secret_source_suffix(source: &SecretSource) -> String {
    match source {
        SecretSource::Env => " (zmienna sesji)".to_string(),
        SecretSource::Keychain => " (Keychain)".to_string(),
        SecretSource::File => " (plik 600)".to_string(),
        SecretSource::Missing => String::new(),
        SecretSource::Insecure(path) => format!(" (insecure permissions: {})", path.display()),
    }
}

pub(crate) fn print_onboard_status(status: &OnboardStatus, db_path: &Path) {
    let root = lab_root_info();
    eprintln!(
        "  Katalog LAB:     {} ({})",
        root.path.display(),
        root.source.as_str()
    );
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
        "  OpenRouter:      {}{}",
        display_secret_value(secret_is_set(Secret::OpenRouterApiKey)),
        secret_source_suffix(&secret_source(Secret::OpenRouterApiKey))
    );
    eprintln!(
        "  openssl:         {}",
        if status.openssl_ok {
            "✓"
        } else {
            "✗ (potrzebny do szyfrowania tokenu KSeF)"
        }
    );
    eprintln!(
        "  Gmail:           {}{}",
        if status.gmail_authed {
            "✓"
        } else if status.token_exists {
            "✗ (token wygasł)"
        } else {
            "✗"
        },
        secret_source_suffix(&status.gmail_token_source)
    );
    eprintln!(
        "  Saldeo:          {}{}",
        if status.saldeo_valid {
            "✓"
        } else if status.saldeo_exists {
            "✗ (sesja wygasła)"
        } else {
            "✗"
        },
        secret_source_suffix(&status.saldeo_state_source)
    );
    let saldeo_username = secret_source(Secret::SaldeoUsername);
    let saldeo_password = secret_source(Secret::SaldeoPassword);
    let saldeo_login = saldeo_username.is_set() && saldeo_password.is_set();
    eprintln!(
        "  Saldeo login:    {}{}",
        if saldeo_login { "✓" } else { "✗" },
        if saldeo_login || saldeo_username.problem().is_some() {
            secret_source_suffix(&saldeo_username)
        } else if saldeo_password.problem().is_some() {
            secret_source_suffix(&saldeo_password)
        } else {
            String::new()
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
        "  KSeF hasło:      {}{}",
        if status.ksef_password_ok {
            "✓"
        } else {
            "✗"
        },
        secret_source_suffix(&status.ksef_password_source)
    );
    eprintln!(
        "  KSeF token:      {}{}",
        if status.ksef_token_ok { "✓" } else { "✗" },
        secret_source_suffix(&status.ksef_token_source)
    );
    eprintln!(
        "  KSeF dane:       {}",
        if status.ksef_data_exists {
            format!("✓ ({})", status.ksef_dir.display())
        } else {
            format!("✗ ({})", status.ksef_dir.display())
        }
    );
    print_tool_warnings(&status.tool_warnings);
    eprintln!();
}

fn print_tool_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("  ⚠ {warning}");
    }
}

pub(crate) fn onboard_next_steps(status: &OnboardStatus, gmail_ok: bool) -> Vec<&'static str> {
    let mut steps: Vec<&str> = Vec::new();
    if !status.pdftotext_ok {
        steps.push("brew install poppler");
    }
    if !status.python_ok {
        steps.push("Zainstaluj ppmlx i pobierz model: ppmlx pull gemma-4-e4b");
    }
    if !status.openssl_ok {
        steps.push("Zainstaluj openssl/libressl CLI potrzebny do szyfrowania tokenu KSeF");
    }
    if !gmail_ok {
        steps.push("lab onboard --gmail-client-secret <ścieżka>");
    }
    if !status.saldeo_valid {
        if secret_is_set(Secret::SaldeoUsername) && secret_is_set(Secret::SaldeoPassword) {
            steps.push(
                "Sesja Saldeo wygasła. Odśwież ją: Menu → Saldeo, albo otwórz tabelę LAB — zaloguje Helium zapisanym hasłem",
            );
        } else {
            steps.push(
                "Ustaw SALDEO_USERNAME i SALDEO_PASSWORD w lab onboard, albo odśwież sesję Helium",
            );
        }
    }
    if !status.ksef_api_ok {
        steps.push("Ustaw KSEF_TOKEN z uprawnieniem InvoiceRead");
    }
    if !status.ksef_data_exists {
        steps.push("Uruchom lab sync --ksef, żeby pobrać metadane KSeF online do lokalnego cache");
    }
    if steps.is_empty() {
        steps.push("Wszystko gotowe. Uruchom: lab sync");
    }
    steps
}

pub(crate) fn write_onboard_check_json(status: &OnboardStatus) -> Result<()> {
    let steps = onboard_next_steps(status, status.gmail_authed);
    let status_json = serde_json::json!({
        "prerequisites": { "pdftotext": status.pdftotext_ok, "ppmlx_gemma": status.python_ok, "openssl": status.openssl_ok },
        "gmail": { "token_valid": status.gmail_authed },
        "saldeo": { "session_valid": status.saldeo_valid },
        "ksef": { "api_ok": status.ksef_api_ok, "data_exists": status.ksef_data_exists },
        "database": { "exists": status.db_exists },
        "warnings": status.tool_warnings,
        "secrets": secrets_status_json(status),
        "next_steps": steps
    });
    write_json(&status_json, None)
}

pub(crate) fn onboard_configure_gmail(
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
    // Nieczytelny plik env to błąd, nie pusta mapa: zapis nie może skasować innych kluczy.
    let mut vars = read_lab_env_file()?;
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

pub(crate) fn onboard_configure_saldeo() -> Result<()> {
    eprintln!("── Saldeo ──");
    let login_ok = saldeo_login_pair()?.is_some();
    if Confirm::new()
        .with_prompt(if login_ok {
            "Login Saldeo jest ustawiony. Zmienić login i hasło do automatycznego logowania?"
        } else {
            "Ustawić login i hasło Saldeo do automatycznego logowania (bez ręcznego Helium)?"
        })
        .default(!login_ok)
        .interact()?
    {
        let username: String = Input::new()
            .with_prompt("SALDEO_USERNAME")
            .allow_empty(true)
            .interact_text()?;
        if !username.trim().is_empty() {
            let password = Password::new()
                .with_prompt("SALDEO_PASSWORD")
                .allow_empty_password(true)
                .interact()?;
            eprintln!("{}\n", save_saldeo_login(&username, &password)?);
        }
    }
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
        // Kopia od razu jako plik 600 (bez okna z prawami umask).
        let bytes = fs::read(&source).with_context(|| format!("odczyt {}", source.display()))?;
        write_private_file(&target, &bytes)
            .with_context(|| format!("kopiowanie {} → {}", source.display(), target.display()))?;
    }
    match save_saldeo_storage_state_secret(&target)? {
        Some(saved) => eprintln!("✓ Saldeo storage state zapisany {}\n", saved.describe()),
        None => eprintln!("✗ Saldeo storage state jest pusty: {}\n", target.display()),
    }
    Ok(())
}

/// Zapis loginu Saldeo z onboardingu. Login bez hasła trafia na dysk tylko wtedy, gdy
/// hasło jest już zapisane; inaczej nic nie jest zapisywane. Zwraca komunikat bez wartości.
pub(crate) fn save_saldeo_login(username: &str, password: &str) -> Result<String> {
    let username = strip_one_line_ending(username).trim();
    let password = strip_one_line_ending(password);
    if username.is_empty() {
        return Ok("⏭ SALDEO_USERNAME pusty; bez zmian.".to_string());
    }
    if password.is_empty() {
        if !secret_is_set(Secret::SaldeoPassword) {
            return Ok(
                "⏭ Hasło puste i brak zapisanego SALDEO_PASSWORD; SALDEO_USERNAME nie został zapisany (LAB zaloguje ręcznie w Helium)."
                    .to_string(),
            );
        }
        let saved = save_secret(Secret::SaldeoUsername, username)?;
        return Ok(format!(
            "⏭ Hasło puste; SALDEO_USERNAME zapisany {}, hasło bez zmian.",
            saved.describe()
        ));
    }
    // Najpierw sprawdzamy oba, potem zapis: błąd hasła nie zostawia samego loginu.
    ensure_single_line_value("SALDEO_USERNAME", username)?;
    ensure_single_line_value("SALDEO_PASSWORD", password)?;
    save_secret(Secret::SaldeoPassword, password)?;
    let saved = save_secret(Secret::SaldeoUsername, username)?;
    Ok(format!(
        "✓ Zapisano SALDEO_USERNAME i SALDEO_PASSWORD {}",
        saved.describe()
    ))
}

pub(crate) fn onboard_edit_env_path(name: &str) -> Result<()> {
    if let Some(value) = prompt_env_path(name, lab_config_var(name))? {
        let mut vars = read_lab_env_file()?;
        vars.insert(name.to_string(), value);
        write_lab_env_file(&vars)?;
        eprintln!("✓ Zapisano {name} w {}\n", lab_env_file_path().display());
    } else {
        eprintln!("⏭ Bez zmian.\n");
    }
    Ok(())
}

pub(crate) fn onboard_edit_env_secret(name: &str) -> Result<()> {
    let secret = Secret::from_env_key(name)
        .ok_or_else(|| anyhow!("{name} nie jest sekretem LAB; użyj edycji ścieżki"))?;
    let current = secret_is_set(secret);
    let prompt = if current {
        format!("{name} (ustawione; puste = bez zmian)")
    } else {
        format!("{name} (puste = bez zmian)")
    };
    let value = Password::new()
        .with_prompt(prompt)
        .allow_empty_password(true)
        .interact()?;
    if value.trim().is_empty() {
        eprintln!("⏭ Bez zmian.\n");
        return Ok(());
    }
    let saved = save_secret(secret, value.trim())?;
    eprintln!("✓ Zapisano {name} {}\n", saved.describe());
    Ok(())
}

pub(crate) fn ensure_saldeo_session_or_auth(progress: Option<Arc<Mutex<String>>>) -> Result<()> {
    ensure_saldeo_session_with(
        progress,
        lab_noninteractive(),
        || saldeo_session_valid(&default_saldeo_storage_state_path()),
        saldeo_auth_noninteractive,
    )
}

/// Komunikat dla przebiegu bez użytkownika; `scripts/lab-automation.sh` szuka frazy
/// „Saldeo session expired”, więc nie zmieniaj jej.
pub(crate) const SALDEO_SESSION_EXPIRED_UNATTENDED: &str = "Saldeo session expired: przy LAB_NONINTERACTIVE=1 LAB nie otwiera Helium i nie instaluje Playwright. Odśwież sesję interaktywnie: lab onboard (Saldeo auth)";

/// Przy `LAB_NONINTERACTIVE=1` błąd zamiast logowania w przeglądarce i `npm install`.
fn refuse_unattended_saldeo_login() -> Result<()> {
    if lab_noninteractive() {
        return Err(anyhow!(SALDEO_SESSION_EXPIRED_UNATTENDED));
    }
    Ok(())
}

/// Logika `ensure_saldeo_session_or_auth` z wstrzykniętym sprawdzeniem sesji i logowaniem.
pub(crate) fn ensure_saldeo_session_with(
    progress: Option<Arc<Mutex<String>>>,
    noninteractive: bool,
    session_valid: impl Fn() -> bool,
    run_auth: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if let Some(progress) = &progress {
        set_progress(progress, "Saldeo: sprawdzam zapisane cookies...");
    }
    if session_valid() {
        return Ok(());
    }
    if noninteractive {
        return Err(anyhow!(SALDEO_SESSION_EXPIRED_UNATTENDED));
    }

    if let Some(progress) = &progress {
        let message = if saldeo_login_pair()?.is_some() {
            "Saldeo: sesja nieważna — loguję zapisanym hasłem..."
        } else {
            "Saldeo: sesja nieważna — zaloguj się w Helium; zapiszę auth automatycznie..."
        };
        set_progress(progress, message);
    }
    run_auth()?;

    if let Some(progress) = &progress {
        set_progress(progress, "Saldeo: sprawdzam nowe cookies...");
    }
    if session_valid() {
        Ok(())
    } else {
        Err(anyhow!(
            "Saldeo auth nie jest jeszcze poprawny; ustaw SALDEO_USERNAME i SALDEO_PASSWORD albo zaloguj się w Helium"
        ))
    }
}

struct SaldeoAuthTempFile {
    path: PathBuf,
}

impl SaldeoAuthTempFile {
    fn path_for(name: &str, extension: &str) -> PathBuf {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "lab-saldeo-{name}-{}-{id}.{extension}",
            std::process::id()
        ))
    }

    fn write(path: PathBuf, bytes: &[u8]) -> Result<Self> {
        use std::io::Write;

        Self::write_with(path, |file| file.write_all(bytes))
    }

    fn write_with(
        path: PathBuf,
        write: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
    ) -> Result<Self> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("utworzenie {}", path.display()))?;
        // Own the path before writing, so a partial write is also cleaned up.
        let temporary = Self { path };
        // Close before cleanup, even on panic or platforms that cannot unlink open files.
        let result = {
            let mut file = file;
            write(&mut file)
        };
        result.with_context(|| format!("zapis {}", temporary.path.display()))?;
        Ok(temporary)
    }
}

impl Drop for SaldeoAuthTempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn saldeo_auth_noninteractive() -> Result<()> {
    // Przed czymkolwiek innym: bez katalogów, bez npm, bez przeglądarki.
    refuse_unattended_saldeo_login()?;
    let target = preferred_saldeo_storage_state_path();
    if let Some(parent) = target.parent() {
        crate::hardening::create_dir_for_private_files(parent)?;
    }
    let url =
        std::env::var("SALDEO_URL").unwrap_or_else(|_| "https://saldeo.brainshare.pl/".to_string());
    let helium = std::env::var("HELIUM_EXECUTABLE")
        .unwrap_or_else(|_| "/Applications/Helium.app/Contents/MacOS/Helium".to_string());
    if !Path::new(&helium).is_file() {
        return Err(anyhow!("nie znalazłem Helium executable: {helium}"));
    }
    let login = saldeo_login_pair()?;
    // Installation can fail or take a long time; do not leave credentials on disk yet.
    let node_path = ensure_playwright_node_path()?;
    let login_file = write_saldeo_login_file(login.as_ref())?;
    let script_file = SaldeoAuthTempFile::write(
        SaldeoAuthTempFile::path_for("login-script", "js"),
        include_str!("../scripts/saldeo-login.js").as_bytes(),
    )?;
    let mut command = saldeo_login_node_command(&SaldeoLoginNodeArgs {
        script: &script_file.path,
        node_path: &node_path,
        target: &target,
        url: &url,
        helium: &helium,
        timeout_ms: saldeo_auth_timeout_ms(),
        login_file: login_file.as_ref().map(|file| file.path.as_path()),
    });
    let status = command_status_with_timeout(&mut command, saldeo_auth_process_timeout())
        .context("uruchomienie node + Playwright + Helium")?;
    drop(login_file);
    drop(script_file);
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
    Ok(())
}

pub(crate) struct SaldeoLoginNodeArgs<'a> {
    pub(crate) script: &'a Path,
    pub(crate) node_path: &'a Path,
    pub(crate) target: &'a Path,
    pub(crate) url: &'a str,
    pub(crate) helium: &'a str,
    pub(crate) timeout_ms: u64,
    pub(crate) login_file: Option<&'a Path>,
}

/// `node saldeo-login.js`: hasło tylko w pliku 0600 (`LAB_SALDEO_LOGIN_FILE`),
/// sekrety LAB usunięte ze środowiska.
pub(crate) fn saldeo_login_node_command(args: &SaldeoLoginNodeArgs<'_>) -> Command {
    let mut command = Command::new("node");
    crate::hardening::remove_secret_env(&mut command);
    command
        .arg(args.script)
        .env("NODE_PATH", args.node_path)
        .env("LAB_SALDEO_STORAGE_STATE", args.target)
        .env("SALDEO_URL", args.url)
        .env("HELIUM_EXECUTABLE", args.helium)
        .env("SALDEO_AUTH_TIMEOUT_MS", args.timeout_ms.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    match args.login_file {
        Some(path) => command.env("LAB_SALDEO_LOGIN_FILE", path),
        None => command.env_remove("LAB_SALDEO_LOGIN_FILE"),
    };
    command
}

pub(crate) fn saldeo_auth_timeout_ms() -> u64 {
    std::env::var("SALDEO_AUTH_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|ms| *ms >= 5_000)
        .unwrap_or(180_000)
}

pub(crate) fn saldeo_auth_process_timeout() -> Duration {
    Duration::from_millis(saldeo_auth_timeout_ms().saturating_add(20_000))
}

fn command_status_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<std::process::ExitStatus> {
    let mut child = command.spawn().context("spawn procesu logowania Saldeo")?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(status),
            None if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(200)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow!(
                    "logowanie Saldeo przekroczyło limit {}s",
                    timeout.as_secs()
                ));
            }
        }
    }
}

pub(crate) fn run_saldeo_auth_script() -> Result<()> {
    eprintln!("── Saldeo auth ──");
    refuse_unattended_saldeo_login()?;
    let target = preferred_saldeo_storage_state_path();
    let script = find_saldeo_auth_script()?;
    let prompt = match &script {
        Some(script) => format!(
            "Uruchomić skrypt {} i zapisać auth do {}?",
            script.display(),
            target.display()
        ),
        None => format!(
            "Brak zaufanego scripts/saldeo-auth.sh. Uruchomić wbudowany Playwright + Helium i zapisać auth do {}?",
            target.display()
        ),
    };
    if !Confirm::new()
        .with_prompt(prompt)
        .default(true)
        .interact()?
    {
        eprintln!("⏭ Pominięto.\n");
        return Ok(());
    }

    if let Some(script) = script {
        // Hasło idzie plikiem 0600 (LAB_SALDEO_LOGIN_FILE), nie przez środowisko skryptu.
        let login_file = write_saldeo_login_file(saldeo_login_pair()?.as_ref())?;
        let status = saldeo_auth_script_command(
            &script,
            &target,
            login_file.as_ref().map(|file| file.path.as_path()),
        )
        .status()
        .with_context(|| format!("uruchomienie {}", script.display()))?;
        drop(login_file);
        if !status.success() {
            return Err(anyhow!("skrypt Saldeo auth zakończył się błędem: {status}"));
        }
        save_saldeo_storage_state_secret(&target)?;
        return Ok(());
    }

    saldeo_auth_noninteractive()?;
    eprintln!("✓ Zapisano Saldeo auth: {}\n", target.display());
    Ok(())
}

/// Plik 0600 z loginem Saldeo dla `saldeo-login.js`; usuwany w Drop.
fn write_saldeo_login_file(login: Option<&(String, String)>) -> Result<Option<SaldeoAuthTempFile>> {
    let Some((username, password)) = login else {
        return Ok(None);
    };
    SaldeoAuthTempFile::write(
        SaldeoAuthTempFile::path_for("login", "json"),
        &serde_json::to_vec(&serde_json::json!({
            "username": username,
            "password": password
        }))?,
    )
    .map(Some)
}

/// Skrypt `saldeo-auth.sh` bez sekretów LAB w środowisku; login tylko przez plik 0600.
pub(crate) fn saldeo_auth_script_command(
    script: &Path,
    target: &Path,
    login_file: Option<&Path>,
) -> Command {
    let mut command = Command::new(script);
    crate::hardening::remove_secret_env(&mut command);
    command
        .arg(target)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    match login_file {
        Some(path) => command.env("LAB_SALDEO_LOGIN_FILE", path),
        None => command.env_remove("LAB_SALDEO_LOGIN_FILE"),
    };
    command
}

fn lab_playwright_prefix() -> PathBuf {
    if let Some(path) = std::env::var_os("LAB_PLAYWRIGHT_PREFIX") {
        return PathBuf::from(path);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("lab")
        .join("playwright")
}

/// Wersja Playwright instalowana dla logowania Saldeo (ta sama w `scripts/saldeo-auth.sh`).
pub(crate) const PLAYWRIGHT_VERSION: &str = "1.63.0";

/// `LAB_PLAYWRIGHT_VERSION` albo [`PLAYWRIGHT_VERSION`]; dozwolone litery, cyfry, `.` i `-`
/// (wersja albo dist-tag), żeby wartość nie stała się innym specyfikatorem npm.
pub(crate) fn playwright_version_from(value: Option<&str>) -> Result<String> {
    // Jak `${LAB_PLAYWRIGHT_VERSION:-…}` w skrypcie: pusta wartość = domyślna.
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return Ok(PLAYWRIGHT_VERSION.to_string());
    };
    let valid = value.len() <= 64
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !valid {
        return Err(anyhow!(
            "LAB_PLAYWRIGHT_VERSION: oczekuję wersji albo dist-tagu (litery, cyfry, '.', '-'), dostałem {value:?}"
        ));
    }
    Ok(value.to_string())
}

fn ensure_playwright_node_path() -> Result<PathBuf> {
    refuse_unattended_saldeo_login()?;
    let prefix = lab_playwright_prefix();
    let node_modules = prefix.join("node_modules");
    let module = node_modules.join("playwright");
    // Zainstalowana wersja zostaje, nawet jeśli inna niż przypięta: bez wymuszonej reinstalacji.
    if module.is_dir() {
        return Ok(node_modules);
    }
    let version = playwright_version_from(std::env::var("LAB_PLAYWRIGHT_VERSION").ok().as_deref())?;
    crate::hardening::create_dir_for_private_files(&prefix)?;
    eprintln!(
        "  [Saldeo] instaluję Playwright {version} w {} (bez przeglądarki Playwright)...",
        prefix.display()
    );
    let status = npm_install_playwright_command(&prefix, &version)
        .status()
        .context("npm install playwright")?;
    if !status.success() {
        return Err(anyhow!(
            "npm install playwright zakończył się błędem: {status}"
        ));
    }
    if !module.is_dir() {
        return Err(anyhow!("brak modułu playwright w {}", module.display()));
    }
    Ok(node_modules)
}

/// `npm install playwright@<version>` bez skryptów pakietów i bez sekretów LAB w środowisku.
pub(crate) fn npm_install_playwright_command(prefix: &Path, version: &str) -> Command {
    // stdout bywa strumieniem JSON-RPC serwera MCP: wyjście npm idzie na stderr,
    // a npm nie czyta stdin.
    let mut command = Command::new("npm");
    crate::hardening::remove_secret_env(&mut command);
    command
        .arg("install")
        .arg("--prefix")
        .arg(prefix)
        .arg("--no-fund")
        .arg("--no-audit")
        .arg("--ignore-scripts")
        .arg(format!("playwright@{version}"))
        .env("PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .stderr(Stdio::inherit());
    command
}

/// Skrypt `saldeo-auth.sh` tylko z zaufanych miejsc: `SALDEO_AUTH_SCRIPT` (ścieżka
/// bezwzględna), drzewo katalogów uruchomionego pliku (po rozwinięciu dowiązań) albo
/// bieżący katalog, gdy jest pakietem `lab-cli`. Nigdy przodkowie bieżącego katalogu.
pub(crate) fn find_saldeo_auth_script() -> Result<Option<PathBuf>> {
    let explicit = std::env::var_os("SALDEO_AUTH_SCRIPT").filter(|value| !value.is_empty());
    let exe = std::env::current_exe().ok();
    let cwd = std::env::current_dir().ok();
    find_saldeo_auth_script_in(
        explicit.as_deref().map(Path::new),
        exe.as_deref(),
        cwd.as_deref(),
        crate::hardening::current_uid(),
    )
}

/// Jawna ścieżka, która nie przechodzi kontroli, to błąd; znaleziona automatycznie jest
/// pomijana z komunikatem na stderr.
pub(crate) fn find_saldeo_auth_script_in(
    explicit: Option<&Path>,
    exe: Option<&Path>,
    cwd: Option<&Path>,
    uid: u32,
) -> Result<Option<PathBuf>> {
    if let Some(path) = explicit {
        if !path.is_absolute() {
            return Err(anyhow!(
                "SALDEO_AUTH_SCRIPT wymaga ścieżki bezwzględnej: {}",
                path.display()
            ));
        }
        return crate::hardening::trusted_user_script(path, uid)
            .map(Some)
            .context("SALDEO_AUTH_SCRIPT");
    }
    let mut roots = Vec::new();
    if let Some(exe) = exe.and_then(|exe| fs::canonicalize(exe).ok())
        && let Some(parent) = exe.parent()
    {
        roots.extend(parent.ancestors().map(Path::to_path_buf));
    }
    if let Some(cwd) = cwd
        && crate::credentials::cwd_is_lab_package(cwd)
    {
        roots.push(cwd.to_path_buf());
    }
    for root in roots {
        let script = root.join("scripts").join("saldeo-auth.sh");
        if !script.is_file() {
            continue;
        }
        match crate::hardening::trusted_user_script(&script, uid) {
            Ok(real) => return Ok(Some(real)),
            Err(err) => warn_once(
                format!("saldeo-auth-script:{}", script.display()),
                &format!("pomijam {}: {err:#}", script.display()),
            ),
        }
    }
    Ok(None)
}

pub(crate) fn prompt_env_path(name: &str, current: Option<String>) -> Result<Option<String>> {
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

pub(crate) fn onboard_configure_ksef_data(current_year: i32) -> Result<()> {
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
    let mut vars = read_lab_env_file()?;
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

pub(crate) fn lab_config_var(name: &str) -> Option<String> {
    if let Some(secret) = Secret::from_env_key(name) {
        return secret_value(secret).ok().flatten();
    }
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| crate::dotenv_secret(name))
        .or_else(|| lab_env_file_value(name))
}

/// Zwykły odczyt z `~/.config/lab/env`; problem z plikiem = brak wartości i komunikat na stderr.
fn lab_env_file_value(name: &str) -> Option<String> {
    match read_lab_env_file() {
        Ok(mut vars) => vars.remove(name),
        Err(err) => {
            crate::warn_unreadable_secret_file(&lab_env_file_path(), &err);
            None
        }
    }
}

pub(crate) fn preferred_saldeo_storage_state_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("lab")
        .join("saldeo-storage-state.json")
}

pub(crate) fn lab_env_file_path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = credentials::test_env_file_path() {
        return path;
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("lab")
        .join("env")
}

/// Brak pliku = pusta mapa. Prawa szersze niż 600, błąd odczytu albo tekst spoza UTF-8
/// to błąd ze ścieżką; wywołujący odczyt-modyfikację-zapis nie może wtedy nic zapisać.
pub(crate) fn read_lab_env_file() -> Result<HashMap<String, String>> {
    crate::read_private_env_file(&lab_env_file_path(), "plik env LAB")
}

pub(crate) fn write_lab_env_file(vars: &HashMap<String, String>) -> Result<()> {
    let mut vars = vars.clone();
    strip_secret_env_keys(&mut vars)?;
    let path = lab_env_file_path();
    let mut keys = vars.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    let mut out = String::from(
        "# LAB local environment. Source it with: set -a; source ~/.config/lab/env; set +a\n",
    );
    for key in keys {
        if let Some(value) = vars.get(&key) {
            ensure_single_line_value(&key, value)?;
            out.push_str(&format!("{}={}\n", key, quote_env_value(value)));
        }
    }
    write_private_file(&path, out.as_bytes())
}

/// Wartość w pojedynczych cudzysłowach; `'` jako `'\''`. Odwrotność: `unquote_env_value`
/// (przez `parse_env_text`) dla każdej wartości bez znaku nowej linii i NUL.
pub(crate) fn quote_env_value(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn unquote_env_value(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].replace("'\\''", "'")
    } else if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

/// `None`, gdy pliku nie ma albo jest pusty; inaczej miejsce zapisu.
pub(crate) fn save_saldeo_storage_state_secret(
    storage_state: &Path,
) -> Result<Option<SavedSecret>> {
    if !storage_state.is_file() {
        return Ok(None);
    }
    // Playwright zapisuje plik z prawami umask; zacieśniamy je przed odczytem.
    crate::hardening::chmod_private(storage_state)?;
    let text = read_secret_file(storage_state, "sesja Saldeo")?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    save_secret(Secret::SaldeoStorageState, &text).map(Some)
}

pub(crate) fn read_saldeo_storage_state(storage_state: &Path) -> Result<String> {
    if let Some(text) = secret_value(Secret::SaldeoStorageState).ok().flatten() {
        return Ok(text);
    }
    read_secret_file(storage_state, "sesja Saldeo")
}

pub(crate) fn saldeo_session_valid(storage_state: &Path) -> bool {
    let Ok(text) = read_saldeo_storage_state(storage_state) else {
        return false;
    };
    // Ciasteczka i XSRF wybrane dla URL-a kontroli wg reguł domena/ścieżka/secure.
    let Ok(session) = saldeo_session_from_storage_text(&text) else {
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
    // Przejściowe błędy (timeout, połączenie, 502/503/504) są ponawiane, żeby jedna czkawka
    // serwera nie wyglądała jak wygasła sesja; 401/403 kończą od razu.
    let response = saldeo_read_with_retry("kontrola sesji", std::thread::sleep, || {
        let request = session
            .authorize(
                client.post(SALDEO_DOCUMENT_SEARCH_URL),
                SALDEO_DOCUMENT_SEARCH_URL,
            )
            .header("saldeoApp", "angularApp")
            .header("timeout", "60000")
            .json(&body);
        let response = request.send().map_err(SaldeoReadError::from_reqwest)?;
        let status = response.status().as_u16();
        if matches!(status, 502..=504) {
            return Err(SaldeoReadError::new(
                SaldeoReadFailure::Status(status),
                anyhow!("HTTP {status}"),
            ));
        }
        let text = response.text().map_err(SaldeoReadError::from_reqwest)?;
        Ok((status, text))
    });
    match response {
        Ok((status, text)) => saldeo_session_response_valid(status, &text),
        Err(_) => false,
    }
}

/// Notatka `doctor` o tym, gdzie faktycznie leżą sekrety; bez wartości.
pub(crate) fn secrets_storage_note() -> String {
    let dotenv = crate::lab_dotenv_path();
    if keychain_enabled() {
        format!(
            "Sekrety tekstowe LAB zapisuje w {} (plik 600) i kopiuje do macOS Keychain (usługa lab-cli, LAB_USE_KEYCHAIN=1); wartości z Keychain nie są kopiowane do tego pliku. Kolejność odczytu: zmienna sesji → ten plik → plik 600 → Keychain. Plik ~/.config/lab/env nie przechowuje sekretów.",
            dotenv.display()
        )
    } else {
        format!(
            "Sekrety tekstowe są w {} (plik 600); Keychain jest wyłączony (LAB_USE_KEYCHAIN=1 go włącza). Zmienna sesji ma pierwszeństwo; plik ~/.config/lab/env nie przechowuje sekretów.",
            dotenv.display()
        )
    }
}

pub(crate) fn doctor(db_path: &Path, token_env: &str) -> Result<()> {
    let gmail_env_present = std::env::var(token_env)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let status = collect_onboard_status(db_path)?;
    let gmail_usable = gmail_env_present || status.gmail_authed;
    let all_ok = status.pdftotext_ok
        && status.python_ok
        && status.openssl_ok
        && gmail_usable
        && status.saldeo_valid
        && status.ksef_api_ok
        && status.ksef_data_exists;
    let year = status.year;
    let mail_candidates = default_mail_candidates_path(year);
    let amazon_mail_candidates = default_amazon_mail_candidates_path(year);
    let lab_root = lab_root_info();
    let ksef_records = configured_ksef_out_path(year);
    let saldeo_records = default_saldeo_records_path(year);
    let ksef_context_type =
        lab_config_var("KSEF_CONTEXT_TYPE").unwrap_or_else(|| "Nip".to_string());
    let raw_ksef_context = lab_config_var("KSEF_CONTEXT_NIP")
        .or_else(|| lab_config_var("KSEF_NIP"))
        .unwrap_or_else(|| DEFAULT_PRODUCTMESH_NIP.to_string());
    let ksef_context_value = if ksef_context_type.eq_ignore_ascii_case("Nip") {
        normalize_tax_id(&raw_ksef_context).unwrap_or(raw_ksef_context)
    } else {
        raw_ksef_context
    };
    let ksef_access_token_path = default_ksef_access_token_path();
    let status_json = serde_json::json!({
        "ok": all_ok,
        "year": year,
        "prerequisites": {
            "pdftotext_present": status.pdftotext_ok,
            "ppmlx_present": status.python_ok,
            "openssl_present": status.openssl_ok
        },
        "warnings": status.tool_warnings,
        "gmail": {
            "token_env": token_env,
            "token_env_present": gmail_env_present,
            "token_file": default_gmail_token_path().display().to_string(),
            "token_file_or_keychain_present": status.token_exists,
            "token_source": status.gmail_token_source.as_str(),
            "token_file_valid": status.gmail_authed,
            "usable": gmail_usable,
            "client_secret_path": lab_config_var("GOOGLE_CLIENT_SECRET_PATH")
        },
        "saldeo": {
            "storage_state": default_saldeo_storage_state_path().display().to_string(),
            "storage_state_present": status.saldeo_exists,
            "storage_state_source": status.saldeo_state_source.as_str(),
            "session_valid": status.saldeo_valid,
            "default_records": saldeo_records.display().to_string(),
            "default_records_present": saldeo_records.exists()
        },
        "ksef": {
            "base_url": ksef_base_url(),
            "context_type": ksef_context_type,
            "context_value": ksef_context_value,
            "access_token_cache": ksef_access_token_path.display().to_string(),
            "access_token_cache_present": status.ksef_access_source.is_set(),
            "access_token_source": status.ksef_access_source.as_str(),
            "data_dir": status.ksef_dir.display().to_string(),
            "data_exists": status.ksef_data_exists,
            "default_records": ksef_records.display().to_string(),
            "default_records_present": ksef_records.exists(),
            "cert_path": status.ksef_cert.clone(),
            "cert_ok": status.ksef_cert_ok,
            "key_path": status.ksef_key.clone(),
            "key_ok": status.ksef_key_ok,
            "password_present": status.ksef_password_ok,
            "password_source": status.ksef_password_source.as_str(),
            "token_present": status.ksef_token_ok,
            "token_source": status.ksef_token_source.as_str(),
            "api_config_ok": status.ksef_api_ok
        },
        "reconcile_defaults": {
            "mail_candidates": mail_candidates.display().to_string(),
            "mail_candidates_present": mail_candidates.exists(),
            "amazon_mail_candidates": amazon_mail_candidates.display().to_string(),
            "amazon_mail_candidates_present": amazon_mail_candidates.exists(),
            "mail_candidates_records": load_default_mail_candidates(year).ok().map(|records| records.len()),
            "ksef": ksef_records.display().to_string(),
            "ksef_present": ksef_records.exists(),
            "saldeo": saldeo_records.display().to_string(),
            "saldeo_present": saldeo_records.exists()
        },
        "database": {
            "path": db_path.display().to_string(),
            "exists": status.db_exists,
            "lab_root": lab_root.path.display().to_string(),
            "lab_root_source": lab_root.source.as_str()
        },
        "secrets": secrets_status_json(&status),
        "notes": [
            "GmailFetch wymaga tokenu OAuth z zakresem gmail.readonly.",
            "PDF-y są parsowane przez pdftotext, potem PyMuPDF/pdfplumber/pypdf jako fallback.",
            "lab reconcile bez własnych --ksef/--saldeo pobiera online metadane KSeF i Saldeo przed porównaniem.",
            "KSeF online używa KSEF_TOKEN, KSEF_CONTEXT_NIP/KSEF_NIP i KSEF_BASE_URL/KSEF_ENV; metadane są cache'owane lokalnie w KSEF_DATA_DIR albo data/ksef-<rok>.",
            secrets_storage_note(),
            "Jeśli OPENROUTER_API_KEY jest ustawiony, LAB odczytuje brakujące PDF-y przez google/gemini-3.8-flash ze structured output. Lokalny Gemma zostaje jako zapas."
        ],
        "next_steps": onboard_next_steps(&status, gmail_usable)
    });
    print_tool_warnings(&status.tool_warnings);
    write_json(&status_json, None)
}

#[cfg(test)]
mod playwright_version_tests {
    use super::*;

    #[test]
    fn default_is_pinned_and_override_is_validated() {
        assert_eq!(playwright_version_from(None).unwrap(), "1.63.0");
        assert_eq!(playwright_version_from(Some("")).unwrap(), "1.63.0");
        for ok in ["1.64.0", "1.64.0-beta-1", "next", "latest"] {
            assert_eq!(playwright_version_from(Some(ok)).unwrap(), ok);
        }
        for bad in [
            "^1.63.0",
            "~1.63",
            ">=1.0",
            "1.63.0 || 2",
            " 1.63.0",
            "git+https://example.invalid/x.git",
            "file:../x",
            "-1.0",
            "1.0;rm",
            "1.63.0\n",
        ] {
            let err = playwright_version_from(Some(bad)).unwrap_err().to_string();
            assert!(err.contains("LAB_PLAYWRIGHT_VERSION"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn script_and_binary_pin_the_same_version() {
        let script = include_str!("../scripts/saldeo-auth.sh");
        assert!(
            script.contains(&format!(
                "PLAYWRIGHT_VERSION=\"${{LAB_PLAYWRIGHT_VERSION:-{PLAYWRIGHT_VERSION}}}\""
            )),
            "scripts/saldeo-auth.sh musi przypinać Playwright {PLAYWRIGHT_VERSION}"
        );
        assert!(script.contains("\"playwright@$PLAYWRIGHT_VERSION\""));
        assert!(script.contains("--ignore-scripts"));
        let command = npm_install_playwright_command(Path::new("/tmp/lab-prefix-dummy"), "next");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args.last().map(String::as_str), Some("playwright@next"));
        assert!(args.iter().any(|arg| arg == "--ignore-scripts"));
    }
}

#[cfg(test)]
mod saldeo_auth_temp_file_tests {
    use super::*;

    fn paths() -> (PathBuf, PathBuf) {
        (
            SaldeoAuthTempFile::path_for("test-login", "json"),
            SaldeoAuthTempFile::path_for("test-script", "js"),
        )
    }

    #[test]
    fn cleans_both_files_on_success() -> Result<()> {
        let (login_path, script_path) = paths();
        let result = (|| -> Result<()> {
            let _login = SaldeoAuthTempFile::write(login_path.clone(), b"dummy login")?;
            let _script = SaldeoAuthTempFile::write(script_path.clone(), b"dummy script")?;
            assert_eq!(fs::read(&login_path)?, b"dummy login");
            assert_eq!(fs::read(&script_path)?, b"dummy script");
            Ok(())
        })();
        assert!(!login_path.exists());
        assert!(!script_path.exists());
        result
    }

    #[test]
    fn cleans_both_files_on_early_error() {
        let (login_path, script_path) = paths();
        let result = (|| -> Result<()> {
            let _login = SaldeoAuthTempFile::write(login_path.clone(), b"dummy login")?;
            let _script = SaldeoAuthTempFile::write(script_path.clone(), b"dummy script")?;
            Err(anyhow!("injected process error"))?;
            Ok(())
        })();
        assert!(result.is_err());
        assert!(!login_path.exists());
        assert!(!script_path.exists());
    }

    #[test]
    fn cleans_partial_write_and_previously_created_file() {
        use std::io::Write;

        let (login_path, script_path) = paths();
        let result = (|| -> Result<()> {
            let _login = SaldeoAuthTempFile::write(login_path.clone(), b"dummy login")?;
            let _script = SaldeoAuthTempFile::write_with(script_path.clone(), |file| {
                file.write_all(b"partial dummy script")?;
                Err(std::io::Error::other("injected write failure"))
            })?;
            Ok(())
        })();
        assert!(result.is_err());
        assert!(!login_path.exists());
        assert!(!script_path.exists());
    }

    #[test]
    fn cleans_login_when_script_creation_fails() {
        let (login_path, _) = paths();
        let script_path = login_path.join("script.js");
        let result = (|| -> Result<()> {
            let _login = SaldeoAuthTempFile::write(login_path.clone(), b"dummy login")?;
            // A regular file cannot be the parent directory of the script.
            let _script = SaldeoAuthTempFile::write(script_path.clone(), b"dummy script")?;
            Ok(())
        })();
        assert!(result.is_err());
        assert!(!login_path.exists());
        assert!(!script_path.exists());
    }

    #[test]
    fn cleans_both_files_when_process_spawn_fails() {
        let (login_path, script_path) = paths();
        let result = (|| -> Result<()> {
            let _login = SaldeoAuthTempFile::write(login_path.clone(), b"dummy login")?;
            let _script = SaldeoAuthTempFile::write(script_path.clone(), b"dummy script")?;
            // No process can run: its parent path is a regular file.
            let mut command = Command::new(login_path.join("missing-node"));
            command_status_with_timeout(&mut command, Duration::ZERO)?;
            Ok(())
        })();
        assert!(result.is_err());
        assert!(!login_path.exists());
        assert!(!script_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cleans_both_files_when_process_times_out() {
        let (login_path, script_path) = paths();
        let result = (|| -> Result<()> {
            let _login = SaldeoAuthTempFile::write(login_path.clone(), b"dummy login")?;
            let _script = SaldeoAuthTempFile::write(script_path.clone(), b"dummy script")?;
            // Shell built-ins only: the child cannot exit before the zero timeout.
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "while :; do :; done"]);
            command_status_with_timeout(&mut command, Duration::ZERO)?;
            Ok(())
        })();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("przekroczyło limit")
        );
        assert!(!login_path.exists());
        assert!(!script_path.exists());
    }

    #[test]
    fn cleans_both_files_during_unwinding() {
        let (login_path, script_path) = paths();
        let result = std::panic::catch_unwind(|| {
            let _login = SaldeoAuthTempFile::write(login_path.clone(), b"dummy login").unwrap();
            let _script = SaldeoAuthTempFile::write(script_path.clone(), b"dummy script").unwrap();
            panic!("injected panic");
        });
        assert!(result.is_err());
        assert!(!login_path.exists());
        assert!(!script_path.exists());
    }

    #[test]
    fn does_not_overwrite_or_remove_an_existing_file() -> Result<()> {
        let (path, _) = paths();
        let existing = SaldeoAuthTempFile::write(path.clone(), b"existing dummy data")?;
        assert!(SaldeoAuthTempFile::write(path.clone(), b"replacement").is_err());
        assert_eq!(fs::read(&path)?, b"existing dummy data");
        drop(existing);
        assert!(!path.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn creates_private_files_before_writing() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let (path, _) = paths();
        let file = SaldeoAuthTempFile::write_with(path.clone(), |file| {
            assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
            Ok(())
        })?;
        drop(file);
        assert!(!path.exists());
        Ok(())
    }
}
