use super::*;
use credentials::testing::{StoreMode, TestStore, TestStoreGuard, install};
use std::rc::Rc;

/// Unikalny katalog na test: pid + licznik procesu (zegar macOS ma rozdzielczość µs,
/// więc równoległe testy dostawały tę samą nazwę).
fn temp_root(name: &str) -> PathBuf {
    static NEXT_ROOT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "lab-credentials-{name}-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

/// Odpina testowy magazyn i usuwa katalog testu.
struct SetupGuard {
    root: PathBuf,
    _store: TestStoreGuard,
}

impl Drop for SetupGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn setup(
    name: &str,
    env: &[(&str, &str)],
    mode: StoreMode,
) -> (PathBuf, Rc<TestStore>, SetupGuard) {
    let root = temp_root(name);
    let (store, guard) = install(root.clone(), env, mode);
    let guard = SetupGuard {
        root: root.clone(),
        _store: guard,
    };
    (root, store, guard)
}

fn env_file_text(root: &Path) -> String {
    fs::read_to_string(root.join("env")).unwrap_or_default()
}

fn dotenv_text(root: &Path) -> String {
    fs::read_to_string(root.join(".env")).unwrap_or_default()
}

struct DotenvPathFixture {
    root: PathBuf,
    cwd: PathBuf,
    home: PathBuf,
}

impl DotenvPathFixture {
    fn new(package_name: &str, ignored: bool) -> Self {
        let root = temp_root("dotenv-path");
        let cwd = root.join("checkout");
        let home = root.join("home");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(
            cwd.join("Cargo.toml"),
            format!("[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
        if ignored {
            fs::write(cwd.join(".gitignore"), ".env\n").unwrap();
        }
        let fixture = Self { root, cwd, home };
        fixture.git(&["init", "--quiet", "--template="]);
        let excludes = fixture.root.join("empty-ignore");
        fs::write(&excludes, "").unwrap();
        fixture.git(&["config", "core.excludesFile", excludes.to_str().unwrap()]);
        fixture
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(&self.cwd)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "fixture git {args:?} failed");
    }

    fn selected(&self, override_path: Option<&Path>) -> PathBuf {
        select_lab_dotenv_path(&self.cwd, &self.home, override_path)
    }

    fn fallback(&self) -> PathBuf {
        self.home.join(".config/lab/.env")
    }
}

impl Drop for DotenvPathFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn dotenv_path_unrelated_rust_project_uses_home() {
    let fixture = DotenvPathFixture::new("unrelated-cli", true);
    fs::write(
        fixture.cwd.join("Cargo.toml"),
        "# name = \"lab-cli\"\n[package]\nname = \"unrelated-cli\"\n[package.metadata]\nname = \"lab-cli\"\n",
    )
    .unwrap();
    assert_eq!(fixture.selected(None), fixture.fallback());
}

#[test]
fn dotenv_path_lab_checkout_not_ignored_uses_home() {
    let fixture = DotenvPathFixture::new("lab-cli", false);
    assert_eq!(fixture.selected(None), fixture.fallback());
}

#[test]
fn dotenv_path_tracked_env_uses_home_even_when_ignored() {
    let fixture = DotenvPathFixture::new("lab-cli", true);
    fs::write(fixture.cwd.join(".env"), "").unwrap();
    fixture.git(&["add", "--force", "--", ".env"]);
    assert_eq!(fixture.selected(None), fixture.fallback());
}

#[test]
fn dotenv_path_ignored_lab_checkout_uses_cwd() {
    let fixture = DotenvPathFixture::new("lab-cli", true);
    assert_eq!(fixture.selected(None), fixture.cwd.join(".env"));
    assert!(!fixture.cwd.join(".env").exists());
}

#[test]
fn dotenv_path_explicit_override_is_preserved() {
    let fixture = DotenvPathFixture::new("unrelated-cli", false);
    let absolute = fixture.root.join("explicit.env");
    for path in [absolute.as_path(), Path::new("relative.env")] {
        assert_eq!(fixture.selected(Some(path)), path);
        assert!(!fixture.cwd.join(".env").exists());
    }
}

#[test]
fn dotenv_path_lab_without_git_uses_home() {
    let fixture = DotenvPathFixture::new("lab-cli", true);
    fs::remove_dir_all(fixture.cwd.join(".git")).unwrap();
    assert_eq!(fixture.selected(None), fixture.fallback());
}

#[test]
fn dotenv_path_lab_package_accepts_literal_name_and_comments() {
    let fixture = DotenvPathFixture::new("lab-cli", true);
    fs::write(
        fixture.cwd.join("Cargo.toml"),
        "[package] # package table\nname = 'lab-cli' # package name\n[package.metadata]\nname = 'other'\n",
    )
    .unwrap();
    assert_eq!(fixture.selected(None), fixture.cwd.join(".env"));
}

#[test]
fn dotenv_path_fake_package_in_multiline_string_uses_home() {
    let fixture = DotenvPathFixture::new("unrelated-cli", true);
    fs::write(
        fixture.cwd.join("Cargo.toml"),
        "[package]\nname = 'unrelated-cli'\ndescription = '''\n[package]\nname = 'lab-cli'\n'''\n",
    )
    .unwrap();
    assert_eq!(fixture.selected(None), fixture.fallback());
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Plik sekretów z prawami 600, jak po zapisie przez LAB.
fn write_secret_text(path: PathBuf, text: &str) -> Result<()> {
    write_private_file(&path, text.as_bytes())
}

#[test]
fn process_env_wins_and_is_not_written_back() {
    let (root, store, _guard) = setup(
        "env-wins",
        &[("KSEF_TOKEN", "z-sesji")],
        StoreMode::Available,
    );
    write_lab_env_file(&HashMap::from([(
        "KSEF_BASE_URL".to_string(),
        "https://api-test.ksef.mf.gov.pl/v2".to_string(),
    )]))
    .unwrap();
    store.set(ACCOUNT_KSEF_TOKEN, "z-keychaina").unwrap();

    assert_eq!(
        secret_value(Secret::KsefToken).unwrap().as_deref(),
        Some("z-sesji")
    );
    assert_eq!(secret_source(Secret::KsefToken), SecretSource::Env);
    assert_eq!(
        store.stored(ACCOUNT_KSEF_TOKEN).as_deref(),
        Some("z-keychaina")
    );
    assert!(!env_file_text(&root).contains("z-sesji"));
    assert!(!env_file_text(&root).contains("KSEF_TOKEN"));
}

#[test]
fn dotenv_wins_over_keychain() {
    let (root, store, _guard) = setup("dotenv-wins", &[], StoreMode::Available);
    write_secret_text(root.join(".env"), "KSEF_TOKEN='z-dotenv'\n").unwrap();
    store.set(ACCOUNT_KSEF_TOKEN, "z-keychaina").unwrap();

    assert_eq!(
        secret_value(Secret::KsefToken).unwrap().as_deref(),
        Some("z-dotenv")
    );
    assert_eq!(secret_source(Secret::KsefToken), SecretSource::File);
    assert_eq!(
        store.stored(ACCOUNT_KSEF_TOKEN).as_deref(),
        Some("z-keychaina")
    );
}

#[test]
fn env_file_secret_moves_to_dotenv_and_other_keys_stay() {
    let (root, store, _guard) = setup("migracja", &[], StoreMode::Available);
    write_secret_text(
        root.join("env"),
        "# LAB\nKSEF_TOKEN='token-ksef'\nKSEF_CERT_PASSWORD='haslo'\nGOOGLE_CLIENT_SECRET_PATH='/tmp/client.json'\nKSEF_BASE_URL='https://api-test.ksef.mf.gov.pl/v2'\nLAB_OCR_MODE='off'\n",
    )
    .unwrap();

    assert_eq!(
        secret_value(Secret::KsefToken).unwrap().as_deref(),
        Some("token-ksef")
    );
    assert_eq!(secret_source(Secret::KsefToken), SecretSource::File);
    assert!(dotenv_text(&root).contains("KSEF_TOKEN"));
    assert!(!dotenv_text(&root).contains("GOOGLE_CLIENT_SECRET_PATH"));
    assert_eq!(store.stored(ACCOUNT_KSEF_TOKEN), None);

    let text = env_file_text(&root);
    assert!(!text.contains("KSEF_TOKEN"));
    assert!(!text.contains("token-ksef"));
    assert!(!text.contains("KSEF_CERT_PASSWORD"));
    assert!(!text.contains("haslo"));
    assert!(text.contains("GOOGLE_CLIENT_SECRET_PATH='/tmp/client.json'"));
    assert!(text.contains("KSEF_BASE_URL='https://api-test.ksef.mf.gov.pl/v2'"));
    assert!(text.contains("LAB_OCR_MODE='off'"));
    assert!(root.join("env").is_file());
    assert_eq!(
        secret_value(Secret::KsefCertPassword).unwrap().as_deref(),
        Some("haslo")
    );
}

#[test]
fn env_file_secret_stays_when_store_is_unavailable() {
    let (root, _store, _guard) = setup("bez-keychaina", &[], StoreMode::Unavailable);
    write_secret_text(
        root.join("env"),
        "KSEF_TOKEN='token-ksef'\nKSEF_BASE_URL='https://api.ksef.mf.gov.pl/v2'\n",
    )
    .unwrap();

    assert_eq!(
        secret_value(Secret::KsefToken).unwrap().as_deref(),
        Some("token-ksef")
    );
    assert_eq!(secret_source(Secret::KsefToken), SecretSource::File);
    assert!(dotenv_text(&root).contains("KSEF_TOKEN"));
    assert!(!env_file_text(&root).contains("KSEF_TOKEN"));
}

#[test]
fn file_fallback_is_read_without_keychain() {
    let (root, store, _guard) = setup("plik-cache", &[], StoreMode::Available);
    let path = Secret::KsefAccessToken.file_path().unwrap();
    write_private_file(&path, br#"{"access_token":"abc"}"#).unwrap();

    assert_eq!(secret_source(Secret::KsefAccessToken), SecretSource::File);
    assert_eq!(store.stored(ACCOUNT_KSEF_ACCESS_TOKEN), None);
    assert!(path.starts_with(&root));
}

#[cfg(unix)]
#[test]
fn world_readable_secret_file_is_rejected() {
    let (_root, _store, _guard) = setup("prawa", &[], StoreMode::Unavailable);
    let path = Secret::GmailToken.file_path().unwrap();
    write_private_file(&path, b"{\"access_token\":\"tajne-abc\"}").unwrap();
    set_mode(&path, 0o644);

    let err = secret_value(Secret::GmailToken).unwrap_err().to_string();
    assert!(err.contains(&path.display().to_string()));
    assert!(err.contains("600"));
    assert!(!err.contains("tajne-abc"));

    let err = read_secret_file(&path, "token Gmail")
        .unwrap_err()
        .to_string();
    assert!(err.contains("644"));
    assert!(!err.contains("tajne-abc"));

    // doctor / onboard --check: "insecure", nie "missing".
    let source = secret_source(Secret::GmailToken);
    assert_eq!(source, SecretSource::Insecure(path.clone()));
    assert_eq!(source.as_str(), "insecure");
    assert!(!source.is_set());

    set_mode(&path, 0o600);
    assert!(secret_value(Secret::GmailToken).unwrap().is_some());
}

#[test]
fn saving_secret_does_not_touch_env_file() {
    let (root, store, _guard) = setup("zapis", &[], StoreMode::Available);
    write_secret_text(
        root.join("env"),
        "KSEF_BASE_URL='https://api.ksef.mf.gov.pl/v2'\n",
    )
    .unwrap();

    assert_eq!(
        save_secret(Secret::KsefToken, "nowy-token").unwrap(),
        SavedSecret {
            file: Some(root.join(".env")),
            keychain: false
        }
    );
    assert_eq!(store.stored(ACCOUNT_KSEF_TOKEN), None);
    assert!(dotenv_text(&root).contains("KSEF_TOKEN"));
    let text = env_file_text(&root);
    assert!(!text.contains("nowy-token"));
    assert!(!text.contains("KSEF_TOKEN"));
    assert!(text.contains("KSEF_BASE_URL"));
}

#[test]
fn write_lab_env_file_drops_secret_keys() {
    let (root, store, _guard) = setup("czyszczenie", &[], StoreMode::Available);
    let vars = HashMap::from([
        ("KSEF_TOKEN".to_string(), "token-ksef".to_string()),
        ("KSEF_CERT_PASSWORD".to_string(), "haslo".to_string()),
        (
            "GOOGLE_CLIENT_SECRET_PATH".to_string(),
            "/tmp/client.json".to_string(),
        ),
    ]);
    write_lab_env_file(&vars).unwrap();

    let text = env_file_text(&root);
    assert!(!text.contains("KSEF_TOKEN"));
    assert!(!text.contains("KSEF_CERT_PASSWORD"));
    assert!(!text.contains("token-ksef"));
    assert!(!text.contains("haslo"));
    assert!(text.contains("GOOGLE_CLIENT_SECRET_PATH='/tmp/client.json'"));
    assert!(dotenv_text(&root).contains("KSEF_TOKEN"));
    assert_eq!(store.stored(ACCOUNT_KSEF_TOKEN), None);
    assert_eq!(store.stored(ACCOUNT_GMAIL_TOKEN), None);
    assert_eq!(store.stored(ACCOUNT_SALDEO_STORAGE_STATE), None);
}

#[test]
fn secret_without_file_goes_to_dotenv() {
    let (root, _store, _guard) = setup("brak-keychaina", &[], StoreMode::Failing);
    assert_eq!(
        save_secret(Secret::KsefToken, "token-ksef").unwrap(),
        SavedSecret {
            file: Some(root.join(".env")),
            keychain: false
        }
    );
    assert!(dotenv_text(&root).contains("KSEF_TOKEN"));
    assert!(!root.join("env").exists());
    assert!(save_secret(Secret::KsefToken, "  ").is_err());
}

#[test]
fn secret_with_file_falls_back_to_private_file() {
    let (root, _store, _guard) = setup("plik-zapas", &[], StoreMode::Unavailable);
    assert_eq!(
        save_secret(Secret::SaldeoStorageState, "{\"cookies\":[]}").unwrap(),
        SavedSecret {
            file: Some(root.join("saldeo-storage-state.json")),
            keychain: false
        }
    );
    let path = Secret::SaldeoStorageState.file_path().unwrap();
    assert!(path.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        secret_value(Secret::SaldeoStorageState).unwrap().as_deref(),
        Some("{\"cookies\":[]}")
    );
}

#[test]
fn keychain_store_replaces_gmail_token_file() {
    let (_root, store, _guard) = setup("gmail-plik", &[], StoreMode::Available);
    let path = Secret::GmailToken.file_path().unwrap();
    write_private_file(&path, b"{\"access_token\":\"stare\"}").unwrap();

    save_secret(Secret::GmailToken, "{\"access_token\":\"nowe\"}").unwrap();
    assert!(path.exists());
    assert_eq!(store.stored(ACCOUNT_GMAIL_TOKEN), None);
    assert_eq!(
        secret_value(Secret::GmailToken).unwrap().as_deref(),
        Some("{\"access_token\":\"nowe\"}")
    );
}

#[test]
fn prepare_skips_keychain_when_disabled() {
    let (root, store, _guard) = setup("prepare-skip", &[], StoreMode::Available);
    store
        .set(ACCOUNT_GMAIL_TOKEN, "{\"access_token\":\"tajne\"}")
        .unwrap();
    prepare_secret_store();
    assert!(!root.join("gmail_token.json").exists());
    assert_eq!(secret_source(Secret::GmailToken), SecretSource::Missing);
}

#[test]
fn missing_secret_is_reported_without_value() {
    let (_root, _store, _guard) = setup("brak", &[], StoreMode::Available);
    assert_eq!(secret_source(Secret::KsefToken), SecretSource::Missing);
    assert!(!secret_is_set(Secret::KsefCertPassword));
    assert_eq!(SecretSource::Missing.as_str(), "missing");
    assert_eq!(SecretSource::Keychain.as_str(), "keychain");
}

#[test]
fn accounts_use_expected_names() {
    assert_eq!(Secret::KsefToken.account(), "ksef_token");
    assert_eq!(Secret::KsefCertPassword.account(), "ksef_cert_password");
    assert_eq!(Secret::KsefAccessToken.account(), "ksef_access_token");
    assert_eq!(Secret::GmailToken.account(), "gmail_token");
    assert_eq!(Secret::SaldeoStorageState.account(), "saldeo_storage_state");
    assert_eq!(Secret::SaldeoUsername.account(), "saldeo_username");
    assert_eq!(Secret::SaldeoPassword.account(), "saldeo_password");
    assert_eq!(Secret::OpenRouterApiKey.account(), "openrouter_api_key");
    assert_eq!(Secret::from_env_key("KSEF_TOKEN"), Some(Secret::KsefToken));
    assert_eq!(
        Secret::from_env_key("SALDEO_USERNAME"),
        Some(Secret::SaldeoUsername)
    );
    assert_eq!(Secret::from_env_key("GOOGLE_CLIENT_SECRET_PATH"), None);
    assert_eq!(
        SECRET_ENV_KEYS,
        [
            "KSEF_TOKEN",
            "KSEF_CERT_PASSWORD",
            "SALDEO_USERNAME",
            "SALDEO_PASSWORD",
            "OPENROUTER_API_KEY"
        ]
    );
    assert_eq!(
        Secret::from_env_key("OPENROUTER_API_KEY"),
        Some(Secret::OpenRouterApiKey)
    );
}

#[test]
fn secrets_never_use_argv_or_security_cli() {
    for source in [
        include_str!("credentials.rs"),
        include_str!("onboard.rs"),
        include_str!("ksef.rs"),
    ] {
        assert!(!source.contains("add-generic-password"));
        assert!(!source.contains("Command::new(\"security\")"));
        assert!(!source.contains("security find-generic-password"));
    }
    assert!(include_str!("credentials.rs").contains("keychain_set_secret"));
}

#[test]
fn saldeo_login_pair_needs_both_fields() {
    let (_root, _store, _guard) = setup("saldeo-para", &[], StoreMode::Available);
    assert_eq!(saldeo_login_pair().unwrap(), None);
    save_secret(Secret::SaldeoUsername, "jan").unwrap();
    // Sam login: logowanie ręczne (ostrzeżenie na stderr), nie twardy błąd.
    assert_eq!(saldeo_login_pair().unwrap(), None);
    save_secret(Secret::SaldeoPassword, "tajne-haslo").unwrap();
    assert_eq!(
        saldeo_login_pair().unwrap(),
        Some(("jan".into(), "tajne-haslo".into()))
    );
}

#[test]
fn saldeo_login_script_reads_file_not_argv() {
    let script = include_str!("../scripts/saldeo-login.js");
    assert!(script.contains("LAB_SALDEO_LOGIN_FILE"));
    assert!(script.contains("unlinkSync"));
    assert!(!script.contains("process.argv["));
    // Hasło tylko z pliku 0600: skrypt nie czyta sekretów LAB ze środowiska.
    for key in SECRET_ENV_KEYS {
        assert!(!script.contains(&format!("process.env.{key}")), "{key}");
    }
}

#[test]
fn saldeo_auth_uses_local_playwright_not_npx_tmp() {
    let source = include_str!("onboard.rs");
    assert!(source.contains("NODE_PATH"));
    assert!(source.contains("ensure_playwright_node_path"));
    assert!(source.contains("PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD"));
    assert!(source.contains("saldeo_auth_timeout_ms"));
    assert!(source.contains("stderr(Stdio::inherit())"));
    assert!(!source.contains("Command::new(\"npx\")"));
    assert!(!source.contains("Duration::from_secs(75)"));
}

#[test]
fn saldeo_auth_timeout_gives_js_time_to_finish() {
    // Test zmienia środowisko procesu: trzyma blokadę crate'u do końca, łącznie z przywróceniem.
    let _env_lock = credentials::testing::env_lock();
    let original = std::env::var("SALDEO_AUTH_TIMEOUT_MS").ok();
    unsafe { std::env::remove_var("SALDEO_AUTH_TIMEOUT_MS") };
    assert_eq!(saldeo_auth_timeout_ms(), 180_000);
    assert_eq!(
        saldeo_auth_process_timeout(),
        std::time::Duration::from_millis(200_000)
    );
    unsafe { std::env::set_var("SALDEO_AUTH_TIMEOUT_MS", "120000") };
    assert_eq!(saldeo_auth_timeout_ms(), 120_000);
    assert_eq!(
        saldeo_auth_process_timeout(),
        std::time::Duration::from_millis(140_000)
    );
    match original {
        Some(value) => unsafe { std::env::set_var("SALDEO_AUTH_TIMEOUT_MS", value) },
        None => unsafe { std::env::remove_var("SALDEO_AUTH_TIMEOUT_MS") },
    }
}

/// Jeden bajt ISO-8859-2 (`ł` = 0xB3) w ręcznie edytowanym haśle.
const NON_UTF8_DOTENV: &[u8] = b"KSEF_TOKEN='stary-token'\nSALDEO_PASSWORD='has\xb3o'\n";

fn leftover_temps(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".lab-tmp-"))
        .collect()
}

#[test]
fn non_utf8_dotenv_blocks_upsert_and_stays_byte_identical() {
    let (root, _store, _guard) = setup("dotenv-latin2", &[], StoreMode::Unavailable);
    let dotenv = root.join(".env");
    write_private_file(&dotenv, NON_UTF8_DOTENV).unwrap();

    let err = save_secret(Secret::OpenRouterApiKey, "nowy-klucz").unwrap_err();
    let text = format!("{err}");
    assert!(text.contains(&dotenv.display().to_string()), "{text}");
    assert!(!format!("{err:#}").contains("nowy-klucz"));
    assert_eq!(fs::read(&dotenv).unwrap(), NON_UTF8_DOTENV);
    assert!(leftover_temps(&root).is_empty());

    // Zwykły odczyt: brak wartości (z komunikatem na stderr), bez błędu i bez zapisu.
    assert_eq!(secret_value(Secret::KsefToken).unwrap(), None);
    assert_eq!(secret_source(Secret::KsefToken), SecretSource::Missing);
    assert_eq!(dotenv_secret("KSEF_TOKEN"), None);
    assert_eq!(fs::read(&dotenv).unwrap(), NON_UTF8_DOTENV);
}

#[test]
fn env_file_secret_is_not_migrated_into_unreadable_dotenv() {
    let (root, _store, _guard) = setup("dotenv-migracja", &[], StoreMode::Unavailable);
    let dotenv = root.join(".env");
    write_private_file(&dotenv, NON_UTF8_DOTENV).unwrap();
    let env_text =
        "KSEF_CERT_PASSWORD='haslo-cert'\nKSEF_BASE_URL='https://api.ksef.mf.gov.pl/v2'\n";
    write_secret_text(root.join("env"), env_text).unwrap();

    assert_eq!(
        secret_value(Secret::KsefCertPassword).unwrap().as_deref(),
        Some("haslo-cert")
    );
    // Migracja do `.env` się nie udała, więc sekret zostaje w pliku env, a `.env` jest nietknięty.
    assert_eq!(fs::read(&dotenv).unwrap(), NON_UTF8_DOTENV);
    assert_eq!(env_file_text(&root), env_text);
}

#[cfg(unix)]
#[test]
fn unreadable_dotenv_blocks_upsert_and_stays_byte_identical() {
    let (root, _store, _guard) = setup("dotenv-000", &[], StoreMode::Unavailable);
    let dotenv = root.join(".env");
    write_secret_text(dotenv.clone(), "KSEF_TOKEN='stary-token'\n").unwrap();
    set_mode(&dotenv, 0o000);
    if fs::read(&dotenv).is_ok() {
        // root ignoruje prawa; test nie ma tu sensu.
        set_mode(&dotenv, 0o600);
        return;
    }

    let err = save_secret(Secret::KsefToken, "nowy-token").unwrap_err();
    assert!(format!("{err}").contains(&dotenv.display().to_string()));
    assert_eq!(secret_value(Secret::KsefToken).unwrap(), None);

    set_mode(&dotenv, 0o600);
    assert_eq!(
        fs::read_to_string(&dotenv).unwrap(),
        "KSEF_TOKEN='stary-token'\n"
    );
}

#[cfg(unix)]
#[test]
fn world_readable_dotenv_is_rejected_and_reported_insecure() {
    let (root, store, _guard) = setup("dotenv-644", &[], StoreMode::Available);
    let dotenv = root.join(".env");
    write_secret_text(dotenv.clone(), "KSEF_TOKEN='tajne-z-dotenv'\n").unwrap();
    set_mode(&dotenv, 0o644);
    store.set(ACCOUNT_KSEF_TOKEN, "z-keychaina").unwrap();

    let err = secret_value(Secret::KsefToken).unwrap_err().to_string();
    assert!(err.contains(&dotenv.display().to_string()), "{err}");
    assert!(err.contains("644"));
    assert!(!err.contains("tajne-z-dotenv"));

    let source = secret_source(Secret::KsefToken);
    assert_eq!(source, SecretSource::Insecure(dotenv.clone()));
    assert_ne!(source.as_str(), "missing");
    assert_eq!(
        source.problem().unwrap(),
        format!("insecure permissions: {}", dotenv.display())
    );
    let suffix = secret_source_suffix(&source);
    assert!(suffix.contains("insecure permissions"));
    assert!(suffix.contains(&dotenv.display().to_string()));
    assert!(!suffix.contains("tajne"));

    // Zapis też odmawia; plik zostaje bez zmian (z szerokimi prawami, do naprawy przez użytkownika).
    let before = fs::read(&dotenv).unwrap();
    assert!(save_secret(Secret::KsefToken, "nowy").is_err());
    assert_eq!(fs::read(&dotenv).unwrap(), before);
    // Zwykły odczyt konfiguracji nie używa odrzuconego pliku.
    assert_eq!(dotenv_secret("KSEF_TOKEN"), None);
}

#[cfg(unix)]
#[test]
fn dotenv_symlink_is_checked_on_its_target_and_survives_save() {
    let (root, _store, _guard) = setup("dotenv-link", &[], StoreMode::Unavailable);
    let store_dir = root.join("config-lab");
    fs::create_dir_all(&store_dir).unwrap();
    let target = store_dir.join(".env");
    write_secret_text(target.clone(), "KSEF_TOKEN='z-celu'\n").unwrap();
    let link = root.join(".env");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    assert_eq!(
        secret_value(Secret::KsefToken).unwrap().as_deref(),
        Some("z-celu")
    );
    save_secret(Secret::OpenRouterApiKey, "klucz-dummy").unwrap();
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let text = fs::read_to_string(&target).unwrap();
    assert!(text.contains("KSEF_TOKEN='z-celu'"));
    assert!(text.contains("OPENROUTER_API_KEY='klucz-dummy'"));

    set_mode(&target, 0o644);
    assert_eq!(
        secret_source(Secret::KsefToken),
        SecretSource::Insecure(link.clone())
    );
}

#[cfg(unix)]
#[test]
fn lab_env_file_problems_are_errors_for_read_modify_write() {
    let (root, _store, _guard) = setup("env-644", &[], StoreMode::Unavailable);
    let env_path = root.join("env");
    write_secret_text(
        env_path.clone(),
        "KSEF_BASE_URL='https://api.ksef.mf.gov.pl/v2'\n",
    )
    .unwrap();
    set_mode(&env_path, 0o644);
    let err = read_lab_env_file().unwrap_err().to_string();
    assert!(err.contains(&env_path.display().to_string()), "{err}");
    assert_eq!(lab_config_var("KSEF_BASE_URL"), None);

    write_private_file(&env_path, b"KSEF_DATA_DIR='/tmp/dane-\xb3'\n").unwrap();
    let err = format!("{:#}", read_lab_env_file().unwrap_err());
    assert!(err.contains(&env_path.display().to_string()), "{err}");
    assert_eq!(
        fs::read(&env_path).unwrap(),
        b"KSEF_DATA_DIR='/tmp/dane-\xb3'\n"
    );
}

#[test]
fn keychain_hit_is_not_copied_into_dotenv() {
    let (root, store, _guard) = setup(
        "keychain-bez-dotenv",
        &[("LAB_USE_KEYCHAIN", "1")],
        StoreMode::Available,
    );
    store.set(ACCOUNT_KSEF_TOKEN, "z-keychaina").unwrap();
    store
        .set(ACCOUNT_SALDEO_PASSWORD, "haslo-z-keychaina")
        .unwrap();

    prepare_secret_store();
    assert!(!root.join(".env").exists());

    assert_eq!(
        secret_value(Secret::KsefToken).unwrap().as_deref(),
        Some("z-keychaina")
    );
    assert_eq!(secret_source(Secret::KsefToken), SecretSource::Keychain);
    assert_eq!(
        secret_value(Secret::SaldeoPassword).unwrap().as_deref(),
        Some("haslo-z-keychaina")
    );
    assert!(!root.join(".env").exists());

    // Istniejący `.env` też nie dostaje kopii z Keychain.
    write_secret_text(root.join(".env"), "OPENROUTER_API_KEY='klucz'\n").unwrap();
    let before = fs::read(root.join(".env")).unwrap();
    prepare_secret_store();
    assert_eq!(secret_source(Secret::KsefToken), SecretSource::Keychain);
    assert_eq!(fs::read(root.join(".env")).unwrap(), before);
}

#[test]
fn keychain_save_reports_where_secret_went() {
    let (root, store, _guard) = setup(
        "keychain-zapis",
        &[("LAB_USE_KEYCHAIN", "1")],
        StoreMode::Available,
    );
    let saved = save_secret(Secret::KsefToken, "token-dummy").unwrap();
    assert_eq!(
        saved,
        SavedSecret {
            file: Some(root.join(".env")),
            keychain: true
        }
    );
    assert_eq!(
        store.stored(ACCOUNT_KSEF_TOKEN).as_deref(),
        Some("token-dummy")
    );
    let text = saved.describe();
    assert!(text.contains(&root.join(".env").display().to_string()));
    assert!(text.contains("Keychain"));
    assert!(!text.contains("token-dummy"));
}

#[test]
fn keychain_failure_is_not_reported_as_keychain_save() {
    let (root, _store, _guard) = setup(
        "keychain-blad",
        &[("LAB_USE_KEYCHAIN", "1")],
        StoreMode::Failing,
    );
    let saved = save_secret(Secret::KsefToken, "token-dummy").unwrap();
    assert_eq!(saved.file, Some(root.join(".env")));
    assert!(!saved.keychain);
    assert!(!saved.describe().contains("Keychain"));
}

#[test]
fn dotenv_git_command_is_absolute_and_hardened() {
    let git = Path::new("/usr/bin/git");
    let cwd = Path::new("/tmp");
    let command = dotenv_git_command(git, cwd, &["ls-files", "--", ".env"]);
    assert_eq!(command.get_program(), git.as_os_str());
    assert_eq!(command.get_current_dir(), Some(cwd));
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let subcommand = args.iter().position(|arg| arg == "ls-files").unwrap();
    let before = &args[..subcommand];
    for pair in [
        ["-c", "core.fsmonitor=false"],
        ["-c", "core.hooksPath=/dev/null"],
    ] {
        assert!(
            before.windows(2).any(|window| window == pair),
            "{pair:?} przed podpoleceniem: {args:?}"
        );
    }
    assert_eq!(&args[subcommand..], ["ls-files", "--", ".env"]);

    let envs = command
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect::<HashMap<_, _>>();
    assert_eq!(envs.get("PATH"), Some(&Some("/usr/bin:/bin".to_string())));
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_CONFIG_PARAMETERS",
        "GIT_EXEC_PATH",
    ] {
        assert!(!matches!(envs.get(key), Some(Some(_))), "{key}");
    }

    let check = dotenv_git_command(
        git,
        cwd,
        &["check-ignore", "--quiet", "--no-index", "--", ".env"],
    );
    let check_args = check
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(check_args.starts_with(&DOTENV_GIT_HARDENING.map(String::from)));

    let source = include_str!("credentials.rs");
    assert!(!source.contains(&format!("Command::new({:?})", "git")));
    assert!(source.contains("system_tool(\"git\")"));
}

#[test]
fn trusted_git_is_absolute_system_binary() {
    if let Some(git) = crate::hardening::system_tool("git") {
        assert!(git.is_absolute());
        assert!(
            ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]
                .iter()
                .any(|dir| git.parent() == Some(Path::new(dir))),
            "{}",
            git.display()
        );
    }
}

