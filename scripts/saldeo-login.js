#!/usr/bin/env node
// Logowanie Saldeo: najpierw istniejący profil Helium, potem formularz z pliku 0600.
// Hasło tylko z LAB_SALDEO_LOGIN_FILE.
const fs = require("fs");
const os = require("os");
const path = require("path");

const USER_SELECTORS = [
  'input[name="username"]',
  'input[name="login"]',
  'input[name="j_username"]',
  'input[formcontrolname="username"]',
  'input[formcontrolname="login"]',
  "input#username",
  "input#login",
  'input[type="email"]',
  'input[autocomplete="username"]',
];
const PASS_SELECTORS = [
  'input[type="password"]',
  'input[name="password"]',
  'input[name="j_password"]',
  'input[formcontrolname="password"]',
  'input[autocomplete="current-password"]',
];
const SUBMIT_SELECTORS = [
  'button[type="submit"]',
  'input[type="submit"]',
  'button:has-text("Zaloguj")',
  'button:has-text("Zaloguj się")',
  'button:has-text("Login")',
];

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

const SALDEO_SEARCH_URL = "https://saldeo.brainshare.pl/rest/client/document/list/search";
const TEMP_PROFILE_PREFIX = "lab-helium-";
// Pozostałości po procesie zabitym SIGKILL (np. limit czasu w `lab`) sprząta następny start.
const STALE_TEMP_PROFILE_MS = 6 * 60 * 60 * 1000;

// RFC 6265: Playwright zapisuje ciasteczka host-only bez wiodącej kropki.
function cookieDomainMatches(cookie, host) {
  const raw = String(cookie.domain || "").trim().toLowerCase();
  const domain = raw.replace(/^\.+/, "");
  if (!domain) {
    return false;
  }
  if (host === domain) {
    return true;
  }
  return raw.startsWith(".") && host.endsWith(`.${domain}`);
}

function cookiePathMatches(cookiePath, requestPath) {
  const path = typeof cookiePath === "string" && cookiePath.startsWith("/") ? cookiePath : "/";
  if (requestPath === path) {
    return true;
  }
  return (
    requestPath.startsWith(path) && (path.endsWith("/") || requestPath[path.length] === "/")
  );
}

// Ciasteczka, które przeglądarka wysłałaby pod `url` (domena, ścieżka, secure, wygaśnięcie).
function cookiesForUrl(cookies, url, nowSeconds = Date.now() / 1000) {
  let parsed;
  try {
    parsed = new URL(url);
  } catch (_) {
    return [];
  }
  const host = parsed.hostname.toLowerCase();
  const https = parsed.protocol === "https:";
  return (cookies || []).filter(
    (cookie) =>
      typeof cookie.name === "string" &&
      typeof cookie.value === "string" &&
      cookieDomainMatches(cookie, host) &&
      cookiePathMatches(cookie.path, parsed.pathname) &&
      (https || !cookie.secure) &&
      !(typeof cookie.expires === "number" && cookie.expires > 0 && cookie.expires <= nowSeconds)
  );
}

function cookieHeader(cookies) {
  return cookies.map((cookie) => `${cookie.name}=${cookie.value}`).join("; ");
}

// Ta sama reguła co `saldeo_session_response_valid` w Rust: 2xx z JSON-em zawierającym
// tablicę data.resultCollection; pole status, jeśli jest, musi być SUCCESS.
function sessionResponseValid(status, bodyText) {
  if (!(status >= 200 && status < 300)) {
    return false;
  }
  let value;
  try {
    value = JSON.parse(bodyText);
  } catch (_) {
    return false;
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }
  if (Object.prototype.hasOwnProperty.call(value, "status") && value.status !== "SUCCESS") {
    return false;
  }
  return Boolean(value.data && Array.isArray(value.data.resultCollection));
}

let tempProfileDir = null;

function createTempProfile(tmpRoot = os.tmpdir()) {
  tempProfileDir = fs.mkdtempSync(path.join(tmpRoot, TEMP_PROFILE_PREFIX));
  return tempProfileDir;
}

