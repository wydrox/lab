use crate::*;

/// Konta sekretów w macOS Keychain; usługa to KEYCHAIN_SERVICE.
pub(crate) const ACCOUNT_GMAIL_TOKEN: &str = "gmail_token";
pub(crate) const ACCOUNT_SALDEO_STORAGE_STATE: &str = "saldeo_storage_state";
pub(crate) const ACCOUNT_KSEF_TOKEN: &str = "ksef_token";
pub(crate) const ACCOUNT_KSEF_CERT_PASSWORD: &str = "ksef_cert_password";
pub(crate) const ACCOUNT_KSEF_ACCESS_TOKEN: &str = "ksef_access_token";
pub(crate) const ACCOUNT_SALDEO_USERNAME: &str = "saldeo_username";
pub(crate) const ACCOUNT_SALDEO_PASSWORD: &str = "saldeo_password";
pub(crate) const ACCOUNT_OPENROUTER_API_KEY: &str = "openrouter_api_key";

/// Klucze, których LAB nie zapisuje już w ~/.config/lab/env.
pub(crate) const SECRET_ENV_KEYS: [&str; 5] = [
    "KSEF_TOKEN",
    "KSEF_CERT_PASSWORD",
    "SALDEO_USERNAME",
    "SALDEO_PASSWORD",
    "OPENROUTER_API_KEY",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Secret {
    GmailToken,
    SaldeoStorageState,
    KsefToken,
    KsefCertPassword,
    KsefAccessToken,
    SaldeoUsername,
    SaldeoPassword,
    OpenRouterApiKey,
}

/// Skąd pochodzi sekret; bez wartości, do raportów `doctor` i `onboard`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecretSource {
    Env,
    Keychain,
    File,
    Missing,
}

impl SecretSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SecretSource::Env => "env",
            SecretSource::Keychain => "keychain",
            SecretSource::File => "file",
            SecretSource::Missing => "missing",
        }
    }

    pub(crate) fn is_set(self) -> bool {
        self != SecretSource::Missing
    }
}

impl Secret {
    pub(crate) fn account(self) -> &'static str {
        match self {
            Secret::GmailToken => ACCOUNT_GMAIL_TOKEN,
            Secret::SaldeoStorageState => ACCOUNT_SALDEO_STORAGE_STATE,
            Secret::KsefToken => ACCOUNT_KSEF_TOKEN,
            Secret::KsefCertPassword => ACCOUNT_KSEF_CERT_PASSWORD,
            Secret::KsefAccessToken => ACCOUNT_KSEF_ACCESS_TOKEN,
            Secret::SaldeoUsername => ACCOUNT_SALDEO_USERNAME,
            Secret::SaldeoPassword => ACCOUNT_SALDEO_PASSWORD,
            Secret::OpenRouterApiKey => ACCOUNT_OPENROUTER_API_KEY,
        }
    }

    /// Zmienna środowiskowa sesji; tylko dla sekretów podawanych jako tekst.
    pub(crate) fn env_key(self) -> Option<&'static str> {
        match self {
            Secret::KsefToken => Some("KSEF_TOKEN"),
            Secret::KsefCertPassword => Some("KSEF_CERT_PASSWORD"),
            Secret::SaldeoUsername => Some("SALDEO_USERNAME"),
            Secret::SaldeoPassword => Some("SALDEO_PASSWORD"),
            Secret::OpenRouterApiKey => Some("OPENROUTER_API_KEY"),
            _ => None,
        }
    }

    pub(crate) fn from_env_key(name: &str) -> Option<Secret> {
        match name {
            "KSEF_TOKEN" => Some(Secret::KsefToken),
            "KSEF_CERT_PASSWORD" => Some(Secret::KsefCertPassword),
            "SALDEO_USERNAME" => Some(Secret::SaldeoUsername),
            "SALDEO_PASSWORD" => Some(Secret::SaldeoPassword),
            "OPENROUTER_API_KEY" => Some(Secret::OpenRouterApiKey),
            _ => None,
        }
    }

    /// Nazwa w komunikatach; nigdy nie zawiera wartości sekretu.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Secret::GmailToken => "token Gmail",
            Secret::SaldeoStorageState => "sesja Saldeo",
            Secret::KsefToken => "KSEF_TOKEN",
            Secret::KsefCertPassword => "KSEF_CERT_PASSWORD",
            Secret::KsefAccessToken => "cache tokenu dostępowego KSeF",
            Secret::SaldeoUsername => "SALDEO_USERNAME",
            Secret::SaldeoPassword => "SALDEO_PASSWORD",
            Secret::OpenRouterApiKey => "OPENROUTER_API_KEY",
        }
    }

    /// Plik zapasowy 0600; tylko sekrety, które mają taką konwencję.
    pub(crate) fn file_path(self) -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(ctx) = testing::current() {
            return self.file_name().map(|name| ctx.root.join(name));
        }
        match self {
            Secret::GmailToken => Some(default_gmail_token_path()),
            Secret::SaldeoStorageState => Some(default_saldeo_storage_state_path()),
            Secret::KsefAccessToken => Some(default_ksef_access_token_path()),
            Secret::KsefToken
            | Secret::KsefCertPassword
            | Secret::SaldeoUsername
            | Secret::SaldeoPassword
            | Secret::OpenRouterApiKey => None,
        }
    }

    #[cfg(test)]
    fn file_name(self) -> Option<&'static str> {
        match self {
            Secret::GmailToken => Some("gmail_token.json"),
            Secret::SaldeoStorageState => Some("saldeo-storage-state.json"),
            Secret::KsefAccessToken => Some("ksef_access_token.json"),
            Secret::KsefToken
            | Secret::KsefCertPassword
            | Secret::SaldeoUsername
            | Secret::SaldeoPassword
            | Secret::OpenRouterApiKey => None,
        }
    }

    /// Po udanym zapisie w Keychain plik zapasowy jest zbędny.
    fn drops_file_after_store(self) -> bool {
        matches!(self, Secret::GmailToken | Secret::KsefAccessToken)
    }
}

