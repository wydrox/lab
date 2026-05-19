# LAB — Lazy Accounting Buddy

CLI do uzgadniania faktur z KSeF, Gmaila i SaldeoSMART. Agent-friendly: komendy nieinteraktywne, JSON na stdout, logi na stderr.

## Instalacja

```bash
git clone https://github.com/wydrox/lab.git
cd lab
cargo build --release
cp target/release/lab-cli ~/.local/bin/lab
```

Wymagane: `pdftotext` (poppler) i `python3 -m pypdf` do parsowania PDF:

```bash
brew install poppler
python3 -m pip install pypdf
```

## Pierwsze uruchomienie

```bash
lab onboard
```

Interaktywnie sprawdza środowisko i konfiguruje Gmail OAuth. Opcjonalnie można przekazać ścieżkę do client secret:

```bash
lab onboard --gmail-client-secret /path/to/google-oauth-client.json
```

Sam test (bez kreatora):

```bash
lab onboard --check
```

## Codzienne użycie

```bash
lab sync                 # synchronizuje wszystkie trzy źródła
lab sync --ksef          # tylko KSeF
lab sync --mail          # tylko Gmail/PDF (pobiera, parsuje, filtruje)
lab sync --saldeo        # tylko Saldeo

lab reconcile \
  --mail ./out/mail-candidates.jsonl \
  --ksef ./data/ksef-2026/records.jsonl \
  --saldeo ./data/saldeo-2026/records.jsonl \
  --output ./out/tri-reconcile-2026.json \
  --csv ./out/tri-reconcile-2026.csv \
  --store --year 2026

lab reconcile --status --year 2026   # ostatni raport z bazy

lab upload --tri-report ./out/tri-reconcile-2026.json
```

Statusy raportu: `in_all_three`, `gmail_ksef_missing_saldeo`, `gmail_saldeo_missing_ksef`, `gmail_only`, `ksef_saldeo_missing_gmail`, `ksef_only`, `saldeo_only`.

## SQLite

Domyślna baza: `./lab.sqlite`. Tabele: `invoices`, `tri_reconcile_runs`, `tri_reconcile_rows`.

```bash
lab --db ./data/full-2026.sqlite db stats
lab --db ./data/full-2026.sqlite db tri-runs --limit 10
```

## MCP

```bash
lab --db ./data/full-2026.sqlite mcp
```

Narzędzia: `sync`, `reconcile`, `reconcile_status`, `upload`, `db_stats`, `tri_runs`.

Konfiguracja w `mcp/lab-mcp.example.json`.

## Pozostałe

- `lab db` — init, stats, list, tri-runs
- `lab doctor` — diagnostyka (pdftotext, GMAIL_ACCESS_TOKEN)

## Model dopasowania (max 100 pkt)

- numer faktury exact: +45, partial: +25
- zgodny NIP: +20, seller NIP ta sama pozycja: +5
- kwota brutto exact: +20, prawie exact (±2 gr): +17
- data exact: +10, ±7 dni: +4
- waluta: +5

```bash
lab reconcile --review-score 50 ...
```

## Uwagi

- Tokeny/auth nie są zapisywane w repo (Gmail: `~/.config/lab/gmail_token.json`, Saldeo: `~/.config/lab/saldeo-storage-state.json`)
- KSeF: lokalny eksport XML/JSON, domyślnie `data/ksef-<rok>/`
- Upload do Saldeo: `generate-urls-for-upload` → `PUT` signed URL → `confirm`
