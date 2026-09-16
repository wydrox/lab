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
fn keychain_wins_over_env_file() {
    let (root, store, _guard) = setup("keychain-wins", &[], StoreMode::Available);
    fs::write(root.join("env"), "KSEF_TOKEN='z-pliku'\n").unwrap();
    store.set(ACCOUNT_KSEF_TOKEN, "z-keychaina").unwrap();

    assert_eq!(
        secret_value(Secret::KsefToken).unwrap().as_deref(),
        Some("z-keychaina")
    );
    assert_eq!(secret_source(Secret::KsefToken), SecretSource::Keychain);
    let text = env_file_text(&root);
    assert!(!text.contains("KSEF_TOKEN"));
    assert!(!text.contains("z-pliku"));
}

#[test]
fn env_file_secret_moves_to_keychain_and_other_keys_stay() {
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
    assert_eq!(secret_source(Secret::KsefToken), SecretSource::Keychain);
    assert_eq!(
        store.stored(ACCOUNT_KSEF_TOKEN).as_deref(),
        Some("token-ksef")
    );
    assert_eq!(
        store.stored(ACCOUNT_KSEF_CERT_PASSWORD).as_deref(),
        Some("haslo")
    );

    let text = env_file_text(&root);
    assert!(!text.contains("KSEF_TOKEN"));
    assert!(!text.contains("token-ksef"));
    assert!(!text.contains("KSEF_CERT_PASSWORD"));
    assert!(!text.contains("haslo"));
    assert!(text.contains("GOOGLE_CLIENT_SECRET_PATH='/tmp/client.json'"));
    assert!(text.contains("KSEF_BASE_URL='https://api-test.ksef.mf.gov.pl/v2'"));
    assert!(text.contains("LAB_OCR_MODE='off'"));
    assert!(root.join("env").is_file());
    // Klucz KSEF_CERT_PASSWORD nie zostaje w pliku env nawet po osobnym odczycie.
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
    assert!(env_file_text(&root).contains("KSEF_TOKEN"));
}

#[test]
fn file_fallback_is_read_and_promoted_to_keychain() {
    let (root, store, _guard) = setup("plik-cache", &[], StoreMode::Available);
    let path = Secret::KsefAccessToken.file_path().unwrap();
    write_private_file(&path, br#"{"access_token":"abc"}"#).unwrap();

    assert_eq!(secret_source(Secret::KsefAccessToken), SecretSource::File);
    assert_eq!(
        store.stored(ACCOUNT_KSEF_ACCESS_TOKEN).as_deref(),
        Some(r#"{"access_token":"abc"}"#)
    );
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
        SecretSource::Keychain
    );
    assert_eq!(
        store.stored(ACCOUNT_KSEF_TOKEN).as_deref(),
        Some("nowy-token")
    );
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
    assert_eq!(
        store.stored(ACCOUNT_KSEF_TOKEN).as_deref(),
        Some("token-ksef")
    );
    assert_eq!(store.stored(ACCOUNT_GMAIL_TOKEN), None);
    assert_eq!(store.stored(ACCOUNT_SALDEO_STORAGE_STATE), None);
}

#[test]
fn secret_without_file_needs_keychain() {
    let (root, _store, _guard) = setup("brak-keychaina", &[], StoreMode::Failing);
    let err = save_secret(Secret::KsefToken, "token-ksef")
        .unwrap_err()
        .to_string();
    assert!(err.contains("KSEF_TOKEN"));
    assert!(!err.contains("token-ksef"));
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
    assert!(!path.exists());
    assert_eq!(
        store.stored(ACCOUNT_GMAIL_TOKEN).as_deref(),
        Some("{\"access_token\":\"nowe\"}")
    );
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
    let (_root, store, _guard) = setup("saldeo-para", &[], StoreMode::Available);
    assert_eq!(saldeo_login_pair().unwrap(), None);
    store.set(ACCOUNT_SALDEO_USERNAME, "jan").unwrap();
    assert!(saldeo_login_pair().is_err());
    store.set(ACCOUNT_SALDEO_PASSWORD, "tajne-haslo").unwrap();
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
