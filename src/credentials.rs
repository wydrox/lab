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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SecretSource {
    Env,
    Keychain,
    File,
    Missing,
    /// Plik z sekretem ma prawa szersze niż 600; LAB go nie użył.
    Insecure(PathBuf),
}

impl SecretSource {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            SecretSource::Env => "env",
            SecretSource::Keychain => "keychain",
            SecretSource::File => "file",
            SecretSource::Missing => "missing",
            SecretSource::Insecure(_) => "insecure",
        }
    }

    pub(crate) fn is_set(&self) -> bool {
        matches!(
            self,
            SecretSource::Env | SecretSource::Keychain | SecretSource::File
        )
    }

    /// Opis problemu do raportu; nigdy nie zawiera wartości sekretu.
    pub(crate) fn problem(&self) -> Option<String> {
        match self {
            SecretSource::Insecure(path) => {
                Some(format!("insecure permissions: {}", path.display()))
            }
            _ => None,
        }
    }
}

/// Gdzie `save_secret` faktycznie zapisał sekret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedSecret {
    pub(crate) file: Option<PathBuf>,
    pub(crate) keychain: bool,
}

impl SavedSecret {
    pub(crate) fn describe(&self) -> String {
        match (&self.file, self.keychain) {
            (Some(path), true) => format!(
                "w {} (plik 600) i w macOS Keychain (lab-cli)",
                path.display()
            ),
            (Some(path), false) => format!("w {} (plik 600)", path.display()),
            (None, true) => "w macOS Keychain (lab-cli)".to_string(),
            (None, false) => "nigdzie".to_string(),
        }
    }
}

/// Plik sekretu czytelny dla innych; komunikat podaje ścieżkę, nigdy wartość.
#[derive(Debug)]
pub(crate) struct InsecureSecretFile {
    pub(crate) path: PathBuf,
    what: String,
    mode: u32,
}

impl std::fmt::Display for InsecureSecretFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: plik {} ma prawa {:o}; wymagane 600 (chmod 600 {})",
            self.what,
            self.path.display(),
            self.mode,
            self.path.display()
        )
    }
}

impl std::error::Error for InsecureSecretFile {}

fn insecure_secret_file(err: &anyhow::Error) -> Option<&InsecureSecretFile> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<InsecureSecretFile>())
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
}

/// Para loginu Saldeo. Samo `SALDEO_USERNAME` (albo samo hasło) to brak danych logowania:
/// LAB loguje ręcznie w Helium i ostrzega na stderr, zamiast przerywać.
pub(crate) fn saldeo_login_pair() -> Result<Option<(String, String)>> {
    let username = secret_value(Secret::SaldeoUsername)?;
    let password = secret_value(Secret::SaldeoPassword)?;
    match (username, password) {
        (Some(username), Some(password)) => Ok(Some((username, password))),
        (None, None) => Ok(None),
        (Some(_), None) => {
            warn_once(
                "saldeo-login:username-only".to_string(),
                "Saldeo: jest SALDEO_USERNAME bez SALDEO_PASSWORD; loguję ręcznie w Helium (ustaw hasło w lab onboard)",
            );
            Ok(None)
        }
        (None, Some(_)) => {
            warn_once(
                "saldeo-login:password-only".to_string(),
                "Saldeo: jest SALDEO_PASSWORD bez SALDEO_USERNAME; loguję ręcznie w Helium (ustaw login w lab onboard)",
            );
            Ok(None)
        }
    }
}

/// Kolejność: env procesu → `.env` → plik 0600 → opcjonalnie Keychain (`LAB_USE_KEYCHAIN=1`).
pub(crate) fn secret_value(secret: Secret) -> Result<Option<String>> {
    Ok(resolve_secret(secret)?.0)
}

/// Źródło sekretu do raportu. Plik z za szerokimi prawami daje `Insecure`, nie `Missing`.
pub(crate) fn secret_source(secret: Secret) -> SecretSource {
    match resolve_secret(secret) {
        Ok((_, source)) => source,
        Err(err) => {
            if let Some(insecure) = insecure_secret_file(&err) {
                return SecretSource::Insecure(insecure.path.clone());
            }
            warn_once(
                format!("secret:{}", secret.account()),
                &format!("pomijam {}: {err:#}", secret.label()),
            );
            SecretSource::Missing
        }
    }
}