#[test]
fn doctor_ppmlx_check_does_not_spawn_python() {
    let source = include_str!("onboard.rs");
    assert!(!source.contains(&format!("Command::new({:?})", "python3")));
    assert!(!source.contains(&format!("shutil.{}", "which")));
    assert!(source.contains("tool_present_for_status(\"ppmlx\")"));
}

#[test]
fn npm_install_output_goes_to_stderr() {
    let source = include_str!("onboard.rs");
    let npm = source.find("Command::new(\"npm\")").unwrap();
    let end = npm + source[npm..].find("\n}\n").unwrap();
    let block = &source[npm..end];
    assert!(block.contains(".stdout(Stdio::from(std::io::stderr()))"));
    assert!(block.contains(".stdin(Stdio::null())"));
}

#[test]
fn temp_roots_are_unique_and_removed() {
    let (first, second);
    {
        let (root_a, _store, _guard) = setup("unikalny", &[], StoreMode::Unavailable);
        let root_b = temp_root("unikalny");
        assert_ne!(root_a, root_b);
        first = root_a;
        second = root_b;
        let _ = fs::remove_dir_all(&second);
    }
    assert!(!first.exists());
    assert!(!second.exists());
}

// ── Zmienne środowiskowe procesów potomnych ─────────────────────────────────

