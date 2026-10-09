use crate::*;

#[derive(Debug, Deserialize)]
struct GoogleClientSecretFile {
    pub(crate) installed: Option<GoogleClientSecret>,
    web: Option<GoogleClientSecret>,
}

#[derive(Debug, Deserialize)]
struct GoogleClientSecret {
    client_id: String,
    pub(crate) client_secret: String,
    auth_uri: String,
    token_uri: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct GmailTokenFile {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    token_type: Option<String>,
    pub(crate) expires_at: Option<DateTime<Utc>>,
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct GmailAuthResult {
    pub(crate) token_file: String,
    refresh_token_saved: bool,
    pub(crate) expires_at: Option<DateTime<Utc>>,
    scope: String,
}

pub(crate) fn default_gmail_token_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("lab")
        .join("gmail_token.json")
}

fn read_google_client_secret(path: &Path) -> Result<GoogleClientSecret> {
    let text = fs::read_to_string(path).with_context(|| format!("odczyt {}", path.display()))?;
    let file: GoogleClientSecretFile = serde_json::from_str(&text)
        .with_context(|| format!("niepoprawny Google client secret JSON: {}", path.display()))?;
    file.installed
        .or(file.web)
        .ok_or_else(|| anyhow!("client secret JSON musi mieć sekcję installed albo web"))
}

pub(crate) fn gmail_auth(
    client_secret_path: &Path,
    token_file: &Path,
    no_browser: bool,
) -> Result<GmailAuthResult> {
    let secret = read_google_client_secret(client_secret_path)?;
    let listener =
        TcpListener::bind("127.0.0.1:0").context("uruchomienie lokalnego OAuth listenera")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let scope = "https://www.googleapis.com/auth/gmail.readonly";
    let state = oauth_state()?;
    let code_verifier = pkce_code_verifier()?;
    let auth_url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}&code_challenge={}&code_challenge_method=S256",
        secret.auth_uri,
        urlencoding::encode(&secret.client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(scope),
        urlencoding::encode(&state),
        pkce_code_challenge(&code_verifier),
    );

    if no_browser || oauth_browser_command(&auth_url).status().is_err() {
        eprintln!("Otwórz URL w przeglądarce:\n{auth_url}");
    }

    let code = wait_for_oauth_code(&listener, &state, OAUTH_CALLBACK_TIMEOUT)?;

    let client = Client::builder().build()?;
    let response = client
        .post(&secret.token_uri)
        .form(&[
            ("code", code.as_str()),
            ("client_id", secret.client_id.as_str()),
            ("client_secret", secret.client_secret.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", code_verifier.as_str()),
        ])
        .send()
        .context("wymiana kodu autoryzacji Gmail na token")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(gmail_code_exchange_error(status.as_u16(), &body));
    }
    let token_response: TokenResponse = response
        .json()
        .context("odpowiedź wymiany kodu autoryzacji Gmail")?;
    let token = token_response_to_file(token_response, None);
    save_gmail_token(token_file, &token)?;

    Ok(GmailAuthResult {
        token_file: token_file.display().to_string(),
        refresh_token_saved: token.refresh_token.is_some(),
        expires_at: token.expires_at,
        scope: scope.to_string(),
    })
}

// The browser needs none of LAB's secrets in its environment.
fn oauth_browser_command(auth_url: &str) -> Command {
    let mut command = Command::new("open");
    command.arg(auth_url);
    crate::hardening::remove_secret_env(&mut command);
    command
}

const OAUTH_CALLBACK_PATH: &str = "/callback";

const OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[cfg(test)]
mod gmail_tests;

fn oauth_random_bytes() -> Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .context("odczyt /dev/urandom dla OAuth")?;
    Ok(bytes)
}

fn oauth_state() -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(oauth_random_bytes()?))
}

// RFC 7636: 32 random bytes give a 43-character verifier.
fn pkce_code_verifier() -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(oauth_random_bytes()?))
}

fn pkce_code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[derive(Debug, PartialEq, Eq)]
enum OAuthRequest {
    /// Not the callback (favicon, probe, other path): answer 404 and keep waiting.
    NotFound,
    /// Callback without the expected `state`: reject it and keep waiting.
    BadState,
    /// Callback with the expected `state` but no code (e.g. consent denied).
    Failed(String),
    Code(String),
}

