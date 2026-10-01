use super::*;
use credentials::testing::{StoreMode, TestStore, TestStoreGuard, install};
use std::rc::Rc;

fn temp_root(name: &str) -> PathBuf {
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(format!("lab-credentials-{name}-{nonce}"));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn setup(
    name: &str,
    env: &[(&str, &str)],
    mode: StoreMode,
) -> (PathBuf, Rc<TestStore>, TestStoreGuard) {
    let root = temp_root(name);
    let (store, guard) = install(root.clone(), env, mode);
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
    fs::write(root.join(".env"), "KSEF_TOKEN='z-dotenv'\n").unwrap();
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
    fs::write(
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
    fs::write(
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

    set_mode(&path, 0o600);
    assert!(secret_value(Secret::GmailToken).unwrap().is_some());
}

#[test]
fn saving_secret_does_not_touch_env_file() {
    let (root, store, _guard) = setup("zapis", &[], StoreMode::Available);
    fs::write(
        root.join("env"),
        "KSEF_BASE_URL='https://api.ksef.mf.gov.pl/v2'\n",
    )
    .unwrap();

    assert_eq!(
        save_secret(Secret::KsefToken, "nowy-token").unwrap(),
        SecretSource::File
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
        SecretSource::File
    );
    assert!(dotenv_text(&root).contains("KSEF_TOKEN"));
    assert!(!root.join("env").exists());
    assert!(save_secret(Secret::KsefToken, "  ").is_err());
}

#[test]
fn secret_with_file_falls_back_to_private_file() {
    let (_root, _store, _guard) = setup("plik-zapas", &[], StoreMode::Unavailable);
    assert_eq!(
        save_secret(Secret::SaldeoStorageState, "{\"cookies\":[]}").unwrap(),
        SecretSource::File
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
    assert!(saldeo_login_pair().is_err());
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
