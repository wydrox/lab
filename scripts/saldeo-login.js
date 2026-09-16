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

function cookieHeader(cookies) {
  return cookies.map((cookie) => `${cookie.name}=${cookie.value}`).join("; ");
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

async function firstLocator(page, selectors) {
  for (const selector of selectors) {
    const locator = page.locator(selector).first();
    if ((await locator.count()) > 0 && (await locator.isVisible().catch(() => false))) {
      return locator;
    }
  }
  return null;
}

async function fillLogin(page, username, password) {
  const user = await firstLocator(page, USER_SELECTORS);
  const pass = await firstLocator(page, PASS_SELECTORS);
  if (!user || !pass) {
    return false;
  }
  await user.fill(username);
  await pass.fill(password);
  const submit = await firstLocator(page, SUBMIT_SELECTORS);
  if (submit) {
    await submit.click();
  } else {
    await pass.press("Enter");
  }
  return true;
}

async function storageAuthenticated(context) {
  const state = await context.storageState();
  const cookies = state.cookies || [];
  const xsrf = xsrfToken(cookies);
  if (!xsrf) {
    return false;
  }
  try {
    const response = await context.request.post(
      "https://saldeo.brainshare.pl/rest/client/document/list/search",
      {
        headers: {
          Cookie: cookieHeader(cookies),
          "X-SALDEO-XSRF-H-TOKEN": xsrf,
          saldeoApp: "angularApp",
          timeout: "60000",
        },
        data: authCheckBody(),
      }
    );
    return response.ok();
  } catch (_) {
    return false;
  }
}

async function main() {
  const { chromium } = require("playwright");
  const out = process.env.LAB_SALDEO_STORAGE_STATE;
  if (!out) {
    throw new Error("brak LAB_SALDEO_STORAGE_STATE");
  }
  const url = process.env.SALDEO_URL || "https://saldeo.brainshare.pl/";
  const executablePath = process.env.HELIUM_EXECUTABLE;
  const timeoutMs = Number.parseInt(process.env.SALDEO_AUTH_TIMEOUT_MS || "180000", 10);
  const login = readLoginFile(process.env.LAB_SALDEO_LOGIN_FILE || "");
  const headless =
    process.env.SALDEO_AUTH_HEADLESS === "1" ||
    (process.env.SALDEO_AUTH_HEADLESS !== "0" && Boolean(login));
  const userDataDir = path.join(os.homedir(), ".config", "lab", "helium-profile");
  fs.mkdirSync(userDataDir, { recursive: true });
  const context = await chromium.launchPersistentContext(userDataDir, {
    executablePath,
    headless,
    viewport: { width: 1400, height: 1000 },
  });
  let closed = false;
  context.once("close", () => {
    closed = true;
  });
  const page = context.pages()[0] || (await context.newPage());
  await page.goto(url, { waitUntil: "domcontentloaded" });

  if (await storageAuthenticated(context)) {
    await context.storageState({ path: out });
    await context.close().catch(() => {});
    return;
  }

  let submitted = false;
  if (login) {
    const loginDeadline = Date.now() + 30000;
    while (!closed && Date.now() < loginDeadline) {
      submitted = await fillLogin(page, login.username, login.password);
      if (submitted) {
        break;
      }
      await sleep(1000);
    }
    if (!submitted) {
      console.error("Saldeo: nie znalazłem formularza logowania");
    }
  }

  const deadline = Date.now() + timeoutMs;
  let authenticated = false;
  while (!closed && Date.now() < deadline) {
    await context.storageState({ path: out }).catch(() => {});
    if (await storageAuthenticated(context)) {
      authenticated = true;
      break;
    }
    await sleep(2000);
  }

  if (!closed) {
    await context.storageState({ path: out });
    await context.close().catch(() => {});
  }
  if (!authenticated) {
    if (login && submitted) {
      console.error(
        "Saldeo: logowanie hasłem nie dało ważnej sesji (2FA, Captcha albo zmiana formularza)"
      );
    } else if (!login) {
      console.error(`Saldeo auth timeout after ${Math.round(timeoutMs / 1000)}s`);
    }
    process.exit(2);
  }
}

if (require.main === module) {
  main().catch((err) => {
    console.error(err && err.stack ? err.stack : err);
    process.exit(1);
  });
}

module.exports = { fillLogin, firstLocator, readLoginFile, USER_SELECTORS };
