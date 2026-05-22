use crate::*;

#[derive(Debug, Clone)]
pub(crate) struct KsefOnlineConfig {
    base_url: String,
    context_type: String,
    context_value: String,
    ksef_token: String,
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
    if use_fresh_cache {
        if let Some(result) = ksef_fresh_cached_sync_result(year, out_dir, progress.clone())? {
            return Ok(result);
        }
    }
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
    let access_token = ksef_access_token(&client, &config)?;
    let page_size = ksef_metadata_page_size();
    let mut metadata = Vec::new();

    for subject_type in ksef_subject_types() {
        for (from, to) in ksef_year_quarter_ranges(year) {
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
                ksef_rate_limit_wait_with_progress("metadata", 8, 16, 20, progress.clone())?;
                let url = format!("{}/invoices/query/metadata", config.base_url);
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
                let response: KsefQueryMetadataResponse = ksef_send_with_retry(
                    client
                        .post(&url)
                        .bearer_auth(&access_token)
                        .header("X-Error-Format", "problem-details")
                        .query(&query)
                        .json(&body),
                    "query invoice metadata",
                )?
                .json()
                .context("KSeF metadata response JSON")?;

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

    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!(
                "KSeF: deduplikacja i zapis {} metadanych...",
                metadata.len()
            ),
        );
    }
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    for item in &metadata {
        if let Some(record) = ksef_metadata_to_record(item) {
            if seen.insert(record.content_hash.clone()) {
                records.push(record);
            }
        }
    }

    let out_dir = ksef_sync_output_dir(year, out_dir);
    fs::create_dir_all(&out_dir).with_context(|| format!("mkdir {}", out_dir.display()))?;
    let raw_output = out_dir.join("ksef_raw_metadata.json");
    let json_output = out_dir.join("records.json");
    let jsonl_output = out_dir.join("records.jsonl");
    fs::write(&raw_output, serde_json::to_vec_pretty(&metadata)?)
        .with_context(|| format!("zapis {}", raw_output.display()))?;
    write_records(&records, OutputFormat::Json, Some(&json_output))?;
    write_records(&records, OutputFormat::Jsonl, Some(&jsonl_output))?;

    Ok(KsefSyncResult {
        summary: KsefSyncSummary {
            year,
            records_count: records.len(),
            input: format!(
                "online:{}:{}:{}",
                config.base_url, config.context_type, config.context_value
            ),
            json_output: json_output.display().to_string(),
            jsonl_output: jsonl_output.display().to_string(),
        },
        records,
    })
}