fn command_envs(command: &Command) -> HashMap<String, Option<String>> {
    command
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect()
}

fn command_args(command: &Command) -> Vec<String> {
    command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn assert_secrets_removed(command: &Command) {
    let envs = command_envs(command);
    for key in SECRET_ENV_KEYS
        .iter()
        .chain(crate::hardening::CHILD_SECRET_ENV_KEYS.iter())
    {
        assert_eq!(
            envs.get(*key),
            Some(&None),
            "{key} musi być usunięty ze środowiska {:?}",
            command.get_program()
        );
    }
}

#[test]
fn npm_install_has_no_secrets_and_ignores_scripts() {
    let command = npm_install_playwright_command(
        Path::new("/tmp/lab-prefix-dummy"),
        crate::onboard::PLAYWRIGHT_VERSION,
    );
    assert_eq!(command.get_program(), "npm");
    assert_secrets_removed(&command);
    let args = command_args(&command);
    assert!(args.contains(&"--ignore-scripts".to_string()), "{args:?}");
    assert_eq!(args.last().map(String::as_str), Some("playwright@1.63.0"));
    assert_eq!(
        command_envs(&command).get("PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD"),
        Some(&Some("1".to_string()))
    );
}

#[test]
fn saldeo_node_login_gets_password_only_through_file() {
    let login_file = Path::new("/tmp/lab-saldeo-login-dummy.json");
    let command = saldeo_login_node_command(&SaldeoLoginNodeArgs {
        script: Path::new("/tmp/lab-saldeo-login-script-dummy.js"),
        node_path: Path::new("/tmp/lab-node-modules-dummy"),
        target: Path::new("/tmp/lab-storage-dummy.json"),
        url: "https://saldeo.example.invalid/",
        helium: "/Applications/Helium.app/Contents/MacOS/Helium",
        timeout_ms: 60_000,
        login_file: Some(login_file),
    });
    assert_eq!(command.get_program(), "node");
    assert_secrets_removed(&command);
    let envs = command_envs(&command);
    assert_eq!(
        envs.get("LAB_SALDEO_LOGIN_FILE"),
        Some(&Some(login_file.display().to_string()))
    );
    assert_eq!(
        command_args(&command),
        ["/tmp/lab-saldeo-login-script-dummy.js"]
    );

    let without_login = saldeo_login_node_command(&SaldeoLoginNodeArgs {
        login_file: None,
        script: Path::new("/tmp/x.js"),
        node_path: Path::new("/tmp/nm"),
        target: Path::new("/tmp/t.json"),
        url: "https://saldeo.example.invalid/",
        helium: "/tmp/helium",
        timeout_ms: 60_000,
    });
    assert_eq!(
        command_envs(&without_login).get("LAB_SALDEO_LOGIN_FILE"),
        Some(&None)
    );
}

#[test]
fn saldeo_auth_shell_script_has_no_secrets_in_env() {
    let login_file = Path::new("/tmp/lab-saldeo-login-dummy.json");
    let command = saldeo_auth_script_command(
        Path::new("/tmp/checkout/scripts/saldeo-auth.sh"),
        Path::new("/tmp/storage.json"),
        Some(login_file),
    );
    assert_secrets_removed(&command);
    assert_eq!(command_args(&command), ["/tmp/storage.json"]);
    assert_eq!(
        command_envs(&command).get("LAB_SALDEO_LOGIN_FILE"),
        Some(&Some(login_file.display().to_string()))
    );
}

// ── LAB_NONINTERACTIVE ──────────────────────────────────────────────────────

#[test]
fn unattended_expired_session_spawns_nothing() {
    let spawned = std::cell::Cell::new(false);
    let err = ensure_saldeo_session_with(
        None,
        true,
        || false,
        || {
            spawned.set(true);
            Ok(())
        },
    )
    .unwrap_err();
    assert!(!spawned.get(), "logowanie nie może się uruchomić");
    let text = format!("{err:#}");
    assert!(text.contains("Saldeo session expired"), "{text}");
    assert!(text.contains("lab onboard"), "{text}");

    // Ważna sesja: bez błędu także w trybie nienadzorowanym.
    assert!(ensure_saldeo_session_with(None, true, || true, || panic!("spawn")).is_ok());
}

#[test]
fn interactive_expired_session_runs_login_once() {
    let calls = std::cell::Cell::new(0);
    let checks = std::cell::Cell::new(0);
    ensure_saldeo_session_with(
        None,
        false,
        || {
            checks.set(checks.get() + 1);
            checks.get() > 1
        },
        || {
            calls.set(calls.get() + 1);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(checks.get(), 2);
}

#[test]
fn unattended_flag_blocks_browser_and_npm_entry_points() {
    let (root, _store, _guard) = setup(
        "nienadzorowany",
        &[("LAB_NONINTERACTIVE", "1")],
        StoreMode::Unavailable,
    );
    assert!(lab_noninteractive());
    let err = format!("{:#}", saldeo_auth_noninteractive().unwrap_err());
    assert!(err.contains("Saldeo session expired"), "{err}");
    let err = format!("{:#}", run_saldeo_auth_script().unwrap_err());
    assert!(err.contains("Saldeo session expired"), "{err}");
    // Nic nie zostało utworzone (katalog Playwright, pliki logowania).
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn unattended_flag_is_off_by_default() {
    let (_root, _store, _guard) = setup("interaktywny", &[], StoreMode::Unavailable);
    assert!(!lab_noninteractive());
    let (_root, _store, _guard) = setup(
        "interaktywny-0",
        &[("LAB_NONINTERACTIVE", "0")],
        StoreMode::Unavailable,
    );
    assert!(!lab_noninteractive());
}

// ── Migracja env → .env ─────────────────────────────────────────────────────

#[test]
fn stale_env_file_secret_never_overwrites_newer_dotenv() {
    let (root, _store, _guard) = setup("bez-cofania", &[], StoreMode::Unavailable);
    // Nowy token z onboardingu w `.env`, stara kopia nadal w env.
    write_secret_text(root.join(".env"), "KSEF_TOKEN='nowy-token'\n").unwrap();
    write_secret_text(
        root.join("env"),
        "KSEF_TOKEN='stary-token'\nKSEF_BASE_URL='https://api.ksef.mf.gov.pl/v2'\n",
    )
    .unwrap();

    // Edycja dowolnego ustawienia jawnego.
    let mut vars = read_lab_env_file().unwrap();
    vars.insert("LAB_OCR_MODE".to_string(), "off".to_string());
    write_lab_env_file(&vars).unwrap();

    // `.env` nietknięty: bez nadpisania starą wartością.
    assert_eq!(dotenv_text(&root), "KSEF_TOKEN='nowy-token'\n");
    let text = env_file_text(&root);
    assert!(!text.contains("KSEF_TOKEN"), "{text}");
    assert!(!text.contains("stary-token"));
    assert!(text.contains("LAB_OCR_MODE='off'"));
    assert!(text.contains("KSEF_BASE_URL"));
    assert_eq!(
        secret_value(Secret::KsefToken).unwrap().as_deref(),
        Some("nowy-token")
    );
}

#[test]
fn migration_still_moves_secret_missing_from_dotenv() {
    let (root, _store, _guard) = setup("migracja-brak", &[], StoreMode::Unavailable);
    write_secret_text(root.join(".env"), "OPENROUTER_API_KEY='klucz'\n").unwrap();
    write_secret_text(root.join("env"), "KSEF_TOKEN='jedyny-token'\n").unwrap();
    write_lab_env_file(&read_lab_env_file().unwrap()).unwrap();
    let dotenv = dotenv_text(&root);
    assert!(dotenv.contains("KSEF_TOKEN='jedyny-token'"), "{dotenv}");
    assert!(dotenv.contains("OPENROUTER_API_KEY='klucz'"), "{dotenv}");
    assert!(!env_file_text(&root).contains("KSEF_TOKEN"));
}

// ── Wartości wieloliniowe i round-trip pliku `.env` ─────────────────────────

const AWKWARD_VALUES: &[&str] = &[
    "",
    "plain",
    "#",
    "# komentarz",
    "a#b",
    "=",
    "a=b=c",
    "'",
    "''",
    "it's",
    "'\\''",
    "\"",
    "\"podwójne\"",
    "\"mieszane' oba\"",
    "\\",
    "back\\slash\\",
    "\\n",
    " leading",
    "trailing ",
    "  both  ",
    "\ttab\t",
    "$HOME",
    "${PATH}",
    "$(rm -rf /)",
    "`id`",
    "export X=1",
    "zażółć gęślą jaźń",
    "KEY='x'",
];

#[test]
fn env_quote_and_parse_round_trip_awkward_values() {
    for value in AWKWARD_VALUES {
        let text = format!("KSEF_TOKEN={}\n", quote_env_value(value));
        let parsed = parse_env_text(&text);
        assert_eq!(parsed.len(), 1, "{value:?} → {text:?}");
        assert_eq!(
            parsed.get("KSEF_TOKEN").map(String::as_str),
            Some(*value),
            "{value:?} → {text:?}"
        );
    }
    // Wszystkie naraz w jednym pliku: żadna wartość nie tworzy ani nie psuje innego klucza.
    let text = AWKWARD_VALUES
        .iter()
        .enumerate()
        .map(|(i, value)| format!("K{i}={}\n", quote_env_value(value)))
        .collect::<String>();
    let parsed = parse_env_text(&text);
    assert_eq!(parsed.len(), AWKWARD_VALUES.len());
    for (i, value) in AWKWARD_VALUES.iter().enumerate() {
        assert_eq!(parsed[&format!("K{i}")], *value);
    }
}

#[test]
fn awkward_secrets_round_trip_through_dotenv() {
    let (root, _store, _guard) = setup("dotenv-round-trip", &[], StoreMode::Unavailable);
    for value in AWKWARD_VALUES
        .iter()
        .filter(|value| !value.trim().is_empty())
    {
        save_secret(Secret::OpenRouterApiKey, value).unwrap();
        assert_eq!(
            secret_value(Secret::OpenRouterApiKey).unwrap().as_deref(),
            Some(*value),
            "{value:?} → {:?}",
            dotenv_text(&root)
        );
        assert_eq!(read_lab_dotenv_keys(&root), ["OPENROUTER_API_KEY"]);
    }
}

fn read_lab_dotenv_keys(root: &Path) -> Vec<String> {
    let mut keys = parse_env_text(&dotenv_text(root))
        .into_keys()
        .collect::<Vec<_>>();
    keys.sort();
    keys
}

#[test]
fn secret_with_newline_or_nul_is_refused() {
    let (root, _store, _guard) = setup("nowa-linia", &[], StoreMode::Unavailable);
    for value in ["tok\n", "x\nEVIL=1", "a\r\nb", "a\rb", "a\0b"] {
        let err = format!("{:#}", save_secret(Secret::KsefToken, value).unwrap_err());
        assert!(err.contains("nowej linii albo NUL"), "{err}");
        assert!(err.contains("KSEF_TOKEN"), "{err}");
        assert!(!err.contains("tok") && !err.contains("EVIL"), "{err}");
    }
    assert!(!root.join(".env").exists());
    // Pliki 0600 (JSON) mogą mieć wiele linii.
    save_secret(Secret::SaldeoStorageState, "{\n  \"cookies\": []\n}\n").unwrap();
    // Jawne ustawienia w env też nie mogą rozbić pliku.
    let vars = HashMap::from([("KSEF_DATA_DIR".to_string(), "/tmp/a\nEVIL=1".to_string())]);
    assert!(write_lab_env_file(&vars).is_err());
    assert!(!root.join("env").exists());
}

#[test]
fn keychain_import_strips_one_trailing_line_ending() {
    let (root, store, _guard) = setup(
        "keychain-newline",
        &[("LAB_USE_KEYCHAIN", "1")],
        StoreMode::Available,
    );
    for (stored, expected) in [
        ("tok\n", "tok"),
        ("tok\r\n", "tok"),
        ("tok\r", "tok"),
        ("tok\n\n", "tok\n"),
        ("tok", "tok"),
    ] {
        store.set(ACCOUNT_KSEF_TOKEN, stored).unwrap();
        assert_eq!(
            secret_value(Secret::KsefToken).unwrap().as_deref(),
            Some(expected),
            "{stored:?}"
        );
    }
    store.set(ACCOUNT_OPENROUTER_API_KEY, "\n").unwrap();
    assert_eq!(secret_value(Secret::OpenRouterApiKey).unwrap(), None);
    assert!(!root.join(".env").exists());
    assert_eq!(strip_one_line_ending("a\n\r"), "a\n");
    assert_eq!(strip_one_line_ending(""), "");
}

// ── Login Saldeo z onboardingu ──────────────────────────────────────────────

#[test]
fn onboarding_does_not_store_username_without_password() {
    let (root, _store, _guard) = setup("saldeo-sam-login", &[], StoreMode::Unavailable);
    let message = save_saldeo_login("jan\n", "").unwrap();
    assert!(message.contains("nie został zapisany"), "{message}");
    assert!(!dotenv_text(&root).contains("SALDEO_USERNAME"));
    assert_eq!(saldeo_login_pair().unwrap(), None);

    // Oba pola: zapis obu; końcowe `\n` z wklejki znika.
    let message = save_saldeo_login("jan", "tajne-haslo\n").unwrap();
    assert!(!message.contains("tajne"), "{message}");
    assert_eq!(
        saldeo_login_pair().unwrap(),
        Some(("jan".into(), "tajne-haslo".into()))
    );

    // Zmiana loginu przy zapisanym haśle: hasło bez zmian.
    save_saldeo_login("anna", "").unwrap();
    assert_eq!(
        saldeo_login_pair().unwrap(),
        Some(("anna".into(), "tajne-haslo".into()))
    );

    // Hasło z nową linią w środku: nic nie jest zapisywane, także login.
    assert!(save_saldeo_login("ewa", "a\nb").is_err());
    assert_eq!(
        saldeo_login_pair().unwrap(),
        Some(("anna".into(), "tajne-haslo".into()))
    );
}

#[test]
fn lone_saldeo_password_is_treated_as_no_credentials() {
    let (_root, _store, _guard) = setup("saldeo-samo-haslo", &[], StoreMode::Unavailable);
    save_secret(Secret::SaldeoPassword, "tajne").unwrap();
    assert_eq!(saldeo_login_pair().unwrap(), None);
}

// ── Wyszukiwanie scripts/saldeo-auth.sh ─────────────────────────────────────

#[cfg(unix)]
fn write_script(path: &Path, mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    set_mode(path, mode);
}

#[cfg(unix)]
fn write_lab_manifest(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"lab-cli\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn planted_script_in_cwd_ancestor_is_ignored() {
    let root = temp_root("auth-script-planted");
    let uid = crate::hardening::current_uid();
    // Podłożony skrypt nad bieżącym katalogiem, z „dobrymi” prawami i właścicielem.
    write_script(&root.join("scripts/saldeo-auth.sh"), 0o700);
    let cwd = root.join("a/b");
    fs::create_dir_all(&cwd).unwrap();
    assert_eq!(
        find_saldeo_auth_script_in(None, None, Some(&cwd), uid).unwrap(),
        None
    );
    // Nawet gdy przodek wygląda jak pakiet lab-cli: liczy się tylko sam cwd.
    write_lab_manifest(&root);
    assert_eq!(
        find_saldeo_auth_script_in(None, None, Some(&cwd), uid).unwrap(),
        None
    );
    // Sam katalog pakietu lab-cli jest zaufanym miejscem.
    let found = find_saldeo_auth_script_in(None, None, Some(&root), uid).unwrap();
    assert_eq!(
        found,
        Some(fs::canonicalize(root.join("scripts/saldeo-auth.sh")).unwrap())
    );
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn script_next_to_symlinked_executable_is_found() {
    let root = temp_root("auth-script-exe");
    let uid = crate::hardening::current_uid();
    let checkout = root.join("checkout");
    write_script(&checkout.join("scripts/saldeo-auth.sh"), 0o755);
    let exe = checkout.join("target/release/lab");
    write_script(&exe, 0o755);
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&exe, bin.join("lab")).unwrap();
    let elsewhere = root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();

    let found = find_saldeo_auth_script_in(None, Some(&bin.join("lab")), Some(&elsewhere), uid)
        .unwrap()
        .unwrap();
    assert_eq!(
        found,
        fs::canonicalize(checkout.join("scripts/saldeo-auth.sh")).unwrap()
    );
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn untrusted_auth_script_is_refused() {
    let root = temp_root("auth-script-untrusted");
    let uid = crate::hardening::current_uid();
    write_lab_manifest(&root);
    let script = root.join("scripts/saldeo-auth.sh");

    // Zapis dla grupy albo innych.
    for mode in [0o775, 0o757, 0o777] {
        write_script(&script, mode);
        assert_eq!(
            find_saldeo_auth_script_in(None, None, Some(&root), uid).unwrap(),
            None,
            "{mode:o}"
        );
        let err = format!(
            "{:#}",
            find_saldeo_auth_script_in(Some(&script), None, None, uid).unwrap_err()
        );
        assert!(err.contains("SALDEO_AUTH_SCRIPT"), "{err}");
        assert!(err.contains("zapis"), "{err}");
    }

    // Inny właściciel (symulowany innym uid).
    write_script(&script, 0o700);
    let other = uid.wrapping_add(1);
    let err = format!(
        "{:#}",
        crate::hardening::trusted_user_script(&script, other).unwrap_err()
    );
    assert!(err.contains("właścicielem"), "{err}");
    assert_eq!(
        find_saldeo_auth_script_in(None, None, Some(&root), other).unwrap(),
        None
    );
    assert!(find_saldeo_auth_script_in(Some(&script), None, None, other).is_err());

    // Katalog skryptu zapisywalny dla innych (bez bitu sticky).
    set_mode(&root.join("scripts"), 0o777);
    assert!(crate::hardening::trusted_user_script(&script, uid).is_err());
    set_mode(&root.join("scripts"), 0o755);

    // Jawna ścieżka: musi być bezwzględna; poprawny skrypt przechodzi.
    assert!(
        find_saldeo_auth_script_in(Some(Path::new("scripts/saldeo-auth.sh")), None, None, uid)
            .is_err()
    );
    assert_eq!(
        find_saldeo_auth_script_in(Some(&script), None, None, uid).unwrap(),
        Some(fs::canonicalize(&script).unwrap())
    );
    let _ = fs::remove_dir_all(&root);
}
