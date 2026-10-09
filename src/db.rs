use crate::*;

pub(crate) fn write_records(
    records: &[InvoiceRecord],
    format: OutputFormat,
    output: Option<&Path>,
) -> Result<()> {
    let bytes = match format {
        OutputFormat::Json => serde_json::to_vec_pretty(records)?,
        OutputFormat::Jsonl => {
            let mut out = Vec::new();
            for record in records {
                serde_json::to_writer(&mut out, record)?;
                out.push(b'\n');
            }
            out
        }
    };
    write_bytes(&bytes, output)
}

pub(crate) fn write_json<T: Serialize>(value: &T, output: Option<&Path>) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_bytes(&bytes, output)?;
    if output.is_none() {
        println!();
    }
    Ok(())
}

fn write_bytes(bytes: &[u8], output: Option<&Path>) -> Result<()> {
    match output {
        Some(path) => write_private_file(path, bytes),
        None => {
            io::stdout().write_all(bytes)?;
            Ok(())
        }
    }
}

pub(crate) fn open_db(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let conn =
        Connection::open(path).with_context(|| format!("otwarcie SQLite {}", path.display()))?;
    chmod_sqlite_files(path)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "trusted_schema", "OFF")?;
    conn.pragma_update(None, "cell_size_check", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    chmod_sqlite_files(path)?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS invoices (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source TEXT NOT NULL,
            source_path TEXT,
            content_hash TEXT NOT NULL,
            invoice_number TEXT,
            seller_tax_id TEXT,
            buyer_tax_id TEXT,
            seller_name TEXT,
            buyer_name TEXT,
            issue_date TEXT,
            sale_date TEXT,
            due_date TEXT,
            gross_amount_minor INTEGER,
            net_amount_minor INTEGER,
            vat_amount_minor INTEGER,
            currency TEXT,
            ksef_reference TEXT,
            email_message_id TEXT,
            email_subject TEXT,
            email_from TEXT,
            warnings_json TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(source, content_hash)
        );
        CREATE INDEX IF NOT EXISTS idx_invoices_source ON invoices(source);
        CREATE INDEX IF NOT EXISTS idx_invoices_invoice_number ON invoices(invoice_number);
        CREATE INDEX IF NOT EXISTS idx_invoices_tax_ids ON invoices(seller_tax_id, buyer_tax_id);
        CREATE TABLE IF NOT EXISTS saldeo_overrides (
            content_hash TEXT PRIMARY KEY,
            invoice_number TEXT,
            seller_tax_id TEXT,
            buyer_tax_id TEXT,
            seller_name TEXT,
            buyer_name TEXT,
            issue_date TEXT,
            gross_amount_minor INTEGER,
            currency TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_saldeo_overrides_updated_at ON saldeo_overrides(updated_at);
        CREATE TABLE IF NOT EXISTS reconcile_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            generated_at TEXT NOT NULL,
            match_score INTEGER NOT NULL,
            review_score INTEGER NOT NULL,
            ksef_count INTEGER NOT NULL,
            mail_count INTEGER NOT NULL,
            matched_count INTEGER NOT NULL,
            review_count INTEGER NOT NULL,
            unmatched_ksef_count INTEGER NOT NULL,
            unmatched_mail_count INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS invoice_matches (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id INTEGER NOT NULL REFERENCES reconcile_runs(id) ON DELETE CASCADE,
            status TEXT NOT NULL,
            score INTEGER NOT NULL,
            reasons_json TEXT NOT NULL,
            ksef_invoice_id INTEGER NOT NULL REFERENCES invoices(id),
            mail_invoice_id INTEGER NOT NULL REFERENCES invoices(id)
        );
        CREATE TABLE IF NOT EXISTS tri_reconcile_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            generated_at TEXT NOT NULL,
            year INTEGER NOT NULL,
            review_score INTEGER NOT NULL,
            mail_count INTEGER NOT NULL,
            ksef_count INTEGER NOT NULL,
            saldeo_count INTEGER NOT NULL,
            summary_json TEXT NOT NULL,
            report_hash TEXT NOT NULL,
            previous_run_id INTEGER,
            added_count INTEGER NOT NULL,
            removed_count INTEGER NOT NULL,
            changed_count INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_tri_reconcile_runs_year ON tri_reconcile_runs(year, id);
        CREATE TABLE IF NOT EXISTS tri_reconcile_rows (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id INTEGER NOT NULL REFERENCES tri_reconcile_runs(id) ON DELETE CASCADE,
            row_key TEXT NOT NULL,
            row_hash TEXT NOT NULL,
            status TEXT NOT NULL,
            mail_invoice_number TEXT,
            ksef_invoice_number TEXT,
            saldeo_invoice_number TEXT,
            issue_date TEXT,
            gross_amount_minor INTEGER,
            currency TEXT,
            row_json TEXT NOT NULL,
            UNIQUE(run_id, row_key)
        );
        CREATE INDEX IF NOT EXISTS idx_tri_reconcile_rows_run_key ON tri_reconcile_rows(run_id, row_key);
        "#,
    )?;
    ensure_invoice_columns(&conn)?;
    ensure_saldeo_upload_ledger_table(&conn)?;
    Ok(conn)
}

