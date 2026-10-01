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
- `OPENROUTER_API_KEY`: jeśli ustawiony, LAB wysyła sam plik PDF do OpenRouter (Gemini) przez zwykłe Chat Completions i żąda JSON według schematu 12 pól. Nie wysyła lokalnego tekstu Poppler/OCR i nie używa Batch API. Domyślny model: `google/gemini-3.8-flash`. Klucz zapisany przez `lab onboard` trafia do `.env`; Keychain jest opcjonalny (`LAB_USE_KEYCHAIN=1`). PDF chroniony hasłem jest pomijany: bez OCR, bez OpenRouter i bez zgadywania numeru z nazwy pliku. Gemini dostaje tylko trudne kandydatów faktur: brak numeru, daty albo kwoty brutto (albo niespójne kwoty) oraz sygnał faktury w numerze, NIP, KSeF, nazwie pliku albo temacie. Sam brak nazwy albo waluty nie uruchamia płatnego modelu. Wybrane wiersze w TUI (LLM) omijają ten filtr.
- `LAB_OPENROUTER_MODEL`: nadpisuje model OpenRouter. Sufiks `:batch` jest obcinany, żeby request szedł na Chat Completions. Postęp: `OpenRouter N/M (xx%)`.
- `LAB_OPENROUTER_TIMEOUT_SECS=120`, `LAB_OPENROUTER_PDF_ENGINE=native`.
- `LAB_LLM_MODEL`: lokalny model tekstowy ppmlx, domyślnie `gemma-4-e4b-it-optiq`. Używany tylko gdy brak `OPENROUTER_API_KEY`.
- `PPMLX_BASE_URL=http://127.0.0.1:6767`, `LAB_LLM_TIMEOUT_SECS=45`.

Gdy jest `OPENROUTER_API_KEY`, lokalny serwer LLM nie jest potrzebny. Bez klucza uruchom serwer osobno:

```bash
ppmlx serve --model gemma-4-e4b-it-optiq
```

Sekrety tekstowe (`KSEF_TOKEN`, `SALDEO_USERNAME`, `SALDEO_PASSWORD`, `OPENROUTER_API_KEY`) trzyma plik `.env` z prawami 600. Lokalny `.env` jest wybierany automatycznie tylko w katalogu pakietu `lab-cli`, gdy Git go ignoruje i nie śledzi. W pozostałych katalogach LAB używa `~/.config/lab/.env`; `LAB_DOTENV` pozwala jawnie wskazać inny plik. Keychain jest wyłączony, chyba że `LAB_USE_KEYCHAIN=1`. Gdy sesja Saldeo wygaśnie, LAB loguje Helium zapisanym `SALDEO_USERNAME`/`SALDEO_PASSWORD` (hasło idzie plikiem 600, nie listą argumentów). Bez loginu otwiera Helium do ręcznego logowania. 2FA i Captcha nadal wymagają Ciebie. Kolejność szukania: zmienna sesji → `.env` → plik 600 → opcjonalnie Keychain. `~/.config/lab/env` nie trzyma sekretów; znalezione tam klucze sekretów trafiają do `.env`. `lab onboard` zapisuje sekrety tekstowe w `.env`. Gdy brak `KSEF_TOKEN`, LAB używa ważnego cache tokenu dostępu albo lokalnych metadanych KSeF; pełny Sync nie przerywa Gmail i Saldeo. Online bez cache wymaga tokenu z uprawnieniem InvoiceRead. `KSEF_TOKEN=... lab ...` nadpisuje Keychain na jeden proces i nie jest nigdzie zapisywane. Plik sekretu z prawami szerszymi niż 600 jest odrzucany z komunikatem podającym ścieżkę.

Przy `LAB_USE_KEYCHAIN=1` zapis tokenów Gmail i sesji Saldeo do Keychain idzie przez Security.framework. Sekret nie jest argumentem procesu `security`. Tymczasowe pliki logowania Saldeo mają prawa 600 i są usuwane także przy błędzie instalacji lub uruchomienia procesu oraz przy przekroczeniu limitu czasu. Zapis tokenów, konfiguracji, cache i bazy używa uprawnień 600. Procesy `pdftotext`, `pdfinfo`, `pdftoppm`, `openssl` i OCR używają narzędzi z katalogów systemowych oraz środowiska bez `PYTHONPATH`. `PPMLX_BASE_URL` musi wskazywać pętlę lokalną. OCR odrzuca PDF większy niż 40 MB i nie pobiera modeli.

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

