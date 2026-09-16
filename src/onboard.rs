use crate::*;

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
    let python_ok = Command::new("python3")
        .arg("-c")
        .arg("import shutil, subprocess, sys; pp=shutil.which('ppmlx'); sys.exit(1 if not pp else 0)")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let openssl_ok = local_tool("openssl").is_ok();

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
    })
}

/// Raport o sekretach: czy są ustawione i skąd pochodzą; bez wartości.
pub(crate) fn secrets_status_json(status: &OnboardStatus) -> Value {
    serde_json::json!({
        "gmail_token": secret_status_entry(status.gmail_token_source),
        "saldeo_storage_state": secret_status_entry(status.saldeo_state_source),
        "saldeo_username": secret_status_entry(secret_source(Secret::SaldeoUsername)),
        "saldeo_password": secret_status_entry(secret_source(Secret::SaldeoPassword)),
        "ksef_token": secret_status_entry(status.ksef_token_source),
        "ksef_cert_password": secret_status_entry(status.ksef_password_source),
        "ksef_access_token": secret_status_entry(status.ksef_access_source),
        "openrouter_api_key": secret_status_entry(secret_source(Secret::OpenRouterApiKey)),
    })
}

fn secret_status_entry(source: SecretSource) -> Value {
    serde_json::json!({ "set": source.is_set(), "source": source.as_str() })
}

/// Skąd LAB wziął sekret; do wydruku statusu, nigdy z wartością.
pub(crate) fn secret_source_suffix(source: SecretSource) -> String {
    match source {
        SecretSource::Env => " (zmienna sesji)".to_string(),
        SecretSource::Keychain => " (Keychain)".to_string(),
        SecretSource::File => " (plik 600)".to_string(),
        SecretSource::Missing => String::new(),
    }
}

