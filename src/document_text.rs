use crate::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum OcrMode {
    Off,
    Auto,
    Always,
}

fn ocr_mode(value: &str) -> Result<OcrMode> {
    match value {
        "off" => Ok(OcrMode::Off),
        "auto" => Ok(OcrMode::Auto),
        "always" => Ok(OcrMode::Always),
        _ => Err(anyhow!("LAB_OCR_MODE: dozwolone auto, off, always")),
    }
}

fn select_text(
    mode: OcrMode,
    text: Result<String>,
    ocr: impl FnOnce() -> Result<String>,
) -> Result<(String, Vec<String>)> {
    let usable = text.as_ref().ok().filter(|s| !s.trim().is_empty());
    let needs_ocr = mode == OcrMode::Always
        || (mode == OcrMode::Auto
            && usable.is_none_or(|s| {
                record_missing_core_fields(&parse_text_invoice(SourceKind::Mail, s))
            }));
    if needs_ocr {
        match ocr() {
            Ok(ocr_text) if !ocr_text.trim().is_empty() => {
                if mode == OcrMode::Auto
                    && let Some(original) = usable
                {
                    let old = parse_text_invoice(SourceKind::Mail, original);
                    let new = parse_text_invoice(SourceKind::Mail, &ocr_text);
                    if (record_amounts_inconsistent(&new) && !record_amounts_inconsistent(&old))
                        || (record_amounts_inconsistent(&new) == record_amounts_inconsistent(&old)
                            && record_quality_score(&new) < record_quality_score(&old))
                    {
                        return Ok((
                            original.clone(),
                            vec!["OCR nie poprawił odczytu; zachowano tekst Poppler".into()],
                        ));
                    }
                }
                return Ok((ocr_text, vec!["PDF odczytany przez GLM-OCR".into()]));
            }
            result => {
                let reason = result
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "pusty tekst".into());
                if mode == OcrMode::Always || usable.is_none() {
                    return Err(anyhow!("OCR nie powiódł się: {reason}"));
                }
                return Ok((
                    text?,
                    vec![format!(
                        "OCR niedostępny; pozostawiono tekst Poppler: {reason}"
                    )],
                ));
            }
        }
    }
    let text = text?;
    if text.trim().is_empty() {
        return Err(anyhow!("PDF nie zawiera tekstu"));
    }
    Ok((text, Vec::new()))
}

pub(crate) fn extract_document_text(path: &Path) -> Result<(String, Vec<String>)> {
    let mode = ocr_mode(&lab_config_var("LAB_OCR_MODE").unwrap_or_else(|| "auto".into()))?;
    select_text(mode, extract_pdf_text(path), || run_ocr(path))
}

