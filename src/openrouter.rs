use crate::*;
use base64::{Engine, engine::general_purpose::STANDARD};

const OPENROUTER_CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub(crate) const DEFAULT_OPENROUTER_MODEL: &str = "google/gemini-3.8-flash";

/// A failure worth retrying on a later sync: timeout, network, HTTP 429/5xx, or
/// a request that never left the machine.
#[derive(Debug)]
pub(crate) struct TransientLlmError(pub(crate) String);

impl std::fmt::Display for TransientLlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TransientLlmError {}

pub(crate) fn transient_llm_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(TransientLlmError(message.into()))
}

pub(crate) fn http_status_is_transient(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

pub(crate) fn llm_error_is_transient(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.downcast_ref::<TransientLlmError>().is_some()
            || cause.downcast_ref::<reqwest::Error>().is_some_and(|err| {
                err.is_timeout()
                    || err.is_connect()
                    || err.is_request()
                    || err.is_body()
                    || err.is_decode()
                    || err
                        .status()
                        .is_some_and(|status| http_status_is_transient(status.as_u16()))
            })
    })
}

pub(crate) fn openrouter_configured() -> bool {
    secret_is_set(Secret::OpenRouterApiKey)
}

pub(crate) fn openrouter_model() -> String {
    lab_config_var("LAB_OPENROUTER_MODEL").unwrap_or_else(|| DEFAULT_OPENROUTER_MODEL.to_string())
}

pub(crate) fn strip_openrouter_batch_variant(model: &str) -> String {
    let stripped = model
        .split(':')
        .filter(|part| !part.eq_ignore_ascii_case("batch"))
        .collect::<Vec<_>>()
        .join(":");
    if stripped.is_empty() {
        DEFAULT_OPENROUTER_MODEL.to_string()
    } else {
        stripped
    }
}

pub(crate) fn openrouter_chat_model() -> String {
    strip_openrouter_batch_variant(&openrouter_model())
}

pub(crate) fn openrouter_progress_line(processed: usize, todo: usize, filename: &str) -> String {
    let percent = processed
        .saturating_mul(100)
        .checked_div(todo)
        .unwrap_or(100);
    format!("OpenRouter {processed}/{todo} ({percent}%) {filename}")
}

fn openrouter_timeout() -> Duration {
    Duration::from_secs(
        lab_config_var("LAB_OPENROUTER_TIMEOUT_SECS")
            .and_then(|value| value.parse().ok())
            .unwrap_or(120)
            .clamp(15, 300),
    )
}

fn openrouter_pdf_engine() -> String {
    lab_config_var("LAB_OPENROUTER_PDF_ENGINE").unwrap_or_else(|| "native".to_string())
}

fn nullable_string(description: &str) -> Value {
    serde_json::json!({
        "type": ["string", "null"],
        "description": description
    })
}

pub(crate) fn invoice_structured_schema() -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "invoice_number".into(),
        nullable_string("Numer faktury z dokumentu, albo null"),
    );
    properties.insert(
        "issue_date".into(),
        nullable_string("Data wystawienia YYYY-MM-DD, albo null"),
    );
    properties.insert(
        "sale_date".into(),
        nullable_string("Data sprzedaży YYYY-MM-DD, albo null"),
    );
    properties.insert(
        "due_date".into(),
        nullable_string("Termin płatności YYYY-MM-DD, albo null"),
    );
    properties.insert(
        "gross_amount".into(),
        nullable_string("Kwota brutto z kropką i dwoma miejscami, np. 1234.56, albo null"),
    );
    properties.insert(
        "net_amount".into(),
        nullable_string("Kwota netto z kropką i dwoma miejscami, albo null"),
    );
    properties.insert(
        "vat_amount".into(),
        nullable_string("Kwota VAT z kropką i dwoma miejscami, albo null"),
    );
    properties.insert(
        "currency".into(),
        nullable_string("Kod waluty PLN, EUR, USD albo GBP, albo null"),
    );
    properties.insert(
        "seller_tax_id".into(),
        nullable_string("Polski NIP sprzedawcy, 10 cyfr, albo null"),
    );
    properties.insert(
        "buyer_tax_id".into(),
        nullable_string("Polski NIP nabywcy, 10 cyfr, albo null"),
    );
    properties.insert(
        "seller_name".into(),
        nullable_string("Pełna nazwa sprzedawcy, nie nagłówek, albo null"),
    );
    properties.insert(
        "buyer_name".into(),
        nullable_string("Pełna nazwa nabywcy, nie nagłówek, albo null"),
    );
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": invoice_validation::FIELDS,
        "properties": properties
    })
}