pub(crate) fn secret_is_set(secret: Secret) -> bool {
    secret_source(secret).is_set()
}

/// Start procesu. Przy `LAB_USE_KEYCHAIN=1` odtwarza z Keychain brakujące pliki 600
/// (token Gmail, sesja Saldeo, cache KSeF). Sekretów tekstowych nie kopiuje do `.env`.
pub(crate) fn prepare_secret_store() {
    if !keychain_enabled() {
        return;
    }
    let _ = export_file_backed_secrets();
}

fn resolve_secret(secret: Secret) -> Result<(Option<String>, SecretSource)> {
    if let Some(key) = secret.env_key() {
        if let Some(value) = process_env(key) {
            return Ok((Some(value), SecretSource::Env));
        }
        if let Some(value) = dotenv_lookup(key)? {
            return Ok((Some(value), SecretSource::File));
        }
    }

    if let Some(path) = secret.file_path().filter(|path| path.is_file()) {
        let text = read_secret_file(&path, secret.label())?;
        if !text.trim().is_empty() {
            return Ok((Some(text), SecretSource::File));
        }
    }

    if let Some(key) = secret.env_key()
        && let Some(value) = env_file_lookup(key)?
    {
        let _ = migrate_env_file_secret(key, &value);
        return Ok((Some(value), SecretSource::File));
    }

    // Trafienie w Keychain nie jest kopiowane do `.env`.
    if keychain_enabled()
        && let Some(value) = store_get(secret)
    {
        return Ok((Some(value), SecretSource::Keychain));
    }

    Ok((None, SecretSource::Missing))
}

/// Zapis sekretu: `.env` albo plik 0600; przy `LAB_USE_KEYCHAIN=1` także kopia w Keychain.
/// Wynik mówi, gdzie sekret faktycznie trafił.
pub(crate) fn save_secret(secret: Secret, value: &str) -> Result<SavedSecret> {
    if value.trim().is_empty() {
        return Err(anyhow!("{} jest puste; nie zapisuję", secret.label()));
    }
    // Sekret tekstowy to jedna linia `.env`; pliki 0600 (JSON) mogą mieć wiele linii.
    if secret.env_key().is_some() {
        ensure_single_line_value(secret.label(), value)?;
    }

    let file = if let Some(key) = secret.env_key() {
        upsert_dotenv_secret(key, value)?;
        Some(lab_dotenv_path())
    } else if let Some(path) = secret.file_path() {
        write_private_file(&path, value.as_bytes())?;
        Some(path)
    } else {
        None
    };
    let keychain = keychain_enabled() && store_set(secret, value).unwrap_or(false);
    if file.is_none() && !keychain {
        return Err(anyhow!(
            "nie mogę zapisać {}; użyj .env albo pliku 0600",
            secret.label()
        ));
    }
    Ok(SavedSecret { file, keychain })
}

pub(crate) fn keychain_enabled() -> bool {
    env_flag("LAB_USE_KEYCHAIN")
}

/// `LAB_NONINTERACTIVE=1`: przebieg bez użytkownika (launchd); LAB nie otwiera przeglądarki
/// i nie instaluje Playwright.
pub(crate) fn lab_noninteractive() -> bool {
    env_flag("LAB_NONINTERACTIVE")
}

fn env_flag(name: &str) -> bool {
    let enabled = |value: &str| value == "1" || value.eq_ignore_ascii_case("true");
    #[cfg(test)]
    if let Some(ctx) = testing::current() {
        return ctx.env.get(name).is_some_and(|value| enabled(value));
    }
    std::env::var(name)
        .ok()
        .is_some_and(|value| enabled(&value))
}

