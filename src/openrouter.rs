use crate::*;
use base64::{Engine, engine::general_purpose::STANDARD};

const OPENROUTER_CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub(crate) const DEFAULT_OPENROUTER_MODEL: &str = "google/gemini-3.8-flash";

pub(crate) fn openrouter_configured() -> bool {
    secret_is_set(Secret::OpenRouterApiKey)
}

pub(crate) fn openrouter_model() -> String {
    lab_config_var("LAB_OPENROUTER_MODEL").unwrap_or_else(|| DEFAULT_OPENROUTER_MODEL.to_string())
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

pub(crate) fn openrouter_invoice_request(
    model: &str,
    filename: &str,
    pdf_base64: &str,
) -> Value {
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
    if let Some(message) = response
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
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
    let api_key = secret_value(Secret::OpenRouterApiKey)?.ok_or_else(|| {
        anyhow!("brak OPENROUTER_API_KEY; ustaw klucz OpenRouter albo użyj lokalnego LLM")
    })?;
    let model = openrouter_model();
    let body = openrouter_invoice_request(&model, filename, &pdf_base64);
    let client = Client::builder().timeout(openrouter_timeout()).build()?;
    let mut last_err = None;
    for attempt in 0..4 {
        match client
            .post(OPENROUTER_CHAT_URL)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://github.com/wydrox/lab")
            .header("X-Title", "LAB")
            .json(&body)
            .send()
        {
            Ok(resp) if resp.status().is_success() => {
                let response: Value = resp.json()?;
                let value = openrouter_response_json(&response)?;
                return invoice_validation::apply_normalized_invoice_json(record, &value);
            }
            Ok(resp) if matches!(resp.status().as_u16(), 429 | 502 | 503) => {
                let delay = Duration::from_secs(2u64.pow(attempt));
                eprintln!(
                    "  [Gmail/OpenRouter] HTTP {}, czekam {}s...",
                    resp.status().as_u16(),
                    delay.as_secs()
                );
                std::thread::sleep(delay);
            }
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().unwrap_or_default();
                return Err(anyhow!("OpenRouter HTTP {status}: {text}"));
            }
            Err(err) => {
                last_err = Some(err);
                std::thread::sleep(Duration::from_secs(2u64.pow(attempt)));
            }
        }
    }
    Err(last_err
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow!("OpenRouter nie odpowiedział po 4 próbach")))
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
        let file = &body["messages"][0]["content"][1]["file"];
        assert_eq!(file["filename"], "Invoice-M73SJH5X-0005.pdf");
        assert_eq!(file["file_data"], "data:application/pdf;base64,AAA");
        let encoded = serde_json::to_string(&body).unwrap();
        assert!(!encoded.contains("Bearer"));
        assert!(!encoded.contains("OPENROUTER"));
    }

    #[test]
    fn complete_json_is_accepted_and_truncated_is_rejected() {
        let ok = serde_json::json!({
            "choices":[{"finish_reason":"stop","message":{"content":"{\"seller_name\":\"Elocity\"}"}}]
        });
        assert_eq!(openrouter_response_json(&ok).unwrap()["seller_name"], "Elocity");
        let truncated = serde_json::json!({
            "choices":[{"finish_reason":"length","message":{"content":"{\"seller_name\":\"Elocity\"}"}}]
        });
        assert!(openrouter_response_json(&truncated).is_err());
        let api_err = serde_json::json!({"error":{"message":"insufficient credits"}});
        let err = openrouter_response_json(&api_err).unwrap_err().to_string();
        assert!(err.contains("insufficient credits"));
        assert!(!err.contains("sk-"));
    }
}
