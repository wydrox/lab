use crate::*;

const KSEF_PROD_BASE_URL: &str = "https://api.ksef.mf.gov.pl/v2";
const KSEF_CACHE_IDENTITY_FILE: &str = "ksef_cache_identity.json";
/// Token dostępu odnawiamy, gdy do końca ważności zostało mniej niż tyle sekund.
const KSEF_ACCESS_TOKEN_MARGIN_SECS: i64 = 120;

#[derive(Debug, Clone)]
pub(crate) struct KsefOnlineConfig {
    base_url: String,
    context_type: String,
    context_value: String,
    ksef_token: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct KsefTokenCache {
    base_url: String,
    context_type: String,
    context_value: String,
    access_token: String,
    access_valid_until: DateTime<Utc>,
    refresh_token: Option<String>,
    refresh_valid_until: Option<DateTime<Utc>>,
}

impl KsefTokenCache {
    fn matches(&self, config: &KsefOnlineConfig) -> bool {
        self.base_url == config.base_url
            && self.context_type == config.context_type
            && self.context_value == config.context_value
    }
}

#[derive(Debug, Clone)]
pub(crate) struct KsefAccessToken {
    token: String,
    /// `None`, gdy ważność nie jest znana (np. `KSEF_ACCESS_TOKEN` z konfiguracji).
    valid_until: Option<DateTime<Utc>>,
}

impl KsefAccessToken {
    fn expires_soon(&self, now: DateTime<Utc>) -> bool {
        self.valid_until.is_some_and(|until| {
            until <= now + chrono::Duration::seconds(KSEF_ACCESS_TOKEN_MARGIN_SECS)
        })
    }
}

/// Wynik zapytania wymagającego tokenu dostępu: 401 wraca osobno, żeby można było
/// zalogować się ponownie i powtórzyć zapytanie.
pub(crate) enum KsefAuthorized<T> {
    Done(T),
    Unauthorized(String),
}

/// Dla czego mają być metadane: środowisko (base URL), kontekst i rok.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KsefCacheTarget {
    year: i32,
    base_url: String,
    context_type: String,
    context_value: String,
}

impl KsefCacheTarget {
    fn current(year: i32) -> Self {
        let (context_type, context_value) = ksef_context();
        Self {
            year,
            base_url: ksef_base_url(),
            context_type,
            context_value,
        }
    }

    fn online_identity(&self) -> KsefCacheIdentity {
        KsefCacheIdentity {
            year: self.year,
            source: "online".to_string(),
            base_url: Some(self.base_url.clone()),
            context_type: Some(self.context_type.clone()),
            context_value: Some(self.context_value.clone()),
            fetched_at: Utc::now(),
        }
    }
}

/// Zapisywane obok `records.jsonl` w pliku `ksef_cache_identity.json`.
/// Import lokalnego eksportu (`--ksef-input`) nie zna środowiska ani kontekstu.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct KsefCacheIdentity {
    year: i32,
    #[serde(default)]
    source: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    context_type: Option<String>,
    #[serde(default)]
    context_value: Option<String>,
    fetched_at: DateTime<Utc>,
}