fn run_ocr(path: &Path) -> Result<String> {
    let meta = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if meta.len() > MAX_OCR_PDF_BYTES {
        return Err(anyhow!(
            "PDF przekracza limit OCR {MAX_OCR_PDF_BYTES} bajtów; nie uruchamiam modelu"
        ));
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let installed = home.join(".local/share/uv/tools/ppmlx/bin/python");
    let python = if let Some(value) = lab_config_var("LAB_OCR_PYTHON") {
        require_existing_file(&PathBuf::from(value), "LAB_OCR_PYTHON")?
    } else if installed.is_file() {
        installed
    } else {
        local_tool("python3")?
    };
    let model = lab_config_var("LAB_OCR_MODEL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".ppmlx/models/mlx-community--GLM-OCR-4bit"));
    if !model.is_dir() {
        return Err(anyhow!(
            "brak lokalnego modelu OCR; ustaw LAB_OCR_MODEL_PATH lub pobierz mlx-community/GLM-OCR-4bit"
        ));
    }
    let cache = lab_config_var("LAB_OCR_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache/lab/ocr"));
    fs::create_dir_all(&cache).with_context(|| format!("mkdir {}", cache.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 700 {}", cache.display()))?;
    }
    let seconds = lab_config_var("LAB_OCR_TIMEOUT_SECS")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(180)
        .clamp(1, 1800);
    let max_pages = lab_config_var("LAB_OCR_MAX_PAGES")
        .unwrap_or_else(|| "10".into())
        .parse::<u32>()
        .ok()
        .filter(|pages| (1..=100).contains(pages))
        .ok_or_else(|| anyhow!("LAB_OCR_MAX_PAGES: dozwolone 1-100"))?;
    let mut command = Command::new(python);
    apply_isolated_env(&mut command);
    let mut child = command
        .arg("-c")
        .arg(include_str!("../scripts/ocr_pdf.py"))
        .arg("--pdf")
        .arg(path)
        .arg("--model")
        .arg(model)
        .arg("--cache-dir")
        .arg(cache)
        .arg("--max-pages")
        .arg(max_pages.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("uruchomienie lokalnego OCR")?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut b = Vec::new();
        stdout.take(2_000_001).read_to_end(&mut b).map(|_| b)
    });
    let err = std::thread::spawn(move || {
        let mut b = Vec::new();
        stderr.take(64_001).read_to_end(&mut b).map(|_| b)
    });
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if start.elapsed() < Duration::from_secs(seconds) => {
                sleep(Duration::from_millis(50))
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(anyhow!(
                    "OCR przerwane: {}",
                    result
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "limit czasu".into())
                ));
            }
        }
    };
    let output = out.join().map_err(|_| anyhow!("błąd wątku OCR"))??;
    let errors = err.join().map_err(|_| anyhow!("błąd wątku OCR"))??;
    let status = status?;
    if !status.success() {
        return Err(anyhow!(
            "adapter OCR zakończony kodem {:?}: {}",
            status.code(),
            String::from_utf8_lossy(&errors)
                .lines()
                .last()
                .unwrap_or("brak szczegółów")
        ));
    }
    if output.len() > 2_000_000 {
        return Err(anyhow!("odpowiedź OCR przekracza limit rozmiaru"));
    }
    let value: Value =
        serde_json::from_slice(&output).context("nieprawidłowy JSON adaptera OCR")?;
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("OCR nie zwrócił tekstu"))?;
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn off_does_not_call_ocr() {
        let (text, _) =
            select_text(OcrMode::Off, Ok("tekst".into()), || panic!("OCR wyłączony")).unwrap();
        assert_eq!(text, "tekst");
    }
    #[test]
    fn empty_pdf_uses_ocr_and_failures_are_explicit() {
        assert_eq!(
            select_text(OcrMode::Auto, Ok(String::new()), || Ok("OCR".into()))
                .unwrap()
                .0,
            "OCR"
        );
        assert!(select_text(OcrMode::Auto, Err(anyhow!("PDF")), || Err(anyhow!("OCR"))).is_err());
        let (text, warnings) = select_text(OcrMode::Auto, Ok("tekst".into()), || {
            Err(anyhow!("brak modelu"))
        })
        .unwrap();
        assert_eq!(text, "tekst");
        assert_eq!(warnings.len(), 1);
        assert!(select_text(OcrMode::Always, Ok("tekst".into()), || Err(anyhow!("OCR"))).is_err());
    }
    #[test]
    fn inconsistent_amounts_trigger_ocr_without_accepting_worse_totals() {
        let good = "Sprzedawca: Firma ABC\nNabywca: Firma XYZ\nFaktura FV/9/2026\nData wystawienia: 2026-05-01\nWartość netto: 18,56 PLN\nWartość VAT: 4,27 PLN\nWartość brutto: 22,83 PLN";
        let bad = good.replace("18,56", "9567,00");
        assert!(record_missing_core_fields(&parse_text_invoice(
            SourceKind::Mail,
            &bad
        )));
        let (text, _) = select_text(OcrMode::Auto, Ok(bad), || Ok(good.into())).unwrap();
        assert_eq!(text, good);
        // Brak numeru wymusza OCR, ale większa liczba pól nie usprawiedliwia złych kwot.
        let partial = good.replace("Faktura FV/9/2026\n", "");
        let (text, warnings) = select_text(OcrMode::Auto, Ok(partial.clone()), || {
            Ok(good.replace("18,56", "9567,00"))
        })
        .unwrap();
        assert_eq!(text, partial);
        assert!(warnings[0].contains("zachowano tekst Poppler"));
    }

    #[test]
    fn complete_invoice_skips_ocr() {
        let text = "Sprzedawca: Firma ABC\nNabywca: Firma XYZ\nFaktura FV/9/2026\nData wystawienia: 2026-05-01\nRazem do zapłaty: 99,90 zł";
        select_text(OcrMode::Auto, Ok(text.into()), || {
            panic!("kompletny tekst nie wymaga OCR")
        })
        .unwrap();
        assert!(ocr_mode("unknown").is_err());
    }
}