fn classify_oauth_request(request_line: &str, expected_state: &str) -> OAuthRequest {
    let mut parts = request_line.split_whitespace();
    let (Some("GET"), Some(target)) = (parts.next(), parts.next()) else {
        return OAuthRequest::NotFound;
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != OAUTH_CALLBACK_PATH {
        return OAuthRequest::NotFound;
    }
    let params = parse_query(query);
    if params.get("state").map(String::as_str) != Some(expected_state) {
        return OAuthRequest::BadState;
    }
    if let Some(error) = params.get("error") {
        return OAuthRequest::Failed(format!("OAuth error: {error}"));
    }
    match params.get("code").filter(|code| !code.is_empty()) {
        Some(code) => OAuthRequest::Code(code.clone()),
        None => OAuthRequest::Failed("OAuth redirect nie zawiera code".to_string()),
    }
}

// Waits for the browser redirect. Stray connections (favicon, port probes, forged
// callbacks) are answered and ignored; only the callback with our `state` ends it.
fn wait_for_oauth_code(
    listener: &TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String> {
    listener
        .set_nonblocking(true)
        .context("ustawienie OAuth listenera")?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let (stream, peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err(anyhow!(
                        "OAuth: brak odpowiedzi z przeglądarki w ciągu {} min; uruchom ponownie `lab onboard` (krok Gmail)",
                        timeout.as_secs().div_ceil(60)
                    ));
                }
                sleep(Duration::from_millis(50));
                continue;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err).context("oczekiwanie na redirect OAuth"),
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        match handle_oauth_connection(stream, expected_state) {
            Ok(OAuthRequest::Code(code)) => return Ok(code),
            Ok(OAuthRequest::Failed(message)) => return Err(anyhow!(message)),
            Ok(OAuthRequest::NotFound | OAuthRequest::BadState) | Err(_) => continue,
        }
    }
}

fn handle_oauth_connection(
    mut stream: std::net::TcpStream,
    expected_state: &str,
) -> Result<OAuthRequest> {
    // On macOS an accepted socket inherits the listener's non-blocking mode.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buffer.contains(&b'\n') && buffer.len() < 8192 {
        let len = stream.read(&mut chunk)?;
        if len == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..len]);
    }
    let request = String::from_utf8_lossy(&buffer);
    let request_line = request.lines().next().unwrap_or_default();
    let outcome = classify_oauth_request(request_line, expected_state);
    let (status, body) = match &outcome {
        OAuthRequest::Code(_) => (
            "200 OK",
            "Autoryzacja Gmail zakończona. Możesz wrócić do terminala.",
        ),
        OAuthRequest::Failed(_) => (
            "200 OK",
            "Autoryzacja Gmail nie powiodła się. Wróć do terminala.",
        ),
        OAuthRequest::BadState => (
            "400 Bad Request",
            "Nieprawidłowy parametr state. Wróć do terminala.",
        ),
        OAuthRequest::NotFound => ("404 Not Found", "Not found"),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // The browser may already have closed the tab; the outcome still counts.
    let _ = stream.write_all(response.as_bytes());
    Ok(outcome)
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((
                urlencoding::decode(key).ok()?.to_string(),
                urlencoding::decode(value).ok()?.to_string(),
            ))
        })
        .collect()
}

fn token_response_to_file(
    response: TokenResponse,
    previous_refresh_token: Option<String>,
) -> GmailTokenFile {
    GmailTokenFile {
        access_token: response.access_token,
        refresh_token: response.refresh_token.or(previous_refresh_token),
        token_type: response.token_type,
        expires_at: response
            .expires_in
            .map(|seconds| Utc::now() + chrono::Duration::seconds(seconds)),
        scope: response.scope,
    }
}

fn save_gmail_token(path: &Path, token: &GmailTokenFile) -> Result<()> {
    let text = serde_json::to_string_pretty(token)?;
    if uses_default_gmail_token_path(path) {
        save_secret(Secret::GmailToken, &text)?;
        return Ok(());
    }

    write_private_file(path, text.as_bytes())
}