pub(crate) fn saldeo_login_pair() -> Result<Option<(String, String)>> {
    let username = secret_value(Secret::SaldeoUsername)?;
    let password = secret_value(Secret::SaldeoPassword)?;
    match (username, password) {
        (Some(username), Some(password)) => Ok(Some((username, password))),
        (None, None) => Ok(None),
        _ => Err(anyhow!(
            "Saldeo: ustaw zarówno SALDEO_USERNAME jak i SALDEO_PASSWORD"
        )),
    }
}

/// Kolejność: env procesu → Keychain → ~/.config/lab/env (z migracją) → plik 0600.
pub(crate) fn secret_value(secret: Secret) -> Result<Option<String>> {
    Ok(resolve_secret(secret)?.0)
}

pub(crate) fn secret_source(secret: Secret) -> SecretSource {
    resolve_secret(secret)
        .map(|(_, source)| source)
        .unwrap_or(SecretSource::Missing)
}

pub(crate) fn secret_is_set(secret: Secret) -> bool {
    secret_source(secret).is_set()
}

fn resolve_secret(secret: Secret) -> Result<(Option<String>, SecretSource)> {
    if let Some(key) = secret.env_key()
        && let Some(value) = process_env(key)
    {
        return Ok((Some(value), SecretSource::Env));
    }

    if let Some(value) = store_get(secret) {
        let _ = drop_secret_keys_from_env_file();
        return Ok((Some(value), SecretSource::Keychain));
    }

    if let Some(key) = secret.env_key()
        && let Some(value) = env_file_secret(key)
    {
        let stored = migrate_env_file_secret(secret, key, &value)?;
        let source = if stored {
            SecretSource::Keychain
        } else {
            SecretSource::File
        };
        return Ok((Some(value), source));
    }

    if let Some(path) = secret.file_path().filter(|path| path.is_file()) {
        let text = read_secret_file(&path, secret.label())?;
        if !text.trim().is_empty() {
            let _ = store_set(secret, &text);
            return Ok((Some(text), SecretSource::File));
        }
    }

    Ok((None, SecretSource::Missing))
}

/// Zapis sekretu: Keychain, a przy jego braku plik 0600 — o ile sekret ma plik.
pub(crate) fn save_secret(secret: Secret, value: &str) -> Result<SecretSource> {
    if value.trim().is_empty() {
        return Err(anyhow!("{} jest puste; nie zapisuję", secret.label()));
    }

    let failure = match store_set(secret, value) {
        Ok(true) => None,
        Ok(false) => Some(anyhow!("Keychain jest niedostępny")),
        Err(err) => Some(err),
    };

    let Some(failure) = failure else {
        if let Some(path) = secret
            .file_path()
            .filter(|path| secret.drops_file_after_store() && path.is_file())
        {
            let _ = fs::remove_file(&path);
        }
        return Ok(SecretSource::Keychain);
    };

    let Some(path) = secret.file_path() else {
        return Err(failure.context(format!(
            "nie mogę zapisać {} w Keychain; plik env nie przechowuje sekretów",
            secret.label()
        )));
    };
    write_private_file(&path, value.as_bytes())?;
    Ok(SecretSource::File)
}

/// Usuwa sekrety z mapy pliku env i przenosi je do Keychain.
pub(crate) fn strip_secret_env_keys(vars: &mut HashMap<String, String>) -> Result<()> {
    for key in SECRET_ENV_KEYS {
        let Some(value) = vars.remove(key) else {
            continue;
        };
        let Some(secret) = Secret::from_env_key(key) else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        save_secret(secret, &value)?;
    }
    Ok(())
}