pub(crate) fn openrouter_invoice_request(model: &str, filename: &str, pdf_base64: &str) -> Value {
    serde_json::json!({
        "model": model,
        "temperature": 0,
        "max_tokens": 4000,
        "provider": { "require_parameters": true },
        "plugins": [{
            "id": "file-parser",
            "pdf": { "engine": openrouter_pdf_engine() }
        }],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "invoice_fields",
                "strict": true,
                "schema": invoice_structured_schema()
            }
        },
        "messages": [{
            "role": "user",
            "content": [
                {
                    "type": "text",
                    "text": "Odczytaj fakturę z załączonego PDF. Zwróć tylko dane widoczne na dokumencie. Nie zgaduj i nie wyliczaj kwot. Jeśli pole nie jest pewne, użyj null. Daty: YYYY-MM-DD. Kwoty: kropka i dwa miejsca, bez waluty. Polski NIP: 10 cyfr. Zagraniczny VAT w polu NIP: null. Nazwy: pełna nazwa strony, nie nagłówek w stylu Nabywca albo DETAILS. Dla faktur zakupowych Productmesh nabywca to zwykle Productmesh, a sprzedawca to kontrahent."
                },
                {
                    "type": "file",
                    "file": {
                        "filename": filename,
                        "file_data": format!("data:application/pdf;base64,{pdf_base64}")
                    }
                }
            ]
        }]
    })
}

pub(crate) fn openrouter_response_json(response: &Value) -> Result<Value> {
    if let Some(error) = response.get("error")
        && let Some(message) = error.get("message").and_then(Value::as_str)
    {
        let code = error.get("code").and_then(|code| {
            code.as_u64()
                .or_else(|| code.as_str().and_then(|s| s.parse().ok()))
        });
        if code.is_some_and(|code| u16::try_from(code).is_ok_and(http_status_is_transient)) {
            return Err(transient_llm_error(format!("OpenRouter: {message}")));
        }
        return Err(anyhow!("OpenRouter: {message}"));
    }
    let choice = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| anyhow!("OpenRouter: brak odpowiedzi modelu"))?;
    if choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| reason != "stop")
    {
        return Err(anyhow!(
            "OpenRouter: odpowiedź nie została zakończona poprawnie; nie zapisuję częściowego JSON"
        ));
    }
    let content = choice
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("OpenRouter: brak treści odpowiedzi"))?;
    crate::parse_json_from_llm(content)
}

pub(crate) fn openrouter_extract_invoice_fields(
    record: &mut InvoiceRecord,
    path: &Path,
) -> Result<bool> {
    if record_is_password_protected(record) || pdf_is_password_protected(path) {
        if !record
            .warnings
            .iter()
            .any(|warning| is_pdf_password_warning(warning))
        {
            record.warnings.push(PDF_PASSWORD_WARNING.to_string());
        }
        return Err(anyhow!(PDF_PASSWORD_WARNING));
    }
    let meta = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if meta.len() > MAX_OCR_PDF_BYTES {
        return Err(anyhow!(
            "PDF przekracza limit OCR {MAX_OCR_PDF_BYTES} bajtów; nie wysyłam do OpenRouter"
        ));
    }
    let bytes = fs::read(path).with_context(|| format!("odczyt {}", path.display()))?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("invoice.pdf");
    let pdf_base64 = STANDARD.encode(&bytes);
    // Nothing is sent without a key, so these failures must not block a later try.
    let api_key = secret_value(Secret::OpenRouterApiKey)
        .map_err(|err| transient_llm_error(format!("odczyt klucza OpenRouter: {err:#}")))?
        .ok_or_else(|| {
            transient_llm_error(
                "brak OPENROUTER_API_KEY; ustaw klucz OpenRouter albo użyj lokalnego LLM",
            )
        })?;
    let model = openrouter_chat_model();
    let body = openrouter_invoice_request(&model, filename, &pdf_base64);
    let client = Client::builder().timeout(openrouter_timeout()).build()?;
    let response = openrouter_send_with_retry(
        || match client
            .post(OPENROUTER_CHAT_URL)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://github.com/wydrox/lab")
            .header("X-Title", "LAB")
            .json(&body)
            .send()
        {
            Ok(resp) if resp.status().is_success() => resp
                .json::<Value>()
                .map_err(|err| (OpenRouterFailure::Other, anyhow::Error::from(err))),
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().unwrap_or_default();
                let message = format!("OpenRouter HTTP {status}: {text}");
                let err = if http_status_is_transient(status.as_u16()) {
                    transient_llm_error(message)
                } else {
                    anyhow!(message)
                };
                Err((OpenRouterFailure::Status(status.as_u16()), err))
            }
            Err(err) => Err((openrouter_failure_kind(&err), anyhow::Error::from(err))),
        },
        std::thread::sleep,
    )?;
    let value = openrouter_response_json(&response)?;
    invoice_validation::apply_normalized_invoice_json(record, &value)
}