pub(crate) fn ksef_online_config() -> Result<KsefOnlineConfig> {
    let base_url = ksef_base_url();
    let context_type = lab_config_var("KSEF_CONTEXT_TYPE").unwrap_or_else(|| "Nip".to_string());
    let raw_context = lab_config_var("KSEF_CONTEXT_NIP")
        .or_else(|| lab_config_var("KSEF_NIP"))
        .unwrap_or_else(|| DEFAULT_PRODUCTMESH_NIP.to_string());
    let context_value = if context_type.eq_ignore_ascii_case("Nip") {
        normalize_tax_id(&raw_context).unwrap_or(raw_context)
    } else {
        raw_context
    };
    let ksef_token = lab_config_var("KSEF_TOKEN").ok_or_else(|| {
        anyhow!("brak KSEF_TOKEN; potrzebny token KSeF z uprawnieniem InvoiceRead")
    })?;
    Ok(KsefOnlineConfig {
        base_url,
        context_type,
        context_value,
        ksef_token,
    })
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
            _ => "https://api.ksef.mf.gov.pl/v2".to_string(),
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

pub(crate) fn ksef_cache_ttl() -> Option<Duration> {
    let minutes = lab_config_var("KSEF_CACHE_TTL_MINS")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(360);
    (minutes > 0).then_some(Duration::from_secs(minutes * 60))
}

pub(crate) fn ksef_fresh_cached_sync_result(
    year: i32,
    out_dir: Option<&Path>,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<Option<KsefSyncResult>> {
    let Some(ttl) = ksef_cache_ttl() else {
        return Ok(None);
    };
    let out_dir = ksef_sync_output_dir(year, out_dir);
    let jsonl_output = out_dir.join("records.jsonl");
    if !jsonl_output.is_file() {
        if let Some(progress) = &progress {
            set_progress(progress, "KSeF: brak lokalnego cache, odświeżam online...");
        }
        return Ok(None);
    }
    let age = fs::metadata(&jsonl_output)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok());
    let Some(age) = age else {
        return Ok(None);
    };
    if age > ttl {
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "KSeF: cache ma {}, odświeżam online...",
                    format_duration_short(age)
                ),
            );
        }
        return Ok(None);
    }
    let records = load_records(SourceKind::Ksef, &jsonl_output)?;
    if let Some(progress) = &progress {
        set_progress(
            progress,
            format!(
                "KSeF: lokalny cache świeży ({}; {} rekordów), pomijam online",
                format_duration_short(age),
                records.len()
            ),
        );
    }
    let json_output = out_dir.join("records.json");
    Ok(Some(KsefSyncResult {
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

pub(crate) fn ksef_year_quarter_ranges(year: i32) -> Vec<(String, String)> {
    let starts = [(year, 1, 1), (year, 4, 1), (year, 7, 1), (year, 10, 1)];
    let ends = [(year, 4, 1), (year, 7, 1), (year, 10, 1), (year + 1, 1, 1)];
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

pub(crate) fn ksef_access_token(client: &Client, config: &KsefOnlineConfig) -> Result<String> {
    if let Some(token) = lab_config_var("KSEF_ACCESS_TOKEN") {
        return Ok(token);
    }

    if let Ok(cache) = read_ksef_token_cache()
        && cache.base_url == config.base_url
        && cache.context_type == config.context_type
        && cache.context_value == config.context_value
    {
        if cache.access_valid_until > Utc::now() + chrono::Duration::seconds(60) {
            return Ok(cache.access_token);
        }
        if let (Some(refresh_token), Some(refresh_valid_until)) =
            (cache.refresh_token.clone(), cache.refresh_valid_until)
            && refresh_valid_until > Utc::now() + chrono::Duration::seconds(60)
            && let Ok(refreshed) = ksef_refresh_access_token(client, config, &cache, &refresh_token)
        {
            return Ok(refreshed);
        }
    }

    ksef_authenticate_with_ksef_token(client, config)
}

pub(crate) fn ksef_refresh_access_token(
    client: &Client,
    config: &KsefOnlineConfig,
    cache: &KsefTokenCache,
    refresh_token: &str,
) -> Result<String> {
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
    Ok(new_cache.access_token)
}

pub(crate) fn ksef_authenticate_with_ksef_token(
    client: &Client,
    config: &KsefOnlineConfig,
) -> Result<String> {
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

    let token_with_timestamp = format!("{}|{}", config.ksef_token, challenge.timestamp_ms);
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
    Ok(cache.access_token)
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
        fs::write(&cert_path, cert).with_context(|| format!("zapis {}", cert_path.display()))?;
        fs::write(&plain_path, plaintext.as_bytes())
            .with_context(|| format!("zapis {}", plain_path.display()))?;

        let output = Command::new("openssl")
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
        fs::write(&pub_path, output.stdout)
            .with_context(|| format!("zapis {}", pub_path.display()))?;

        let output = Command::new("openssl")
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
    fs::write(path, serde_json::to_vec(values)?)
        .with_context(|| format!("zapis {}", path.display()))
}

pub(crate) fn read_ksef_token_cache() -> Result<KsefTokenCache> {
    let path = default_ksef_access_token_path();
    let text = fs::read_to_string(&path).with_context(|| format!("odczyt {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("JSON {}", path.display()))
}

pub(crate) fn save_ksef_token_cache(cache: &KsefTokenCache) -> Result<()> {
    let path = default_ksef_access_token_path();
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    fs::write(&path, serde_json::to_vec_pretty(cache)?)
        .with_context(|| format!("zapis {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
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
    record.warnings.push("ksef online metadata".to_string());
    Some(record)
}

pub(crate) fn ksef_sync(year: i32, input: &Path, out_dir: Option<&Path>) -> Result<KsefSyncResult> {
    let records = load_records(SourceKind::Ksef, input)?;
    let out_dir = out_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| configured_ksef_out_path(year));
    fs::create_dir_all(&out_dir).with_context(|| format!("mkdir {}", out_dir.display()))?;
    let json_output = out_dir.join("records.json");
    let jsonl_output = out_dir.join("records.jsonl");
    write_records(&records, OutputFormat::Json, Some(&json_output))?;
    write_records(&records, OutputFormat::Jsonl, Some(&jsonl_output))?;
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
