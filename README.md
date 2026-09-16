# LAB — Lazy Accounting Buddy

CLI do uzgadniania faktur z KSeF, Gmaila i SaldeoSMART. Agent-friendly: komendy nieinteraktywne, JSON na stdout, logi na stderr.

## Instalacja

```bash
git clone https://github.com/wydrox/lab.git
cd lab
cargo build --release
cp target/release/lab-cli ~/.local/bin/lab
```

Wymagane: Poppler (`pdftotext`, `pdfinfo`, `pdftoppm`). Przepływ: odczyt PDF → parser kolumn → opcjonalny GLM-OCR → parser → wybór kandydatów → opcjonalny LLM → walidacja → lokalny zapis.

```bash
brew install poppler
ppmlx pull mlx-community/GLM-OCR-4bit
```

OCR działa lokalnie przez `mlx_vlm` w środowisku ppmlx. Adapter używa szablonu modelu, odczytuje `result.text` i przetwarza wszystkie strony. Nie korzysta z wadliwego adaptera obrazowego serwera ppmlx. Nie pobiera modeli automatycznie.

Ustawienia przez zmienne środowiskowe lub `~/.config/lab/env`:
- `LAB_OCR_MODE=auto` (domyślnie): OCR dla pustego, niekompletnego odczytu lub niespójnych kwot. `off` wyłącza OCR, `always` wymusza OCR. Tryb `auto` preferuje spójne kwoty przed liczbą pól; zachowuje tekst Poppler z ostrzeżeniem, jeśli OCR zawiedzie albo pogarsza odczyt. To kontrola kompletności i spójności, nie gwarancja zgodności z fakturą.
- `LAB_OCR_MODEL_PATH`: lokalny katalog modelu, domyślnie `~/.ppmlx/models/mlx-community--GLM-OCR-4bit`.
- `LAB_OCR_PYTHON`: interpreter ze zainstalowanym `mlx_vlm`; domyślnie środowisko uv ppmlx, a jeśli go brak — `python3`.
- `LAB_OCR_MAX_PAGES=10`, `LAB_OCR_TIMEOUT_SECS=180`: limity. Przekroczenie limitu nie daje częściowego wyniku.
- `LAB_OCR_CACHE_DIR`: domyślnie `~/.cache/lab/ocr`. Cache jest powiązany z zawartością PDF, modelem i wersją adaptera. Tekst faktur jest zapisywany z uprawnieniami 600.
- `LAB_LLM_MODEL`: model tekstowy ppmlx, domyślnie `gemma-4-e4b-it-optiq`. MiniCPM5 można wybrać do eksperymentów, ale próba na fakturach wykazała braki pól.
- `PPMLX_BASE_URL=http://127.0.0.1:6767`, `LAB_LLM_TIMEOUT_SECS=45`.

Serwer LLM uruchom osobno, np. po pobraniu wybranego modelu:

```bash
ppmlx serve --model gemma-4-e4b-it-optiq
```

Sekrety trzyma macOS Keychain (usługa `lab-cli`, konta `gmail_token`, `saldeo_storage_state`, `saldeo_username`, `saldeo_password`, `ksef_token`, `ksef_cert_password`, `ksef_access_token`). Gdy sesja Saldeo wygaśnie, LAB loguje Helium zapisanym `SALDEO_USERNAME`/`SALDEO_PASSWORD` (hasło idzie plikiem 600, nie listą argumentów). Bez loginu otwiera Helium do ręcznego logowania. 2FA i Captcha nadal wymagają Ciebie. Kolejność szukania: niepusta zmienna środowiskowa sesji → Keychain → `~/.config/lab/env` → plik 600. Wartość znaleziona w `~/.config/lab/env` trafia do Keychain, a jej klucz znika z pliku; pozostałe klucze (`GOOGLE_CLIENT_SECRET_PATH`, `KSEF_BASE_URL`, ustawienia OCR/LLM, ścieżki) zostają. `lab onboard` zapisuje `KSEF_TOKEN`, `KSEF_CERT_PASSWORD`, `SALDEO_USERNAME` i `SALDEO_PASSWORD` wyłącznie w Keychain. `KSEF_TOKEN=... lab ...` nadpisuje Keychain na jeden proces i nie jest nigdzie zapisywane. Plik sekretu z prawami szerszymi niż 600 jest odrzucany z komunikatem podającym ścieżkę.

Zapis tokenów Gmail i sesji Saldeo do Keychain idzie przez Security.framework. Sekret nie jest argumentem procesu `security`. Zapis tokenów, konfiguracji, cache i bazy używa uprawnień 600. Procesy `pdftotext`, `pdfinfo`, `pdftoppm`, `openssl` i OCR używają narzędzi z katalogów systemowych oraz środowiska bez `PYTHONPATH`. `PPMLX_BASE_URL` musi wskazywać pętlę lokalną. OCR odrzuca PDF większy niż 40 MB i nie pobiera modeli.

LAB nie uruchamia ukrytego procesu serwera. Brak serwera daje ostrzeżenie i nie blokuje synchronizacji pozostałych źródeł. LLM otrzymuje cały odczyt do 60000 znaków; większe dokumenty są odrzucane bez obcinania. Odpowiedzi ucięte limitem tokenów nie są stosowane.

