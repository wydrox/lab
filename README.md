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
- `OPENROUTER_API_KEY`: jeśli ustawiony, LAB wysyła sam plik PDF do OpenRouter (Gemini) przez zwykłe Chat Completions i żąda JSON według schematu 12 pól. Nie wysyła lokalnego tekstu Poppler/OCR i nie używa Batch API. Domyślny model: `google/gemini-3.8-flash`. Klucz zapisany przez `lab onboard` trafia do `.env`; Keychain jest opcjonalny (`LAB_USE_KEYCHAIN=1`). PDF chroniony hasłem jest pomijany: bez OCR, bez OpenRouter i bez zgadywania numeru z nazwy pliku. Gemini dostaje tylko trudne kandydatów faktur: brak numeru, daty albo kwoty brutto (albo niespójne kwoty) oraz sygnał faktury w numerze, NIP, KSeF, nazwie pliku albo temacie. Sam brak nazwy albo waluty nie uruchamia płatnego modelu. Wybrane wiersze w TUI (LLM) omijają ten filtr. Każdy PDF wysłany do modelu jest zapisywany w `llm_attempts.jsonl` (prawa 600) w katalogu poczty. Automatyczny sync nie wysyła ponownie PDF-a już wysłanego do tego samego modelu z tą samą wersją promptu, chyba że próba skończyła się błędem przejściowym (timeout, sieć, HTTP 429/5xx). Wiersze wybrane w TUI i `lab repair --llm` wysyłają zawsze. PDF, z którego nie udało się wyciągnąć tekstu (skan bez OCR), trafia do modelu, jeśli ma sygnał faktury w nazwie pliku albo temacie wiadomości; filtr ProductMesh (NIP albo nazwa firmy) działa dla niego dopiero po odpowiedzi modelu. Żądanie do OpenRoutera jest ponawiane tylko przy błędzie połączenia oraz HTTP 429/502/503/504; po przekroczeniu czasu odpowiedzi LAB nie ponawia od razu (PDF mógł zostać przetworzony i rozliczony), a kolejna synchronizacja może spróbować ponownie.
- `LAB_OPENROUTER_MODEL`: nadpisuje model OpenRouter. Sufiks `:batch` jest obcinany, żeby request szedł na Chat Completions. Postęp: `OpenRouter N/M (xx%)`.
- `LAB_OPENROUTER_TIMEOUT_SECS=120`, `LAB_OPENROUTER_PDF_ENGINE=native`.
- `LAB_LLM_MODEL`: lokalny model tekstowy ppmlx, domyślnie `gemma-4-e4b-it-optiq`. Używany tylko gdy brak `OPENROUTER_API_KEY`.
- `PPMLX_BASE_URL=http://127.0.0.1:6767`, `LAB_LLM_TIMEOUT_SECS=45`.

Gdy jest `OPENROUTER_API_KEY`, lokalny serwer LLM nie jest potrzebny. Bez klucza uruchom serwer osobno:

```bash
ppmlx serve --model gemma-4-e4b-it-optiq
```