// Usuwa tymczasowy profil (zawiera ciasteczka sesji Saldeo). Synchroniczne, więc działa też
// w handlerze `exit`.
function removeTempProfile() {
  const dir = tempProfileDir;
  tempProfileDir = null;
  if (!dir) {
    return;
  }
  try {
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
  } catch (err) {
    console.error(`Saldeo: nie usunąłem tymczasowego profilu ${dir}: ${err.message}`);
  }
}

function sweepStaleTempProfiles(tmpRoot = os.tmpdir(), nowMs = Date.now(), maxAgeMs = STALE_TEMP_PROFILE_MS) {
  let entries;
  try {
    entries = fs.readdirSync(tmpRoot);
  } catch (_) {
    return [];
  }
  const removed = [];
  for (const name of entries) {
    if (!name.startsWith(TEMP_PROFILE_PREFIX)) {
      continue;
    }
    const dir = path.join(tmpRoot, name);
    try {
      const stat = fs.lstatSync(dir);
      if (!stat.isDirectory() || stat.isSymbolicLink() || nowMs - stat.mtimeMs < maxAgeMs) {
        continue;
      }
      if (typeof process.getuid === "function" && stat.uid !== process.getuid()) {
        continue;
      }
      fs.rmSync(dir, { recursive: true, force: true });
      removed.push(dir);
    } catch (_) {
      /* inny proces mógł go właśnie usunąć */
    }
  }
  return removed;
}

function xsrfToken(cookies) {
  const cookie = cookies.find((item) => item.name === "X-SALDEO-XSRF-C-TOKEN");
  return cookie && cookie.value;
}

function authCheckBody() {
  return {
    pagination: {
      pageNumber: 0,
      pageSize: 1,
      totalCount: 0,
      columnSorted: { sortColumn: "DOCUMENT_CREATE_DATE", sortDirection: "ASC" },
    },
    filter: {
      period: {
        partOfYear: 1,
        year: new Date().getFullYear(),
        selectionType: "selectedMonth",
      },
      duplicatesEnable: false,
      duplicates: false,
      splitPayment: false,
      types: [],
      contractors: [],
      stages: [],
      categories: [],
      registers: [],
      tags: [],
      assignUsers: [],
      addedBy: [],
      added: [],
      paymentStatuses: [],
      accountingPaymentTypes: [],
      searchQuery: "",
      selectKsefDocumentsYesCheckbox: false,
      selectKsefDocumentsNoCheckbox: false,
      ksefNumber: "",
      ksefMiniWorkflowStatus: null,
      ksefBoId: null,
      dimensionReportDocumentIds: [],
      dimensions: null,
    },
  };
}

function readLoginFile(filePath) {
  if (!filePath) {
    return null;
  }
  const raw = fs.readFileSync(filePath, "utf8");
  try {
    fs.unlinkSync(filePath);
  } catch (_) {
    /* Rust też usuwa plik. */
  }
  const data = JSON.parse(raw);
  const username = typeof data.username === "string" ? data.username.trim() : "";
  const password = typeof data.password === "string" ? data.password : "";
  if (!username || !password) {
    throw new Error("plik logowania Saldeo nie zawiera username i password");
  }
  return { username, password };
}

function cssSelectorList(selectors) {
  return selectors.filter((selector) => !selector.includes(":has-text(")).join(", ");
}

async function firstLocator(page, selectors) {
  for (const selector of selectors) {
    const locator = page.locator(selector).first();
    if ((await locator.count()) > 0 && (await locator.isVisible().catch(() => false))) {
      return locator;
    }
  }
  return null;
}