fn ensure_invoice_columns(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(invoices)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = HashSet::new();
    for row in rows {
        columns.insert(row?);
    }
    for (name, sql_type) in [
        ("seller_name", "TEXT"),
        ("buyer_name", "TEXT"),
        ("sale_date", "TEXT"),
        ("due_date", "TEXT"),
    ] {
        if !columns.contains(name) {
            conn.execute(
                &format!("ALTER TABLE invoices ADD COLUMN {name} {sql_type}"),
                [],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn source_as_str(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Ksef => "ksef",
        SourceKind::Mail => "mail",
        SourceKind::Saldeo => "saldeo",
    }
}

fn source_from_db(value: &str) -> rusqlite::Result<SourceKind> {
    match value {
        "ksef" => Ok(SourceKind::Ksef),
        "mail" => Ok(SourceKind::Mail),
        "saldeo" => Ok(SourceKind::Saldeo),
        _ => Err(rusqlite::Error::InvalidColumnType(
            0,
            "source".to_string(),
            Type::Text,
        )),
    }
}

/// Runs `f` inside one IMMEDIATE transaction; any error rolls everything back.
/// When the connection is already inside a transaction, `f` joins it instead.
pub(crate) fn with_sqlite_transaction<T>(
    conn: &Connection,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    if !conn.is_autocommit() {
        return f(conn);
    }
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let value = f(&tx)?;
    tx.commit()?;
    Ok(value)
}

pub(crate) fn store_records(conn: &Connection, records: &[InvoiceRecord]) -> Result<Vec<i64>> {
    with_sqlite_transaction(conn, |conn| {
        records
            .iter()
            .map(|record| upsert_invoice(conn, record))
            .collect()
    })
}

fn upsert_invoice(conn: &Connection, record: &InvoiceRecord) -> Result<i64> {
    let now = Utc::now().to_rfc3339();
    let issue_date = record.issue_date.map(|d| d.to_string());
    let sale_date = record.sale_date.map(|d| d.to_string());
    let due_date = record.due_date.map(|d| d.to_string());
    let warnings_json = serde_json::to_string(&record.warnings)?;
    conn.execute(
        r#"
        INSERT INTO invoices (
            source, source_path, content_hash, invoice_number, seller_tax_id, buyer_tax_id,
            seller_name, buyer_name, issue_date, sale_date, due_date, gross_amount_minor,
            net_amount_minor, vat_amount_minor, currency, ksef_reference, email_message_id,
            email_subject, email_from, warnings_json, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?21)
        ON CONFLICT(source, content_hash) DO UPDATE SET
            source_path = excluded.source_path,
            invoice_number = excluded.invoice_number,
            seller_tax_id = excluded.seller_tax_id,
            buyer_tax_id = excluded.buyer_tax_id,
            seller_name = excluded.seller_name,
            buyer_name = excluded.buyer_name,
            issue_date = excluded.issue_date,
            sale_date = excluded.sale_date,
            due_date = excluded.due_date,
            gross_amount_minor = excluded.gross_amount_minor,
            net_amount_minor = excluded.net_amount_minor,
            vat_amount_minor = excluded.vat_amount_minor,
            currency = excluded.currency,
            ksef_reference = excluded.ksef_reference,
            email_message_id = excluded.email_message_id,
            email_subject = excluded.email_subject,
            email_from = excluded.email_from,
            warnings_json = excluded.warnings_json,
            updated_at = excluded.updated_at
        "#,
        params![
            source_as_str(record.source),
            record.source_path,
            record.content_hash,
            record.invoice_number,
            record.seller_tax_id,
            record.buyer_tax_id,
            record.seller_name,
            record.buyer_name,
            issue_date,
            sale_date,
            due_date,
            record.gross_amount_minor,
            record.net_amount_minor,
            record.vat_amount_minor,
            record.currency,
            record.ksef_reference,
            record.email_message_id,
            record.email_subject,
            record.email_from,
            warnings_json,
            now,
        ],
    )?;
    let id = conn.query_row(
        "SELECT id FROM invoices WHERE source = ?1 AND content_hash = ?2",
        params![source_as_str(record.source), record.content_hash],
        |row| row.get(0),
    )?;
    Ok(id)
}

pub(crate) fn load_records_from_db(
    conn: &Connection,
    db_path: &Path,
    source: Option<SourceKind>,
    limit: Option<usize>,
) -> Result<Vec<InvoiceRecord>> {
    let limit = limit.unwrap_or(usize::MAX).min(i64::MAX as usize) as i64;
    let mut records = Vec::new();
    if let Some(source) = source {
        let mut stmt = conn.prepare(
            r#"
            SELECT source, source_path, content_hash, invoice_number, seller_tax_id, buyer_tax_id,
                   seller_name, buyer_name, issue_date, sale_date, due_date, gross_amount_minor,
                   net_amount_minor, vat_amount_minor, currency, ksef_reference, email_message_id,
                   email_subject, email_from, warnings_json
            FROM invoices WHERE source = ?1 ORDER BY updated_at DESC, id DESC LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![source_as_str(source), limit], invoice_from_row)?;
        for row in rows {
            records.push(row?);
        }
    } else {
        let mut stmt = conn.prepare(
            r#"
            SELECT source, source_path, content_hash, invoice_number, seller_tax_id, buyer_tax_id,
                   seller_name, buyer_name, issue_date, sale_date, due_date, gross_amount_minor,
                   net_amount_minor, vat_amount_minor, currency, ksef_reference, email_message_id,
                   email_subject, email_from, warnings_json
            FROM invoices ORDER BY updated_at DESC, id DESC LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(params![limit], invoice_from_row)?;
        for row in rows {
            records.push(row?);
        }
    }
    if source.is_none() || matches!(source, Some(SourceKind::Saldeo)) {
        let _ = apply_saldeo_record_overrides(&mut records, Some(db_path))?;
    }
    Ok(records)
}

fn invoice_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InvoiceRecord> {
    let source_text: String = row.get(0)?;
    let issue_date_text: Option<String> = row.get(8)?;
    let sale_date_text: Option<String> = row.get(9)?;
    let due_date_text: Option<String> = row.get(10)?;
    let warnings_json: String = row.get(19)?;
    Ok(InvoiceRecord {
        source: source_from_db(&source_text)?,
        source_path: row.get(1)?,
        content_hash: row.get(2)?,
        invoice_number: row.get(3)?,
        seller_tax_id: row.get(4)?,
        buyer_tax_id: row.get(5)?,
        seller_name: row.get(6)?,
        buyer_name: row.get(7)?,
        issue_date: issue_date_text.as_deref().and_then(parse_date),
        sale_date: sale_date_text.as_deref().and_then(parse_date),
        due_date: due_date_text.as_deref().and_then(parse_date),
        gross_amount_minor: row.get(11)?,
        net_amount_minor: row.get(12)?,
        vat_amount_minor: row.get(13)?,
        currency: row.get(14)?,
        ksef_reference: row.get(15)?,
        email_message_id: row.get(16)?,
        email_subject: row.get(17)?,
        email_from: row.get(18)?,
        warnings: serde_json::from_str(&warnings_json).unwrap_or_default(),
    })
}

pub(crate) fn store_tri_reconcile_report(
    conn: &Connection,
    year: i32,
    report: &TriReconcileReport,
) -> Result<TemporalDiffSummary> {
    // Header and rows commit together: a failure leaves no partial run behind.
    with_sqlite_transaction(conn, |conn| {
        store_tri_reconcile_report_rows(conn, year, report)
    })
}

fn store_tri_reconcile_report_rows(
    conn: &Connection,
    year: i32,
    report: &TriReconcileReport,
) -> Result<TemporalDiffSummary> {
    use rusqlite::OptionalExtension;
    let previous_run_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM tri_reconcile_runs WHERE year = ?1 ORDER BY id DESC LIMIT 1",
            params![year],
            |row| row.get(0),
        )
        .optional()?;
    let previous_rows = if let Some(run_id) = previous_run_id {
        load_tri_row_hashes(conn, run_id)?
    } else {
        HashMap::new()
    };
    let entries = tri_row_entries(report)?;
    let current_rows = entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.hash.clone()))
        .collect::<HashMap<_, _>>();
    let added_count = current_rows
        .keys()
        .filter(|key| !previous_rows.contains_key(*key))
        .count();
    let removed_count = previous_rows
        .keys()
        .filter(|key| !current_rows.contains_key(*key))
        .count();
    let changed_count = current_rows
        .iter()
        .filter(|(key, hash)| previous_rows.get(*key).is_some_and(|old| old != *hash))
        .count();
    let report_json = serde_json::to_vec(report)?;
    let report_hash = hex::encode(Sha256::digest(&report_json));
    conn.execute(
        r#"
        INSERT INTO tri_reconcile_runs (
            generated_at, year, review_score, mail_count, ksef_count, saldeo_count,
            summary_json, report_hash, previous_run_id, added_count, removed_count, changed_count
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        "#,
        params![
            report.generated_at.to_rfc3339(),
            year,
            report.review_score,
            report.summary.mail_count as i64,
            report.summary.ksef_count as i64,
            report.summary.saldeo_count as i64,
            serde_json::to_string(&report.summary)?,
            report_hash,
            previous_run_id,
            added_count as i64,
            removed_count as i64,
            changed_count as i64,
        ],
    )?;
    let run_id = conn.last_insert_rowid();
    for (row, entry) in report.rows.iter().zip(entries) {
        let TriRowEntry {
            key: row_key,
            hash: row_hash,
            json: row_json,
        } = entry;
        let primary = tri_row_display_record(row);
        let primary = primary.as_ref();
        conn.execute(
            r#"
            INSERT INTO tri_reconcile_rows (
                run_id, row_key, row_hash, status, mail_invoice_number, ksef_invoice_number,
                saldeo_invoice_number, issue_date, gross_amount_minor, currency, row_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                run_id,
                row_key,
                row_hash,
                row.status,
                row.mail.as_ref().and_then(|r| r.invoice_number.clone()),
                row.ksef.as_ref().and_then(|r| r.invoice_number.clone()),
                row.saldeo.as_ref().and_then(|r| r.invoice_number.clone()),
                primary.and_then(|r| r.issue_date).map(|d| d.to_string()),
                primary.and_then(|r| r.gross_amount_minor),
                primary.and_then(|r| r.currency.clone()),
                row_json,
            ],
        )?;
    }
    Ok(TemporalDiffSummary {
        run_id,
        previous_run_id,
        added_count,
        removed_count,
        changed_count,
    })
}

fn load_tri_row_hashes(conn: &Connection, run_id: i64) -> Result<HashMap<String, String>> {
    let mut stmt =
        conn.prepare("SELECT row_key, row_hash FROM tri_reconcile_rows WHERE run_id = ?1")?;
    let rows = stmt.query_map(params![run_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (key, hash) = row?;
        out.insert(key, hash);
    }
    Ok(out)
}

struct TriRowEntry {
    pub(crate) key: String,
    pub(crate) hash: String,
    pub(crate) json: String,
}

/// Row keys unique within one report and stable across runs of the same data.
/// Colliding base keys get the counterparty NIP, then the records' content hashes,
/// and finally an ordinal ordered by row hash (so it does not depend on row order).
fn tri_row_entries(report: &TriReconcileReport) -> Result<Vec<TriRowEntry>> {
    let mut entries = report
        .rows
        .iter()
        .map(|row| {
            let json = serde_json::to_string(row)?;
            Ok(TriRowEntry {
                key: tri_row_key(row),
                hash: hex::encode(Sha256::digest(json.as_bytes())),
                json,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    disambiguate_tri_row_keys(&mut entries, |idx, _| {
        tri_row_counterparty_suffix(&report.rows[idx])
    });
    disambiguate_tri_row_keys(&mut entries, |idx, key| {
        if key.contains("|ids:") {
            String::new()
        } else {
            tri_row_identity_suffix(&report.rows[idx])
        }
    });

    let mut counts = HashMap::<String, usize>::new();
    for entry in &entries {
        *counts.entry(entry.key.clone()).or_default() += 1;
    }
    let mut order = (0..entries.len()).collect::<Vec<_>>();
    order.sort_by(|&a, &b| {
        entries[a]
            .key
            .cmp(&entries[b].key)
            .then_with(|| entries[a].hash.cmp(&entries[b].hash))
            .then(a.cmp(&b))
    });
    let mut ordinals = HashMap::<String, usize>::new();
    for idx in order {
        let key = entries[idx].key.clone();
        if counts.get(&key).copied().unwrap_or(0) > 1 {
            let ordinal = ordinals.entry(key.clone()).or_default();
            entries[idx].key = format!("{key}|n:{ordinal}");
            *ordinal += 1;
        }
    }
    // A generated key could still equal another row's natural key; keep them apart.
    let mut used = HashSet::new();
    for entry in &mut entries {
        while !used.insert(entry.key.clone()) {
            entry.key.push('+');
        }
    }
    Ok(entries)
}

fn disambiguate_tri_row_keys(entries: &mut [TriRowEntry], suffix: impl Fn(usize, &str) -> String) {
    let mut counts = HashMap::<String, usize>::new();
    for entry in entries.iter() {
        *counts.entry(entry.key.clone()).or_default() += 1;
    }
    for (idx, entry) in entries.iter_mut().enumerate() {
        if counts.get(&entry.key).copied().unwrap_or(0) > 1 {
            let suffix = suffix(idx, &entry.key);
            entry.key.push_str(&suffix);
        }
    }
}

fn tri_row_counterparty_suffix(row: &TriRow) -> String {
    let Some(record) = tri_row_display_record(row) else {
        return String::new();
    };
    let seller = record.seller_tax_id.unwrap_or_default();
    let buyer = record.buyer_tax_id.unwrap_or_default();
    if seller.is_empty() && buyer.is_empty() {
        return String::new();
    }
    format!("|nip:{seller}/{buyer}")
}

fn tri_row_identity_suffix(row: &TriRow) -> String {
    let hash = |record: Option<&InvoiceRecord>| {
        record
            .map(|record| record.content_hash.clone())
            .unwrap_or_default()
    };
    format!(
        "|ids:{}/{}/{}",
        hash(row.mail.as_ref()),
        hash(row.ksef.as_ref()),
        hash(row.saldeo.as_ref())
    )
}

pub(crate) fn tri_row_key(row: &TriRow) -> String {
    for record in [row.ksef.as_ref(), row.mail.as_ref(), row.saldeo.as_ref()]
        .into_iter()
        .flatten()
    {
        if let Some(reference) = &record.ksef_reference {
            return format!("ksef:{reference}");
        }
    }
    let primary = tri_row_display_record(row);
    if let Some(record) = primary.as_ref() {
        let key = format!(
            "inv:{}|date:{}|gross:{}|cur:{}",
            record.invoice_number.clone().unwrap_or_default(),
            record.issue_date.map(|d| d.to_string()).unwrap_or_default(),
            record
                .gross_amount_minor
                .map(|v| v.to_string())
                .unwrap_or_default(),
            record.currency.clone().unwrap_or_default()
        );
        // Without an invoice number the fields above identify almost nothing,
        // so the records themselves become part of the key.
        if record.invoice_number.is_none() {
            return format!("{key}{}", tri_row_identity_suffix(row));
        }
        return key;
    }
    "empty".to_string()
}

#[cfg(test)]
mod tri_store_tests;

/// `open_db` dla ścieżek tylko do odczytu (`db stats/list/tri-runs`, `reconcile --status`
/// i ich odpowiedniki MCP): pomyłka w `--db` daje błąd zamiast nowej, pustej bazy.
pub(crate) fn open_existing_db(path: &Path) -> Result<Connection> {
    if !path.is_file() {
        return Err(anyhow!(
            "baza SQLite {} nie istnieje — sprawdź --db (baza powstaje przy `lab db init`, `lab sync` albo `lab reconcile --store`)",
            path.display()
        ));
    }
    open_db(path)
}

pub(crate) fn list_tri_runs(conn: &Connection, limit: usize) -> Result<Value> {
    let mut stmt = conn.prepare(
        r#"
        SELECT id, generated_at, year, mail_count, ksef_count, saldeo_count,
               previous_run_id, added_count, removed_count, changed_count, report_hash
        FROM tri_reconcile_runs
        ORDER BY id DESC
        LIMIT ?1
        "#,
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, i64>(0)?,
            "generated_at": row.get::<_, String>(1)?,
            "year": row.get::<_, i64>(2)?,
            "mail_count": row.get::<_, i64>(3)?,
            "ksef_count": row.get::<_, i64>(4)?,
            "saldeo_count": row.get::<_, i64>(5)?,
            "previous_run_id": row.get::<_, Option<i64>>(6)?,
            "added_count": row.get::<_, i64>(7)?,
            "removed_count": row.get::<_, i64>(8)?,
            "changed_count": row.get::<_, i64>(9)?,
            "report_hash": row.get::<_, String>(10)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(serde_json::json!(out))
}

/// Ostatni zapisany przebieg tri-reconcile dla roku — z jego własnym czasem i progiem
/// dopasowania. Uszkodzony wiersz jest błędem, nie pustym wierszem.
pub(crate) fn load_last_tri_report(conn: &Connection, year: i32) -> Result<TriReconcileReport> {
    let (run_id, generated_at, review_score): (i64, String, i64) = conn
        .query_row(
            "SELECT id, generated_at, review_score FROM tri_reconcile_runs
             WHERE year = ?1 ORDER BY id DESC LIMIT 1",
            params![year],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .with_context(|| format!("brak przebiegu tri-reconcile dla roku {year}"))?;
    let generated_at = DateTime::parse_from_rfc3339(&generated_at)
        .with_context(|| {
            format!("przebieg tri-reconcile {run_id}: niepoprawne generated_at {generated_at:?}")
        })?
        .with_timezone(&Utc);
    let review_score = u8::try_from(review_score).map_err(|_| {
        anyhow!("przebieg tri-reconcile {run_id}: niepoprawny review_score {review_score}")
    })?;
    let mut stmt =
        conn.prepare("SELECT id, row_json FROM tri_reconcile_rows WHERE run_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map(params![run_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut tri_rows = Vec::new();
    for row in rows {
        let (row_id, row_json) = row?;
        let tri_row: TriRow = serde_json::from_str(&row_json).with_context(|| {
            format!(
                "przebieg tri-reconcile {run_id}: nie można odczytać wiersza {row_id} (row_json)"
            )
        })?;
        tri_rows.push(tri_row);
    }
    let summary = tri_summary_from_rows(&tri_rows);
    Ok(TriReconcileReport {
        generated_at,
        review_score,
        summary,
        rows: tri_rows,
    })
}

fn tri_summary_from_rows(rows: &[TriRow]) -> TriSummary {
    TriSummary {
        mail_count: rows.iter().filter(|r| r.mail.is_some()).count(),
        ksef_count: rows.iter().filter(|r| r.ksef.is_some()).count(),
        saldeo_count: rows.iter().filter(|r| r.saldeo.is_some()).count(),
        in_all_three: rows.iter().filter(|r| r.status == "in_all_three").count(),
        gmail_ksef_missing_saldeo: rows
            .iter()
            .filter(|r| r.status == "gmail_ksef_missing_saldeo")
            .count(),
        gmail_saldeo_missing_ksef: rows
            .iter()
            .filter(|r| r.status == "gmail_saldeo_missing_ksef")
            .count(),
        gmail_only: rows.iter().filter(|r| r.status == "gmail_only").count(),
        ksef_saldeo_missing_gmail: rows
            .iter()
            .filter(|r| r.status == "ksef_saldeo_missing_gmail")
            .count(),
        ksef_only: rows.iter().filter(|r| r.status == "ksef_only").count(),
        saldeo_only: rows.iter().filter(|r| r.status == "saldeo_only").count(),
    }
}

pub(crate) fn db_stats(conn: &Connection) -> Result<Value> {
    let mut stmt =
        conn.prepare("SELECT source, COUNT(*) FROM invoices GROUP BY source ORDER BY source")?;
    let mut by_source = serde_json::Map::new();
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (source, count) = row?;
        by_source.insert(source, serde_json::json!(count));
    }
    let runs: i64 = conn.query_row("SELECT COUNT(*) FROM reconcile_runs", [], |row| row.get(0))?;
    let matches: i64 =
        conn.query_row("SELECT COUNT(*) FROM invoice_matches", [], |row| row.get(0))?;
    let tri_runs: i64 = conn.query_row("SELECT COUNT(*) FROM tri_reconcile_runs", [], |row| {
        row.get(0)
    })?;
    let tri_rows: i64 = conn.query_row("SELECT COUNT(*) FROM tri_reconcile_rows", [], |row| {
        row.get(0)
    })?;
    Ok(serde_json::json!({
        "invoices_by_source": by_source,
        "reconcile_runs": runs,
        "invoice_matches": matches,
        "tri_reconcile_runs": tri_runs,
        "tri_reconcile_rows": tri_rows,
    }))
}

pub(crate) fn load_records_if_present(
    source: SourceKind,
    input: &Path,
) -> Result<Vec<InvoiceRecord>> {
    if input.exists() {
        load_records(source, input)
    } else {
        Ok(Vec::new())
    }
}

pub(crate) fn load_records(source: SourceKind, input: &Path) -> Result<Vec<InvoiceRecord>> {
    if input.is_dir() {
        for file_name in [
            "ksef_records.jsonl",
            "records.jsonl",
            "ksef_records.json",
            "records.json",
        ] {
            let candidate = input.join(file_name);
            if candidate.is_file() {
                return load_records(source, &candidate);
            }
        }
        return scan_input(source, input);
    }
    let extension = input
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if extension.as_deref() == Some("jsonl") {
        let file = fs::File::open(input).with_context(|| format!("odczyt {}", input.display()))?;
        let reader = io::BufReader::new(file);
        let mut records = Vec::new();
        for (idx, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let mut record: InvoiceRecord = serde_json::from_str(&line).with_context(|| {
                format!("niepoprawny JSONL {} linia {}", input.display(), idx + 1)
            })?;
            record.source = source;
            records.push(record);
        }
        return Ok(records);
    }
    if extension.as_deref() == Some("json") {
        let text =
            fs::read_to_string(input).with_context(|| format!("odczyt {}", input.display()))?;
        // Niepoprawny JSON nie może po cichu stać się fakturą odczytaną z tekstu.
        let value: Value = serde_json::from_str(&text)
            .with_context(|| format!("niepoprawny JSON {}", input.display()))?;
        if value.is_array() {
            let mut records: Vec<InvoiceRecord> = serde_json::from_value(value)
                .with_context(|| format!("niepoprawna lista rekordów JSON {}", input.display()))?;
            for record in &mut records {
                record.source = source;
            }
            return Ok(records);
        }
        // Pojedynczy obiekt JSON to faktura (np. eksport KSeF) — czyta ją parser JSON.
    }
    scan_input(source, input)
}

fn scan_input(source: SourceKind, input: &Path) -> Result<Vec<InvoiceRecord>> {
    let mut files = Vec::new();
    if input.is_dir() {
        for entry in WalkDir::new(input).into_iter().filter_map(Result::ok) {
            let path = entry.path();
            if entry.file_type().is_file() && is_supported_file(path) {
                files.push(path.to_path_buf());
            }
        }
    } else if input.is_file() {
        files.push(input.to_path_buf());
    } else {
        return Err(anyhow!("input nie istnieje: {}", input.display()));
    }

    files.sort();
    files
        .iter()
        .map(|path| parse_file(source, path))
        .collect::<Result<Vec<_>>>()
}

pub(crate) fn is_supported_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("xml" | "json" | "txt" | "eml" | "pdf")
    )
}