/// Odczyt pliku sekretu; plik czytelny dla innych jest odrzucany.
pub(crate) fn read_secret_file(path: &Path, what: &str) -> Result<String> {
    ensure_private_mode(path, what)?;
    fs::read_to_string(path).with_context(|| format!("odczyt {what} {}", path.display()))
}

fn ensure_private_mode(path: &Path, what: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)
            .with_context(|| format!("odczyt praw {}", path.display()))?
            .permissions()
            .mode()
            & 0o777;
        if mode & 0o077 != 0 {
            return Err(anyhow!(
                "{what}: plik {} ma prawa {mode:o}; wymagane 600 (chmod 600 {})",
                path.display(),
                path.display()
            ));
        }
    }
    let _ = (path, what);
    Ok(())
}

fn drop_secret_keys_from_env_file() -> Result<()> {
    let mut vars = read_lab_env_file()?;
    let mut changed = false;
    for key in SECRET_ENV_KEYS {
        if vars.remove(key).is_some() {
            changed = true;
        }
    }
    if changed {
        write_lab_env_file(&vars)?;
    }
    Ok(())
}

fn migrate_env_file_secret(secret: Secret, key: &str, value: &str) -> Result<bool> {
    if !store_set(secret, value).unwrap_or(false) {
        return Ok(false);
    }
    let mut vars = read_lab_env_file()?;
    if vars.remove(key).is_some() {
        write_lab_env_file(&vars)?;
    }
    Ok(true)
}

fn env_file_secret(key: &str) -> Option<String> {
    read_lab_env_file()
        .ok()?
        .remove(key)
        .filter(|value| !value.trim().is_empty())
}

fn process_env(key: &str) -> Option<String> {
    #[cfg(test)]
    if let Some(ctx) = testing::current() {
        return ctx.env.get(key).cloned().filter(|v| !v.trim().is_empty());
    }
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn store_get(secret: Secret) -> Option<String> {
    #[cfg(test)]
    if let Some(ctx) = testing::current() {
        return ctx
            .get(secret.account())
            .ok()
            .flatten()
            .filter(|v| !v.trim().is_empty());
    }
    keychain_get_secret(secret.account())
        .ok()
        .flatten()
        .filter(|value| !value.trim().is_empty())
}

fn store_set(secret: Secret, value: &str) -> Result<bool> {
    #[cfg(test)]
    if let Some(ctx) = testing::current() {
        return ctx.set(secret.account(), value);
    }
    keychain_set_secret(secret.account(), value)
}

/// Ścieżka pliku env na czas testów; produkcyjnie zawsze None.
#[cfg(test)]
pub(crate) fn test_env_file_path() -> Option<PathBuf> {
    testing::current().map(|ctx| ctx.root.join("env"))
}

/// Podmiana magazynu i ścieżek na czas testów; bez kontaktu z Keychain systemu.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(crate) enum StoreMode {
        Available,
        Unavailable,
        Failing,
    }

    pub(crate) struct TestStore {
        pub(crate) root: PathBuf,
        pub(crate) env: HashMap<String, String>,
        mode: StoreMode,
        items: RefCell<HashMap<String, String>>,
    }

    impl TestStore {
        pub(crate) fn get(&self, account: &str) -> Result<Option<String>> {
            match self.mode {
                StoreMode::Failing => Err(anyhow!("Keychain: odczyt status -25293")),
                StoreMode::Unavailable => Ok(None),
                StoreMode::Available => Ok(self.items.borrow().get(account).cloned()),
            }
        }

        pub(crate) fn set(&self, account: &str, secret: &str) -> Result<bool> {
            match self.mode {
                StoreMode::Failing => Err(anyhow!("Keychain: zapis status -25293")),
                StoreMode::Unavailable => Ok(false),
                StoreMode::Available => {
                    self.items
                        .borrow_mut()
                        .insert(account.to_string(), secret.to_string());
                    Ok(true)
                }
            }
        }

        pub(crate) fn stored(&self, account: &str) -> Option<String> {
            self.items.borrow().get(account).cloned()
        }
    }

    thread_local! {
        static CTX: RefCell<Option<Rc<TestStore>>> = const { RefCell::new(None) };
    }

    pub(crate) struct TestStoreGuard;

    impl Drop for TestStoreGuard {
        fn drop(&mut self) {
            CTX.with(|ctx| *ctx.borrow_mut() = None);
        }
    }

    pub(crate) fn current() -> Option<Rc<TestStore>> {
        CTX.with(|ctx| ctx.borrow().clone())
    }

    pub(crate) fn install(
        root: PathBuf,
        env: &[(&str, &str)],
        mode: StoreMode,
    ) -> (Rc<TestStore>, TestStoreGuard) {
        fs::create_dir_all(&root).unwrap();
        let store = Rc::new(TestStore {
            root,
            env: env
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            mode,
            items: RefCell::new(HashMap::new()),
        });
        CTX.with(|ctx| *ctx.borrow_mut() = Some(Rc::clone(&store)));
        (store, TestStoreGuard)
    }
}