/// Wartość pliku `KLUCZ=wartość` musi zmieścić się w jednej linii; inaczej zapis
/// rozbiłby plik na fałszywe klucze. Komunikat nie zawiera wartości.
pub(crate) fn ensure_single_line_value(what: &str, value: &str) -> Result<()> {
    if value.contains(['\n', '\r', '\0']) {
        return Err(anyhow!(
            "{what} zawiera znak nowej linii albo NUL; nie zapisuję (usuń go i spróbuj ponownie)"
        ));
    }
    Ok(())
}

/// Usuwa jedno końcowe `\n`, `\r\n` albo `\r` (wklejony tekst, wpis z Keychain).
pub(crate) fn strip_one_line_ending(value: &str) -> &str {
    value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .or_else(|| value.strip_suffix('\r'))
        .unwrap_or(value)
}

/// Usuwa sekrety z mapy pliku env i zapisuje je przez `save_secret` (`.env`).
/// Klucz, który ma już niepustą wartość w `.env`, nie jest nadpisywany starą kopią z env:
/// znika tylko z pliku env (komunikat na stderr, bez wartości).
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
        if dotenv_has_value(key)? {
            eprintln!(
                "  [LAB] {key} jest już w {}; usuwam starszą kopię z {} bez nadpisywania",
                lab_dotenv_path().display(),
                lab_env_file_path().display()
            );
            continue;
        }
        save_secret(secret, &value)?;
    }
    Ok(())
}

/// Czy `.env` ma niepustą wartość klucza. Nieczytelny `.env` to błąd (jak przy zapisie).
fn dotenv_has_value(key: &str) -> Result<bool> {
    let vars = read_dotenv().with_context(|| {
        format!(
            "nie przenoszę {key}: nie mogę odczytać obecnego pliku {}",
            lab_dotenv_path().display()
        )
    })?;
    Ok(vars.get(key).is_some_and(|value| !value.trim().is_empty()))
}

/// Odczyt pliku sekretu; plik czytelny dla innych jest odrzucany.
pub(crate) fn read_secret_file(path: &Path, what: &str) -> Result<String> {
    ensure_private_mode(path, what)?;
    fs::read_to_string(path).with_context(|| format!("odczyt {what} {}", path.display()))
}

/// Dla dowiązania symbolicznego sprawdza prawa jego celu.
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
            return Err(anyhow::Error::new(InsecureSecretFile {
                path: path.to_path_buf(),
                what: what.to_string(),
                mode,
            }));
        }
    }
    let _ = (path, what);
    Ok(())
}

/// Odczyt pliku `KLUCZ=wartość` z sekretami (`.env`, `~/.config/lab/env`).
/// Brak pliku to pusta mapa; prawa szersze niż 600, błąd odczytu albo tekst spoza UTF-8
/// to błąd podający ścieżkę, nigdy pusta mapa.
pub(crate) fn read_private_env_file(path: &Path, what: &str) -> Result<HashMap<String, String>> {
    match fs::metadata(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => {
            return Err(err).with_context(|| format!("odczyt {what} {}", path.display()));
        }
        Ok(meta) if !meta.is_file() => {
            return Err(anyhow!(
                "{what}: {} nie jest zwykłym plikiem",
                path.display()
            ));
        }
        Ok(_) => {}
    }
    let text = read_secret_file(path, what)?;
    Ok(parse_env_text(&text))
}

/// Komunikat na stderr najwyżej raz na proces dla danego klucza; bez wartości sekretów.
pub(crate) fn warn_once(key: String, message: &str) {
    static WARNED: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let first = warned.lock().map(|mut set| set.insert(key)).unwrap_or(true);
    if first {
        eprintln!("  [LAB] {message}");
    }
}

/// Zwykły odczyt bez zapisu: problem z plikiem = brak wartości plus jednorazowy komunikat.
pub(crate) fn warn_unreadable_secret_file(path: &Path, err: &anyhow::Error) {
    warn_once(
        format!("file:{}", path.display()),
        &format!("pomijam {}: {err:#}", path.display()),
    );
}