Sekrety tekstowe (`KSEF_TOKEN`, `SALDEO_USERNAME`, `SALDEO_PASSWORD`, `OPENROUTER_API_KEY`) trzyma plik `.env` z prawami 600. Lokalny `.env` jest wybierany automatycznie tylko w katalogu pakietu `lab-cli`, gdy Git go ignoruje i nie śledzi. W pozostałych katalogach LAB używa `~/.config/lab/.env`; `LAB_DOTENV` pozwala jawnie wskazać inny plik. Keychain jest wyłączony, chyba że `LAB_USE_KEYCHAIN=1`. Gdy sesja Saldeo wygaśnie, LAB loguje Helium zapisanym `SALDEO_USERNAME`/`SALDEO_PASSWORD` (hasło idzie plikiem 600, nie listą argumentów). Bez loginu otwiera Helium do ręcznego logowania. 2FA i Captcha nadal wymagają Ciebie. Kolejność szukania: zmienna sesji → `.env` → plik 600 → opcjonalnie Keychain. `~/.config/lab/env` nie trzyma sekretów; znalezione tam klucze sekretów trafiają do `.env`. `lab onboard` zapisuje sekrety tekstowe w `.env`. Gdy brak `KSEF_TOKEN`, LAB używa ważnego cache tokenu dostępu albo lokalnych metadanych KSeF; pełny Sync nie przerywa Gmail i Saldeo. Online bez cache wymaga tokenu z uprawnieniem InvoiceRead. `KSEF_TOKEN=... lab ...` nadpisuje Keychain na jeden proces i nie jest nigdzie zapisywane. Plik sekretu (`.env` — przy dowiązaniu symbolicznym sprawdzany i zapisywany jest jego cel — `~/.config/lab/env`, pliki tokenów) z prawami szerszymi niż 600 jest odrzucany z komunikatem podającym ścieżkę; `lab doctor` pokazuje wtedy `insecure`. Nieczytelny `.env` nie jest nadpisywany: zapis kończy się błędem. Przy `LAB_USE_KEYCHAIN=1` `lab onboard` zapisuje sekret w `.env` i kopiuje go do Keychain; wartości z Keychain nie są kopiowane z powrotem do `.env`. Klucz sekretu, który jest już w `.env`, nie jest nadpisywany starszą kopią z `~/.config/lab/env` — ta kopia jest tylko usuwana. Sekret ze znakiem nowej linii albo NUL nie jest zapisywany; jedno końcowe `\n` z Keychain albo wklejki jest obcinane. Sam `SALDEO_USERNAME` bez hasła oznacza logowanie ręczne (z ostrzeżeniem); `lab onboard` nie zapisze loginu bez hasła.

Przy `LAB_USE_KEYCHAIN=1` zapis tokenów Gmail i sesji Saldeo do Keychain idzie przez Security.framework. Sekret nie jest argumentem procesu `security`. Tymczasowe pliki logowania Saldeo mają prawa 600 i są usuwane także przy błędzie instalacji lub uruchomienia procesu oraz przy przekroczeniu limitu czasu. Zapis tokenów, konfiguracji, cache i bazy używa uprawnień 600. Procesy `pdftotext`, `pdfinfo`, `pdftoppm`, `openssl` i OCR używają narzędzi z katalogów systemowych oraz środowiska bez `PYTHONPATH`. Narzędzie jest pomijane, gdy jego plik albo katalog — także po rozwinięciu dowiązań, jak w Homebrew — jest zapisywalny dla wszystkich albo należy do kogoś innego niż root i Ty; ta sama reguła dotyczy katalogów w `PATH` procesów potomnych, a pomocnik OCR dostaje ścieżki sprawdzonych `pdfinfo` i `pdftoppm`. Katalog zapisywalny dla grupy (np. `/opt/homebrew/bin` z grupą `admin`) jest dozwolony, ale `lab doctor` i `lab onboard --check` pokazują ostrzeżenie: inni członkowie tej grupy mogą podmienić narzędzia. `PPMLX_BASE_URL` musi wskazywać pętlę lokalną. OCR odrzuca PDF większy niż 40 MB i nie pobiera modeli. Wybór lokalnego `.env` używa Git z katalogów systemowych z wyłączonym `core.fsmonitor` i hookami. Pomocnik OCR działa w prywatnym katalogu cache OCR i dostaje ścieżki bezwzględne. `node`, `npm`, skrypt logowania i przeglądarka otwierana do autoryzacji Gmail nie dostają w środowisku `KSEF_TOKEN`, `SALDEO_*`, `OPENROUTER_API_KEY` ani tokenów Gmail; `npm install` instaluje przypiętą wersję `playwright@1.63.0` (nadpisanie: `LAB_PLAYWRIGHT_VERSION`, tylko litery, cyfry, `.` i `-`) z `--ignore-scripts`; już zainstalowany Playwright zostaje bez zmian. Ciasteczka Saldeo są wysyłane tylko do hosta Saldeo, zgodnie z domeną, ścieżką i flagą `secure`. Względny `downloadUrl` jest rozwiązywany względem `https://saldeo.brainshare.pl/`; LAB pobiera dokumenty tylko przez https.