pub(crate) fn read_gmail_token(path: &Path) -> Result<GmailTokenFile> {
    if uses_default_gmail_token_path(path) {
        let text = secret_value(Secret::GmailToken)?.ok_or_else(|| {
            anyhow!(
                "brak tokenu Gmail w Keychain i w {}; uruchom lab onboard",
                path.display()
            )
        })?;
        return serde_json::from_str(&text).context("niepoprawny token Gmail");
    }

    let text = read_secret_file(path, "token Gmail")?;
    serde_json::from_str(&text)
        .with_context(|| format!("niepoprawny token Gmail {}", path.display()))
}

fn uses_default_gmail_token_path(path: &Path) -> bool {
    path == default_gmail_token_path().as_path()
}

const GMAIL_REFRESH_MARGIN_SECS: i64 = 60;

type GmailTokenRefresher = Box<dyn Fn(&GmailTokenFile) -> Result<GmailTokenFile>>;

/// Gmail access token for one run. A first sync of a year can outlive the ~1 h
/// token, so it is refreshed shortly before expiry and once after an HTTP 401.
pub(crate) struct GmailAccessToken {
    pub(crate) token: std::cell::RefCell<GmailTokenFile>,
    /// None for a token from the environment, which cannot be refreshed.
    refresher: Option<GmailTokenRefresher>,
}

impl GmailAccessToken {
    fn fixed(access_token: String) -> Self {
        Self {
            token: std::cell::RefCell::new(GmailTokenFile {
                access_token,
                refresh_token: None,
                token_type: None,
                expires_at: None,
                scope: None,
            }),
            refresher: None,
        }
    }

    /// Token for the next request; refreshed first when the known expiry is near.
    pub(crate) fn current(&self) -> Result<String> {
        let expiring = self.token.borrow().expires_at.is_some_and(|expires_at| {
            expires_at <= Utc::now() + chrono::Duration::seconds(GMAIL_REFRESH_MARGIN_SECS)
        });
        if expiring && self.refresher.is_some() {
            return self.refresh();
        }
        Ok(self.token.borrow().access_token.clone())
    }

    pub(crate) fn refresh(&self) -> Result<String> {
        let refresher = self.refresher.as_ref().ok_or_else(|| {
            anyhow!(
                "Gmail odrzucił token dostępu (HTTP 401), a tokenu podanego w zmiennej środowiskowej nie da się odświeżyć; uruchom ponownie `lab onboard` (krok Gmail)"
            )
        })?;
        let next = refresher(&self.token.borrow())?;
        let access_token = next.access_token.clone();
        *self.token.borrow_mut() = next;
        Ok(access_token)
    }
}

// Unknown expiry counts as expired when the token can be refreshed.
fn gmail_token_needs_refresh(token: &GmailTokenFile, now: DateTime<Utc>) -> bool {
    match token.expires_at {
        Some(expires_at) => {
            expires_at <= now + chrono::Duration::seconds(GMAIL_REFRESH_MARGIN_SECS)
        }
        None => token.refresh_token.is_some(),
    }
}

pub(crate) fn gmail_access_token(
    token_env: &str,
    token_file: &Path,
    client_secret_path: Option<&Path>,
) -> Result<GmailAccessToken> {
    if let Ok(token) = std::env::var(token_env)
        && !token.trim().is_empty()
    {
        return Ok(GmailAccessToken::fixed(token));
    }

    let token = read_gmail_token(token_file).with_context(|| {
        format!(
            "brak {token_env} i tokenu Gmail ({}); uruchom `lab onboard` (krok Gmail)",
            token_file.display()
        )
    })?;
    let needs_refresh = gmail_token_needs_refresh(&token, Utc::now());
    let client_secret_path = client_secret_path
        .map(PathBuf::from)
        .or_else(|| lab_config_var("GOOGLE_CLIENT_SECRET_PATH").map(PathBuf::from));
    let token_file = token_file.to_path_buf();
    let access = GmailAccessToken {
        token: std::cell::RefCell::new(token),
        refresher: Some(Box::new(move |previous| {
            let client_secret_path = client_secret_path.as_deref().ok_or_else(|| {
                anyhow!("token Gmail wygasł; podaj --gmail-client-secret albo ustaw GOOGLE_CLIENT_SECRET_PATH w lab onboard, żeby go odświeżyć")
            })?;
            refresh_gmail_token(client_secret_path, &token_file, previous.clone())
        })),
    };
    if needs_refresh {
        access.refresh()?;
    }
    Ok(access)
}