enum KsefCacheLookup {
    Hit(KsefSyncResult),
    Missing,
    Stale,
    Refused(String),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KsefPublicKeyCertificate {
    certificate: String,
    public_key_id: String,
    valid_from: DateTime<Utc>,
    valid_to: DateTime<Utc>,
    usage: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KsefChallengeResponse {
    challenge: String,
    #[serde(rename = "timestampMs")]
    timestamp_ms: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KsefTokenInfo {
    token: String,
    valid_until: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KsefAuthInitResponse {
    reference_number: String,
    authentication_token: KsefTokenInfo,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KsefAuthStatusResponse {
    status: KsefStatusInfo,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KsefStatusInfo {
    code: i64,
    description: Option<String>,
    details: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KsefTokensResponse {
    access_token: KsefTokenInfo,
    refresh_token: KsefTokenInfo,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KsefRefreshResponse {
    access_token: KsefTokenInfo,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KsefQueryMetadataResponse {
    has_more: bool,
    is_truncated: bool,
    invoices: Vec<Value>,
}

#[allow(dead_code)]
pub(crate) fn ksef_online_sync(year: i32, out_dir: Option<&Path>) -> Result<KsefSyncResult> {
    ksef_online_sync_with_progress(year, out_dir, None)
}

pub(crate) fn ksef_online_sync_with_progress(
    year: i32,
    out_dir: Option<&Path>,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<KsefSyncResult> {
    ksef_online_sync_with_progress_and_cache(year, out_dir, progress, false)
}

pub(crate) fn ksef_online_sync_cached_with_progress(
    year: i32,
    out_dir: Option<&Path>,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<KsefSyncResult> {
    ksef_online_sync_with_progress_and_cache(year, out_dir, progress, true)
}

pub(crate) fn ksef_online_sync_with_progress_and_cache(
    year: i32,
    out_dir: Option<&Path>,
    progress: Option<Arc<Mutex<String>>>,
    use_fresh_cache: bool,
) -> Result<KsefSyncResult> {
    let target = KsefCacheTarget::current(year);
    let fresh_ttl = if use_fresh_cache {
        ksef_cache_ttl()
    } else {
        None
    };
    ksef_sync_with_fetcher(year, out_dir, progress, fresh_ttl, &target, |progress| {
        ksef_fetch_metadata_online(year, progress)
    })
}

/// Rdzeń synchronizacji: świeży cache (gdy `fresh_ttl`), inaczej `fetch`; przy braku
/// `KSEF_TOKEN` cache bez limitu wieku, ale tylko o tożsamości zgodnej z `target`.
fn ksef_sync_with_fetcher(
    year: i32,
    out_dir: Option<&Path>,
    progress: Option<Arc<Mutex<String>>>,
    fresh_ttl: Option<Duration>,
    target: &KsefCacheTarget,
    fetch: impl FnOnce(Option<Arc<Mutex<String>>>) -> Result<Vec<Value>>,
) -> Result<KsefSyncResult> {
    if let Some(ttl) = fresh_ttl {
        match ksef_cache_lookup(year, out_dir, progress.clone(), Some(ttl), target)? {
            KsefCacheLookup::Hit(result) => return Ok(result),
            KsefCacheLookup::Refused(reason) => {
                eprintln!("  [KSeF] pomijam lokalny cache: {reason}");
                if let Some(progress) = &progress {
                    set_progress(
                        progress,
                        format!("KSeF: cache nie pasuje ({reason}), odświeżam online..."),
                    );
                }
            }
            KsefCacheLookup::Missing | KsefCacheLookup::Stale => {}
        }
    }
    let metadata = match fetch(progress.clone()) {
        Ok(metadata) => metadata,
        Err(err) if is_missing_ksef_token(&err) => {
            return match ksef_cache_lookup(year, out_dir, progress.clone(), None, target)? {
                KsefCacheLookup::Hit(result) => {
                    eprintln!(
                        "  [KSeF] brak KSEF_TOKEN; używam lokalnego cache ({} rekordów)",
                        result.summary.records_count
                    );
                    if let Some(progress) = &progress {
                        set_progress(
                            progress,
                            format!(
                                "KSeF: brak tokenu, lokalny cache ({} rekordów)",
                                result.summary.records_count
                            ),
                        );
                    }
                    Ok(result)
                }
                KsefCacheLookup::Refused(reason) => Err(anyhow!(
                    "brak KSEF_TOKEN, a lokalnego cache KSeF nie używam: {reason}"
                )),
                KsefCacheLookup::Missing | KsefCacheLookup::Stale => Err(err),
            };
        }
        Err(err) => return Err(err),
    };
    ksef_store_online_metadata(year, out_dir, progress, target, &metadata)
}

fn ksef_fetch_metadata_online(
    year: i32,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<Vec<Value>> {
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!("KSeF: konfiguracja i logowanie ({year})..."),
        );
    }
    let config = ksef_online_config()?;
    let client = Client::builder()
        .timeout(ksef_http_timeout())
        .build()
        .context("KSeF HTTP client")?;
    if let Some(progress) = &progress {
        set_progress(progress, "KSeF: pobieram token dostępu...");
    }
    let mut access_token = ksef_access_token(&client, &config)?;
    let page_size = ksef_metadata_page_size();
    let url = format!("{}/invoices/query/metadata", config.base_url);
    let mut metadata = Vec::new();

    for subject_type in ksef_subject_types() {
        let exact_ranges = ksef_year_exact_quarter_ranges(year);
        for (range_index, (mut from, mut to)) in
            ksef_year_quarter_ranges(year).into_iter().enumerate()
        {
            let mut page_offset = 0usize;
            loop {
                if let Some(progress) = &progress {
                    set_progress(
                        progress,
                        format!(
                            "KSeF: metadata {subject_type} {from}..{to}, strona {}...",
                            page_offset + 1
                        ),
                    );
                }
                let query = vec![
                    ("sortOrder".to_string(), "Asc".to_string()),
                    ("pageOffset".to_string(), page_offset.to_string()),
                    ("pageSize".to_string(), page_size.to_string()),
                ];
                let body = serde_json::json!({
                    "subjectType": subject_type,
                    "dateRange": {
                        "dateType": "Issue",
                        "from": from,
                        "to": to,
                    }
                });
                let response: Result<KsefQueryMetadataResponse> = ksef_authorized_request(
                    &mut access_token,
                    "query invoice metadata",
                    || ksef_rate_limit_wait_with_progress("metadata", 8, 16, 20, progress.clone()),
                    Utc::now,
                    |rejected| ksef_renew_access_token(&client, &config, rejected),
                    |token| {
                        let response = ksef_send_with_retry_inner(
                            client
                                .post(&url)
                                .bearer_auth(token)
                                .header("X-Error-Format", "problem-details")
                                .query(&query)
                                .json(&body),
                            "query invoice metadata",
                            true,
                        )?;
                        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                            return Ok(KsefAuthorized::Unauthorized(
                                response.text().unwrap_or_default(),
                            ));
                        }
                        Ok(KsefAuthorized::Done(
                            response.json().context("KSeF metadata response JSON")?,
                        ))
                    },
                );
                let response = match response {
                    Ok(response) => response,
                    // A KSeF version with a shorter range cap rejects the widened first or
                    // last quarter: query the exact quarter instead.
                    Err(err)
                        if page_offset == 0
                            && ksef_error_is_bad_request(&err)
                            && exact_ranges
                                .get(range_index)
                                .is_some_and(|exact| *exact != (from.clone(), to.clone())) =>
                    {
                        eprintln!(
                            "  [KSeF] zakres {from}..{to} odrzucony ({err}); ponawiam dokładny kwartał"
                        );
                        (from, to) = exact_ranges[range_index].clone();
                        continue;
                    }
                    Err(err) => return Err(err),
                };

                if response.is_truncated {
                    return Err(anyhow!(
                        "KSeF metadata query truncated for {subject_type} {from}..{to}; zmniejsz zakres dat albo uruchom mniejszymi partiami"
                    ));
                }
                let count = response.invoices.len();
                eprintln!(
                    "  [KSeF] metadata {subject_type} {from}..{to}, strona {page_offset}: {count}"
                );
                metadata.extend(response.invoices);
                if let Some(progress) = &progress {
                    set_progress(
                        progress,
                        format!(
                            "KSeF: {subject_type} {from}..{to}, strona {}: +{count} (razem {})",
                            page_offset + 1,
                            metadata.len()
                        ),
                    );
                }
                if !response.has_more {
                    break;
                }
                page_offset += 1;
            }
        }
    }
    Ok(metadata)
}

fn ksef_store_online_metadata(
    year: i32,
    out_dir: Option<&Path>,
    progress: Option<Arc<Mutex<String>>>,
    target: &KsefCacheTarget,
    metadata: &[Value],
) -> Result<KsefSyncResult> {
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!(
                "KSeF: deduplikacja i zapis {} metadanych...",
                metadata.len()
            ),
        );
    }
    let records = ksef_metadata_records_for_year(metadata, year);

    let out_dir = ksef_sync_output_dir(year, out_dir);
    fs::create_dir_all(&out_dir).with_context(|| format!("mkdir {}", out_dir.display()))?;
    // Bez tożsamości w trakcie zapisu: przerwany zapis nie zostawi danych pod starą etykietą.
    clear_ksef_cache_identity(&out_dir)?;
    let raw_output = out_dir.join("ksef_raw_metadata.json");
    let json_output = out_dir.join("records.json");
    let jsonl_output = out_dir.join("records.jsonl");
    fs::write(&raw_output, serde_json::to_vec_pretty(metadata)?)
        .with_context(|| format!("zapis {}", raw_output.display()))?;
    write_records(&records, OutputFormat::Json, Some(&json_output))?;
    write_records(&records, OutputFormat::Jsonl, Some(&jsonl_output))?;
    write_ksef_cache_identity(&out_dir, &target.online_identity())?;

    Ok(KsefSyncResult {
        summary: KsefSyncSummary {
            year,
            records_count: records.len(),
            input: format!(
                "online:{}:{}:{}",
                target.base_url, target.context_type, target.context_value
            ),
            json_output: json_output.display().to_string(),
            jsonl_output: jsonl_output.display().to_string(),
        },
        records,
    })
}

pub(crate) fn ksef_online_config() -> Result<KsefOnlineConfig> {
    let base_url = ksef_base_url();
    let (context_type, context_value) = ksef_context();
    let ksef_token = secret_value(Secret::KsefToken)?.filter(|value| !value.trim().is_empty());
    Ok(KsefOnlineConfig {
        base_url,
        context_type,
        context_value,
        ksef_token,
    })
}

fn ksef_context() -> (String, String) {
    let context_type = lab_config_var("KSEF_CONTEXT_TYPE").unwrap_or_else(|| "Nip".to_string());
    let raw_context = lab_config_var("KSEF_CONTEXT_NIP")
        .or_else(|| lab_config_var("KSEF_NIP"))
        .unwrap_or_else(|| DEFAULT_PRODUCTMESH_NIP.to_string());
    let context_value = if context_type.eq_ignore_ascii_case("Nip") {
        normalize_tax_id(&raw_context).unwrap_or(raw_context)
    } else {
        raw_context
    };
    (context_type, context_value)
}

pub(crate) fn missing_ksef_token_error() -> anyhow::Error {
    anyhow!("brak KSEF_TOKEN; potrzebny token KSeF z uprawnieniem InvoiceRead")
}

pub(crate) fn is_missing_ksef_token(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("brak KSEF_TOKEN"))
}

pub(crate) fn ksef_base_url() -> String {
    let url = lab_config_var("KSEF_BASE_URL").unwrap_or_else(|| {
        match lab_config_var("KSEF_ENV")
            .unwrap_or_else(|| "prod".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "test" | "te" => "https://api-test.ksef.mf.gov.pl/v2".to_string(),
            "demo" | "preprod" | "pre-production" => {
                "https://api-demo.ksef.mf.gov.pl/v2".to_string()
            }
            _ => KSEF_PROD_BASE_URL.to_string(),
        }
    });
    url.trim_end_matches('/').to_string()
}

pub(crate) fn ksef_http_timeout() -> Duration {
    Duration::from_secs(
        lab_config_var("KSEF_TIMEOUT_SECS")
            .and_then(|value| value.parse().ok())
            .unwrap_or(60),
    )
}

pub(crate) fn ksef_metadata_page_size() -> usize {
    lab_config_var("KSEF_PAGE_SIZE")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(250)
        .clamp(10, 250)
}

pub(crate) fn ksef_sync_output_dir(year: i32, out_dir: Option<&Path>) -> PathBuf {
    out_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| configured_ksef_out_path(year))
}

/// Katalog roku w `KSEF_DATA_DIR`: `<KSEF_DATA_DIR>/ksef-<rok>`. Dla zgodności wstecz
/// sam `KSEF_DATA_DIR` (bez podkatalogu roku), gdy nie ma jeszcze katalogu roku, a cache
/// w nim jest zapisany dla tego roku.
pub(crate) fn ksef_data_dir_year_path(root: &Path, year: i32) -> PathBuf {
    let per_year = root.join(format!("ksef-{year}"));
    if per_year.join("records.jsonl").is_file() {
        return per_year;
    }
    if root.join("records.jsonl").is_file() && ksef_cache_dir_year(root) == Some(year) {
        return root.to_path_buf();
    }
    per_year
}

/// Rok cache: z `ksef_cache_identity.json`, a dla cache sprzed tej wersji z nazwy katalogu
/// (`ksef-2026`) albo z dat wystawienia zapisanych faktur.
fn ksef_cache_dir_year(dir: &Path) -> Option<i32> {
    match read_ksef_cache_identity(dir) {
        Ok(Some(identity)) => Some(identity.year),
        Ok(None) => ksef_legacy_dir_year(dir),
        Err(_) => None,
    }
}

fn ksef_legacy_dir_year(dir: &Path) -> Option<i32> {
    ksef_dir_name_year(dir).or_else(|| {
        let records = load_records(SourceKind::Ksef, &dir.join("records.jsonl")).ok()?;
        let mut years = records
            .iter()
            .filter_map(|record| record.issue_date.map(|date| date.year()));
        let first = years.next()?;
        years.all(|year| year == first).then_some(first)
    })
}

fn ksef_dir_name_year(dir: &Path) -> Option<i32> {
    let name = dir.file_name()?.to_str()?;
    name.split(|c: char| !c.is_ascii_digit())
        .filter(|part| part.len() == 4)
        .filter_map(|part| part.parse::<i32>().ok())
        .rfind(|year| (2000..=2100).contains(year))
}

fn ksef_cache_identity_path(dir: &Path) -> PathBuf {
    dir.join(KSEF_CACHE_IDENTITY_FILE)
}

fn read_ksef_cache_identity(dir: &Path) -> Result<Option<KsefCacheIdentity>> {
    let path = ksef_cache_identity_path(dir);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("odczyt {}", path.display())),
    };
    serde_json::from_str(&text)
        .map(Some)
        .with_context(|| format!("JSON {}", path.display()))
}

fn write_ksef_cache_identity(dir: &Path, identity: &KsefCacheIdentity) -> Result<()> {
    let path = ksef_cache_identity_path(dir);
    fs::write(&path, serde_json::to_vec_pretty(identity)?)
        .with_context(|| format!("zapis {}", path.display()))
}

fn clear_ksef_cache_identity(dir: &Path) -> Result<()> {
    let path = ksef_cache_identity_path(dir);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("usunięcie {}", path.display())),
    }
}

/// Powód odrzucenia cache z katalogu `dir` dla `target`; `None` = cache pasuje.
fn ksef_cache_refusal(dir: &Path, target: &KsefCacheTarget) -> Option<String> {
    match read_ksef_cache_identity(dir) {
        Ok(stored) => ksef_identity_refusal(stored.as_ref(), target, || ksef_legacy_dir_year(dir)),
        Err(err) => Some(format!("nieczytelna tożsamość cache: {err:#}")),
    }
}

fn ksef_identity_refusal(
    stored: Option<&KsefCacheIdentity>,
    target: &KsefCacheTarget,
    legacy_year: impl FnOnce() -> Option<i32>,
) -> Option<String> {
    let Some(stored) = stored else {
        if target.base_url != KSEF_PROD_BASE_URL {
            return Some(format!(
                "cache bez zapisanego środowiska jest ważny tylko dla produkcyjnego KSeF ({KSEF_PROD_BASE_URL}), bieżące: {}",
                target.base_url
            ));
        }
        return match legacy_year() {
            Some(year) if year == target.year => None,
            Some(year) => Some(format!(
                "cache jest dla roku {year}, potrzebny {}",
                target.year
            )),
            None => Some(format!(
                "cache bez zapisanego roku; nie potwierdzę, że dotyczy roku {}",
                target.year
            )),
        };
    };
    if stored.year != target.year {
        return Some(format!(
            "cache jest dla roku {}, potrzebny {}",
            stored.year, target.year
        ));
    }
    let Some(base_url) = stored.base_url.as_deref() else {
        // Import lokalnego eksportu: środowisko nieznane, jak w cache sprzed tej wersji.
        return (target.base_url != KSEF_PROD_BASE_URL).then(|| {
            format!(
                "cache z importu lokalnego eksportu jest ważny tylko dla produkcyjnego KSeF ({KSEF_PROD_BASE_URL}), bieżące: {}",
                target.base_url
            )
        });
    };
    if base_url != target.base_url {
        return Some(format!(
            "cache pochodzi ze środowiska {base_url}, bieżące: {}",
            target.base_url
        ));
    }
    let context_matches = stored
        .context_type
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(&target.context_type))
        && stored.context_value.as_deref() == Some(target.context_value.as_str());
    if !context_matches {
        return Some(format!(
            "cache dotyczy kontekstu {}:{}, bieżący: {}:{}",
            stored.context_type.as_deref().unwrap_or("?"),
            stored.context_value.as_deref().unwrap_or("?"),
            target.context_type,
            target.context_value
        ));
    }
    None
}

