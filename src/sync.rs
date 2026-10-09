use crate::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_sync_sources(
    year: i32,
    ksef: bool,
    mail: bool,
    amazon_mail: bool,
    saldeo: bool,
    ksef_input: Option<&Path>,
    gmail_client_secret: Option<&Path>,
    gmail_token_file: Option<&Path>,
    productmesh_nip: &str,
    db_path: Option<&Path>,
    conn: Option<&Connection>,
) -> Result<SyncRunSummary> {
    run_sync_sources_with_progress(
        year,
        ksef,
        mail,
        amazon_mail,
        saldeo,
        ksef_input,
        gmail_client_secret,
        gmail_token_file,
        productmesh_nip,
        db_path,
        conn,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_sync_sources_with_progress(
    year: i32,
    ksef: bool,
    mail: bool,
    amazon_mail: bool,
    saldeo: bool,
    ksef_input: Option<&Path>,
    gmail_client_secret: Option<&Path>,
    gmail_token_file: Option<&Path>,
    productmesh_nip: &str,
    db_path: Option<&Path>,
    conn: Option<&Connection>,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<SyncRunSummary> {
    let all = !ksef && !mail && !amazon_mail && !saldeo;
    if all {
        eprintln!("Sync: wszystkie źródła (KSeF + Gmail/PDF + Saldeo)");
    }
    let stored = conn.is_some();
    let mut synced: Vec<String> = Vec::new();
    let mut records_count = 0usize;

    if ksef || all {
        let ksef_result = if let Some(input) = ksef_input {
            eprintln!("  [KSeF] synchronizacja z lokalnego eksportu...");
            if let Some(progress) = &progress {
                set_progress(progress, "KSeF: synchronizacja z lokalnego eksportu...");
            }
            ksef_sync(year, input, None)
        } else {
            eprintln!("  [KSeF] synchronizacja online...");
            if progress.is_some() {
                ksef_online_sync_cached_with_progress(year, None, progress.clone())
            } else {
                ksef_online_sync_with_progress(year, None, progress.clone())
            }
        };
        match ksef_result {
            Ok(result) => {
                records_count += result.records.len();
                if let Some(progress) = &progress {
                    set_progress(
                        progress,
                        format!("KSeF: zapis do bazy ({} rekordów)...", result.records.len()),
                    );
                }
                if let Some(conn) = conn {
                    store_records(conn, &result.records)?;
                }
                eprintln!("  [KSeF] gotowe: {} rekordów", result.summary.records_count);
                if let Some(progress) = &progress {
                    set_progress(
                        progress,
                        format!("KSeF: gotowe — {} rekordów", result.summary.records_count),
                    );
                }
                synced.push(format!("ksef ({})", result.summary.records_count));
            }
            Err(err) if all && is_missing_ksef_token(&err) => {
                eprintln!("  [KSeF] pominięto: {err}");
                if let Some(progress) = &progress {
                    set_progress(progress, format!("KSeF: pominięto ({err})"));
                }
                synced.push("ksef (pominięte: brak tokenu)".to_string());
            }
            Err(err) => return Err(err),
        }
    }

    if mail || amazon_mail || all {
        let mail_source_label = if amazon_mail { "Amazon" } else { "Gmail" };
        let mail_cache = if amazon_mail {
            default_amazon_mail_out_path(year)
        } else {
            default_mail_out_path(year)
        };
        let mail_query = if amazon_mail {
            amazon_gmail_query(year)
        } else {
            default_gmail_query(year)
        };
        let mail_candidates_path = mail_cache.join("candidates.jsonl");
        eprintln!("  [{mail_source_label}] sprawdzanie wiadomości i cache załączników...");
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!("{mail_source_label}: sprawdzanie tokena..."),
            );
        }
        let token_path = gmail_token_file
            .map(PathBuf::from)
            .unwrap_or_else(default_gmail_token_path);
        let token = gmail_access_token("GMAIL_ACCESS_TOKEN", &token_path, gmail_client_secret)?;
        let gmail_result = gmail_fetch(
            &token,
            "me",
            &mail_query,
            &mail_cache,
            usize::MAX,
            &["pdf".to_string()],
            progress.clone(),
        )?;
        eprintln!(
            "  [{mail_source_label}] wiadomości: {} znalezionych, {} z cache, {} pobranych z API; nowe pliki: {} metadane, {} załączniki",
            gmail_result.messages_seen,
            gmail_result.messages_cached,
            gmail_result.messages_fetched,
            gmail_result.metadata_saved,
            gmail_result.attachments_saved
        );
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "{mail_source_label}: {} wiadomości, cache {}, API {}, załączniki {}",
                    gmail_result.messages_seen,
                    gmail_result.messages_cached,
                    gmail_result.messages_fetched,
                    gmail_result.attachments_saved
                ),
            );
        }
        eprintln!("  [{mail_source_label}] skanowanie nowych PDF...");
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!("{mail_source_label}: skanowanie nowych PDF..."),
            );
        }
        let (mail_records, parsed_count) =
            sync_mail_records(&mail_cache, &gmail_result.saved_files)?;
        eprintln!(
            "  [{mail_source_label}] sparsowano {} nowych PDF",
            parsed_count
        );
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "{mail_source_label}: sparsowano {parsed_count} nowych PDF, razem {}",
                    mail_records.len()
                ),
            );
        }
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!("{mail_source_label}: wybór kandydatów ProductMesh..."),
            );
        }
        let mut candidates = productmesh_invoice_candidates(&mail_records, productmesh_nip);
        let mut other_year = retain_mail_candidates_for_year(&mut candidates, year);
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "{mail_source_label}: cache kandydatów ({} faktur)...",
                    candidates.len()
                ),
            );
        }
        let cached_candidates =
            apply_cached_mail_candidates(&mail_candidates_path, &mut candidates)?;
        enrich_candidates_with_gemma(&mut candidates, &cached_candidates, progress.clone())?;
        // Skany bez tekstu czekały na LLM; teraz reguła ProductMesh obejmuje i je.
        let before_llm_filter = candidates.len();
        candidates = productmesh_candidates_after_llm(&candidates, productmesh_nip);
        let unrelated = before_llm_filter - candidates.len();
        if unrelated > 0 {
            eprintln!(
                "  [{mail_source_label}] po kroku LLM pominięto {unrelated} PDF (bez powiązania z firmą albo duplikat)"
            );
        }
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "{mail_source_label}: zapis {} kandydatów...",
                    candidates.len()
                ),
            );
        }
        // The candidate file keeps LLM results for records the LLM dated into
        // another year, so the next sync reuses them instead of asking again.
        write_records(
            &candidates,
            OutputFormat::Jsonl,
            Some(&mail_candidates_path),
        )?;
        other_year += retain_mail_candidates_for_year(&mut candidates, year);
        if other_year > 0 {
            eprintln!(
                "  [{mail_source_label}] pominięto {other_year} faktur z datą wystawienia spoza {year}"
            );
        }
        records_count += candidates.len();
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "{mail_source_label}: zapis do bazy ({} rekordów)...",
                    candidates.len()
                ),
            );
        }
        if let Some(conn) = conn {
            store_records(conn, &candidates)?;
        }
        eprintln!(
            "  [{mail_source_label}] gotowe: {} PDF, {} faktur",
            mail_records.len(),
            candidates.len()
        );
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "{mail_source_label}: gotowe — {} PDF, {} faktur",
                    mail_records.len(),
                    candidates.len()
                ),
            );
        }
        synced.push(format!(
            "{} ({} new attachments, {} pdfs, {} candidates)",
            if amazon_mail { "amazon-mail" } else { "mail" },
            gmail_result.attachments_saved,
            mail_records.len(),
            candidates.len()
        ));
    }

    if saldeo || all {
        eprintln!("  [Saldeo] pobieranie dokumentów...");
        let result = saldeo_fetch_with_progress(
            year,
            &default_saldeo_storage_state_path(),
            &default_saldeo_out_path(year),
            db_path,
            progress.clone(),
        )?;
        records_count += result.records.len();
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Saldeo: zapis do bazy ({} rekordów)...",
                    result.records.len()
                ),
            );
        }
        if let Some(conn) = conn {
            store_records(conn, &result.records)?;
        }
        eprintln!(
            "  [Saldeo] gotowe: {} dokumentów",
            result.summary.documents_count
        );
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Saldeo: gotowe — {} dokumentów",
                    result.summary.documents_count
                ),
            );
        }
        synced.push(format!("saldeo ({})", result.summary.documents_count));
    }

    Ok(SyncRunSummary {
        synced,
        year,
        records_count,
        stored,
    })
}