/// Odczyt klucza z pliku env do rozwiązywania sekretu. Za szerokie prawa to błąd
/// (plik sekretu jest odrzucany); nieczytelny plik traktujemy jak brak wartości.
fn lookup_private_env_file(path: &Path, what: &str, key: &str) -> Result<Option<String>> {
    match read_private_env_file(path, what) {
        Ok(mut vars) => Ok(vars.remove(key).filter(|value| !value.trim().is_empty())),
        Err(err) if insecure_secret_file(&err).is_some() => Err(err),
        Err(err) => {
            warn_unreadable_secret_file(path, &err);
            Ok(None)
        }
    }
}

fn dotenv_lookup(key: &str) -> Result<Option<String>> {
    lookup_private_env_file(&lab_dotenv_path(), "plik .env", key)
}

fn env_file_lookup(key: &str) -> Result<Option<String>> {
    lookup_private_env_file(&lab_env_file_path(), "plik env LAB", key)
}

fn migrate_env_file_secret(key: &str, value: &str) -> Result<bool> {
    // Nowsza wartość w `.env` wygrywa; stara kopia z env tylko znika.
    if !dotenv_has_value(key)? {
        upsert_dotenv_secret(key, value)?;
    }
    let mut vars = read_lab_env_file()?;
    if vars.remove(key).is_some() {
        write_lab_env_file(&vars)?;
    }
    Ok(true)
}

/// Ścieżka `.env`, liczona raz na proces (wybór lokalnego `.env` pyta Git).
pub(crate) fn lab_dotenv_path() -> PathBuf {
    #[cfg(test)]
    if let Some(ctx) = testing::current() {
        return ctx.root.join(".env");
    }
    static DOTENV_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DOTENV_PATH
        .get_or_init(|| {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            let override_path = std::env::var_os("LAB_DOTENV").map(PathBuf::from);
            select_lab_dotenv_path(&cwd, &home, override_path.as_deref())
        })
        .clone()
}

pub(crate) fn select_lab_dotenv_path(
    cwd: &Path,
    home: &Path,
    override_path: Option<&Path>,
) -> PathBuf {
    if let Some(path) = override_path {
        return path.to_path_buf();
    }
    if cwd_is_lab_package(cwd) && cwd_dotenv_is_untracked_and_ignored(cwd) {
        return cwd.join(".env");
    }
    home.join(".config").join("lab").join(".env")
}

pub(crate) fn cwd_is_lab_package(cwd: &Path) -> bool {
    let Ok(manifest) = fs::read_to_string(cwd.join("Cargo.toml")) else {
        return false;
    };
    // Fail closed for multiline TOML strings, which can contain fake table headers.
    if manifest.contains("\"\"\"") || manifest.contains("'''") {
        return false;
    }
    let package_header = Regex::new(r"^\[package\]\s*(?:#.*)?$").unwrap();
    let package_name = Regex::new(r#"^(?:"lab-cli"|'lab-cli')\s*(?:#.*)?$"#).unwrap();
    let mut in_package = false;
    let mut seen_package = false;
    let mut name_matches = None;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = package_header.is_match(line);
            if in_package {
                if seen_package {
                    return false;
                }
                seen_package = true;
            }
        } else if in_package
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "name"
        {
            if name_matches.is_some() {
                return false;
            }
            name_matches = Some(package_name.is_match(value.trim()));
        }
    }
    name_matches == Some(true)
}

/// Konfiguracja Git z linii poleceń; wygrywa z `.git/config` repozytorium, więc dwa
/// polecenia tylko do odczytu nie uruchomią programów wskazanych przez repozytorium.
pub(crate) const DOTENV_GIT_HARDENING: [&str; 10] = [
    "--no-pager",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.pager=cat",
    "-c",
    "core.untrackedCache=false",
    "--no-optional-locks",
];

fn cwd_dotenv_is_untracked_and_ignored(cwd: &Path) -> bool {
    // Tylko bezwzględna ścieżka z katalogów systemowych; bez Git zostaje ~/.config/lab/.env.
    let Some(git) = crate::hardening::system_tool("git") else {
        return false;
    };
    let Ok(tracked) = dotenv_git_command(&git, cwd, &["ls-files", "--", ".env"]).output() else {
        return false;
    };
    if !tracked.status.success() || !tracked.stdout.is_empty() {
        return false;
    }
    dotenv_git_command(
        &git,
        cwd,
        &["check-ignore", "--quiet", "--no-index", "--", ".env"],
    )
    .stdout(Stdio::null())
    .status()
    .is_ok_and(|status| status.success())
}