Pobranie auth do Saldeo przez Playwright: przy wygasłej sesji LAB samo loguje zapisanym `SALDEO_USERNAME`/`SALDEO_PASSWORD` w Helium i czeka na formularz po starcie SPA. Używa Helium Browser (`/Applications/Helium.app/Contents/MacOS/Helium`); można nadpisać `HELIUM_EXECUTABLE`. Timeout: `SALDEO_AUTH_TIMEOUT_MS` (domyślnie 180000). Headless tylko przy `SALDEO_AUTH_HEADLESS=1`. 2FA i Captcha nadal wymagają Ciebie.

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
lab upload --confirm --approve
                           # upload, potem zatwierdzenie nieoznaczonych dokumentów KSeF w Saldeo
lab repair                 # plan: uzupełnij puste pola Saldeo z KSeF/Gmail i zgłoś duplikaty
lab repair --confirm       # zapisz lokalne poprawki Saldeo (gwiazdka w TUI)
lab repair --llm --confirm # dodatkowo odczytaj trudne PDF-y Gmail przez LLM/OpenRouter
lab approve                # plan nieoznaczonych dokumentów KSeF w Saldeo
lab approve --confirm      # zatwierdź wszystkie nieoznaczone dokumenty KSeF w Saldeo
```

Puste `lab` otwiera interaktywną tabelę faktur z tri-reconcile. Skróty: `j/k` lub strzałki — ruch, `u` — upload do Saldeo, `a` — zatwierdź KSeF, `r` — odrzuć KSeF, `n` — wyczyść, `f` — ukryj faktury zatwierdzone i obecne w KSeF oraz Saldeo, `e` — lokalna poprawka Saldeo, `c` — wykonaj, `q` — wyjdź. Zmiana roku w menu uruchamia pełny sync dla tego roku. `Akceptuj` wykonuje wybrane operacje bez wychodzenia z tabeli i odświeża status/tabelę na bieżąco. Poprawione rekordy Saldeo mają `*` w kolumnie źródeł.

## Automatyzacja macOS

Auto-sync/reconcile/upload przy logowaniu i cyklicznie przez launchd:

```bash
scripts/install-launchd.sh
```

Domyślnie uruchamia się przy logowaniu i co 4h, robiąc: `sync`, `reconcile --store`, `repair --confirm`, `upload --confirm --approve`, `sync --saldeo`, finalne `reconcile --store`.
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

Narzędzia: `sync`, `reconcile`, `reconcile_status`, `upload`, `repair`, `approve`, `db_stats`, `tri_runs`.

Konfiguracja w `mcp/lab-mcp.example.json`.

## Pozostałe

- `lab repair` — lokalne uzupełnienie pól Saldeo z KSeF/Gmail i raport duplikatów
- `lab approve` — zatwierdzenie nieoznaczonych dokumentów KSeF w Saldeo
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

- Sekrety nie są śledzone przez Git. LAB zapisuje Gmail token, Saldeo storage state i cache tokenu dostępowego KSeF w prywatnych plikach `~/.config/lab/gmail_token.json` / `~/.config/lab/saldeo-storage-state.json` / `~/.config/lab/ksef_access_token.json`. Na macOS Keychain jest dodatkowym magazynem przy `LAB_USE_KEYCHAIN=1`. `lab doctor` i `lab onboard --check` pokazują dla każdego sekretu tylko to, czy jest ustawiony i skąd pochodzi (`env`, `keychain`, `file`, `missing`).
- KSeF: domyślnie online API v2 (`KSEF_TOKEN`, opcjonalnie `KSEF_CONTEXT_NIP`/`KSEF_ENV`/`KSEF_BASE_URL`); metadane są cache’owane w `data/ksef-<rok>/` albo `KSEF_DATA_DIR`. `KSEF_CACHE_TTL_MINS=0` wyłącza używanie cache zamiast odświeżenia online. Gdy brakuje tokenu, LAB nadal może użyć lokalnego cache bez limitu wieku.
- Upload do Saldeo: `generate-urls-for-upload` → `PUT` signed URL → `confirm`. Jeśli miesiąc faktury jest zamknięty, LAB zapisuje dokument w najnowszym otwartym miesiącu (bieżący miesiąc, a gdy i on jest zamknięty — kolejny otwarty).