fn refresh_gmail_token(
    client_secret_path: &Path,
    token_file: &Path,
    previous: GmailTokenFile,
) -> Result<GmailTokenFile> {
    let refresh_token = previous.refresh_token.clone().ok_or_else(|| {
        anyhow!(
            "token Gmail nie zawiera refresh_token; uruchom ponownie `lab onboard` (krok Gmail)"
        )
    })?;
    let secret = read_google_client_secret(client_secret_path)?;
    let client = Client::builder().build()?;
    let response = client
        .post(&secret.token_uri)
        .form(&[
            ("client_id", secret.client_id.as_str()),
            ("client_secret", secret.client_secret.as_str()),
            ("refresh_token", refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .context("odświeżenie tokenu Gmail")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(gmail_refresh_error(status.as_u16(), &body));
    }
    let response: TokenResponse = response
        .json()
        .context("odpowiedź odświeżenia tokenu Gmail")?;
    let token = token_response_to_file(response, previous.refresh_token);
    save_gmail_token(token_file, &token)?;
    Ok(token)
}

// Google's OAuth `error` code from a token endpoint answer; anything that does
// not look like a code is dropped, so no part of the body is echoed.
fn google_oauth_error_code(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("error")
        .and_then(Value::as_str)
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 64
                && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .map(str::to_string)
}

// Google answers 400 `invalid_grant` for an expired or revoked refresh token and
// 401 for a deleted client; both need a new consent. Only the error code is shown.
fn gmail_refresh_error(status: u16, body: &str) -> anyhow::Error {
    let detail = google_oauth_error_code(body)
        .map(|code| format!(", {code}"))
        .unwrap_or_default();
    if matches!(status, 400 | 401 | 403) {
        anyhow!(
            "autoryzacja Gmail wygasła albo została cofnięta (HTTP {status}{detail}); uruchom ponownie `lab onboard` (krok Gmail)"
        )
    } else {
        anyhow!("odświeżenie tokenu Gmail nie powiodło się (HTTP {status}{detail})")
    }
}

// The first exchange fails with 400 `invalid_grant` for a used or expired code,
// `redirect_uri_mismatch` or 401 `invalid_client` for a wrong client secret file.
// Google's short `error_description` ("Code was already redeemed.") tells these
// apart, so it is shown when it is one plain line; the body itself never is.
fn gmail_code_exchange_error(status: u16, body: &str) -> anyhow::Error {
    let description = serde_json::from_str::<Value>(body).ok().and_then(|value| {
        value
            .get("error_description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| {
                !text.is_empty()
                    && text.len() <= 120
                    && text.chars().all(|c| c.is_ascii_graphic() || c == ' ')
            })
            .map(str::to_string)
    });
    let detail = match (google_oauth_error_code(body), description) {
        (Some(code), Some(description)) => format!(", {code}: {description}"),
        (Some(code), None) => format!(", {code}"),
        (None, _) => String::new(),
    };
    anyhow!(
        "Google odrzucił wymianę kodu autoryzacji Gmail na token (HTTP {status}{detail}); uruchom ponownie `lab onboard` (krok Gmail)"
    )
}

/// Runs one Gmail API call. `Ok(None)` from the call means HTTP 401: the token is
/// refreshed once and the call repeated; a second 401 is an error, not a loop.
fn gmail_call_with_refresh<T>(
    token: &GmailAccessToken,
    mut call: impl FnMut(&str) -> Result<Option<T>>,
) -> Result<T> {
    if let Some(value) = call(&token.current()?)? {
        return Ok(value);
    }
    let access_token = token.refresh()?;
    call(&access_token)?.ok_or_else(|| {
        anyhow!(
            "Gmail odrzucił odświeżony token (HTTP 401); uruchom ponownie `lab onboard` (krok Gmail)"
        )
    })
}

fn gmail_get_json(
    client: &Client,
    token: &GmailAccessToken,
    url: &str,
    query: &[(&str, &str)],
) -> Result<Value> {
    gmail_call_with_refresh(token, |access_token| {
        let response = client
            .get(url)
            .bearer_auth(access_token)
            .query(query)
            .send()?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Ok(None);
        }
        Ok(Some(response.error_for_status()?.json()?))
    })
}