pub(crate) fn ksef_cache_ttl() -> Option<Duration> {
    ksef_cache_ttl_from(lab_config_var("KSEF_CACHE_TTL_MINS").as_deref())
}

fn ksef_cache_ttl_from(value: Option<&str>) -> Option<Duration> {
    let minutes = value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(360);
    (minutes > 0).then_some(Duration::from_secs(minutes * 60))
}

/// Cache metadanych z katalogu roku. `ttl = None` oznacza brak limitu wieku (fallback
/// bez tokenu); tożsamość cache musi zgadzać się z `target` w obu przypadkach.
fn ksef_cache_lookup(
    year: i32,
    out_dir: Option<&Path>,
    progress: Option<Arc<Mutex<String>>>,
    ttl: Option<Duration>,
    target: &KsefCacheTarget,
) -> Result<KsefCacheLookup> {
    let out_dir = ksef_sync_output_dir(year, out_dir);
    let jsonl_output = out_dir.join("records.jsonl");
    if !jsonl_output.is_file() {
        if ttl.is_some()
            && let Some(progress) = &progress
        {
            set_progress(progress, "KSeF: brak lokalnego cache, odświeżam online...");
        }
        return Ok(KsefCacheLookup::Missing);
    }
    if let Some(reason) = ksef_cache_refusal(&out_dir, target) {
        return Ok(KsefCacheLookup::Refused(format!(
            "{} ({reason})",
            out_dir.display()
        )));
    }
    let age = fs::metadata(&jsonl_output)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok());
    let Some(age) = age else {
        return Ok(KsefCacheLookup::Stale);
    };
    if let Some(ttl) = ttl
        && age > ttl
    {
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "KSeF: cache ma {}, odświeżam online...",
                    format_duration_short(age)
                ),
            );
        }
        return Ok(KsefCacheLookup::Stale);
    }
    let records = load_records(SourceKind::Ksef, &jsonl_output)?;
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!(
                "KSeF: lokalny cache ({}; {} rekordów){}",
                format_duration_short(age),
                records.len(),
                if ttl.is_some() {
                    ", pomijam online"
                } else {
                    ", brak tokenu"
                }
            ),
        );
    }
    let json_output = out_dir.join("records.json");
    Ok(KsefCacheLookup::Hit(KsefSyncResult {
        summary: KsefSyncSummary {
            year,
            records_count: records.len(),
            input: format!("cache:{}", jsonl_output.display()),
            json_output: json_output.display().to_string(),
            jsonl_output: jsonl_output.display().to_string(),
        },
        records,
    }))
}

pub(crate) fn format_duration_short(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    if total_secs >= 3_600 {
        format!("{}h {}m", total_secs / 3_600, (total_secs % 3_600) / 60)
    } else if total_secs >= 60 {
        format!("{}m {}s", total_secs / 60, total_secs % 60)
    } else {
        format!("{}s", total_secs)
    }
}

