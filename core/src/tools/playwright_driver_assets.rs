/// Embedded `index.js` source for Mirage's managed Playwright driver.
pub const PLAYWRIGHT_DRIVER_INDEX_JS: &str = r#"const fs = require("fs/promises");
const path = require("path");
const readline = require("readline");
const { chromium } = require("playwright");

let persistentContextPromise = null;
let nextSessionId = 1;
const sessions = new Map();

function stateRoot() {
  return process.env.MIRAGE_PLAYWRIGHT_STATE_ROOT || path.join(process.cwd(), ".mirage-browser");
}

function profileDir() {
  return (
    process.env.MIRAGE_PLAYWRIGHT_PROFILE_DIR ||
    path.join(stateRoot(), "profiles", "default")
  );
}

function screenshotDir() {
  return (
    process.env.MIRAGE_PLAYWRIGHT_SCREENSHOT_DIR ||
    path.join(stateRoot(), "screenshots")
  );
}

async function getPersistentContext() {
  if (!persistentContextPromise) {
    await fs.mkdir(profileDir(), { recursive: true });
    persistentContextPromise = chromium.launchPersistentContext(profileDir(), {
      headless: true,
    });
  }
  return persistentContextPromise;
}

function defaultTimeoutMs(request) {
  return typeof request.timeout_ms === "number" ? request.timeout_ms : 10_000;
}

function normalizeWaitUntil(value) {
  switch (value) {
    case "dom_content_loaded":
      return "domcontentloaded";
    case "network_idle":
      return "networkidle";
    case "load":
    case undefined:
    case null:
      return "load";
    default:
      throw new Error(`unsupported wait_until value: ${value}`);
  }
}

function defaultScreenshotPath(sessionId) {
  return path.join(screenshotDir(), `${sessionId}-${Date.now()}.png`);
}

async function ensureParentDirectory(filePath) {
  await fs.mkdir(path.dirname(filePath), { recursive: true });
}

async function getSession(sessionId) {
  const session = sessions.get(sessionId);
  if (!session) {
    throw new Error(`unknown Playwright session: ${sessionId}`);
  }
  return session;
}

async function pageMetadata(page) {
  let title = null;
  try {
    title = await page.title();
  } catch {
    title = null;
  }

  return {
    url: page.url() || null,
    title: title || null,
  };
}

async function createSession(request) {
  const context = await getPersistentContext();
  const trackedPages = new Set(Array.from(sessions.values()).map((session) => session.page));
  const reusablePage = context
    .pages()
    .find((page) => !trackedPages.has(page) && page.url() === "about:blank");
  const page = reusablePage || (await context.newPage());
  const sessionId = `browser-${nextSessionId++}`;
  sessions.set(sessionId, { page });

  if (request.url) {
    await page.goto(request.url, {
      waitUntil: normalizeWaitUntil(request.wait_until),
      timeout: defaultTimeoutMs(request),
    });
  }

  return {
    session_id: sessionId,
    ...(await pageMetadata(page)),
  };
}

async function navigate(request) {
  const { page } = await getSession(request.session_id);
  await page.goto(request.url, {
    waitUntil: normalizeWaitUntil(request.wait_until),
    timeout: defaultTimeoutMs(request),
  });

  return {
    session_id: request.session_id,
    ...(await pageMetadata(page)),
  };
}

async function click(request) {
  const { page } = await getSession(request.session_id);
  await page.click(request.selector, {
    timeout: defaultTimeoutMs(request),
  });

  return {
    session_id: request.session_id,
    ...(await pageMetadata(page)),
  };
}

async function fill(request) {
  const { page } = await getSession(request.session_id);
  await page.fill(request.selector, request.text, {
    timeout: defaultTimeoutMs(request),
  });

  return {
    session_id: request.session_id,
    ...(await pageMetadata(page)),
  };
}

async function press(request) {
  const { page } = await getSession(request.session_id);
  await page.press(request.selector, request.key, {
    timeout: defaultTimeoutMs(request),
  });

  return {
    session_id: request.session_id,
    ...(await pageMetadata(page)),
  };
}