LAB nie uruchamia ukrytego procesu serwera. Brak serwera daje ostrzeżenie i nie blokuje synchronizacji pozostałych źródeł. LLM otrzymuje cały odczyt do 60000 znaków; większe dokumenty są odrzucane bez obcinania. Odpowiedzi ucięte limitem tokenów nie są stosowane.

Przed walidacją odpowiedzi LLM adapter zamienia jednoznaczne kwoty typu `22,83` na `22.83`, usuwa prefiks PL i separatory polskiego NIP oraz pomija rozpoznane zagraniczne VAT-y w polach przeznaczonych wyłącznie na polski NIP. Każda taka zmiana ma ostrzeżenie. Formaty niejednoznaczne są odrzucane; kwoty nie są wyliczane. Walidacja nadal sprawdza schemat 12 pól, daty, kwoty, sumę kontrolną polskiego NIP i sumę netto + VAT. Odrzuca też dodatnie netto lub VAT większe od brutto, gdy drugiej kwoty brakuje. Zmiany stosowane są atomowo. Nazwy będące nagłówkami, np. „Nabywca” lub „DETAILS”, mogą być poprawione; niespójne kwoty netto/VAT/brutto mogą być zastąpione spójnym kompletem z LLM, a numer odczytany z nazwy pliku — numerem z dokumentu (oba z ostrzeżeniem); pozostałe istniejące pola nie są nadpisywane. To nie jest gwarancja zgodności z dokumentem — wątpliwe wyniki wymagają przeglądu.

Cache parsera ma wersję. Następny `sync --mail` ponownie odczyta starsze lokalne PDF-y, a wartości z nowej wersji parsera zastępują błędne wartości starej (np. zamienione NIP-y, numer faktury korygowanej, kwoty). Pole, którego nowy odczyt nie znalazł, zachowuje dotychczasową wartość; numer zgadnięty z nazwy pliku nie zastępuje odczytanego numeru, niespójne nowe kwoty nie zastępują spójnych, a temat, nadawca i Message-ID wiadomości zostają bez zmian. Błędy odczytu nie usuwają wcześniejszych danych i pozostają do ponowienia. Niekompletny cache nie może zastąpić lepszego świeżego odczytu. Rok oznacza rok daty wystawienia faktury: Gmail jest przeszukiwany od 1 grudnia poprzedniego roku do 1 lutego następnego, faktury z datą w innym roku są pomijane, a faktury bez daty zostają. Załączniki są zapisywane atomowo z prawami 600; przerwane pobieranie jest dokańczane przy następnym syncu. Pliki tymczasowe po przerwanym zapisie (`.*.lab-tmp-*`) starsze niż godzina są usuwane z katalogu poczty na początku pobierania. Data z ukośnikiem albo myślnikiem, którą da się odczytać na dwa sposoby (np. `03/04/2026`), jest czytana jako dzień/miesiąc, chyba że inna data w tym samym dokumencie rozstrzyga kolejność (np. `03/25/2026` albo `March 2, 2026`); bez takiej wskazówki rekord dostaje ostrzeżenie „niejednoznaczny zapis daty”, które samo nie wysyła PDF-a do płatnego modelu. Daty z kropkami (`03.04.2026`) są zawsze dzień/miesiąc. Numer faktury odczytany tylko z nazwy pliku ma ostrzeżenie i nadal kwalifikuje fakturę do LLM. Kandydaci Gmail z tym samym numerem są scalani tylko przy wspólnym NIP kontrahenta albo równej kwocie brutto. Faktura korygująca dostaje własny numer; numer po „do faktury”, „dotyczy faktury”, „original invoice” itp. jest używany tylko wtedy, gdy innego brak. NIP-y są przypisywane według etykiet „Sprzedawca”/„Nabywca” (także w kolumnach). „Net 30” w warunkach płatności nie jest kwotą netto. Temat, nadawca i Message-ID wiadomości są dopisywane do rekordów załączników PDF z pliku `{id}_message.json`.