pub(crate) fn sync_reconcile_metadata(
    year: i32,
    ksef: bool,
    saldeo: bool,
    db_path: &Path,
) -> Result<()> {
    sync_reconcile_metadata_with_progress(year, ksef, saldeo, db_path, None)
}

pub(crate) fn sync_reconcile_metadata_with_progress(
    year: i32,
    ksef: bool,
    saldeo: bool,
    db_path: &Path,
    progress: Option<Arc<Mutex<String>>>,
) -> Result<()> {
    if let Some(progress) = &progress {
        set_progress(progress, "Reconcile: otwieranie lokalnej bazy...");
    }
    let conn = open_db(db_path)?;
    if ksef {
        eprintln!("  [KSeF] pobieranie metadanych online do lokalnej bazy...");
        // Cache świeży wg KSEF_CACHE_TTL_MINS (np. zapisany chwilę wcześniej przez `sync`)
        // zamiast drugiego zapytania online; TTL 0 wymusza odświeżenie.
        let result = ksef_online_sync_cached_with_progress(year, None, progress.clone())?;
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Reconcile/KSeF: zapis do bazy ({} rekordów)...",
                    result.records.len()
                ),
            );
        }
        store_records(&conn, &result.records)?;
    }

    if saldeo {
        eprintln!("  [Saldeo] pobieranie metadanych reconcile do lokalnej bazy...");
        let result = saldeo_fetch_with_progress(
            year,
            &default_saldeo_storage_state_path(),
            &default_saldeo_out_path(year),
            Some(db_path),
            progress.clone(),
        )?;
        if let Some(progress) = &progress {
            set_progress(
                progress,
                format!(
                    "Reconcile/Saldeo: zapis do bazy ({} rekordów)...",
                    result.records.len()
                ),
            );
        }
        store_records(&conn, &result.records)?;
    }
    if let Some(progress) = &progress {
        set_progress(progress, "Reconcile: metadane odświeżone");
    }
    Ok(())
}