async function waitFor(request) {
  const { page } = await getSession(request.session_id);
  await page.waitForSelector(request.selector, {
    timeout: defaultTimeoutMs(request),
  });

  return {
    session_id: request.session_id,
    ...(await pageMetadata(page)),
  };
}

async function extractText(request) {
  const { page } = await getSession(request.session_id);
  const selector = request.selector || "body";
  const text = await page.locator(selector).first().innerText({
    timeout: defaultTimeoutMs(request),
  });

  return {
    session_id: request.session_id,
    ...(await pageMetadata(page)),
    text,
  };
}

async function screenshot(request) {
  const { page } = await getSession(request.session_id);
  const screenshotPath = request.path
    ? path.resolve(request.path)
    : defaultScreenshotPath(request.session_id);
  await ensureParentDirectory(screenshotPath);
  await page.screenshot({
    path: screenshotPath,
    fullPage: true,
    timeout: defaultTimeoutMs(request),
  });

  return {
    session_id: request.session_id,
    ...(await pageMetadata(page)),
    screenshot_path: screenshotPath,
  };
}

async function closeSession(request) {
  const session = await getSession(request.session_id);
  await session.page.close();
  sessions.delete(request.session_id);

  return {
    session_id: request.session_id,
  };
}

async function handleRequest(request) {
  switch (request.action) {
    case "create_session":
      return createSession(request);
    case "navigate":
      return navigate(request);
    case "click":
      return click(request);
    case "fill":
      return fill(request);
    case "press":
      return press(request);
    case "wait_for":
      return waitFor(request);
    case "extract_text":
      return extractText(request);
    case "screenshot":
      return screenshot(request);
    case "close_session":
      return closeSession(request);
    default:
      throw new Error(`unsupported action: ${request.action}`);
  }
}

function writeResponse(response) {
  process.stdout.write(`${JSON.stringify(response)}\n`);
}

async function shutdown() {
  for (const session of sessions.values()) {
    try {
      await session.page.close();
    } catch (error) {
      console.error("failed to close Playwright page during shutdown:", error);
    }
  }
  sessions.clear();

  if (persistentContextPromise) {
    try {
      const context = await persistentContextPromise;
      await context.close();
    } catch (error) {
      console.error("failed to close Playwright context during shutdown:", error);
    }
  }
}

process.on("SIGINT", () => {
  shutdown()
    .catch((error) => console.error("Playwright driver shutdown failed:", error))
    .finally(() => process.exit(0));
});

process.on("SIGTERM", () => {
  shutdown()
    .catch((error) => console.error("Playwright driver shutdown failed:", error))
    .finally(() => process.exit(0));
});

(async () => {
  const rl = readline.createInterface({
    input: process.stdin,
    crlfDelay: Infinity,
  });

  for await (const line of rl) {
    if (!line.trim()) {
      continue;
    }

    let request;
    try {
      request = JSON.parse(line);
    } catch (error) {
      writeResponse({
        id: null,
        ok: false,
        error: `invalid JSON request: ${error.message}`,
      });
      continue;
    }

    try {
      const payload = await handleRequest(request);
      writeResponse({
        id: request.id,
        ok: true,
        ...payload,
      });
    } catch (error) {
      writeResponse({
        id: request.id ?? null,
        ok: false,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  await shutdown();
})().catch((error) => {
  console.error("Playwright driver crashed:", error);
  process.exit(1);
});
"#;

/// Embedded `package.json` for Mirage's managed Playwright driver package.
pub const PLAYWRIGHT_DRIVER_PACKAGE_JSON: &str = r#"{
  "name": "mirage-playwright-driver",
  "private": true,
  "version": "0.1.0",
  "description": "Headless Playwright driver used by Mirage's Rust tool wrapper",
  "main": "index.js",
  "scripts": {
    "start": "node index.js"
  },
  "dependencies": {
    "playwright": "1.59.1"
  }
}
"#;