## Pierwsze uruchomienie

```bash
lab onboard
```

Uruchamia interaktywne menu TUI z jednym widokiem wszystkich parametrów (Gmail, Saldeo, KSeF, katalog danych i DB). Opcjonalnie można przekazać ścieżkę do client secret:

```bash
lab onboard --gmail-client-secret /path/to/google-oauth-client.json
```

Autoryzacja Gmail używa PKCE i lokalnego przekierowania na `127.0.0.1`; LAB czeka na nie do 5 minut. Jeśli token Gmail wygaśnie albo zostanie cofnięty (`invalid_grant`), uruchom ponownie `lab onboard` (krok Gmail). Token dostępu jest odświeżany także w trakcie długiego pobierania.

Sam test (bez kreatora):

```bash
lab onboard --check
```

Pobranie auth do Saldeo przez Playwright: przy wygasłej sesji LAB samo loguje zapisanym `SALDEO_USERNAME`/`SALDEO_PASSWORD` w Helium i czeka na formularz po starcie SPA. Używa Helium Browser (`/Applications/Helium.app/Contents/MacOS/Helium`); można nadpisać `HELIUM_EXECUTABLE`. Timeout: `SALDEO_AUTH_TIMEOUT_MS` (domyślnie 180000). Headless tylko przy `SALDEO_AUTH_HEADLESS=1`. 2FA i Captcha nadal wymagają Ciebie. Skrypt `scripts/saldeo-auth.sh` LAB uruchamia tylko z zaufanego miejsca: ścieżka bezwzględna w `SALDEO_AUTH_SCRIPT`, katalogi nad plikiem `lab` (po rozwinięciu dowiązań) albo bieżący katalog, gdy jest pakietem `lab-cli` — nigdy z katalogów nadrzędnych bieżącego katalogu. Skrypt musi należeć do Ciebie i nie może być zapisywalny dla grupy ani innych; przed uruchomieniem `lab onboard` pokazuje jego pełną ścieżkę. Hasło Saldeo idzie do skryptu plikiem 600 (`LAB_SALDEO_LOGIN_FILE`). Przy `LAB_NONINTERACTIVE=1` LAB nie otwiera Helium i nie instaluje Playwright: wygasła sesja kończy się błędem „Saldeo session expired”. Tymczasowy profil przeglądarki jest usuwany po każdym zakończeniu logowania.

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
lab sync --amazon-mail   # tylko Amazon.it / Amazon.es z osobnym cache; reconcile, upload, repair i tabela czytają go razem z głównym
lab sync --saldeo        # tylko Saldeo

lab reconcile              # odświeża KSeF/Saldeo (KSeF z cache, jeśli świeży) i porównuje domyślne wyniki dla bieżącego roku
lab reconcile \
  --mail ./data/mail-all-pdf-2026-pdfs/records.jsonl \
  --ksef ./data/ksef-2026/records.jsonl \
  --saldeo ./data/saldeo-2026/records.jsonl \
  --output ./out/tri-reconcile-2026.json \
  --csv ./out/tri-reconcile-2026.csv \
  --store --year 2026

lab reconcile --status --year 2026   # ostatni raport z bazy

lab upload                 # plan brakujących załączników Gmail → Saldeo
lab upload --confirm       # odświeża Saldeo, wysyła brakujące załączniki, potem ponownie odświeża Saldeo cache
lab upload --confirm --approve
                           # upload, potem zatwierdzenie nieoznaczonych dokumentów KSeF w Saldeo