// Domyślne katalogi danych leżą w katalogu LAB (`lab_root`), nie w bieżącym katalogu.
pub(crate) fn default_mail_out_path(year: i32) -> PathBuf {
    lab_path(format!("data/mail-all-pdf-{year}-pdfs"))
}

pub(crate) fn default_amazon_mail_out_path(year: i32) -> PathBuf {
    lab_path(format!("data/mail-amazon-{year}-pdfs"))
}

pub(crate) fn default_mail_candidates_path(year: i32) -> PathBuf {
    default_mail_out_path(year).join("candidates.jsonl")
}

/// Kandydaci presetu `sync --amazon-mail`; czytani razem z głównymi przez
/// `load_default_mail_candidates`.
pub(crate) fn default_amazon_mail_candidates_path(year: i32) -> PathBuf {
    default_amazon_mail_out_path(year).join("candidates.jsonl")
}

pub(crate) fn default_saldeo_out_path(year: i32) -> PathBuf {
    lab_path(format!("data/saldeo-{year}"))
}

pub(crate) fn default_saldeo_records_path(year: i32) -> PathBuf {
    default_saldeo_out_path(year).join("records.jsonl")
}

pub(crate) fn default_ksef_out_path(year: i32) -> PathBuf {
    lab_path(format!("data/ksef-{year}"))
}

/// Względny `KSEF_DATA_DIR` jest liczony od katalogu LAB.
pub(crate) fn configured_ksef_out_path(year: i32) -> PathBuf {
    lab_config_var("KSEF_DATA_DIR")
        .map(|dir| ksef_data_dir_year_path(&lab_path(dir), year))
        .unwrap_or_else(|| default_ksef_out_path(year))
}