pub(crate) fn ksef_subject_types() -> Vec<String> {
    lab_config_var("KSEF_SUBJECT_TYPES")
        .map(|value| {
            value
                .split(',')
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| vec!["Subject1".to_string(), "Subject2".to_string()])
}

/// Metadata query ranges for `year`, one per quarter. The year is widened by a
/// day on both sides (31 Dec of the previous year .. 2 Jan of the next), so an
/// invoice dated 1 Jan or 31 Dec is fetched whether KSeF compares the issue date
/// in UTC or Polish time and treats `to` as inclusive or exclusive. Each range
/// starts where the previous one ends, so no day falls between them. The
/// neighbouring days are dropped afterwards by `ksef_metadata_records_for_year`.
pub(crate) fn ksef_year_quarter_ranges(year: i32) -> Vec<(String, String)> {
    let starts = [
        (year - 1, 12, 31),
        (year, 4, 1),
        (year, 7, 1),
        (year, 10, 1),
    ];
    let ends = [(year, 4, 1), (year, 7, 1), (year, 10, 1), (year + 1, 1, 2)];
    starts
        .into_iter()
        .zip(ends)
        .map(|((sy, sm, sd), (ey, em, ed))| {
            (
                format!("{sy:04}-{sm:02}-{sd:02}T00:00:00+00:00"),
                format!("{ey:04}-{em:02}-{ed:02}T00:00:00+00:00"),
            )
        })
        .collect()
}

/// The year's quarters without the neighbouring days, used when KSeF rejects a
/// widened range.
pub(crate) fn ksef_year_exact_quarter_ranges(year: i32) -> Vec<(String, String)> {
    let bounds = [(year, 1), (year, 4), (year, 7), (year, 10), (year + 1, 1)];
    bounds
        .windows(2)
        .map(|pair| {
            (
                format!("{:04}-{:02}-01T00:00:00+00:00", pair[0].0, pair[0].1),
                format!("{:04}-{:02}-01T00:00:00+00:00", pair[1].0, pair[1].1),
            )
        })
        .collect()
}

pub(crate) fn ksef_error_is_bad_request(err: &anyhow::Error) -> bool {
    err.to_string().contains("HTTP 400")
}

/// Records of `year` from metadata fetched over the widened ranges: records
/// issued in another year are dropped, records without a readable issue date
/// kept, and a document returned by two overlapping ranges kept once (by KSeF
/// number).
pub(crate) fn ksef_metadata_records_for_year(metadata: &[Value], year: i32) -> Vec<InvoiceRecord> {
    let mut seen = HashSet::new();
    metadata
        .iter()
        .filter_map(ksef_metadata_to_record)
        .filter(|record| record.issue_date.is_none_or(|date| date.year() == year))
        .filter(|record| {
            seen.insert(
                normalized_ksef_reference(record).unwrap_or_else(|| record.content_hash.clone()),
            )
        })
        .collect()
}

pub(crate) fn ksef_access_token(
    client: &Client,
    config: &KsefOnlineConfig,
) -> Result<KsefAccessToken> {
    if let Some(token) = lab_config_var("KSEF_ACCESS_TOKEN") {
        return Ok(KsefAccessToken {
            token,
            valid_until: None,
        });
    }

    let margin = chrono::Duration::seconds(KSEF_ACCESS_TOKEN_MARGIN_SECS);
    if let Ok(cache) = read_ksef_token_cache()
        && cache.matches(config)
    {
        if cache.access_valid_until > Utc::now() + margin {
            return Ok(KsefAccessToken {
                token: cache.access_token,
                valid_until: Some(cache.access_valid_until),
            });
        }
        if let (Some(refresh_token), Some(refresh_valid_until)) =
            (cache.refresh_token.clone(), cache.refresh_valid_until)
            && refresh_valid_until > Utc::now() + margin
            && let Ok(refreshed) = ksef_refresh_access_token(client, config, &cache, &refresh_token)
        {
            return Ok(refreshed);
        }
    }

    ksef_authenticate_with_ksef_token(client, config)
}

/// Nowy token dostępu w trakcie synchronizacji: `rejected = None` przed wygaśnięciem
/// (cache → refresh → logowanie), `Some(token)` po HTTP 401 dla tego tokenu.
fn ksef_renew_access_token(
    client: &Client,
    config: &KsefOnlineConfig,
    rejected: Option<&str>,
) -> Result<KsefAccessToken> {
    let Some(rejected) = rejected else {
        return ksef_access_token(client, config);
    };
    if lab_config_var("KSEF_ACCESS_TOKEN").is_some() {
        return Err(anyhow!(
            "KSeF odrzucił KSEF_ACCESS_TOKEN (HTTP 401); usuń tę zmienną albo ustaw ważny token"
        ));
    }
    let margin = chrono::Duration::seconds(KSEF_ACCESS_TOKEN_MARGIN_SECS);
    if let Ok(cache) = read_ksef_token_cache()
        && cache.matches(config)
    {
        if cache.access_token != rejected && cache.access_valid_until > Utc::now() + margin {
            return Ok(KsefAccessToken {
                token: cache.access_token,
                valid_until: Some(cache.access_valid_until),
            });
        }
        if let (Some(refresh_token), Some(refresh_valid_until)) =
            (cache.refresh_token.clone(), cache.refresh_valid_until)
            && refresh_valid_until > Utc::now() + margin
        {
            match ksef_refresh_access_token(client, config, &cache, &refresh_token) {
                Ok(token) if token.token != rejected => return Ok(token),
                Ok(_) => eprintln!("  [KSeF] refresh zwrócił odrzucony token; loguję się od nowa"),
                Err(err) => {
                    eprintln!("  [KSeF] refresh tokenu nieudany ({err:#}); loguję się od nowa")
                }
            }
        }
    }
    ksef_authenticate_with_ksef_token(client, config)
}

/// Jedno zapytanie z tokenem dostępu. Przed wysłaniem (po `before_send`, np. lokalnym
/// limiterze, który potrafi długo czekać) odnawia token bliski wygaśnięcia; po HTTP 401
/// loguje się ponownie i powtarza zapytanie raz. Drugie 401 kończy się błędem.
pub(crate) fn ksef_authorized_request<T>(
    token: &mut KsefAccessToken,
    description: &str,
    mut before_send: impl FnMut() -> Result<()>,
    mut now: impl FnMut() -> DateTime<Utc>,
    mut renew: impl FnMut(Option<&str>) -> Result<KsefAccessToken>,
    mut send: impl FnMut(&str) -> Result<KsefAuthorized<T>>,
) -> Result<T> {
    let mut reauthenticated = false;
    loop {
        before_send()?;
        if token.expires_soon(now()) {
            eprintln!("  [KSeF] token dostępu wygasa ({description}), odnawiam...");
            *token = renew(None)?;
        }
        match send(&token.token)? {
            KsefAuthorized::Done(value) => return Ok(value),
            KsefAuthorized::Unauthorized(body) if reauthenticated => {
                return Err(anyhow!(
                    "KSeF {description} HTTP 401 Unauthorized także po ponownym logowaniu: {body}"
                ));
            }
            KsefAuthorized::Unauthorized(_) => {
                eprintln!("  [KSeF] HTTP 401 ({description}), loguję się ponownie...");
                reauthenticated = true;
                let rejected = token.token.clone();
                *token = renew(Some(&rejected))?;
            }
        }
    }
}

pub(crate) fn ksef_refresh_access_token(
    client: &Client,
    config: &KsefOnlineConfig,
    cache: &KsefTokenCache,
    refresh_token: &str,
) -> Result<KsefAccessToken> {
    let url = format!("{}/auth/token/refresh", config.base_url);
    let response: KsefRefreshResponse = ksef_send_with_retry(
        client
            .post(&url)
            .bearer_auth(refresh_token)
            .header("X-Error-Format", "problem-details"),
        "refresh access token",
    )?
    .json()
    .context("KSeF refresh response JSON")?;
    let new_cache = KsefTokenCache {
        base_url: config.base_url.clone(),
        context_type: config.context_type.clone(),
        context_value: config.context_value.clone(),
        access_token: response.access_token.token.clone(),
        access_valid_until: response.access_token.valid_until,
        refresh_token: cache.refresh_token.clone(),
        refresh_valid_until: cache.refresh_valid_until,
    };
    save_ksef_token_cache(&new_cache)?;
    Ok(KsefAccessToken {
        token: new_cache.access_token,
        valid_until: Some(new_cache.access_valid_until),
    })
}

pub(crate) fn ksef_authenticate_with_ksef_token(
    client: &Client,
    config: &KsefOnlineConfig,
) -> Result<KsefAccessToken> {
    let ksef_token = config
        .ksef_token
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(missing_ksef_token_error)?;
    let key = ksef_token_encryption_key(client, &config.base_url)?;
    let challenge_url = format!("{}/auth/challenge", config.base_url);
    let challenge: KsefChallengeResponse = ksef_send_with_retry(
        client
            .post(&challenge_url)
            .header("X-Error-Format", "problem-details"),
        "auth challenge",
    )?
    .json()
    .context("KSeF challenge response JSON")?;

    let token_with_timestamp = format!("{ksef_token}|{}", challenge.timestamp_ms);
    let encrypted_token =
        ksef_encrypt_token_with_certificate(&key.certificate, &token_with_timestamp)?;
    let auth_url = format!("{}/auth/ksef-token", config.base_url);
    let auth_body = serde_json::json!({
        "challenge": challenge.challenge,
        "contextIdentifier": {
            "type": config.context_type,
            "value": config.context_value,
        },
        "encryptedToken": encrypted_token,
        "publicKeyId": key.public_key_id,
    });
    let init: KsefAuthInitResponse = ksef_send_with_retry(
        client
            .post(&auth_url)
            .header("X-Error-Format", "problem-details")
            .json(&auth_body),
        "authenticate by KSeF token",
    )?
    .json()
    .context("KSeF auth init response JSON")?;

    ksef_wait_for_auth(client, config, &init)?;

    let redeem_url = format!("{}/auth/token/redeem", config.base_url);
    let tokens: KsefTokensResponse = ksef_send_with_retry(
        client
            .post(&redeem_url)
            .bearer_auth(&init.authentication_token.token)
            .header("X-Error-Format", "problem-details"),
        "redeem access token",
    )?
    .json()
    .context("KSeF redeem response JSON")?;

    let cache = KsefTokenCache {
        base_url: config.base_url.clone(),
        context_type: config.context_type.clone(),
        context_value: config.context_value.clone(),
        access_token: tokens.access_token.token.clone(),
        access_valid_until: tokens.access_token.valid_until,
        refresh_token: Some(tokens.refresh_token.token),
        refresh_valid_until: Some(tokens.refresh_token.valid_until),
    };
    save_ksef_token_cache(&cache)?;
    Ok(KsefAccessToken {
        token: cache.access_token,
        valid_until: Some(cache.access_valid_until),
    })
}

pub(crate) fn ksef_wait_for_auth(
    client: &Client,
    config: &KsefOnlineConfig,
    init: &KsefAuthInitResponse,
) -> Result<()> {
    let status_url = format!("{}/auth/{}", config.base_url, init.reference_number);
    for attempt in 0..30 {
        let status: KsefAuthStatusResponse = ksef_send_with_retry(
            client
                .get(&status_url)
                .bearer_auth(&init.authentication_token.token)
                .header("X-Error-Format", "problem-details"),
            "auth status",
        )?
        .json()
        .context("KSeF auth status response JSON")?;
        match status.status.code {
            200 => return Ok(()),
            100 => {
                sleep(Duration::from_secs(1));
            }
            code => {
                return Err(anyhow!(
                    "KSeF auth failed: code={} description={} details={}",
                    code,
                    status.status.description.unwrap_or_default(),
                    status.status.details.unwrap_or_default().join("; ")
                ));
            }
        }
        if attempt == 29 {
            return Err(anyhow!("KSeF auth timeout for {}", init.reference_number));
        }
    }
    Ok(())
}

pub(crate) fn ksef_token_encryption_key(
    client: &Client,
    base_url: &str,
) -> Result<KsefPublicKeyCertificate> {
    let url = format!("{base_url}/security/public-key-certificates");
    let certificates: Vec<KsefPublicKeyCertificate> = ksef_send_with_retry(
        client.get(&url).header("X-Error-Format", "problem-details"),
        "public key certificates",
    )?
    .json()
    .context("KSeF public key certificates response JSON")?;
    let now = Utc::now();
    certificates
        .into_iter()
        .filter(|cert| {
            cert.usage
                .iter()
                .any(|usage| usage == "KsefTokenEncryption")
        })
        .max_by_key(|cert| {
            let active = cert.valid_from <= now && cert.valid_to > now;
            (active, cert.valid_from)
        })
        .ok_or_else(|| anyhow!("KSeF nie zwrócił certyfikatu KsefTokenEncryption"))
}

pub(crate) fn ksef_encrypt_token_with_certificate(
    certificate_b64: &str,
    plaintext: &str,
) -> Result<String> {
    let tmp_dir = std::env::temp_dir();
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let cert_path = tmp_dir.join(format!("lab-ksef-{nonce}.der"));
    let pub_path = tmp_dir.join(format!("lab-ksef-{nonce}.pem"));
    let plain_path = tmp_dir.join(format!("lab-ksef-{nonce}.txt"));
    let encrypted_path = tmp_dir.join(format!("lab-ksef-{nonce}.bin"));

    let result = (|| -> Result<String> {
        let cert = STANDARD
            .decode(certificate_b64)
            .context("dekodowanie certyfikatu KSeF")?;
        write_private_file(&cert_path, &cert)?;
        write_private_file(&plain_path, plaintext.as_bytes())?;

        let mut openssl = Command::new(local_tool("openssl")?);
        apply_isolated_env(&mut openssl);
        let output = openssl
            .arg("x509")
            .arg("-inform")
            .arg("DER")
            .arg("-in")
            .arg(&cert_path)
            .arg("-pubkey")
            .arg("-noout")
            .output()
            .context("openssl x509 -pubkey")?;
        if !output.status.success() {
            return Err(anyhow!(
                "openssl x509 failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        write_private_file(&pub_path, &output.stdout)?;

        let mut openssl = Command::new(local_tool("openssl")?);
        apply_isolated_env(&mut openssl);
        let output = openssl
            .arg("pkeyutl")
            .arg("-encrypt")
            .arg("-pubin")
            .arg("-inkey")
            .arg(&pub_path)
            .arg("-in")
            .arg(&plain_path)
            .arg("-out")
            .arg(&encrypted_path)
            .arg("-pkeyopt")
            .arg("rsa_padding_mode:oaep")
            .arg("-pkeyopt")
            .arg("rsa_oaep_md:sha256")
            .arg("-pkeyopt")
            .arg("rsa_mgf1_md:sha256")
            .output()
            .context("openssl pkeyutl RSA-OAEP SHA-256")?;
        if !output.status.success() {
            return Err(anyhow!(
                "openssl pkeyutl failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let encrypted = fs::read(&encrypted_path)
            .with_context(|| format!("odczyt {}", encrypted_path.display()))?;
        Ok(STANDARD.encode(encrypted))
    })();

    for path in [&cert_path, &pub_path, &plain_path, &encrypted_path] {
        let _ = fs::remove_file(path);
    }
    result
}

pub(crate) fn ksef_send_with_retry(
    builder: reqwest::blocking::RequestBuilder,
    description: &str,
) -> Result<reqwest::blocking::Response> {
    ksef_send_with_retry_inner(builder, description, false)
}

/// `pass_unauthorized`: HTTP 401 wraca jako odpowiedź (do ponownego logowania), nie błąd.
fn ksef_send_with_retry_inner(
    builder: reqwest::blocking::RequestBuilder,
    description: &str,
    pass_unauthorized: bool,
) -> Result<reqwest::blocking::Response> {
    let mut delay = Duration::from_secs(1);
    for attempt in 0..6 {
        let request = builder
            .try_clone()
            .ok_or_else(|| anyhow!("KSeF request cannot be cloned: {description}"))?;
        match request.send() {
            Ok(response) => {
                let status = response.status();
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    let wait = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(Duration::from_secs)
                        .unwrap_or(delay);
                    eprintln!(
                        "  [KSeF] rate limit 429 ({description}), czekam {}s",
                        wait.as_secs()
                    );
                    sleep(wait + Duration::from_millis(250));
                    delay = (delay * 2).min(Duration::from_secs(60));
                    continue;
                }
                if status.is_server_error() && attempt < 5 {
                    eprintln!(
                        "  [KSeF] HTTP {} ({description}), retry za {}s",
                        status,
                        delay.as_secs()
                    );
                    sleep(delay);
                    delay = (delay * 2).min(Duration::from_secs(60));
                    continue;
                }
                if pass_unauthorized && status == reqwest::StatusCode::UNAUTHORIZED {
                    return Ok(response);
                }
                if !status.is_success() {
                    let body = response.text().unwrap_or_default();
                    return Err(anyhow!("KSeF {description} HTTP {status}: {body}"));
                }
                return Ok(response);
            }
            Err(err) if attempt < 5 => {
                eprintln!(
                    "  [KSeF] błąd sieci ({description}): {err}; retry za {}s",
                    delay.as_secs()
                );
                sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(60));
            }
            Err(err) => return Err(err).context(format!("KSeF {description}")),
        }
    }
    Err(anyhow!("KSeF {description}: retry exhausted"))
}

#[allow(dead_code)]
pub(crate) fn ksef_rate_limit_wait(
    group: &str,
    per_second: usize,
    per_minute: usize,
    per_hour: usize,
) -> Result<()> {
    ksef_rate_limit_wait_with_progress(group, per_second, per_minute, per_hour, None)
}

pub(crate) fn ksef_rate_limit_wait_with_progress(
    group: &str,
    per_second: usize,
    per_minute: usize,
    per_hour: usize,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<()> {
    let path = ksef_rate_limit_path(group);
    loop {
        let now = Utc::now().timestamp_millis();
        let mut timestamps = read_i64_json_array(&path).unwrap_or_default();
        timestamps.retain(|ts| now - *ts < 3_600_000);
        timestamps.sort_unstable();

        let wait_ms = [
            rate_limit_wait_for_window(&timestamps, now, 1_000, per_second),
            rate_limit_wait_for_window(&timestamps, now, 60_000, per_minute),
            rate_limit_wait_for_window(&timestamps, now, 3_600_000, per_hour),
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(0);

        if wait_ms <= 0 {
            timestamps.push(now);
            write_i64_json_array(&path, &timestamps)?;
            return Ok(());
        }

        let wait = Duration::from_millis(wait_ms as u64 + 250);
        eprintln!(
            "  [KSeF] lokalny limiter {group}: czekam {}",
            format_duration_short(wait)
        );
        sleep_with_rate_limit_progress(group, wait, progress.clone());
    }
}

pub(crate) fn sleep_with_rate_limit_progress(
    group: &str,
    wait: Duration,
    progress: Option<Arc<Mutex<String>>>,
) {
    let started = std::time::Instant::now();
    while started.elapsed() < wait {
        let remaining = wait.saturating_sub(started.elapsed());
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "KSeF: lokalny limiter {group}, czekam {}",
                    format_duration_short(remaining)
                ),
            );
        }
        sleep(remaining.min(Duration::from_secs(10)));
    }
}

pub(crate) fn rate_limit_wait_for_window(
    timestamps: &[i64],
    now: i64,
    window_ms: i64,
    limit: usize,
) -> Option<i64> {
    if limit == 0 {
        return None;
    }
    let mut in_window = timestamps
        .iter()
        .copied()
        .filter(|ts| now - *ts < window_ms)
        .collect::<Vec<_>>();
    if in_window.len() < limit {
        return None;
    }
    in_window.sort_unstable();
    let oldest_blocking = in_window[in_window.len().saturating_sub(limit)];
    Some((oldest_blocking + window_ms - now).max(0))
}

pub(crate) fn ksef_rate_limit_path(group: &str) -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("lab")
        .join(format!("ksef-rate-{group}.json"))
}

pub(crate) fn read_i64_json_array(path: &Path) -> Result<Vec<i64>> {
    let text = fs::read_to_string(path).with_context(|| format!("odczyt {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("JSON {}", path.display()))
}

pub(crate) fn write_i64_json_array(path: &Path, values: &[i64]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    write_private_file(path, &serde_json::to_vec(values)?)
}

pub(crate) fn read_ksef_token_cache() -> Result<KsefTokenCache> {
    let text = secret_value(Secret::KsefAccessToken)?
        .ok_or_else(|| anyhow!("brak cache tokenu dostępowego KSeF"))?;
    serde_json::from_str(&text).context("niepoprawny cache tokenu dostępowego KSeF")
}

pub(crate) fn save_ksef_token_cache(cache: &KsefTokenCache) -> Result<()> {
    save_secret(
        Secret::KsefAccessToken,
        &serde_json::to_string_pretty(cache)?,
    )?;
    Ok(())
}

pub(crate) fn default_ksef_access_token_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("lab")
        .join("ksef_access_token.json")
}

pub(crate) fn ksef_metadata_to_record(item: &Value) -> Option<InvoiceRecord> {
    let ksef_number = json_string(item, "ksefNumber")?;
    let mut record = empty_record(SourceKind::Ksef);
    record.content_hash = format!("ksef:{ksef_number}");
    record.ksef_reference = Some(ksef_number.clone());
    record.source_path = Some(format!("ksef:{ksef_number}"));
    record.invoice_number = json_string(item, "invoiceNumber").map(|v| clean_invoice_number(&v));
    record.issue_date = json_string(item, "issueDate").and_then(|v| parse_date(&v));
    record.gross_amount_minor = money_value_to_minor(item.get("grossAmount"));
    record.net_amount_minor = money_value_to_minor(item.get("netAmount"));
    record.vat_amount_minor = money_value_to_minor(item.get("vatAmount"));
    record.currency = json_string(item, "currency").and_then(|v| normalize_currency(&v));

    if let Some(seller) = item.get("seller") {
        record.seller_tax_id = json_string(seller, "nip").and_then(|v| normalize_tax_id(&v));
        record.seller_name = json_string(seller, "name").and_then(|v| clean_name(&v));
    }
    if let Some(buyer) = item.get("buyer") {
        record.buyer_name = json_string(buyer, "name").and_then(|v| clean_name(&v));
        record.buyer_tax_id = buyer.get("identifier").and_then(|identifier| {
            let id_type = json_string(identifier, "type")?;
            if id_type.eq_ignore_ascii_case("Nip") {
                json_string(identifier, "value").and_then(|v| normalize_tax_id(&v))
            } else {
                None
            }
        });
    }
    record
        .warnings
        .push(KSEF_ONLINE_METADATA_MARKER.to_string());
    Some(record)
}

/// Provenance marker of a record built from KSeF online metadata; not a note for the user.
pub(crate) const KSEF_ONLINE_METADATA_MARKER: &str = "ksef online metadata";

pub(crate) fn ksef_sync(year: i32, input: &Path, out_dir: Option<&Path>) -> Result<KsefSyncResult> {
    let records = load_records(SourceKind::Ksef, input)?;
    let out_dir = ksef_sync_output_dir(year, out_dir);
    fs::create_dir_all(&out_dir).with_context(|| format!("mkdir {}", out_dir.display()))?;
    clear_ksef_cache_identity(&out_dir)?;
    let json_output = out_dir.join("records.json");
    let jsonl_output = out_dir.join("records.jsonl");
    write_records(&records, OutputFormat::Json, Some(&json_output))?;
    write_records(&records, OutputFormat::Jsonl, Some(&jsonl_output))?;
    write_ksef_cache_identity(
        &out_dir,
        &KsefCacheIdentity {
            year,
            source: "import".to_string(),
            base_url: None,
            context_type: None,
            context_value: None,
            fetched_at: Utc::now(),
        },
    )?;
    Ok(KsefSyncResult {
        summary: KsefSyncSummary {
            year,
            records_count: records.len(),
            input: input.display().to_string(),
            json_output: json_output.display().to_string(),
            jsonl_output: jsonl_output.display().to_string(),
        },
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::testing::{StoreMode, install};

    #[test]
    fn missing_ksef_token_is_detected() {
        let err = missing_ksef_token_error();
        assert!(is_missing_ksef_token(&err));
        assert!(is_missing_ksef_token(
            &anyhow!("KSeF HTTP client").context(missing_ksef_token_error())
        ));
        assert!(!is_missing_ksef_token(&anyhow!("inny błąd KSeF")));
    }

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lab-ksef-{tag}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn target(year: i32, base_url: &str, context_value: &str) -> KsefCacheTarget {
        KsefCacheTarget {
            year,
            base_url: base_url.to_string(),
            context_type: "Nip".to_string(),
            context_value: context_value.to_string(),
        }
    }

    const TEST_BASE_URL: &str = "https://api-test.ksef.mf.gov.pl/v2";

    fn write_cached_records(dir: &Path, issue_date: Option<&str>) {
        fs::create_dir_all(dir).unwrap();
        let mut record = empty_record(SourceKind::Ksef);
        record.content_hash = "ksef:cache-test".to_string();
        record.issue_date = issue_date.and_then(parse_date);
        fs::write(
            dir.join("records.jsonl"),
            serde_json::to_string(&record).unwrap(),
        )
        .unwrap();
    }

    fn metadata_page() -> Vec<Value> {
        vec![serde_json::json!({
            "ksefNumber": "1111111111-20260201-ABCDEF-01",
            "invoiceNumber": "FV/1/2026",
            "issueDate": "2026-02-01",
        })]
    }

    #[test]
    fn disabled_fresh_cache_preserves_explicit_tokenless_fallback() {
        let root = temp_root("cache");
        // Cache sprzed zapisu tożsamości: rok z nazwy katalogu, środowisko produkcyjne.
        let dir = root.join("ksef-2026");
        write_cached_records(&dir, None);
        let prod = target(2026, KSEF_PROD_BASE_URL, "1111111111");

        let fetches = std::cell::Cell::new(0);
        let result = ksef_sync_with_fetcher(2026, Some(&dir), None, None, &prod, |_| {
            fetches.set(fetches.get() + 1);
            Err(missing_ksef_token_error())
        })
        .unwrap();
        assert_eq!(fetches.get(), 1, "bez TTL zawsze najpierw online");
        assert_eq!(result.records.len(), 1);
        assert!(result.summary.input.starts_with("cache:"));

        let fresh = ksef_sync_with_fetcher(
            2026,
            Some(&dir),
            None,
            Some(Duration::from_secs(3600)),
            &prod,
            |_| panic!("świeży cache nie powinien iść online"),
        )
        .unwrap();
        assert_eq!(fresh.records.len(), 1);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn second_call_within_ttl_reuses_cache_and_ttl_zero_forces_refresh() {
        let root = temp_root("ttl");
        let dir = root.join("cache");
        let prod = target(2026, KSEF_PROD_BASE_URL, "1111111111");
        let fetches = std::cell::Cell::new(0);
        let fetch = |_: Option<Arc<Mutex<String>>>| {
            fetches.set(fetches.get() + 1);
            Ok(metadata_page())
        };
        let ttl = ksef_cache_ttl_from(None);
        assert_eq!(ttl, Some(Duration::from_secs(360 * 60)));

        // `sync`: brak cache → online, zapis z tożsamością.
        let first = ksef_sync_with_fetcher(2026, Some(&dir), None, ttl, &prod, fetch).unwrap();
        assert_eq!(fetches.get(), 1);
        assert!(first.summary.input.starts_with("online:"));
        let identity = read_ksef_cache_identity(&dir).unwrap().unwrap();
        assert_eq!(identity.year, 2026);
        assert_eq!(identity.base_url.as_deref(), Some(KSEF_PROD_BASE_URL));
        assert_eq!(identity.context_value.as_deref(), Some("1111111111"));

        // `reconcile` chwilę później: ten sam cache, bez zapytania online.
        let second = ksef_sync_with_fetcher(2026, Some(&dir), None, ttl, &prod, fetch).unwrap();
        assert_eq!(fetches.get(), 1);
        assert!(second.summary.input.starts_with("cache:"));
        assert_eq!(second.records.len(), 1);

        // KSEF_CACHE_TTL_MINS=0: zawsze online.
        let zero = ksef_cache_ttl_from(Some("0"));
        assert_eq!(zero, None);
        let third = ksef_sync_with_fetcher(2026, Some(&dir), None, zero, &prod, fetch).unwrap();
        assert_eq!(fetches.get(), 2);
        assert!(third.summary.input.starts_with("online:"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cache_refused_when_environment_context_or_year_differs() {
        let root = temp_root("identity");
        let dir = root.join("cache");
        let prod = target(2026, KSEF_PROD_BASE_URL, "1111111111");
        let ttl = Some(Duration::from_secs(3600));
        ksef_sync_with_fetcher(2026, Some(&dir), None, None, &prod, |_| Ok(metadata_page()))
            .unwrap();

        let refused = |year: i32, target: &KsefCacheTarget| match ksef_cache_lookup(
            year,
            Some(&dir),
            None,
            ttl,
            target,
        )
        .unwrap()
        {
            KsefCacheLookup::Refused(reason) => reason,
            KsefCacheLookup::Hit(_) => panic!("cache nie powinien pasować"),
            KsefCacheLookup::Missing | KsefCacheLookup::Stale => panic!("brak/stary cache"),
        };
        assert!(matches!(
            ksef_cache_lookup(2026, Some(&dir), None, ttl, &prod).unwrap(),
            KsefCacheLookup::Hit(_)
        ));
        assert!(refused(2026, &target(2026, TEST_BASE_URL, "1111111111")).contains("środowiska"));
        assert!(
            refused(2026, &target(2026, KSEF_PROD_BASE_URL, "2222222222")).contains("kontekstu")
        );
        assert!(refused(2025, &target(2025, KSEF_PROD_BASE_URL, "1111111111")).contains("roku"));

        // Brak tokenu + cache z innego środowiska: błąd z powodem, nadal "brak KSEF_TOKEN".
        let test_env = target(2026, TEST_BASE_URL, "1111111111");
        let err = ksef_sync_with_fetcher(2026, Some(&dir), None, None, &test_env, |_| {
            Err(missing_ksef_token_error())
        })
        .err()
        .unwrap();
        assert!(is_missing_ksef_token(&err));
        assert!(err.to_string().contains("nie używam"), "{err}");

        // Świeży, ale niepasujący cache → online; nowa tożsamość zastępuje starą.
        let fetches = std::cell::Cell::new(0);
        ksef_sync_with_fetcher(2026, Some(&dir), None, ttl, &test_env, |_| {
            fetches.set(fetches.get() + 1);
            Ok(metadata_page())
        })
        .unwrap();
        assert_eq!(fetches.get(), 1);
        let identity = read_ksef_cache_identity(&dir).unwrap().unwrap();
        assert_eq!(identity.base_url.as_deref(), Some(TEST_BASE_URL));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn legacy_cache_without_identity_only_for_production_and_its_year() {
        let root = temp_root("legacy");
        let ttl = Some(Duration::from_secs(3600));
        let prod = |year| target(year, KSEF_PROD_BASE_URL, "1111111111");
        let is_hit = |dir: &Path, year: i32, target: &KsefCacheTarget| {
            matches!(
                ksef_cache_lookup(year, Some(dir), None, ttl, target).unwrap(),
                KsefCacheLookup::Hit(_)
            )
        };

        let named = root.join("ksef-2026");
        write_cached_records(&named, None);
        assert!(is_hit(&named, 2026, &prod(2026)));
        assert!(!is_hit(&named, 2025, &prod(2025)));
        assert!(!is_hit(
            &named,
            2026,
            &target(2026, TEST_BASE_URL, "1111111111")
        ));

        // Katalog bez roku w nazwie: rok z dat wystawienia; bez dat — odrzucony.
        let dated = root.join("dane");
        write_cached_records(&dated, Some("2026-03-15"));
        assert!(is_hit(&dated, 2026, &prod(2026)));
        assert!(!is_hit(&dated, 2025, &prod(2025)));
        let undated = root.join("bez-dat");
        write_cached_records(&undated, None);
        assert!(!is_hit(&undated, 2026, &prod(2026)));

        // Następny udany fetch zapisuje tożsamość.
        assert!(read_ksef_cache_identity(&named).unwrap().is_none());
        ksef_sync_with_fetcher(2026, Some(&named), None, None, &prod(2026), |_| {
            Ok(metadata_page())
        })
        .unwrap();
        let identity = read_ksef_cache_identity(&named).unwrap().unwrap();
        assert_eq!(identity.source, "online");
        assert_eq!(identity.base_url.as_deref(), Some(KSEF_PROD_BASE_URL));

        // Import lokalnego eksportu: rok zapisany, środowisko nieznane → tylko produkcja.
        let imported = root.join("import");
        let input = root.join("export.jsonl");
        write_cached_records(&root.join("export-src"), None);
        fs::copy(root.join("export-src").join("records.jsonl"), &input).unwrap();
        ksef_sync(2026, &input, Some(&imported)).unwrap();
        let identity = read_ksef_cache_identity(&imported).unwrap().unwrap();
        assert_eq!((identity.year, identity.source.as_str()), (2026, "import"));
        assert!(is_hit(&imported, 2026, &prod(2026)));
        assert!(!is_hit(&imported, 2025, &prod(2025)));
        assert!(!is_hit(
            &imported,
            2026,
            &target(2026, TEST_BASE_URL, "1111111111")
        ));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn ksef_data_dir_uses_per_year_directory() {
        let root = temp_root("datadir");
        let data_dir = root.join("dane");
        fs::create_dir_all(&data_dir).unwrap();
        assert_eq!(
            ksef_data_dir_year_path(&data_dir, 2026),
            data_dir.join("ksef-2026")
        );
        assert_eq!(
            ksef_data_dir_year_path(&data_dir, 2025),
            data_dir.join("ksef-2025")
        );

        // Stary katalog bez podkatalogu roku: czytany tylko dla roku z jego tożsamości.
        write_cached_records(&data_dir, Some("2026-01-10"));
        assert_eq!(ksef_data_dir_year_path(&data_dir, 2026), data_dir);
        assert_eq!(
            ksef_data_dir_year_path(&data_dir, 2025),
            data_dir.join("ksef-2025")
        );
        write_ksef_cache_identity(
            &data_dir,
            &target(2025, KSEF_PROD_BASE_URL, "1111111111").online_identity(),
        )
        .unwrap();
        assert_eq!(ksef_data_dir_year_path(&data_dir, 2025), data_dir);
        assert_eq!(
            ksef_data_dir_year_path(&data_dir, 2026),
            data_dir.join("ksef-2026")
        );

        // Istniejący katalog roku ma pierwszeństwo.
        write_cached_records(&data_dir.join("ksef-2025"), Some("2025-05-05"));
        assert_eq!(
            ksef_data_dir_year_path(&data_dir, 2025),
            data_dir.join("ksef-2025")
        );
        fs::remove_dir_all(&root).unwrap();
    }

    fn token(value: &str, valid_until: Option<DateTime<Utc>>) -> KsefAccessToken {
        KsefAccessToken {
            token: value.to_string(),
            valid_until,
        }
    }

    #[test]
    fn access_token_renewed_before_request_when_near_expiry() {
        let now = Utc::now();
        let renewals = std::cell::RefCell::new(Vec::new());
        let sent = std::cell::RefCell::new(Vec::new());
        let mut current = token("stary", Some(now + chrono::Duration::seconds(30)));
        let value = ksef_authorized_request(
            &mut current,
            "test",
            || Ok(()),
            || now,
            |rejected| {
                renewals.borrow_mut().push(rejected.map(str::to_string));
                Ok(token("nowy", Some(now + chrono::Duration::minutes(15))))
            },
            |token| {
                sent.borrow_mut().push(token.to_string());
                Ok(KsefAuthorized::Done(7))
            },
        )
        .unwrap();
        assert_eq!(value, 7);
        assert_eq!(*renewals.borrow(), vec![None]);
        assert_eq!(*sent.borrow(), vec!["nowy".to_string()]);
        assert_eq!(current.token, "nowy");

        // Ważny token i token bez znanej ważności: bez odnawiania.
        for mut valid in [
            token("ważny", Some(now + chrono::Duration::minutes(10))),
            token("env", None),
        ] {
            ksef_authorized_request(
                &mut valid,
                "test",
                || Ok(()),
                || now,
                |_| panic!("nie odnawiaj ważnego tokenu"),
                |_| Ok(KsefAuthorized::Done(())),
            )
            .unwrap();
        }
    }

    #[test]
    fn unauthorized_response_retried_once_after_reauthentication() {
        let now = Utc::now();
        let far = Some(now + chrono::Duration::minutes(15));
        let renewals = std::cell::RefCell::new(Vec::new());
        let sent = std::cell::RefCell::new(Vec::new());
        let waits = std::cell::Cell::new(0);
        let mut current = token("odrzucony", far);
        let value = ksef_authorized_request(
            &mut current,
            "test",
            || {
                waits.set(waits.get() + 1);
                Ok(())
            },
            || now,
            |rejected| {
                renewals.borrow_mut().push(rejected.map(str::to_string));
                Ok(token("nowy", far))
            },
            |token| {
                sent.borrow_mut().push(token.to_string());
                Ok(if token == "odrzucony" {
                    KsefAuthorized::Unauthorized("expired".to_string())
                } else {
                    KsefAuthorized::Done("ok")
                })
            },
        )
        .unwrap();
        assert_eq!(value, "ok");
        assert_eq!(*renewals.borrow(), vec![Some("odrzucony".to_string())]);
        assert_eq!(*sent.borrow(), vec!["odrzucony", "nowy"]);
        assert_eq!(waits.get(), 2, "powtórka też przechodzi przez limiter");

        // Ciągłe 401: jedno ponowne logowanie, dwa zapytania, potem błąd.
        let renew_count = std::cell::Cell::new(0);
        let send_count = std::cell::Cell::new(0);
        let mut current = token("a", far);
        let err = ksef_authorized_request(
            &mut current,
            "test",
            || Ok(()),
            || now,
            |_| {
                renew_count.set(renew_count.get() + 1);
                Ok(token("b", far))
            },
            |_| {
                send_count.set(send_count.get() + 1);
                Ok(KsefAuthorized::<()>::Unauthorized("nope".to_string()))
            },
        )
        .unwrap_err();
        assert_eq!((renew_count.get(), send_count.get()), (1, 2));
        assert!(err.to_string().contains("401"), "{err}");
    }

    #[test]
    fn online_config_does_not_require_ksef_token() {
        let root = std::env::temp_dir().join(format!(
            "lab-ksef-config-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let (_store, _guard) = install(root, &[], StoreMode::Available);
        let config = ksef_online_config().unwrap();
        assert!(config.ksef_token.is_none());
        assert_eq!(config.context_value, DEFAULT_PRODUCTMESH_NIP);
    }

    #[test]
    fn exact_ranges_are_the_plain_quarters_and_bad_request_is_detected() {
        let exact = ksef_year_exact_quarter_ranges(2026);
        assert_eq!(exact.len(), ksef_year_quarter_ranges(2026).len());
        assert_eq!(exact[0].0, "2026-01-01T00:00:00+00:00");
        assert_eq!(exact[0].1, "2026-04-01T00:00:00+00:00");
        assert_eq!(exact[3].0, "2026-10-01T00:00:00+00:00");
        assert_eq!(exact[3].1, "2027-01-01T00:00:00+00:00");
        // Only the first and last widened ranges differ from the exact ones.
        let wide = ksef_year_quarter_ranges(2026);
        assert_ne!(wide[0], exact[0]);
        assert_eq!(wide[1], exact[1]);
        assert_eq!(wide[2], exact[2]);
        assert_ne!(wide[3], exact[3]);
        assert!(ksef_error_is_bad_request(&anyhow!(
            "KSeF query invoice metadata HTTP 400 Bad Request: zakres"
        )));
        assert!(!ksef_error_is_bad_request(&anyhow!(
            "KSeF query invoice metadata HTTP 403 Forbidden: x"
        )));
    }

    #[test]
    fn year_ranges_cover_neighbour_days_without_gaps() {
        let parse = |value: &str| {
            DateTime::parse_from_rfc3339(value)
                .unwrap()
                .with_timezone(&Utc)
        };
        let day = |y: i32, m: u32, d: u32| {
            NaiveDate::from_ymd_opt(y, m, d)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
        };
        for year in [2024, 2026] {
            let ranges = ksef_year_quarter_ranges(year);
            // Same number of metadata requests per subject type as before.
            assert_eq!(ranges.len(), 4);
            let bounds = ranges
                .iter()
                .map(|(from, to)| (parse(from), parse(to)))
                .collect::<Vec<_>>();
            assert!(bounds[0].0 <= day(year - 1, 12, 31));
            // 1 Jan of the next year is wholly inside, even with an exclusive `to`.
            assert!(bounds[3].1 >= day(year + 1, 1, 2));
            for (from, to) in &bounds {
                assert!(from < to, "{from}..{to}");
            }
            for pair in bounds.windows(2) {
                assert!(pair[1].0 <= pair[0].1, "gap between {pair:?}");
            }
        }
    }

    #[test]
    fn year_records_drop_neighbour_days_and_dedupe_overlaps() {
        let item = |number: &str, issue_date: Option<&str>| {
            let mut item = serde_json::json!({
                "ksefNumber": number,
                "invoiceNumber": format!("FV/{number}"),
            });
            if let Some(date) = issue_date {
                item["issueDate"] = serde_json::json!(date);
            }
            item
        };
        let metadata = vec![
            item("1111111111-20251231-AAAAAAAAAAAA-01", Some("2025-12-31")),
            item("1111111111-20260101-BBBBBBBBBBBB-02", Some("2026-01-01")),
            item("1111111111-20260401-CCCCCCCCCCCC-03", Some("2026-04-01")),
            // Same document from the overlapping neighbour range.
            item("1111111111-20260401-CCCCCCCCCCCC-03", Some("2026-04-01")),
            item("1111111111-20260101-bbbbbbbbbbbb-02", Some("2026-01-01")),
            item("1111111111-20261231-DDDDDDDDDDDD-04", Some("2026-12-31")),
            item("1111111111-20270101-EEEEEEEEEEEE-05", Some("2027-01-01")),
            item("1111111111-20260000-FFFFFFFFFFFF-06", None),
            item("1111111111-20260000-GGGGGGGGGGGG-07", Some("bez daty")),
        ];
        let records = ksef_metadata_records_for_year(&metadata, 2026);
        let numbers = records
            .iter()
            .map(|record| record.ksef_reference.clone().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            numbers,
            vec![
                "1111111111-20260101-BBBBBBBBBBBB-02",
                "1111111111-20260401-CCCCCCCCCCCC-03",
                "1111111111-20261231-DDDDDDDDDDDD-04",
                "1111111111-20260000-FFFFFFFFFFFF-06",
                "1111111111-20260000-GGGGGGGGGGGG-07",
            ]
        );
    }

    #[test]
    fn stored_online_metadata_keeps_only_the_requested_year() {
        let root = temp_root("year-filter");
        let dir = root.join("cache");
        let prod = target(2026, KSEF_PROD_BASE_URL, "1111111111");
        let mut metadata = metadata_page();
        metadata.push(serde_json::json!({
            "ksefNumber": "1111111111-20270101-ABCDEF-02",
            "invoiceNumber": "FV/1/2027",
            "issueDate": "2027-01-01",
        }));
        let result =
            ksef_sync_with_fetcher(2026, Some(&dir), None, None, &prod, |_| Ok(metadata)).unwrap();
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.summary.records_count, 1);
        assert_eq!(
            result.records[0].invoice_number.as_deref(),
            Some("FV/1/2026")
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