#[derive(Debug, Serialize)]
pub(crate) struct GmailFetchResult {
    pub(crate) query: String,
    pub(crate) messages_seen: usize,
    pub(crate) messages_cached: usize,
    pub(crate) messages_fetched: usize,
    pub(crate) metadata_saved: usize,
    pub(crate) attachments_saved: usize,
    files_saved: usize,
    pub(crate) out_dir: String,
    pub(crate) saved_files: Vec<String>,
}

pub(crate) fn gmail_fetch(
    token: &GmailAccessToken,
    user: &str,
    query: &str,
    out_dir: &Path,
    max: usize,
    extensions: &[String],
    progress: Option<Arc<Mutex<String>>>,
) -> Result<GmailFetchResult> {
    if let Some(progress) = &progress {
        set_progress(progress, "Gmail: przygotowanie katalogu i klienta...");
    }
    fs::create_dir_all(out_dir).with_context(|| format!("mkdir {}", out_dir.display()))?;
    let removed = remove_stale_temp_files(out_dir, std::time::SystemTime::now());
    if removed > 0 {
        eprintln!("  [Gmail] usunięte pliki tymczasowe po przerwanym zapisie: {removed}");
    }
    let client = Client::builder().build()?;
    let allowed_exts = extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut page_token: Option<String> = None;
    let mut message_ids = Vec::new();

    while message_ids.len() < max {
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Gmail: lista wiadomości (znaleziono {})...",
                    message_ids.len()
                ),
            );
        }
        let mut params = vec![("q", query), ("maxResults", "100")];
        if let Some(page_token) = &page_token {
            params.push(("pageToken", page_token.as_str()));
        }
        let value = gmail_get_json(
            &client,
            token,
            &format!(
                "https://gmail.googleapis.com/gmail/v1/users/{}/messages",
                user
            ),
            &params,
        )?;
        if let Some(messages) = value.get("messages").and_then(|v| v.as_array()) {
            for msg in messages {
                if let Some(id) = msg.get("id").and_then(|v| v.as_str())
                    && message_ids.len() < max
                {
                    message_ids.push(id.to_string());
                }
            }
        }
        page_token = value
            .get("nextPageToken")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Gmail: lista wiadomości: {} znalezionych",
                    message_ids.len()
                ),
            );
        }
        if page_token.is_none() {
            break;
        }
    }

    let mut saved_files = Vec::new();
    let mut messages_cached = 0usize;
    let mut messages_fetched = 0usize;
    let mut metadata_saved = 0usize;
    let mut attachments_saved = 0usize;
    for id in &message_ids {
        let metadata_path = out_dir.join(format!("{}_message.json", sanitize_filename(id)));
        if gmail_message_cached(out_dir, id, &allowed_exts) {
            messages_cached += 1;
            if let Some(progress) = &progress {
                set_progress(
                    progress,
                    format!(
                        "Gmail: cache {}/{} wiadomości, API {}, pliki {}",
                        messages_cached,
                        message_ids.len(),
                        messages_fetched,
                        saved_files.len()
                    ),
                );
            }
            continue;
        }

        messages_fetched += 1;
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Gmail: pobieram wiadomość {}/{} (cache {}, API {})...",
                    messages_cached + messages_fetched,
                    message_ids.len(),
                    messages_cached,
                    messages_fetched
                ),
            );
        }
        let msg = gmail_get_json(
            &client,
            token,
            &format!(
                "https://gmail.googleapis.com/gmail/v1/users/{}/messages/{}",
                user, id
            ),
            &[("format", "full")],
        )?;

        let stored = gmail_store_message(out_dir, id, &msg, &allowed_exts, |attachment_id| {
            let attachment = gmail_get_json(
                &client,
                token,
                &format!(
                    "https://gmail.googleapis.com/gmail/v1/users/{}/messages/{}/attachments/{}",
                    user, id, attachment_id
                ),
                &[],
            )?;
            Ok(attachment
                .get("data")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()))
        })?;
        attachments_saved += stored.len();
        saved_files.extend(stored);
        saved_files.push(metadata_path.display().to_string());
        metadata_saved += 1;
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Gmail: zapisano {} metadanych i {} załączników",
                    metadata_saved, attachments_saved
                ),
            );
        }
    }

    Ok(GmailFetchResult {
        query: query.to_string(),
        messages_seen: message_ids.len(),
        messages_cached,
        messages_fetched,
        metadata_saved,
        attachments_saved,
        files_saved: saved_files.len(),
        out_dir: out_dir.display().to_string(),
        saved_files,
    })
}