Przed walidacją odpowiedzi LLM adapter zamienia jednoznaczne kwoty typu `22,83` na `22.83`, usuwa prefiks PL i separatory polskiego NIP oraz pomija rozpoznane zagraniczne VAT-y w polach przeznaczonych wyłącznie na polski NIP. Każda taka zmiana ma ostrzeżenie. Formaty niejednoznaczne są odrzucane; kwoty nie są wyliczane. Walidacja nadal sprawdza schemat 12 pól, daty, kwoty, sumę kontrolną polskiego NIP i sumę netto + VAT. Odrzuca też dodatnie netto lub VAT większe od brutto, gdy drugiej kwoty brakuje. Zmiany stosowane są atomowo. Nazwy będące nagłówkami, np. „Nabywca” lub „DETAILS”, mogą być poprawione; inne istniejące pola nie są nadpisywane. To nie jest gwarancja zgodności z dokumentem — wątpliwe wyniki wymagają przeglądu.

Cache parsera ma wersję. Następny `sync --mail` ponownie odczyta starsze lokalne PDF-y; błędy odczytu nie usuwają wcześniejszych danych i pozostają do ponowienia. Niekompletny cache nie może zastąpić lepszego świeżego odczytu.

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

Pobranie auth do Saldeo przez Playwright: wybierz `SALDEO_AUTH_SCRIPT` w `lab onboard`, albo uruchom ręcznie. Używa Helium Browser (`/Applications/Helium.app/Contents/MacOS/Helium`); można nadpisać `HELIUM_EXECUTABLE=/ścieżka/do/Helium`. Skrypt automatycznie zapisuje auth po poprawnym sprawdzeniu cookies (bez ręcznego Enter); timeout można zmienić przez `SALDEO_AUTH_TIMEOUT_MS`.

```bash
./scripts/saldeo-auth.sh
# opcjonalnie inna ścieżka storage state:
./scripts/saldeo-auth.sh ~/.config/lab/saldeo-storage-state.json
```

## Codzienne użycie

```bash
lab                      # interaktywna tabela faktur: akcje upload / zatwierdź KSeF / odrzuć KSeF
lab sync                 # synchronizuje wszystkie trzy źródła
lab sync --ksef          # tylko KSeF online; zapisuje metadane także do lokalnej SQLite
lab sync --mail          # tylko Gmail/PDF (pobiera, parsuje, filtruje)
lab sync --amazon-mail   # tylko Amazon.it / Amazon.es z osobnym cache
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
lab upload --confirm       # faktyczny upload brakujących załączników; po nim LAB odświeża Saldeo cache
```

Puste `lab` otwiera interaktywną tabelę faktur z tri-reconcile. Skróty: `j/k` lub strzałki — ruch, `u` — upload do Saldeo, `a` — zatwierdź KSeF, `r` — odrzuć KSeF, `n` — wyczyść, `f` — ukryj faktury zatwierdzone i obecne w KSeF oraz Saldeo, `e` — lokalna poprawka Saldeo, `c` — wykonaj, `q` — wyjdź. Zmiana roku w menu uruchamia pełny sync dla tego roku. `Akceptuj` wykonuje wybrane operacje bez wychodzenia z tabeli i odświeża status/tabelę na bieżąco. Poprawione rekordy Saldeo mają `*` w kolumnie źródeł.

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

- numer faktury exact: +45, partial: +25. Ten sam numer albo numer KSeF łączy rekordy nawet przy progu 70.
- zgodny NIP kontrahenta: +20, seller NIP ta sama pozycja: +5. NIP firmy (nabywcy) nie liczy się jako zgodność.
- kwota brutto exact: +20, prawie exact (±2 gr): +17. Kwota 0 nie liczy się jako zgodność.
- data exact: +10, ±7 dni: +4
- waluta: +5
- ten sam numer faktury i kwota w jednym źródle (np. dwa dokumenty Saldeo) są scalane, także gdy daty się różnią.

```bash
lab reconcile --review-score 50 ...
```

## Uwagi

- Tokeny/auth nie są zapisywane w repo. Na macOS LAB zapisuje Gmail token, Saldeo storage state i cache tokenu dostępowego KSeF w Keychain; pliki `~/.config/lab/gmail_token.json` / `~/.config/lab/saldeo-storage-state.json` / `~/.config/lab/ksef_access_token.json` są fallbackiem lub wejściem migracyjnym. `lab doctor` i `lab onboard --check` pokazują dla każdego sekretu tylko to, czy jest ustawiony i skąd pochodzi (`env`, `keychain`, `file`, `missing`).
- KSeF: domyślnie online API v2 (`KSEF_TOKEN`, opcjonalnie `KSEF_CONTEXT_NIP`/`KSEF_ENV`/`KSEF_BASE_URL`); metadane są cache’owane w `data/ksef-<rok>/` albo `KSEF_DATA_DIR`.
- Upload do Saldeo: `generate-urls-for-upload` → `PUT` signed URL → `confirm`. Jeśli miesiąc faktury jest zamknięty, LAB zapisuje dokument w najnowszym otwartym miesiącu (bieżący miesiąc, a gdy i on jest zamknięty — kolejny otwarty).
