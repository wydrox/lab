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
    let openssl_ok = Command::new("openssl")
        .arg("version")
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
    })
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
        steps.push("Odśwież sesję Saldeo (~/.config/lab/saldeo-storage-state.json)");
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

pub(crate) fn ensure_saldeo_session_or_auth(progress: Option<Arc<Mutex<String>>>) -> Result<()> {
    let storage_state = default_saldeo_storage_state_path();
    if let Some(progress) = &progress {
        set_progress(progress, "Saldeo: sprawdzam zapisane cookies...");
    }
    if saldeo_session_valid(&storage_state) {
        return Ok(());
    }

    if let Some(progress) = &progress {
        set_progress(
            progress,
            "Saldeo: sesja nieważna — zaloguj się w Helium; zapiszę auth automatycznie...",
        );
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
            "Saldeo auth nie jest jeszcze poprawny; zaloguj się w Helium i spróbuj ponownie"
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
    let node_script = r#"
const { chromium } = require('playwright');
const fs = require('fs');
const os = require('os');
const path = require('path');

function sleep(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}

function cookieHeader(cookies) {
  return cookies.map(cookie => `${cookie.name}=${cookie.value}`).join('; ');
}

function xsrfToken(cookies) {
  const cookie = cookies.find(cookie => cookie.name === 'X-SALDEO-XSRF-C-TOKEN');
  return cookie && cookie.value;
}

function authCheckBody() {
  return {
    pagination: { pageNumber: 0, pageSize: 1, totalCount: 0,
      columnSorted: { sortColumn: 'DOCUMENT_CREATE_DATE', sortDirection: 'ASC' } },
    filter: { period: { partOfYear: 1, year: new Date().getFullYear(), selectionType: 'selectedMonth' },
      duplicatesEnable: false, duplicates: false, splitPayment: false,
      types: [], contractors: [], stages: [], categories: [], registers: [],
      tags: [], assignUsers: [], addedBy: [], added: [],
      paymentStatuses: [], accountingPaymentTypes: [],
      searchQuery: '', selectKsefDocumentsYesCheckbox: false,
      selectKsefDocumentsNoCheckbox: false, ksefNumber: '',
      ksefMiniWorkflowStatus: null, ksefBoId: null,
      dimensionReportDocumentIds: [], dimensions: null }
  };
}

async function storageAuthenticated(context) {
  const state = await context.storageState();
  const cookies = state.cookies || [];
  const xsrf = xsrfToken(cookies);
  if (!xsrf) return false;
  try {
    const response = await context.request.post('https://saldeo.brainshare.pl/rest/client/document/list/search', {
      headers: {
        Cookie: cookieHeader(cookies),
        'X-SALDEO-XSRF-H-TOKEN': xsrf,
        saldeoApp: 'angularApp',
        timeout: '60000',
      },
      data: authCheckBody(),
    });
    return response.ok();
  } catch (_) {
    return false;
  }
}

(async () => {
  const out = process.env.LAB_SALDEO_STORAGE_STATE;
  const url = process.env.SALDEO_URL || 'https://saldeo.brainshare.pl/';
  const executablePath = process.env.HELIUM_EXECUTABLE;
  const timeoutMs = Number.parseInt(process.env.SALDEO_AUTH_TIMEOUT_MS || '180000', 10);
  const userDataDir = path.join(os.homedir(), '.config', 'lab', 'helium-profile');
  fs.mkdirSync(userDataDir, { recursive: true });
  const context = await chromium.launchPersistentContext(userDataDir, {
    executablePath,
    headless: false,
    viewport: { width: 1400, height: 1000 },
  });
  let closed = false;
  context.once('close', () => { closed = true; });
  const page = context.pages()[0] || await context.newPage();
  await page.goto(url, { waitUntil: 'domcontentloaded' });

  const deadline = Date.now() + timeoutMs;
  let authenticated = false;
  while (!closed && Date.now() < deadline) {
    await context.storageState({ path: out }).catch(() => {});
    if (await storageAuthenticated(context)) {
      authenticated = true;
      break;
    }
    await sleep(2000);
  }

  if (!closed) {
    await context.storageState({ path: out });
    await context.close().catch(() => {});
  }
  if (!authenticated) {
    console.error(`Saldeo auth timeout after ${Math.round(timeoutMs / 1000)}s`);
    process.exit(2);
  }
})().catch(err => { console.error(err && err.stack ? err.stack : err); process.exit(1); });
"#;
    let output = Command::new("npx")
        .arg("--yes")
        .arg("-p")
        .arg("playwright")
        .arg("node")
        .arg("-e")
        .arg(node_script)
        .env("LAB_SALDEO_STORAGE_STATE", &target)
        .env("SALDEO_URL", &url)
        .env("HELIUM_EXECUTABLE", &helium)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .context("uruchomienie npx playwright + Helium (noninteractive)")?;
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
    if let Ok(text) = fs::read_to_string(storage_state)
        && keychain_set_secret(KEYCHAIN_ACCOUNT_SALDEO_STORAGE_STATE, &text)?
    {
        return Ok(());
    }
    Ok(())
}

pub(crate) fn read_saldeo_storage_state(storage_state: &Path) -> Result<String> {
    if let Some(text) = keychain_get_secret(KEYCHAIN_ACCOUNT_SALDEO_STORAGE_STATE)? {
        return Ok(text);
    }
    fs::read_to_string(storage_state)
        .with_context(|| format!("odczyt sesji Saldeo {}", storage_state.display()))
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
            "token_file_valid": status.gmail_authed,
            "usable": gmail_usable,
            "client_secret_path": lab_config_var("GOOGLE_CLIENT_SECRET_PATH")
        },
        "saldeo": {
            "storage_state": default_saldeo_storage_state_path().display().to_string(),
            "storage_state_present": status.saldeo_exists,
            "session_valid": status.saldeo_valid,
            "default_records": saldeo_records.display().to_string(),
            "default_records_present": saldeo_records.exists()
        },
        "ksef": {
            "base_url": ksef_base_url(),
            "context_type": ksef_context_type,
            "context_value": ksef_context_value,
            "access_token_cache": ksef_access_token_path.display().to_string(),
            "access_token_cache_present": ksef_access_token_path.exists(),
            "data_dir": status.ksef_dir.display().to_string(),
            "data_exists": status.ksef_data_exists,
            "default_records": ksef_records.display().to_string(),
            "default_records_present": ksef_records.exists(),
            "cert_path": status.ksef_cert.clone(),
            "cert_ok": status.ksef_cert_ok,
            "key_path": status.ksef_key.clone(),
            "key_ok": status.ksef_key_ok,
            "password_present": status.ksef_password_ok,
            "token_present": status.ksef_token_ok,
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
        "notes": [
            "GmailFetch wymaga tokenu OAuth z zakresem gmail.readonly.",
            "PDF-y są parsowane przez pdftotext, potem PyMuPDF/pdfplumber/pypdf jako fallback.",
            "lab reconcile bez własnych --ksef/--saldeo pobiera online metadane KSeF i Saldeo przed porównaniem.",
            "KSeF online używa KSEF_TOKEN, KSEF_CONTEXT_NIP/KSEF_NIP i KSEF_BASE_URL/KSEF_ENV; metadane są cache'owane lokalnie w KSEF_DATA_DIR albo data/ksef-<rok>."
        ],
        "next_steps": onboard_next_steps(&status, gmail_usable)
    });
    write_json(&status_json, None)
}