const STALE_TEMP_FILE_AGE: Duration = Duration::from_secs(60 * 60);

// write_private_file writes `.<name>.lab-tmp-<suffix>` and renames it; a run killed
// in between leaves it behind. A younger one may be a write still in progress
// (the TUI and a sync can share the folder), so only old ones are removed.
fn remove_stale_temp_files(dir: &Path, now: std::time::SystemTime) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        if !name
            .to_str()
            .is_some_and(|name| name.starts_with('.') && name.contains(".lab-tmp-"))
        {
            continue;
        }
        // DirEntry::metadata does not follow symlinks: only plain files go.
        let stale = entry.metadata().is_ok_and(|metadata| {
            metadata.is_file()
                && metadata
                    .modified()
                    .ok()
                    .and_then(|modified| now.duration_since(modified).ok())
                    .is_some_and(|age| age >= STALE_TEMP_FILE_AGE)
        });
        if stale && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

// The metadata file is written last, so it marks a complete message. Caches written
// before that rule could hold metadata with only some attachments, so every
// attachment the metadata describes must also be on disk.
pub(crate) fn gmail_message_cached(
    out_dir: &Path,
    message_id: &str,
    allowed_exts: &HashSet<String>,
) -> bool {
    let metadata_path = out_dir.join(format!("{}_message.json", sanitize_filename(message_id)));
    let Ok(bytes) = fs::read(&metadata_path) else {
        return false;
    };
    let Ok(msg) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    gmail_attachment_plan(message_id, &msg["payload"], allowed_exts)
        .iter()
        .all(|part| out_dir.join(&part.file_name).is_file())
}

pub(crate) struct GmailAttachmentPart<'a> {
    pub(crate) file_name: String,
    pub(crate) ext: String,
    pub(crate) body: &'a Value,
}

// Lists the parts gmail_fetch saves, with their on-disk names, in traversal order.
pub(crate) fn gmail_attachment_plan<'a>(
    message_id: &str,
    payload: &'a Value,
    allowed_exts: &HashSet<String>,
) -> Vec<GmailAttachmentPart<'a>> {
    let mut plan = Vec::new();
    let mut part_index = 0usize;
    collect_gmail_parts(
        message_id,
        payload,
        allowed_exts,
        &mut part_index,
        &mut plan,
    );
    plan
}

// Writes each missing attachment atomically, then the message metadata, so an
// interrupted run never leaves metadata that claims the message is complete.
pub(crate) fn gmail_store_message(
    out_dir: &Path,
    message_id: &str,
    msg: &Value,
    allowed_exts: &HashSet<String>,
    mut fetch_attachment: impl FnMut(&str) -> Result<Option<String>>,
) -> Result<Vec<String>> {
    let headers = gmail_headers(msg);
    let mut saved_files = Vec::new();
    for part in gmail_attachment_plan(message_id, &msg["payload"], allowed_exts) {
        let path = out_dir.join(&part.file_name);
        if path.exists() {
            continue;
        }
        let data =
            if let Some(attachment_id) = part.body.get("attachmentId").and_then(|v| v.as_str()) {
                fetch_attachment(attachment_id)?
            } else {
                part.body
                    .get("data")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            };
        let Some(data) = data else {
            continue;
        };
        let decoded = decode_gmail_base64(&data)?;
        let bytes = if part.ext == "txt" && !headers.is_empty() {
            let mut with_headers = String::new();
            for key in ["message-id", "subject", "from", "date"] {
                if let Some(value) = headers.get(key) {
                    with_headers.push_str(&format!("{}: {}\n", canonical_header(key), value));
                }
            }
            with_headers.push('\n');
            with_headers.push_str(&String::from_utf8_lossy(&decoded));
            with_headers.into_bytes()
        } else {
            decoded
        };
        write_private_file(&path, &bytes)?;
        saved_files.push(path.display().to_string());
    }
    let metadata_path = out_dir.join(format!("{}_message.json", sanitize_filename(message_id)));
    write_private_file(&metadata_path, &serde_json::to_vec_pretty(msg)?)?;
    Ok(saved_files)
}