/// Polecenie Git do wyboru `.env`: czyste środowisko, flagi z `DOTENV_GIT_HARDENING`.
pub(crate) fn dotenv_git_command(git: &Path, cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(git);
    command.current_dir(cwd).env_clear();
    // HOME i XDG_CONFIG_HOME zostają: globalny gitignore użytkownika nadal się liczy.
    for key in ["HOME", "XDG_CONFIG_HOME", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(DOTENV_GIT_HARDENING)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

pub(crate) fn parse_env_text(text: &str) -> HashMap<String, String> {
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
    vars
}

fn read_dotenv() -> Result<HashMap<String, String>> {
    read_private_env_file(&lab_dotenv_path(), "plik .env")
}

/// Zwykły odczyt klucza z `.env`; problem z plikiem = brak wartości i komunikat na stderr.
pub(crate) fn dotenv_secret(key: &str) -> Option<String> {
    let path = lab_dotenv_path();
    match read_private_env_file(&path, "plik .env") {
        Ok(mut vars) => vars.remove(key).filter(|value| !value.trim().is_empty()),
        Err(err) => {
            warn_unreadable_secret_file(&path, &err);
            None
        }
    }
}

/// Odczyt-modyfikacja-zapis: gdy `.env` istnieje, ale nie da się go odczytać, nic nie zapisuje.
fn upsert_dotenv_secret(key: &str, value: &str) -> Result<()> {
    let mut vars = read_dotenv().with_context(|| {
        format!(
            "nie zapisuję {key}: nie mogę odczytać obecnego pliku {}",
            lab_dotenv_path().display()
        )
    })?;
    vars.insert(key.to_string(), value.to_string());
    write_dotenv(&vars)
}

fn write_dotenv(vars: &HashMap<String, String>) -> Result<()> {
    let path = lab_dotenv_path();
    let mut keys = vars.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    let mut out = String::from("# LAB secrets. Do not commit. chmod 600.\n");
    for key in keys {
        if let Some(value) = vars.get(&key) {
            ensure_single_line_value(&key, value)?;
            out.push_str(&format!("{}={}\n", key, quote_env_value(value)));
        }
    }
    write_private_file(&path, out.as_bytes())
}

fn export_file_backed_secrets() -> Result<usize> {
    let mut added = 0usize;
    for secret in [
        Secret::GmailToken,
        Secret::SaldeoStorageState,
        Secret::KsefAccessToken,
    ] {
        let Some(path) = secret.file_path() else {
            continue;
        };
        if path.is_file() {
            continue;
        }
        let Some(value) = store_get(secret) else {
            continue;
        };
        write_private_file(&path, value.as_bytes())?;
        added += 1;
    }
    Ok(added)
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
            .map(|v| strip_one_line_ending(&v).to_string())
            .filter(|v| !v.trim().is_empty());
    }
    if !keychain_enabled() {
        return None;
    }
    // Wpis dodany ręcznie poza LAB (np. wklejka) bywa zakończony `\n`.
    keychain_get_secret(secret.account())
        .ok()
        .flatten()
        .map(|value| strip_one_line_ending(&value).to_string())
        .filter(|value| !value.trim().is_empty())
}

fn store_set(secret: Secret, value: &str) -> Result<bool> {
    #[cfg(test)]
    if let Some(ctx) = testing::current() {
        return ctx.set(secret.account(), value);
    }
    if !keychain_enabled() {
        return Ok(false);
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

    /// Jedna blokada na cały crate dla testów, które zmieniają środowisko procesu
    /// (`set_var`/`remove_var`). Trzymaj ją przez cały czas zmiany, łącznie z przywróceniem
    /// wartości; zatruta blokada (panika innego testu) nadal działa.
    pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