async function waitFirstLocator(page, selectors, timeoutMs) {
  const css = cssSelectorList(selectors);
  if (css) {
    try {
      await page.locator(css).first().waitFor({ state: "visible", timeout: timeoutMs });
    } catch (_) {
      /* spadamy na pętlę poniżej */
    }
  }
  const deadline = Date.now() + Math.min(2000, timeoutMs);
  while (Date.now() < deadline) {
    const found = await firstLocator(page, selectors);
    if (found) {
      return found;
    }
    await sleep(200);
  }
  return firstLocator(page, selectors);
}

async function fillLogin(page, username, password) {
  const user = await firstLocator(page, USER_SELECTORS);
  const pass = await firstLocator(page, PASS_SELECTORS);
  if (!user || !pass) {
    return false;
  }
  await user.click();
  await user.fill(username);
  await pass.click();
  await pass.fill(password);
  const submit = await firstLocator(page, SUBMIT_SELECTORS);
  if (submit) {
    await submit.click();
  } else {
    await pass.press("Enter");
  }
  return true;
}

async function waitAndFillLogin(page, username, password, timeoutMs) {
  const pass = await waitFirstLocator(page, PASS_SELECTORS, timeoutMs);
  if (!pass) {
    return false;
  }
  await waitFirstLocator(page, USER_SELECTORS, 5000);
  return fillLogin(page, username, password);
}

async function storageAuthenticated(context) {
  const state = await context.storageState();
  const cookies = cookiesForUrl(state.cookies || [], SALDEO_SEARCH_URL);
  const xsrf = xsrfToken(cookies);
  if (!xsrf) {
    return false;
  }
  try {
    const response = await context.request.post(SALDEO_SEARCH_URL, {
      headers: {
        Cookie: cookieHeader(cookies),
        "X-SALDEO-XSRF-H-TOKEN": xsrf,
        saldeoApp: "angularApp",
        timeout: "60000",
      },
      data: authCheckBody(),
    });
    return sessionResponseValid(response.status(), await response.text());
  } catch (_) {
    return false;
  }
}

let activeContext = null;
let shuttingDown = false;

async function closeActiveContext(timeoutMs = 5000) {
  const context = activeContext;
  activeContext = null;
  if (!context) {
    return;
  }
  await Promise.race([context.close().catch(() => {}), sleep(timeoutMs)]);
}

// Jedyne wyjście z procesu: zamyka Helium, usuwa tymczasowy profil, kończy z `code`.
async function shutdown(code, reason) {
  if (shuttingDown) {
    return;
  }
  shuttingDown = true;
  if (reason) {
    console.error(reason);
  }
  await closeActiveContext();
  removeTempProfile();
  process.exit(code);
}

function authTimeoutMs(raw) {
  const value = Number.parseInt(raw || "180000", 10);
  return Number.isFinite(value) && value > 0 ? value : 180000;
}