pub(crate) fn gmail_headers(msg: &Value) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    if let Some(items) = msg
        .get("payload")
        .and_then(|p| p.get("headers"))
        .and_then(|h| h.as_array())
    {
        for item in items {
            if let (Some(name), Some(value)) = (
                item.get("name").and_then(|v| v.as_str()),
                item.get("value").and_then(|v| v.as_str()),
            ) {
                headers.insert(name.to_ascii_lowercase(), value.to_string());
            }
        }
    }
    headers
}

fn collect_gmail_parts<'a>(
    message_id: &str,
    part: &'a Value,
    allowed_exts: &HashSet<String>,
    part_index: &mut usize,
    plan: &mut Vec<GmailAttachmentPart<'a>>,
) {
    if let Some(parts) = part.get("parts").and_then(|v| v.as_array()) {
        for child in parts {
            collect_gmail_parts(message_id, child, allowed_exts, part_index, plan);
        }
    }

    let filename = part.get("filename").and_then(|v| v.as_str()).unwrap_or("");
    let mime_type = part.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");
    let body = &part["body"];

    let save_name = if !filename.is_empty() {
        filename.to_string()
    } else if mime_type == "text/plain" {
        format!("{}_body_{}.txt", message_id, *part_index)
    } else {
        return;
    };
    let ext = Path::new(&save_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !allowed_exts
        .iter()
        .any(|allowed| allowed.trim_start_matches('.').eq_ignore_ascii_case(&ext))
    {
        return;
    }

    // Numbering counts every allowed part, as earlier versions did, so cached
    // file names stay stable.
    *part_index += 1;
    if body.get("attachmentId").and_then(|v| v.as_str()).is_none()
        && body.get("data").and_then(|v| v.as_str()).is_none()
    {
        return;
    }
    plan.push(GmailAttachmentPart {
        file_name: format!(
            "{}_{}_{}",
            sanitize_filename(message_id),
            *part_index,
            sanitize_filename(&save_name)
        ),
        ext,
        body,
    });
}

fn canonical_header(key: &str) -> &str {
    match key {
        "message-id" => "Message-ID",
        "subject" => "Subject",
        "from" => "From",
        "date" => "Date",
        _ => key,
    }
}

fn decode_gmail_base64(value: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(value)
        .or_else(|_| URL_SAFE.decode(value))
        .context("dekodowanie base64url Gmail")
}

const MAX_SANITIZED_FILENAME: usize = 180;

// Names up to the limit are unchanged (files already on disk keep their names);
// longer ones lose the end of the stem, never the extension.
pub(crate) fn sanitize_filename(value: &str) -> String {
    let s: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Only ASCII is left, so byte offsets are character offsets.
    let s = s.trim_matches('_');
    if s.len() <= MAX_SANITIZED_FILENAME {
        return s.to_string();
    }
    match s.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && (1..=16).contains(&ext.len())
                && ext.bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            format!("{}.{ext}", &stem[..MAX_SANITIZED_FILENAME - ext.len() - 1])
        }
        _ => s[..MAX_SANITIZED_FILENAME].to_string(),
    }
}

// Year Y means invoices issued in Y. Invoices from late December are often
// mailed in January, so the window runs to Feb 1 of Y+1; one dated early in Y
// can arrive in December of Y-1, so it starts on Dec 1 of Y-1. The sync then
// drops candidates whose issue date falls in another year. Messages are cached
// per folder by id, so a wider window only fetches the newly covered ones.
fn gmail_year_window(year: i32) -> String {
    format!("after:{}/12/01 before:{}/02/01", year - 1, year + 1)
}

pub(crate) fn default_gmail_query(year: i32) -> String {
    format!("{} has:attachment filename:pdf", gmail_year_window(year))
}

pub(crate) fn amazon_gmail_query(year: i32) -> String {
    format!(
        "{} (from:amazon.it OR from:amazon.es) has:attachment filename:pdf",
        gmail_year_window(year)
    )
}
