# LAB — Lazy Accounting Buddy

CLI do uzgadniania faktur z KSeF, Gmaila i SaldeoSMART. Agent-friendly: komendy nieinteraktywne, JSON na stdout, logi na stderr.

## Instalacja

```bash
git clone https://github.com/wydrox/lab.git
cd lab
cargo build --release
cp target/release/lab-cli ~/.local/bin/lab
```

Wymagane: `pdftotext` (poppler) do lekkiego wyciągania tekstu z PDF. Dla przefiltrowanych kandydatów LAB opcjonalnie wzbogaca brakujące pola lokalnym modelem `gemma-4-e4b` przez `ppmlx`.

```bash
brew install poppler
ppmlx pull gemma-4-e4b
```

Opcjonalne ustawienia LLM:

```bash
LAB_LLM_MODEL=gemma-4-e4b PPMLX_BASE_URL=http://127.0.0.1:6767 lab sync --mail
```

## Pierwsze uruchomienie

```bash
lab onboard
```

Uruchamia interaktywne menu TUI z jednym widokiem wszystkich parametrów (Gmail, Saldeo, KSeF, katalog danych i DB). Opcjonalnie można przekazać ścieżkę do client secret:

```bash
lab onboard --gmail-client-secret /path/to/google-oauth-client.json
```

Sam test (bez kreatora):

```bash
lab onboard --check
```

Pobranie auth do Saldeo przez Playwright: wybierz `SALDEO_AUTH_SCRIPT` w `lab onboard`, albo uruchom ręcznie. Używa Helium Browser (`/Applications/Helium.app/Contents/MacOS/Helium`); można nadpisać `HELIUM_EXECUTABLE=/ścieżka/do/Helium`.

```bash
./scripts/saldeo-auth.sh
# opcjonalnie inna ścieżka storage state:
./scripts/saldeo-auth.sh ~/.config/lab/saldeo-storage-state.json
```

## Codzienne użycie

```bash
lab                      # interaktywna tabela faktur: akcje upload / zatwierdź KSeF / odrzuć KSeF
lab sync                 # synchronizuje wszystkie trzy źródła
lab sync --ksef          # tylko KSeF
lab sync --mail          # tylko Gmail/PDF (pobiera, parsuje, filtruje)
lab sync --saldeo        # tylko Saldeo

lab reconcile              # odświeża KSeF/Saldeo i porównuje domyślne wyniki dla roku 2026
lab reconcile \
  --mail ./data/mail-all-pdf-2026-pdfs/records.jsonl \
  --ksef ./data/ksef-2026/records.jsonl \
  --saldeo ./data/saldeo-2026/records.jsonl \
  --output ./out/tri-reconcile-2026.json \
  --csv ./out/tri-reconcile-2026.csv \
  --store --year 2026

lab reconcile --status --year 2026   # ostatni raport z bazy

lab upload                 # plan brakujących załączników Gmail → Saldeo
lab upload --confirm       # faktyczny upload brakujących załączników
```

Puste `lab` otwiera interaktywną tabelę faktur z tri-reconcile. Skróty: `j/k` lub strzałki — ruch, `u` — upload do Saldeo, `a` — zatwierdź KSeF, `r` — odrzuć KSeF, `n` — wyczyść, `f` — filtr pozycji z możliwymi akcjami, `c` — wykonaj, `q` — wyjdź. Zapis w Saldeo wymaga dodatkowego potwierdzenia po wyjściu z tabeli.

## Automatyzacja macOS

Auto-sync/reconcile/upload przy logowaniu i cyklicznie przez launchd:

```bash
scripts/install-launchd.sh
```

Domyślnie uruchamia się przy logowaniu i co 4h, robiąc: `sync`, `reconcile --store`, `upload --confirm`, `sync --saldeo`, finalne `reconcile --store`.
Logi: `~/Library/Logs/lab/automation.log`.

Konfiguracja instalacji:

```bash
LAB_INTERVAL_SECONDS=86400 LAB_YEAR=2026 scripts/install-launchd.sh
scripts/uninstall-launchd.sh
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
- `lab doctor` — diagnostyka Gmail, Saldeo, KSeF online, DB i domyślnych źródeł reconcile

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

- Tokeny/auth nie są zapisywane w repo. Na macOS LAB zapisuje Gmail token i Saldeo storage state w Keychain; pliki `~/.config/lab/gmail_token.json` / `~/.config/lab/saldeo-storage-state.json` są fallbackiem lub wejściem migracyjnym.
- KSeF: domyślnie online API v2 (`KSEF_TOKEN`, opcjonalnie `KSEF_CONTEXT_NIP`/`KSEF_ENV`/`KSEF_BASE_URL`); metadane są cache’owane w `data/ksef-<rok>/` albo `KSEF_DATA_DIR`.
- Upload do Saldeo: `generate-urls-for-upload` → `PUT` signed URL → `confirm`