// Zwraca kod wyjścia: 0 = sesja zapisana, 2 = brak ważnej sesji.
async function main() {
  const { chromium } = require("playwright");
  const out = process.env.LAB_SALDEO_STORAGE_STATE;
  if (!out) {
    throw new Error("brak LAB_SALDEO_STORAGE_STATE");
  }
  const url = process.env.SALDEO_URL || "https://saldeo.brainshare.pl/";
  const executablePath = process.env.HELIUM_EXECUTABLE;
  const timeoutMs = authTimeoutMs(process.env.SALDEO_AUTH_TIMEOUT_MS);
  // `lab` zabija proces (SIGKILL) po timeoutMs + 20 s; kończymy wcześniej, żeby posprzątać.
  // Timer nie jest `unref`: każde wyjście i tak idzie przez `shutdown` → `process.exit`.
  setTimeout(() => {
    shutdown(2, `Saldeo auth: przekroczony limit ${Math.round(timeoutMs / 1000)}s`);
  }, timeoutMs + 10000);
  const login = readLoginFile(process.env.LAB_SALDEO_LOGIN_FILE || "");
  const headless = process.env.SALDEO_AUTH_HEADLESS === "1";
  const userDataDir = path.join(os.homedir(), ".config", "lab", "helium-profile");
  fs.mkdirSync(userDataDir, { recursive: true });
  sweepStaleTempProfiles();
  const launchOptions = {
    executablePath,
    headless,
    viewport: { width: 1400, height: 1000 },
    args: ["--disable-session-crashed-bubble", "--hide-crash-restore-bubble"],
    // Sygnały obsługujemy sami (zamknięcie Helium, potem usunięcie profilu tymczasowego).
    handleSIGINT: false,
    handleSIGTERM: false,
    handleSIGHUP: false,
  };
  console.error(
    `Saldeo: otwieram Helium (${headless ? "headless" : "okno"}; timeout ${Math.round(
      timeoutMs / 1000
    )}s)...`
  );
  let context;
  try {
    context = await chromium.launchPersistentContext(userDataDir, launchOptions);
  } catch (err) {
    const tmp = createTempProfile();
    console.error(`Saldeo: profil zajęty (${err.message}); używam ${tmp}`);
    context = await chromium.launchPersistentContext(tmp, launchOptions);
  }
  activeContext = context;
  let closed = false;
  context.once("close", () => {
    closed = true;
  });
  const page = context.pages()[0] || (await context.newPage());
  await page.goto(url, { waitUntil: "domcontentloaded" });
  await page.waitForLoadState("networkidle", { timeout: Math.min(30000, timeoutMs) }).catch(() => {});

  if (await storageAuthenticated(context)) {
    console.error("Saldeo: sesja z profilu Helium jest ważna");
    await context.storageState({ path: out });
    return 0;
  }

  let submitted = false;
  if (login) {
    const formWaitMs = Math.min(120000, Math.max(15000, timeoutMs - 20000));
    console.error("Saldeo: czekam na formularz logowania po starcie SPA...");
    submitted = await waitAndFillLogin(page, login.username, login.password, formWaitMs);
    if (!submitted) {
      console.error("Saldeo: nie znalazłem formularza logowania");
    } else {
      console.error("Saldeo: wysłałem login i hasło; czekam na sesję...");
    }
  } else {
    console.error("Saldeo: brak SALDEO_USERNAME/PASSWORD; zaloguj się w oknie Helium");
  }

  const deadline = Date.now() + timeoutMs;
  let authenticated = false;
  while (!closed && Date.now() < deadline) {
    if (await storageAuthenticated(context)) {
      authenticated = true;
      break;
    }
    if (login && !submitted) {
      submitted = await fillLogin(page, login.username, login.password);
      if (submitted) {
        console.error("Saldeo: wysłałem login i hasło; czekam na sesję...");
      }
    }
    await sleep(2000);
  }

  if (authenticated) {
    await context.storageState({ path: out });
    console.error("Saldeo: sesja zapisana");
    return 0;
  }
  if (login && submitted) {
    console.error(
      "Saldeo: logowanie hasłem nie dało ważnej sesji (2FA, Captcha albo zmiana formularza)"
    );
  } else if (!login) {
    console.error(`Saldeo auth timeout after ${Math.round(timeoutMs / 1000)}s`);
  }
  return 2;
}

if (require.main === module) {
  // Ostatnia linia obrony, gdyby proces kończył się inną drogą niż `shutdown`.
  process.on("exit", removeTempProfile);
  for (const [signal, code] of [
    ["SIGINT", 130],
    ["SIGTERM", 143],
    ["SIGHUP", 129],
  ]) {
    process.on(signal, () => {
      shutdown(code, `Saldeo: przerwano (${signal})`);
    });
  }
  main()
    .then((code) => shutdown(code))
    .catch((err) => shutdown(1, err && err.stack ? err.stack : String(err)));
}

module.exports = {
  fillLogin,
  firstLocator,
  waitFirstLocator,
  waitAndFillLogin,
  readLoginFile,
  cookiesForUrl,
  sessionResponseValid,
  createTempProfile,
  removeTempProfile,
  sweepStaleTempProfiles,
  shutdown,
  USER_SELECTORS,
  PASS_SELECTORS,
};