pub(crate) fn print_onboard_status(status: &OnboardStatus, db_path: &Path) {
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
        secret_source_suffix(status.gmail_token_source)
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
        secret_source_suffix(status.saldeo_state_source)
    );
    let saldeo_login =
        secret_is_set(Secret::SaldeoUsername) && secret_is_set(Secret::SaldeoPassword);
    eprintln!(
        "  Saldeo login:    {}{}",
        if saldeo_login { "✓" } else { "✗" },
        if saldeo_login {
            secret_source_suffix(secret_source(Secret::SaldeoUsername))
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
        secret_source_suffix(status.ksef_password_source)
    );
    eprintln!(
        "  KSeF token:      {}{}",
        if status.ksef_token_ok { "✓" } else { "✗" },
        secret_source_suffix(status.ksef_token_source)
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
        steps.push(
            "Ustaw SALDEO_USERNAME i SALDEO_PASSWORD w lab onboard, albo odśwież sesję Helium",
        );
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
            save_secret(Secret::SaldeoUsername, username.trim())?;
            let password = Password::new()
                .with_prompt("SALDEO_PASSWORD")
                .allow_empty_password(true)
                .interact()?;
            if password.is_empty() {
                eprintln!("⏭ Hasło puste; login zapisany, hasło bez zmian.\n");
            } else {
                save_secret(Secret::SaldeoPassword, &password)?;
                eprintln!("✓ Zapisano SALDEO_USERNAME i SALDEO_PASSWORD w Keychain\n");
            }
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

pub(crate) fn onboard_edit_env_path(name: &str) -> Result<()> {
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
    match save_secret(secret, value.trim())? {
        SecretSource::File => eprintln!("✓ Zapisano {name} w pliku 0600\n"),
        _ => eprintln!("✓ Zapisano {name} w macOS Keychain (lab-cli)\n"),
    }
    Ok(())
}

pub(crate) fn ensure_saldeo_session_or_auth(progress: Option<Arc<Mutex<String>>>) -> Result<()> {
    let storage_state = default_saldeo_storage_state_path();
    if let Some(progress) = &progress {
        set_progress(progress, "Saldeo: sprawdzam zapisane cookies...");
    }
    if saldeo_session_valid(&storage_state) {
        return Ok(());
    }

    if let Some(progress) = &progress {
        let message = if saldeo_login_pair()?.is_some() {
            "Saldeo: sesja nieważna — loguję zapisanym hasłem..."
        } else {
            "Saldeo: sesja nieważna — zaloguj się w Helium; zapiszę auth automatycznie..."
        };
        set_progress(progress, message);
    }
    saldeo_auth_noninteractive()?;

    let storage_state = default_saldeo_storage_state_path();
    if let Some(progress) = &progress {
        set_progress(progress, "Saldeo: sprawdzam nowe cookies...");
    }
    if saldeo_session_valid(&storage_state) {
        Ok(())
    } else {
        Err(anyhow!(
            "Saldeo auth nie jest jeszcze poprawny; ustaw SALDEO_USERNAME i SALDEO_PASSWORD albo zaloguj się w Helium"
        ))
    }
}

pub(crate) fn saldeo_auth_noninteractive() -> Result<()> {
    let target = preferred_saldeo_storage_state_path();
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
    let login = saldeo_login_pair()?;
    let login_path = if let Some((username, password)) = &login {
        let path =
            std::env::temp_dir().join(format!("lab-saldeo-login-{}.json", std::process::id()));
        write_private_file(
            &path,
            &serde_json::to_vec(&serde_json::json!({
                "username": username,
                "password": password
            }))?,
        )?;
        Some(path)
    } else {
        None
    };
    let script_path =
        std::env::temp_dir().join(format!("lab-saldeo-login-script-{}.js", std::process::id()));
    write_private_file(
        &script_path,
        include_str!("../scripts/saldeo-login.js").as_bytes(),
    )?;
    let mut command = Command::new("npx");
    command
        .arg("--yes")
        .arg("-p")
        .arg("playwright")
        .arg("node")
        .arg(&script_path)
        .env("LAB_SALDEO_STORAGE_STATE", &target)
        .env("SALDEO_URL", &url)
        .env("HELIUM_EXECUTABLE", &helium)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    if let Some(path) = &login_path {
        command.env("LAB_SALDEO_LOGIN_FILE", path);
    }
    let output = command
        .output()
        .context("uruchomienie npx playwright + Helium")?;
    if let Some(path) = &login_path {
        let _ = fs::remove_file(path);
    }
    let _ = fs::remove_file(&script_path);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "Playwright auth zakończył się błędem: {} (stderr: {})",
            output.status,
            stderr.trim()
        ));
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

pub(crate) fn run_saldeo_auth_script() -> Result<()> {
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
    saldeo_auth_noninteractive()?;
    eprintln!("✓ Zapisano Saldeo auth: {}\n", target.display());
    Ok(())
}

pub(crate) fn find_saldeo_auth_script() -> Option<PathBuf> {
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

pub(crate) fn lab_config_var(name: &str) -> Option<String> {
    if let Some(secret) = Secret::from_env_key(name) {
        return secret_value(secret).ok().flatten();
    }
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| read_lab_env_file().ok()?.remove(name))
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

pub(crate) fn read_lab_env_file() -> Result<HashMap<String, String>> {
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

pub(crate) fn write_lab_env_file(vars: &HashMap<String, String>) -> Result<()> {
    let mut vars = vars.clone();
    strip_secret_env_keys(&mut vars)?;
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
    write_private_file(&path, out.as_bytes())
}

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

pub(crate) fn save_saldeo_storage_state_secret(storage_state: &Path) -> Result<()> {
    if !storage_state.is_file() {
        return Ok(());
    }
    // Playwright zapisuje plik z prawami umask; zacieśniamy je przed odczytem.
    crate::hardening::chmod_private(storage_state)?;
    let text = read_secret_file(storage_state, "sesja Saldeo")?;
    if text.trim().is_empty() {
        return Ok(());
    }
    save_secret(Secret::SaldeoStorageState, &text)?;
    Ok(())
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
            "ksef": ksef_records.display().to_string(),
            "ksef_present": ksef_records.exists(),
            "saldeo": saldeo_records.display().to_string(),
            "saldeo_present": saldeo_records.exists()
        },
        "database": {
            "path": db_path.display().to_string(),
            "exists": status.db_exists
        },
        "secrets": secrets_status_json(&status),
        "notes": [
            "GmailFetch wymaga tokenu OAuth z zakresem gmail.readonly.",
            "PDF-y są parsowane przez pdftotext, potem PyMuPDF/pdfplumber/pypdf jako fallback.",
            "lab reconcile bez własnych --ksef/--saldeo pobiera online metadane KSeF i Saldeo przed porównaniem.",
            "KSeF online używa KSEF_TOKEN, KSEF_CONTEXT_NIP/KSEF_NIP i KSEF_BASE_URL/KSEF_ENV; metadane są cache'owane lokalnie w KSEF_DATA_DIR albo data/ksef-<rok>.",
            "Sekrety trzyma macOS Keychain (usługa lab-cli); zmienna środowiskowa sesji ma pierwszeństwo, plik ~/.config/lab/env już ich nie przechowuje."
        ],
        "next_steps": onboard_next_steps(&status, gmail_usable)
    });
    write_json(&status_json, None)
}