const OPENROUTER_MAX_ATTEMPTS: u32 = 4;

/// Why a request to OpenRouter failed, as far as retrying is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenRouterFailure {
    /// No connection was made, so nothing reached OpenRouter.
    Connect,
    /// No answer in time; the PDF may have been processed and billed.
    Timeout,
    /// Any other client-side failure after the request may have been sent.
    Other,
    Status(u16),
}

// A connect error (including a connect timeout) happens before any byte of the
// request is sent; every other timeout may come after OpenRouter got the PDF.
pub(crate) fn openrouter_failure_kind(err: &reqwest::Error) -> OpenRouterFailure {
    if err.is_connect() {
        OpenRouterFailure::Connect
    } else if err.is_timeout() {
        OpenRouterFailure::Timeout
    } else {
        OpenRouterFailure::Other
    }
}

/// Retry only when the request provably was not processed: no connection, rate
/// limit, or a gateway that did not reach the model. A timeout is not retried here;
/// the attempts ledger marks it transient so a later sync may try again.
pub(crate) fn openrouter_failure_is_retryable(failure: OpenRouterFailure) -> bool {
    matches!(
        failure,
        OpenRouterFailure::Connect | OpenRouterFailure::Status(429 | 502 | 503 | 504)
    )
}

pub(crate) fn openrouter_send_with_retry<T>(
    mut send: impl FnMut() -> std::result::Result<T, (OpenRouterFailure, anyhow::Error)>,
    mut wait: impl FnMut(Duration),
) -> Result<T> {
    let mut attempt = 0;
    loop {
        let (failure, err) = match send() {
            Ok(value) => return Ok(value),
            Err(failed) => failed,
        };
        attempt += 1;
        if !openrouter_failure_is_retryable(failure) || attempt >= OPENROUTER_MAX_ATTEMPTS {
            return Err(match failure {
                OpenRouterFailure::Timeout => err.context(
                    "OpenRouter nie odpowiedział w limicie czasu; nie ponawiam od razu, bo PDF mógł zostać przetworzony i rozliczony",
                ),
                _ => err,
            });
        }
        let delay = Duration::from_secs(2u64.pow(attempt - 1));
        eprintln!(
            "  [Gmail/OpenRouter] {}, ponawiam za {}s...",
            match failure {
                OpenRouterFailure::Status(status) => format!("HTTP {status}"),
                _ => "brak połączenia".to_string(),
            },
            delay.as_secs()
        );
        wait(delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_covers_invoice_fields() {
        let schema = invoice_structured_schema();
        let required = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert_eq!(required, invoice_validation::FIELDS);
        assert_eq!(schema["additionalProperties"], false);
        for field in invoice_validation::FIELDS {
            assert_eq!(schema["properties"][field]["type"][0], "string");
            assert_eq!(schema["properties"][field]["type"][1], "null");
        }
    }

    #[test]
    fn request_uses_structured_output_and_native_pdf() {
        let body = openrouter_invoice_request(
            DEFAULT_OPENROUTER_MODEL,
            "Invoice-M73SJH5X-0005.pdf",
            "AAA",
        );
        assert_eq!(body["model"], DEFAULT_OPENROUTER_MODEL);
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(body["plugins"][0]["pdf"]["engine"], "native");
        assert_eq!(body["provider"]["require_parameters"], true);
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "file");
        let file = &content[1]["file"];
        assert_eq!(file["filename"], "Invoice-M73SJH5X-0005.pdf");
        assert_eq!(file["file_data"], "data:application/pdf;base64,AAA");
        let encoded = serde_json::to_string(&body).unwrap();
        assert!(!encoded.contains("Bearer"));
        assert!(!encoded.contains("OPENROUTER"));
        assert!(!encoded.contains("Tekst PDF"));
    }

    #[test]
    fn chat_completions_use_base_model_and_progress() {
        assert_eq!(
            strip_openrouter_batch_variant("google/gemini-3.8-flash:batch"),
            "google/gemini-3.8-flash"
        );
        assert_eq!(
            strip_openrouter_batch_variant("google/gemini-3.8-flash:batch:nitro"),
            "google/gemini-3.8-flash:nitro"
        );
        assert_eq!(
            openrouter_progress_line(2, 8, "Invoice.pdf"),
            "OpenRouter 2/8 (25%) Invoice.pdf"
        );
    }

    #[test]
    fn complete_json_is_accepted_and_truncated_is_rejected() {
        let ok = serde_json::json!({
            "choices":[{"finish_reason":"stop","message":{"content":"{\"seller_name\":\"Elocity\"}"}}]
        });
        assert_eq!(
            openrouter_response_json(&ok).unwrap()["seller_name"],
            "Elocity"
        );
        let truncated = serde_json::json!({
            "choices":[{"finish_reason":"length","message":{"content":"{\"seller_name\":\"Elocity\"}"}}]
        });
        assert!(openrouter_response_json(&truncated).is_err());
        let api_err = serde_json::json!({"error":{"message":"insufficient credits"}});
        let err = openrouter_response_json(&api_err).unwrap_err().to_string();
        assert!(err.contains("insufficient credits"));
        assert!(!err.contains("sk-"));
    }

    #[test]
    fn only_unprocessed_requests_are_retried() {
        use OpenRouterFailure::*;
        for failure in [Connect, Status(429), Status(502), Status(503), Status(504)] {
            assert!(openrouter_failure_is_retryable(failure), "{failure:?}");
        }
        for failure in [Timeout, Other, Status(500), Status(400), Status(402)] {
            assert!(!openrouter_failure_is_retryable(failure), "{failure:?}");
        }
    }

    fn run_with(failures: &[OpenRouterFailure]) -> (Result<&'static str>, usize, Vec<Duration>) {
        let mut calls = 0;
        let mut waits = Vec::new();
        let result = openrouter_send_with_retry(
            || {
                calls += 1;
                match failures.get(calls - 1) {
                    Some(&failure) => Err((failure, transient_llm_error(format!("{failure:?}")))),
                    None => Ok("ok"),
                }
            },
            |delay| waits.push(delay),
        );
        (result, calls, waits)
    }

    #[test]
    fn timeout_is_not_retried_but_stays_transient() {
        let (result, calls, waits) = run_with(&[OpenRouterFailure::Timeout]);
        let err = result.unwrap_err();
        assert_eq!(calls, 1);
        assert!(waits.is_empty());
        assert!(format!("{err:#}").contains("nie ponawiam"), "{err:#}");
        assert!(llm_error_is_transient(&err));
    }

    #[test]
    fn rate_limit_and_unavailable_are_retried() {
        let (result, calls, waits) = run_with(&[
            OpenRouterFailure::Status(503),
            OpenRouterFailure::Status(429),
            OpenRouterFailure::Connect,
        ]);
        assert_eq!(result.unwrap(), "ok");
        assert_eq!(calls, 4);
        assert_eq!(
            waits,
            [1, 2, 4].map(Duration::from_secs),
            "exponential back-off"
        );
        // Bounded: four attempts, then the last error.
        let (result, calls, _) = run_with(&[OpenRouterFailure::Status(503); 6]);
        assert!(llm_error_is_transient(&result.unwrap_err()));
        assert_eq!(calls, 4);
        // A non-retryable status ends at once.
        let (result, calls, _) = run_with(&[OpenRouterFailure::Status(400)]);
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }
}