lab repair                 # plan: uzupełnij puste pola Saldeo z KSeF/Gmail i zgłoś duplikaty
lab repair --confirm       # zapisz lokalne poprawki Saldeo (gwiazdka w TUI)
lab repair --llm --confirm # dodatkowo odczytaj trudne PDF-y Gmail przez LLM/OpenRouter
lab approve                # plan nieoznaczonych dokumentów KSeF w Saldeo
lab approve --confirm      # zatwierdź nieoznaczone dokumenty KSeF w Saldeo (limit --max-approve, domyślnie 50)
lab approve --confirm --max-approve 0 --require-mail   # bez limitu, tylko dokumenty z fakturą z Gmaila
```

`--year` domyślnie oznacza bieżący rok. `lab reconcile` (i narzędzie MCP `reconcile`) bez `--mail` bierze z kandydatów Gmail tylko faktury wystawione w danym roku i faktury bez daty, jak tabela TUI; jawny `--mail` jest używany bez filtrowania. `lab upload --confirm` najpierw odświeża Saldeo (błąd przerywa upload), pomija pliki zapisane w rejestrze uploadów (tabela `saldeo_upload_ledger`, klucz sha256 pliku) i kończy się kodem błędu, gdy którykolwiek plik się nie udał albo został niepotwierdzony; wynik zawsze trafia do `--output`/`--csv`. Plan dla roku wysyła tylko faktury wystawione w tym roku; pliki bez daty trafiają do bieżącego miesiąca i tylko w planie bieżącego roku. Pusty zbiór Saldeo blokuje upload, chyba że `LAB_ALLOW_EMPTY_SALDEO=1`. Pozycje `unconfirmed` trzeba sprawdzić ręcznie w Saldeo; ponowną próbę odblokowuje usunięcie wiersza z `saldeo_upload_ledger`. Naraz może wysyłać lub zatwierdzać tylko jeden proces LAB (blokada `~/.config/lab/saldeo-write.lock`). Zatwierdzić lub odrzucić można tylko dokument Saldeo, którego numer KSeF jest równy numerowi KSeF wiersza. Poprawki z `lab repair --confirm` i z TUI są zapisywane per pole razem z wartością Saldeo, którą zastąpiły; gdy Saldeo później zmieni to pole, wygrywa wartość Saldeo. Zatwierdzanie KSeF (`lab approve --confirm`, `lab upload --confirm --approve`, MCP `approve`/`upload`) ma limit `--max-approve` (domyślnie 50, `0` = bez limitu): gdy dokumentów do zatwierdzenia jest więcej, LAB nie zatwierdza żadnego, wpisuje liczbę i limit do wyniku (`summary.cap_exceeded`, `blocked_reason`) i kończy się błędem — przejrzyj listę `document_ids` i uruchom ponownie z wyższym limitem. Próba na sucho pokazuje to samo bez błędu. `--require-mail` (albo `LAB_APPROVE_REQUIRE_MAIL=1`) zatwierdza tylko dokumenty, których wiersz uzgodnienia ma też fakturę z Gmaila; pozostałe trafiają do `skipped` z powodem. Interaktywne zatwierdzanie w TUI nie ma tych ograniczeń. Dokument w Saldeo bez numeru (np. czekający na OCR) jest liczony jako obecny w Saldeo i ma ostrzeżenie, że Saldeo nie odczytało jeszcze numeru.

Puste `lab` otwiera interaktywną tabelę faktur z tri-reconcile. Skróty: `j/k` lub strzałki — ruch, spacja — zaznacz wiele wierszy, `u` — upload do Saldeo, `a` — zatwierdź KSeF, `r` — odrzuć KSeF, `n` — wyczyść (podświetlony wiersz albo zaznaczone), `f` — ukryj faktury zatwierdzone i obecne w KSeF oraz Saldeo, `e` — lokalna poprawka Saldeo, `c` — wykonaj (Akceptuj), `q`/Esc — wyjdź (podczas operacji pierwsze naciśnięcie ostrzega, drugie przerywa), Ctrl+C — wyjdź. Akcje dotyczą podświetlonego wiersza, a gdy zaznaczono wiersze spacją — zaznaczonych. Po częściowo nieudanym `Akceptuj` akcje zostają tylko na wierszach, które się nie udały. Logi TUI: `~/Library/Logs/lab/lab.log` (prawa 600); `LAB_LOG` zmienia ścieżkę. Zmiana roku w menu uruchamia pełny sync dla tego roku. `Akceptuj` wykonuje wybrane operacje bez wychodzenia z tabeli i odświeża status/tabelę na bieżąco. Poprawione rekordy Saldeo mają `*` w kolumnie źródeł, a `!` oznacza, że rekord z Gmaila, KSeF lub Saldeo ma ostrzeżenie (np. niejednoznaczna data, numer odczytany z nazwy pliku, dokument Saldeo bez numeru, zmiana z LLM). Ostrzeżenia podświetlonego wiersza, z literą źródła (G/K/S), są na dolnej krawędzi tabeli. Tabela roku pokazuje też faktury bez daty (przy sortowaniu po dacie na końcu); faktury z datą w innym roku są pomijane. Upload pliku bez daty jest dostępny tylko w tabeli bieżącego roku, tak jak w `lab upload`. Wynik LLM zmienia tylko część Gmail wiersza: gdy wiersz ma dane z KSeF lub Saldeo, tabela dalej pokazuje ich numer i kwotę. Motyw: `LAB_THEME=light|dark` wymusza jasny lub ciemny; bez tego LAB patrzy na `COLORFGBG`, a w Terminal.app — na wygląd systemu macOS.

## Automatyzacja macOS

Auto-sync/reconcile/upload przy logowaniu i cyklicznie przez launchd:

```bash
scripts/install-launchd.sh
```

Domyślnie uruchamia się przy logowaniu i co 4h, robiąc: `sync`, `reconcile --store`, `repair --confirm`, `upload --confirm --approve`, `sync --saldeo`, finalne `reconcile --store`.
Logi: `~/Library/Logs/lab/automation.log` (katalog 700, pliki 600). Gdy log przekroczy 5 MB, przy następnym uruchomieniu jest rotowany (zostają `automation.log.1`–`.3`); `launchd.out.log` w normalnym przebiegu jest pusty, a w `launchd.err.log` lądują tylko nieoczekiwane błędy skryptu. Powiadomienie i ostatnia linia logu zawierają liczbę wysłanych i nieudanych uploadów także przy błędzie. Lock po przerwanym przebiegu jest przejmowany automatycznie. Zadanie launchd działa z `LAB_NONINTERACTIVE=1` i nigdy nie otwiera okna logowania: gdy sesja Saldeo wygaśnie, dostaniesz powiadomienie „LAB: Saldeo login needed” — uruchom wtedy `lab onboard` w terminalu. Krok `upload --confirm --approve` używa domyślnego limitu 50 zatwierdzeń; przy większej zaległości zatwierdź ręcznie.

Bez `LAB_YEAR` rok jest liczony przy każdym uruchomieniu; w styczniu i lutym najpierw przetwarzany jest poprzedni rok, potem bieżący (błąd w poprzednim roku nie blokuje bieżącego). Binarka: `LAB_BIN`, domyślnie `~/.local/bin/lab` (fallback `~/.local/bin/lab-cli`). Instalator zapisuje w plist `PATH` z katalogami `node`/`npm` znalezionymi w chwili instalacji (potrzebne do ponownego logowania do Saldeo); po zmianie wersji lub lokalizacji Node uruchom `scripts/install-launchd.sh` ponownie.

Konfiguracja instalacji:

```bash
LAB_INTERVAL_SECONDS=86400 scripts/install-launchd.sh
LAB_YEAR=2025 scripts/install-launchd.sh   # opcjonalnie: przypięcie jednego roku
scripts/uninstall-launchd.sh
```

Statusy raportu: `in_all_three`, `gmail_ksef_missing_saldeo`, `gmail_saldeo_missing_ksef`, `gmail_only`, `ksef_saldeo_missing_gmail`, `ksef_only`, `saldeo_only`.

## SQLite

Domyślna baza: `lab.sqlite` w katalogu LAB; tam też trafia drzewo `data/`. Katalog LAB to `LAB_ROOT` (zmienna sesji albo `~/.config/lab/env`); bez niego bieżący katalog, jeśli ma `lab.sqlite`, `data/` albo jest checkoutem `lab-cli` — przy pierwszym takim uruchomieniu LAB zapisuje go jako `LAB_ROOT`, więc `lab` uruchomiony z innego folderu używa tej samej bazy i cache; w ostatniej kolejności `~/.local/share/lab`. Jawne `--db`, `--mail`, `--ksef`, `--saldeo`, `--output` są liczone od bieżącego katalogu jak dotąd. `lab doctor` pokazuje wybrany katalog (`database.lab_root`). Tabele: `invoices`, `tri_reconcile_runs`, `tri_reconcile_rows`, `saldeo_overrides`, `saldeo_upload_ledger`. Każdy `reconcile --store` zapisuje się atomowo — nieudany zapis nie zostawia częściowego przebiegu. `db stats`, `db list`, `db tri-runs` i `reconcile --status` nie tworzą bazy — przy błędnym `--db` kończą się błędem.

```bash
lab --db ./data/full-2026.sqlite db stats
lab --db ./data/full-2026.sqlite db tri-runs --limit 10
```

## MCP

```bash
lab --db ./data/full-2026.sqlite mcp
```

Narzędzia: `sync`, `reconcile`, `reconcile_status`, `upload`, `repair`, `approve`, `db_stats`, `tri_runs`.

Serwer używa JSON rozdzielanego znakami nowej linii na stdio (jedna wiadomość na linię); ramkowaniem `Content-Length` odpowiada tylko na żądania wysłane w ten sposób. `upload`, `repair` i `approve` są próbą na sucho, dopóki nie podasz `"confirm": true`. Argumenty złego typu albo spoza zakresu są odrzucane (-32602), nieznane argumenty też (z listą przyjmowanych). `initialize` odpowiada wersją protokołu klienta, jeśli to `2024-11-05` albo `2025-06-18`; inaczej `2025-06-18`. `year` domyślnie oznacza bieżący rok, `review_score` — 70 (zakres 50–100). `upload` przechodzi ten sam przebieg co `lab upload` i ma `isError: true` (z planem w wyniku) zawsze wtedy, gdy CLI zakończyłby się błędem: przerwane odświeżenie Saldeo przed planem, nieudany lub niepotwierdzony plik, błąd zatwierdzania albo przekroczony `max_approve`. Ostrzeżenia przebiegu są w `warnings`. `upload` i `approve` przyjmują `max_approve` i `require_mail`.

Konfiguracja w `mcp/lab-mcp.example.json`.

## Pozostałe

- `lab repair` — lokalne uzupełnienie pól Saldeo z KSeF/Gmail i raport duplikatów
- `lab approve` — zatwierdzenie nieoznaczonych dokumentów KSeF w Saldeo
- `lab db` — init, stats, list, tri-runs
- `lab doctor` — diagnostyka Gmail, Saldeo, KSeF online, DB i domyślnych źródeł reconcile

## Model dopasowania (max 100 pkt)

- numer faktury exact: +45 — numery są równe, gdy mają te same człony po podziale na separatorach i na granicy liter i cyfr, bez względu na wielkość liter i zera wiodące (`FV12/2026` = `FV/12/2026` = `fv-12-2026` = `FV/012/2026`; `1/1/2026` ≠ `11/2026`); partial (numer zawarty w drugim na granicy separatorów, np. `FV/12/2026` w `FV/12/2026/A`; `1/2026` nie jest zawarte w `11/2026`): +25. Ten sam numer KSeF zawsze łączy rekordy. Ten sam numer faktury łączy rekordy nawet poniżej progu, gdy zgadza się NIP kontrahenta; gdy jednej stronie brakuje NIP-u kontrahenta, kwota brutto musi się zgadzać (±2 gr), a waluty nie mogą się różnić.
- zgodny NIP kontrahenta: +20, seller NIP ta sama pozycja: +5. NIP firmy (nabywcy) nie liczy się jako zgodność.
- kwota brutto exact: +20, prawie exact (±2 gr): +17. Kwota 0 albo różne waluty nie liczą się jako zgodność.
- różne numery KSeF wykluczają dopasowanie (wynik 0); jeden wiersz nigdy nie łączy dokumentów o różnych numerach KSeF.
- przydział jest globalny i nie zależy od kolejności danych: najpierw numer KSeF, potem ten sam numer faktury, potem pozostałe pary od najwyższego wyniku; każdy rekord trafia do jednego wiersza.
- data exact: +10, ±7 dni: +4
- waluta: +5
- ten sam numer faktury i kwota w jednym źródle (np. dwa dokumenty Saldeo) są scalane, także gdy daty się różnią.

```bash
lab reconcile --review-score 50 ...
```

Domyślny próg to 70 dla wszystkich komend (także `reconcile`); dozwolony zakres 50–100.

## Uwagi

- Sekrety nie są śledzone przez Git. LAB zapisuje Gmail token, Saldeo storage state i cache tokenu dostępowego KSeF w prywatnych plikach `~/.config/lab/gmail_token.json` / `~/.config/lab/saldeo-storage-state.json` / `~/.config/lab/ksef_access_token.json`. Na macOS Keychain jest dodatkowym magazynem przy `LAB_USE_KEYCHAIN=1`. `lab doctor` i `lab onboard --check` pokazują dla każdego sekretu tylko to, czy jest ustawiony i skąd pochodzi (`env`, `keychain`, `file`, `missing`, `insecure`).
- KSeF: domyślnie online API v2 (`KSEF_TOKEN`, opcjonalnie `KSEF_CONTEXT_NIP`/`KSEF_ENV`/`KSEF_BASE_URL`); metadane są cache’owane w `<katalog LAB>/data/ksef-<rok>/` albo w `KSEF_DATA_DIR/ksef-<rok>/` (względny `KSEF_DATA_DIR` jest liczony od katalogu LAB). Zapytanie o metadane roku obejmuje też 31 grudnia roku poprzedniego i 1 stycznia następnego (nadal 4 zapytania na typ podmiotu; gdy KSeF odrzuci poszerzony zakres, LAB ponawia dokładny kwartał); faktury z datą wystawienia spoza roku są potem odrzucane, a dokument zwrócony dwa razy jest liczony raz. Przy cache LAB zapisuje `ksef_cache_identity.json` (środowisko, kontekst, rok) i nie używa cache z innego środowiska, kontekstu albo roku. Cache sprzed tej wersji jest ważny tylko dla produkcyjnego KSeF i swojego roku; stary katalog `KSEF_DATA_DIR` bez podkatalogu roku jest czytany tylko dla roku, którego dotyczy. `lab reconcile` używa cache młodszego niż `KSEF_CACHE_TTL_MINS` (domyślnie 360), np. zapisanego chwilę wcześniej przez `lab sync`; `KSEF_CACHE_TTL_MINS=0` wymusza odświeżenie online. Gdy brakuje tokenu, LAB nadal może użyć pasującego lokalnego cache bez limitu wieku. Token dostępu jest odnawiany przed wygaśnięciem (np. po długim czekaniu na limit 20 zapytań/h), a po HTTP 401 LAB loguje się ponownie i powtarza zapytanie raz.
- Upload do Saldeo: `generate-urls-for-upload` → `PUT` signed URL → `confirm`. Jeśli miesiąc faktury jest zamknięty, LAB zapisuje dokument w najnowszym otwartym miesiącu (bieżący miesiąc, a gdy i on jest zamknięty — kolejny otwarty).

### Document NIP check

LAB checks document text for NIP `5242920020`, including `PL` prefixes, spaces, and digit separators. The check uses the PDF text or OCR result, not the filename or an LLM answer. A NIP inside a KSeF reference does not prove that the document contains a separate NIP. This check does not confirm the buyer or seller role.

A missing NIP or unreadable document produces a warning in the document record and the TUI. Upload JSON and CSV include `own_nip_check`: `found`, `not_found`, `unreadable`, or `not_checked`. Existing records without a check stay `not_checked` until the source is read. The upload plan checks local files again. A missing NIP does not block upload.
