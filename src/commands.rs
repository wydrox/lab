use crate::*;

/// Hamulce zatwierdzania KSeF bez nadzoru (CLI `approve`/`upload --approve`, MCP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApprovePolicy {
    /// Więcej dokumentów niż limit → nie zatwierdzamy żadnego; `0` = bez limitu.
    pub(crate) max_approve: usize,
    /// Zatwierdzaj tylko dokumenty, których wiersz uzgodnienia ma fakturę z Gmaila.
    pub(crate) require_mail: bool,
}

impl ApprovePolicy {
    /// Polityka z flag CLI / argumentów MCP; `LAB_APPROVE_REQUIRE_MAIL` może tylko włączyć filtr.
    pub(crate) fn from_args(max_approve: usize, require_mail: bool) -> Self {
        Self {
            max_approve,
            require_mail: require_mail || cli::approve_require_mail_from_env(),
        }
    }
}

impl Default for ApprovePolicy {
    fn default() -> Self {
        Self {
            max_approve: cli::DEFAULT_MAX_APPROVE,
            require_mail: false,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_sync_command(
    db_path: &Path,
    year: i32,
    ksef: bool,
    mail: bool,
    amazon_mail: bool,
    saldeo: bool,
    ksef_input: Option<PathBuf>,
    gmail_client_secret: Option<PathBuf>,
    gmail_token_file: Option<PathBuf>,
    productmesh_nip: String,
    store: bool,
) -> Result<()> {
    let conn = if sync_stores_to_db(
        store,
        ksef_input.as_deref(),
        ksef,
        mail,
        amazon_mail,
        saldeo,
    ) {
        Some(open_db(db_path)?)
    } else {
        None
    };
    let summary = run_sync_sources(
        year,
        ksef,
        mail,
        amazon_mail,
        saldeo,
        ksef_input.as_deref(),
        gmail_client_secret.as_deref(),
        gmail_token_file.as_deref(),
        &productmesh_nip,
        Some(db_path),
        conn.as_ref(),
    )?;
    write_json(&summary, None)
}

/// Czy sync (CLI i MCP) zapisuje rekordy w SQLite: na żądanie (`store`) albo
/// automatycznie przy KSeF pobieranym online (`--ksef` lub sync wszystkich źródeł).
pub(crate) fn sync_stores_to_db(
    store: bool,
    ksef_input: Option<&Path>,
    ksef: bool,
    mail: bool,
    amazon_mail: bool,
    saldeo: bool,
) -> bool {
    let all_sources = !ksef && !mail && !amazon_mail && !saldeo;
    store || (ksef_input.is_none() && (ksef || all_sources))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_reconcile_command(
    db_path: &Path,
    status: bool,
    mail: Option<PathBuf>,
    ksef: Option<PathBuf>,
    saldeo: Option<PathBuf>,
    review_score: u8,
    output: Option<PathBuf>,
    raw: bool,
    csv: Option<PathBuf>,
    store: bool,
    year: i32,
) -> Result<()> {
    if status {
        let conn = open_existing_db(db_path)?;
        let report = load_last_tri_report(&conn, year)?;
        if raw || output.is_some() {
            return write_json(&report, output.as_deref());
        }
        return write_reconcile_human(&report, None);
    }

    let refresh_ksef = ksef.is_none();
    let refresh_saldeo = saldeo.is_none();
    sync_reconcile_metadata(year, refresh_ksef, refresh_saldeo, db_path)?;

    let ksef_path = ksef.unwrap_or_else(|| configured_ksef_out_path(year));
    let saldeo_path = saldeo.unwrap_or_else(|| default_saldeo_records_path(year));
    let mail_records = load_reconcile_mail_candidates(mail.as_deref(), year)?;
    let ksef_records = load_records(SourceKind::Ksef, &ksef_path)?;
    let saldeo_records = load_saldeo_records(&saldeo_path, Some(db_path))?;
    let report = tri_reconcile(mail_records, ksef_records, saldeo_records, review_score);

    let temporal_diff = if store {
        let conn = open_db(db_path)?;
        Some(store_tri_reconcile_report(&conn, year, &report)?)
    } else {
        None
    };
    if let Some(csv_path) = csv {
        write_tri_csv(&report, &csv_path)?;
    }
    if output.is_some() {
        write_json(&report, output.as_deref())
    } else if raw {
        if temporal_diff.is_some() {
            write_json(
                &serde_json::json!({"report": report, "temporal_diff": temporal_diff}),
                None,
            )
        } else {
            write_json(&report, None)
        }
    } else {
        write_reconcile_human(&report, temporal_diff.as_ref())
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_upload_command(
    db_path: &Path,
    year: i32,
    tri_report: Option<PathBuf>,
    mail: Option<PathBuf>,
    ksef: Option<PathBuf>,
    saldeo: Option<PathBuf>,
    review_score: u8,
    output: Option<PathBuf>,
    csv: Option<PathBuf>,
    confirm: bool,
    approve: bool,
    approve_policy: ApprovePolicy,
) -> Result<()> {
    let outcome = run_upload_flow(
        &UploadFlowConfig {
            db_path,
            year,
            tri_report: tri_report.as_deref(),
            mail: mail.as_deref(),
            ksef: ksef.as_deref(),
            saldeo: saldeo.as_deref(),
            review_score,
            confirm,
            approve,
            approve_policy,
        },
        &mut LiveUploadFlowSteps,
    )?;
    // Wynik przebiegu zapisujemy zawsze, także gdy coś wyżej się nie udało.
    let json_result = write_json(&outcome.plan, output.as_deref());
    let csv_result = csv
        .as_deref()
        .map(|csv_path| write_saldeo_sync_csv(&outcome.plan, csv_path))
        .unwrap_or(Ok(()));
    if let Some(err) = outcome.error {
        for write_err in [json_result.err(), csv_result.err()].into_iter().flatten() {
            eprintln!("  [LAB] zapis wyniku uploadu nie powiódł się: {write_err:#}");
        }
        return Err(err);
    }
    json_result?;
    csv_result
}

/// Wejście wspólnego przebiegu uploadu: CLI `upload` i narzędzie MCP `upload`.
pub(crate) struct UploadFlowConfig<'a> {
    pub(crate) db_path: &'a Path,
    pub(crate) year: i32,
    pub(crate) tri_report: Option<&'a Path>,
    pub(crate) mail: Option<&'a Path>,
    pub(crate) ksef: Option<&'a Path>,
    pub(crate) saldeo: Option<&'a Path>,
    pub(crate) review_score: u8,
    pub(crate) confirm: bool,
    pub(crate) approve: bool,
    pub(crate) approve_policy: ApprovePolicy,
}

/// Kroki przebiegu uploadu z efektami ubocznymi (Saldeo, pliki); testy podstawiają atrapy.
pub(crate) trait UploadFlowSteps {
    fn refresh_saldeo(&mut self, config: &UploadFlowConfig<'_>) -> Result<()>;
    fn plan(&mut self, config: &UploadFlowConfig<'_>) -> Result<SaldeoSyncPlan>;
    fn upload(&mut self, plan: &mut SaldeoSyncPlan) -> Result<()>;
    fn approve(&mut self, config: &UploadFlowConfig<'_>) -> Result<SaldeoApprovePlan>;
}

pub(crate) struct LiveUploadFlowSteps;

impl UploadFlowSteps for LiveUploadFlowSteps {
    fn refresh_saldeo(&mut self, config: &UploadFlowConfig<'_>) -> Result<()> {
        sync_reconcile_metadata(config.year, false, true, config.db_path)
    }

    fn plan(&mut self, config: &UploadFlowConfig<'_>) -> Result<SaldeoSyncPlan> {
        saldeo_sync_plan(SaldeoSyncPlanConfig {
            year: config.year,
            tri_report: config.tri_report,
            mail: config.mail,
            ksef: config.ksef,
            saldeo: config.saldeo,
            db_path: Some(config.db_path),
            review_score: config.review_score,
            confirm: config.confirm,
            upload_url: None,
        })
    }

    fn upload(&mut self, plan: &mut SaldeoSyncPlan) -> Result<()> {
        ensure_saldeo_session()?;
        saldeo_upload_plan(
            plan,
            &default_saldeo_storage_state_path(),
            DEFAULT_SALDEO_UPLOAD_URL,
            "file",
        )
    }

    fn approve(&mut self, config: &UploadFlowConfig<'_>) -> Result<SaldeoApprovePlan> {
        saldeo_approve_pending_ksef(
            config.db_path,
            config.year,
            config.review_score,
            config.confirm,
            config.approve_policy,
        )
    }
}

/// Plan do zapisania/zwrócenia oraz błąd, z którym przebieg ma się zakończyć
/// (CLI: kod wyjścia ≠ 0, MCP: `isError: true`).
pub(crate) struct UploadFlowOutcome {
    pub(crate) plan: SaldeoSyncPlan,
    pub(crate) error: Option<anyhow::Error>,
}

/// Wspólny przebieg uploadu. `Err` oznacza błąd planowania, zanim cokolwiek wysłano;
/// każdy późniejszy problem trafia do `outcome.error`, a plan opisuje, co się stało.
pub(crate) fn run_upload_flow(
    config: &UploadFlowConfig<'_>,
    steps: &mut impl UploadFlowSteps,
) -> Result<UploadFlowOutcome> {
    let (year, confirm, approve) = (config.year, config.confirm, config.approve);
    // Upload z domyślnych źródeł planuje się z świeżych danych Saldeo; bez nich plan
    // z cache uznałby za brakujące pliki dodane do Saldeo od ostatniego odczytu.
    if confirm && config.tri_report.is_none() && config.saldeo.is_none() {
        eprintln!("  [Saldeo] odświeżam dane Saldeo przed planem uploadu...");
        if let Err(err) = steps.refresh_saldeo(config) {
            let err = err
                .context("odświeżenie Saldeo przed uploadem nie powiodło się — upload przerwany");
            // Pusty wynik z ostrzeżeniem zastępuje plik z poprzedniego przebiegu.
            let plan = SaldeoSyncPlan {
                generated_at: Utc::now(),
                year,
                confirm,
                warnings: vec![format!("{err:#}")],
                ..Default::default()
            };
            return Ok(UploadFlowOutcome {
                plan,
                error: Some(err),
            });
        }
    }
    let mut plan = steps.plan(config)?;
    let mut run_error: Option<anyhow::Error> = None;
    if confirm && let Err(err) = steps.upload(&mut plan) {
        plan.warnings
            .push(format!("upload Saldeo przerwany: {err:#}"));
        run_error = Some(err);
    }
    let refresh_after_upload = confirm && plan.summary.uploaded_count > 0;
    if (refresh_after_upload || (confirm && approve && run_error.is_none()))
        && let Err(err) = steps.refresh_saldeo(config)
    {
        eprintln!("  [Saldeo] refresh po uploadzie nie powiódł się: {err:#}");
        plan.warnings.push(format!(
            "refresh Saldeo po uploadzie nie powiódł się: {err:#}"
        ));
        if run_error.is_none() {
            run_error = Some(err.context("refresh Saldeo po uploadzie nie powiódł się"));
        }
    }
    if approve && run_error.is_none() {
        match steps.approve(config) {
            Ok(approve_plan) => {
                if let Some(message) = approve_plan.blocking_error() {
                    plan.warnings
                        .push(format!("zatwierdzanie KSeF wstrzymane: {message}"));
                    run_error = Some(anyhow!(message));
                }
                plan.ksef_approve = Some(approve_plan);
            }
            Err(err) => {
                plan.warnings
                    .push(format!("zatwierdzanie KSeF nie powiodło się: {err:#}"));
                run_error = Some(err);
            }
        }
    } else if approve {
        plan.warnings
            .push("zatwierdzanie KSeF pominięte z powodu błędu przebiegu uploadu".to_string());
    }
    let failed = plan.summary.failed_count;
    if run_error.is_none() && failed > 0 {
        run_error = Some(anyhow!(
            "upload Saldeo: {failed} z {} plików nie trafiło do Saldeo albo nie ma potwierdzenia (szczegóły w wyniku: upload_status failed/unconfirmed)",
            plan.summary.uploadable_count
        ));
    }
    Ok(UploadFlowOutcome {
        plan,
        error: run_error,
    })
}

pub(crate) fn handle_repair_command(
    db_path: &Path,
    year: i32,
    review_score: u8,
    llm: bool,
    confirm: bool,
    output: Option<PathBuf>,
) -> Result<()> {
    write_json(
        &saldeo_repair_plan(db_path, year, review_score, llm, confirm)?,
        output.as_deref(),
    )
}

pub(crate) fn handle_approve_command(
    db_path: &Path,
    year: i32,
    review_score: u8,
    confirm: bool,
    policy: ApprovePolicy,
    output: Option<PathBuf>,
) -> Result<()> {
    let plan = saldeo_approve_pending_ksef(db_path, year, review_score, confirm, policy)?;
    // Plan (z liczbą dokumentów i limitem) zapisujemy także wtedy, gdy limit zablokował przebieg.
    let written = write_json(&plan, output.as_deref());
    if let Some(message) = plan.blocking_error() {
        if let Err(write_err) = written {
            eprintln!("  [LAB] zapis wyniku zatwierdzania nie powiódł się: {write_err:#}");
        }
        return Err(anyhow!(message));
    }
    written
}

fn ensure_saldeo_session() -> Result<SaldeoSession> {
    ensure_saldeo_session_or_auth(None)?;
    read_saldeo_session(&default_saldeo_storage_state_path())
}

pub(crate) fn enrich_repair_mail_with(
    candidates: &mut [InvoiceRecord],
    confirm: bool,
    enrich: impl FnOnce(&mut [InvoiceRecord]) -> Result<()>,
    persist: impl FnOnce(&[InvoiceRecord]) -> Result<()>,
) -> Result<usize> {
    let before = candidates.to_vec();
    enrich(candidates)?;
    let enriched_count = before
        .iter()
        .zip(candidates.iter())
        .filter(|(old, record)| {
            old.invoice_number != record.invoice_number
                || old.seller_tax_id != record.seller_tax_id
                || old.buyer_tax_id != record.buyer_tax_id
                || old.seller_name != record.seller_name
                || old.buyer_name != record.buyer_name
                || old.issue_date != record.issue_date
                || old.gross_amount_minor != record.gross_amount_minor
                || old.currency != record.currency
        })
        .count();
    if confirm && enriched_count > 0 {
        persist(candidates)?;
    }
    Ok(enriched_count)
}

pub(crate) fn saldeo_repair_plan(
    db_path: &Path,
    year: i32,
    review_score: u8,
    llm: bool,
    confirm: bool,
) -> Result<SaldeoRepairPlan> {
    // LLM uzupełnia i zapisuje każdy plik kandydatów osobno (główny i Amazon), a reconcile
    // dostaje ich złączenie jak domyślne źródło Gmail.
    let mut llm_enriched_count = 0usize;
    let mut load_mail = |mail_path: PathBuf| -> Result<Vec<InvoiceRecord>> {
        let mut mail = load_records_if_present(SourceKind::Mail, &mail_path)?;
        if llm && mail_path.exists() {
            let cached = apply_cached_mail_candidates(&mail_path, &mut mail)?;
            llm_enriched_count += enrich_repair_mail_with(
                &mut mail,
                confirm,
                |candidates| enrich_candidates_with_llm_explicit(candidates, &cached, None),
                |candidates| {
                    write_records(candidates, OutputFormat::Jsonl, Some(&mail_path))?;
                    let conn = open_db(db_path)?;
                    store_records(&conn, candidates).map(|_| ())
                },
            )?;
        }
        Ok(mail)
    };
    let mail_path = default_mail_candidates_path(year);
    if llm && !mail_path.exists() {
        eprintln!("  [LAB] brak pliku Gmail do LLM: {}", mail_path.display());
    }
    let main_mail = load_mail(mail_path)?;
    let amazon_mail = load_mail(default_amazon_mail_candidates_path(year))?;
    let mail = filter_invoice_records_for_year(merge_mail_candidates(main_mail, amazon_mail), year);
    let ksef = filter_invoice_records_for_year(
        load_records_if_present(SourceKind::Ksef, &configured_ksef_out_path(year))?,
        year,
    );
    let saldeo_path = default_saldeo_records_path(year);
    let saldeo = if saldeo_path.exists() {
        filter_invoice_records_for_year(load_saldeo_records(&saldeo_path, Some(db_path))?, year)
    } else {
        Vec::new()
    };
    let duplicates = saldeo_duplicate_groups(&saldeo);
    let report = tri_reconcile(mail, ksef, saldeo, review_score);
    let items = repair_saldeo_items_from_report(&report);
    if confirm {
        // Only the fields repair filled are stored, each with the Saldeo value it replaced,
        // so a later change in Saldeo takes precedence over the repair.
        let override_rows = items
            .iter()
            .map(|item| item.override_row.clone())
            .collect::<Vec<_>>();
        save_saldeo_record_overrides(db_path, &override_rows)?;
        eprintln!("  [LAB] zapisano {} poprawek Saldeo", items.len());
    }
    Ok(SaldeoRepairPlan {
        generated_at: Utc::now(),
        year,
        confirm,
        llm,
        summary: SaldeoRepairSummary {
            repaired_count: items.len(),
            duplicate_group_count: duplicates.len(),
            duplicate_record_count: duplicates
                .iter()
                .map(|group| group.content_hashes.len())
                .sum(),
            llm_enriched_count,
        },
        items: items
            .into_iter()
            .map(|item| SaldeoRepairItem {
                content_hash: item.override_row.content_hash,
                invoice_number: item.invoice_number,
                changed_fields: item.changed_fields,
            })
            .collect(),
        duplicates,
    })
}

pub(crate) fn saldeo_approve_pending_ksef(
    db_path: &Path,
    year: i32,
    review_score: u8,
    confirm: bool,
    policy: ApprovePolicy,
) -> Result<SaldeoApprovePlan> {
    if confirm {
        ensure_saldeo_session()?;
    } else if !saldeo_session_valid(&default_saldeo_storage_state_path()) {
        eprintln!(
            "  [Saldeo] sesja nieważna — plan zatwierdzenia może być pusty. Użyj --confirm, żeby zalogować Helium."
        );
    }
    let rows = build_invoice_table_rows(year, review_score, db_path)?;
    approve_ksef_rows_with(&rows, year, confirm, policy, |document_ids| {
        let session = ensure_saldeo_session()?;
        eprintln!(
            "  [Saldeo] zatwierdzam KSeF ({} dokumentów)...",
            document_ids.len()
        );
        let approved = saldeo_mark_ksef_documents(&session, document_ids, true)?;
        let mut statuses = load_ksef_accounting_cache(year);
        for document_id in &approved {
            statuses.insert(*document_id, Some(true));
        }
        if let Err(err) = save_ksef_accounting_cache(year, &statuses) {
            eprintln!("  [Saldeo] nie zapisałem cache statusów KSeF: {err}");
        }
        if let Err(err) = saldeo_fetch_with_progress(
            year,
            &default_saldeo_storage_state_path(),
            &default_saldeo_out_path(year),
            Some(db_path),
            None,
        ) {
            eprintln!("  [Saldeo] refresh po zatwierdzeniu nie powiódł się: {err}");
        }
        Ok(approved)
    })
}

const APPROVE_SKIP_NO_MAIL: &str =
    "brak faktury z Gmaila w wierszu uzgodnienia (wymagana przez --require-mail / require_mail)";

/// Plan zatwierdzenia z wierszy tabeli: filtr `require_mail`, potem limit `max_approve`.
/// `mark` (oznaczenie w Saldeo) jest wołane tylko z `confirm`, gdy limit nie został przekroczony.
pub(crate) fn approve_ksef_rows_with(
    rows: &[InvoiceTableRow],
    year: i32,
    confirm: bool,
    policy: ApprovePolicy,
    mark: impl FnOnce(&[i64]) -> Result<Vec<i64>>,
) -> Result<SaldeoApprovePlan> {
    let mut document_ids = Vec::new();
    let mut skipped = Vec::new();
    for document_id in pending_ksef_approve_ids(rows) {
        let document_rows = rows
            .iter()
            .filter(|row| row.ksef_document_id == Some(document_id))
            .collect::<Vec<_>>();
        let has_mail = document_rows
            .iter()
            .any(|row| invoice_table_source_parts(&row.sources)[0] == "G");
        if policy.require_mail && !has_mail {
            let first = document_rows.first();
            skipped.push(SaldeoApproveSkip {
                document_id,
                invoice_number: first.and_then(|row| row.record.invoice_number.clone()),
                sources: first.map(|row| row.sources.clone()).unwrap_or_default(),
                reason: APPROVE_SKIP_NO_MAIL.to_string(),
            });
        } else {
            document_ids.push(document_id);
        }
    }
    let cap_exceeded = policy.max_approve > 0 && document_ids.len() > policy.max_approve;
    let blocked_reason = cap_exceeded.then(|| {
        format!(
            "{} dokumentów KSeF do zatwierdzenia przekracza limit {} (--max-approve / max_approve) — nic nie zatwierdzono. Przejrzyj listę document_ids (np. `lab approve` bez --confirm) i uruchom ponownie z wyższym limitem (0 = bez limitu).",
            document_ids.len(),
            policy.max_approve
        )
    });
    let approved_document_ids = if confirm && !cap_exceeded && !document_ids.is_empty() {
        mark(&document_ids)?
    } else {
        Vec::new()
    };
    Ok(SaldeoApprovePlan {
        generated_at: Utc::now(),
        year,
        confirm,
        max_approve: policy.max_approve,
        require_mail: policy.require_mail,
        summary: SaldeoApproveSummary {
            pending_count: document_ids.len(),
            approved_count: approved_document_ids.len(),
            skipped_count: skipped.len(),
            cap_exceeded,
        },
        document_ids,
        approved_document_ids,
        skipped,
        blocked_reason,
    })
}

#[cfg(test)]
mod cli_policy_tests;

pub(crate) fn handle_db_command(path: &Path, command: DbCommands) -> Result<()> {
    match command {
        DbCommands::Init => {
            open_db(path)?;
            write_json(
                &serde_json::json!({ "db": path, "initialized": true }),
                None,
            )
        }
        DbCommands::Stats => write_json(&db_stats(&open_existing_db(path)?)?, None),
        DbCommands::List { source, limit } => {
            let conn = open_existing_db(path)?;
            let records = load_records_from_db(&conn, path, source, Some(limit))?;
            write_json(&records, None)
        }
        DbCommands::TriRuns { limit } => {
            write_json(&list_tri_runs(&open_existing_db(path)?, limit)?, None)
        }
    }
}
