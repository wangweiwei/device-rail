import { createHash } from "node:crypto";

import type {
  BridgeFailure,
  BridgeRequest,
  BridgeResponse,
  BrowserKind,
  PageIdentity,
  PageSnapshot,
  RemoteAction,
  RemoteActionOutput,
} from "./types.js";

const MAX_CONTEXTS = 32;
const MAX_PAGES_PER_CONTEXT = 64;
const MAX_TOTAL_PAGES = 128;
const MAX_SCREENSHOT_BYTES = 16 * 1024 * 1024;
const MAX_URL_BYTES = 8 * 1024;
const MAX_TITLE_BYTES = 4 * 1024;
const PAGE_GUID = /^page@[0-9a-f]{32}$/u;

export class RemotePortError extends Error {
  constructor(
    readonly code: BridgeFailure["error"]["code"],
    readonly retryable: boolean,
  ) {
    super(code);
  }
}

export interface RemoteLocator {
  click(options: { readonly button: "left" | "middle" | "right"; readonly timeout: number }): Promise<void>;
  count(): Promise<number>;
  fill(value: string, options: { readonly timeout: number }): Promise<void>;
  press(key: string, options: { readonly timeout: number }): Promise<void>;
  selectOption(value: string, options: { readonly timeout: number }): Promise<unknown>;
  textContains(text: string, caseSensitive: boolean): Promise<boolean>;
  first(): RemoteLocator;
  waitFor(options: {
    readonly state: "attached" | "detached" | "visible" | "hidden";
    readonly timeout: number;
  }): Promise<void>;
}

export interface RemotePage {
  readonly stableGuid: string;
  deviceScaleFactor(): Promise<number>;
  url(): string;
  title(): Promise<string>;
  viewportSize(): { readonly width: number; readonly height: number } | null;
  screenshot(options: { readonly animations: "disabled"; readonly caret: "hide"; readonly type: "png" }): Promise<Buffer>;
  goto(url: string, options: { readonly timeout: number; readonly waitUntil: "load" }): Promise<unknown>;
  locator(selector: string): RemoteLocator;
  /** Text lookup over VISIBLE elements only (the port applies the filter). */
  getByText(text: string, exact: boolean): RemoteLocator;
  /**
   * Geometric label→value read. The algorithm is FIXED inside the port (the
   * wire only ever carries `{label, direction, exact}` data — never code), so
   * this cannot become an arbitrary-script escape hatch.
   * Returns undefined when the label is missing/ambiguous or nothing sits in
   * the requested direction: the caller turns that into a failed step.
   */
  readValueNearLabel(
    label: string,
    direction: "right" | "below",
    exact: boolean,
    timeout: number,
  ): Promise<string | undefined>;
  wheel(deltaX: number, deltaY: number): Promise<void>;
  waitForLoadState(
    state: "load" | "domcontentloaded" | "networkidle",
    options: { readonly timeout: number },
  ): Promise<void>;
}

export interface RemoteSession {
  readonly contexts: readonly (readonly RemotePage[])[];
}

export interface PlaywrightPort {
  connect(browser: BrowserKind, endpoint: string, timeoutMs: number): Promise<RemoteSession>;
}

function boundedUtf8(value: string, maximum: number): string {
  if (Buffer.byteLength(value) <= maximum) return value;
  let end = Math.min(value.length, maximum);
  while (end > 0 && Buffer.byteLength(value.slice(0, end)) > maximum - 3) end -= 1;
  return `${value.slice(0, end)}…`;
}

async function pageState(
  page: RemotePage,
): Promise<{ readonly fingerprint: string; readonly title: string; readonly url: string; readonly viewport: PageSnapshot["viewport"] }> {
  const viewport = page.viewportSize();
  if (!viewport || !Number.isSafeInteger(viewport.width) || !Number.isSafeInteger(viewport.height) || viewport.width <= 0 || viewport.height <= 0) {
    throw new RemotePortError("operation_failed", false);
  }
  const url = boundedUtf8(page.url(), MAX_URL_BYTES);
  const title = boundedUtf8(await page.title(), MAX_TITLE_BYTES);
  const scaleFactor = await page.deviceScaleFactor();
  if (!Number.isFinite(scaleFactor) || scaleFactor <= 0 || scaleFactor > 16) {
    throw new RemotePortError("operation_failed", false);
  }
  if (!PAGE_GUID.test(page.stableGuid)) {
    throw new RemotePortError("page_identity_unavailable", false);
  }
  const fingerprint = createHash("sha256")
    .update("devicerail.playwright.page-guid.v1\0", "utf8")
    .update(page.stableGuid, "utf8")
    .digest("hex");
  return { fingerprint, title, url, viewport: { ...viewport, scaleFactor } };
}

async function snapshot(
  page: RemotePage,
  contextIndex: number,
  pageIndex: number,
  screenshot: boolean,
  redactMetadata = false,
): Promise<PageSnapshot> {
  const state = await pageState(page);
  let screenshotBase64: string | undefined;
  if (screenshot) {
    const bytes = await page.screenshot({ animations: "disabled", caret: "hide", type: "png" });
    if (bytes.length === 0 || bytes.length > MAX_SCREENSHOT_BYTES) {
      throw new RemotePortError("response_too_large", false);
    }
    screenshotBase64 = bytes.toString("base64");
  }
  return {
    contextIndex,
    fingerprint: state.fingerprint,
    pageIndex,
    ...(redactMetadata ? { title: "", url: "" } : { title: state.title, url: state.url }),
    viewport: state.viewport,
    ...(screenshotBase64 === undefined ? {} : { screenshotBase64 }),
  };
}

function exactPage(session: RemoteSession, identity: PageIdentity): RemotePage {
  const context = session.contexts[identity.contextIndex];
  const page = context?.[identity.pageIndex];
  if (!page) throw new RemotePortError("page_not_found", false);
  return page;
}

async function requireIdentity(page: RemotePage, identity: PageIdentity): Promise<void> {
  const state = await pageState(page);
  if (state.fingerprint !== identity.fingerprint) {
    throw new RemotePortError("page_identity_changed", false);
  }
}

/**
 * The Rust supervisor races the WHOLE exchange against the same timeoutMs it
 * ships in the request — measured from before this worker even reads it — and
 * SIGKILLs the process when it fires. A wait that spends the full budget can
 * therefore never deliver its designed fail-closed answer: the supervisor
 * always wins, reports bridge_timeout, and tears down the cached browser
 * connection. Wait-shaped actions must answer strictly earlier.
 */
const WAIT_HEADROOM_MS = 2_000;
const MIN_WAIT_MS = 1_000;

function waitBudget(timeout: number): number {
  return Math.max(MIN_WAIT_MS, timeout - WAIT_HEADROOM_MS);
}

/** Surface Playwright wait timeouts as the designed bridge code instead of the
 * catch-all operation_failed. */
async function mapTimeout<T>(work: Promise<T>): Promise<T> {
  try {
    return await work;
  } catch (error) {
    if (error instanceof Error && error.name === "TimeoutError") {
      throw new RemotePortError("operation_timed_out", true);
    }
    throw error;
  }
}

async function perform(
  page: RemotePage,
  action: RemoteAction,
  timeout: number,
): Promise<RemoteActionOutput | undefined> {
  switch (action.type) {
    case "navigate":
      await page.goto(action.url, { timeout, waitUntil: "load" });
      return undefined;
    case "click":
      await page.locator(action.selector).click({ button: action.button, timeout });
      return undefined;
    case "fill":
    case "fillSecret":
      await page.locator(action.selector).fill(action.text, { timeout });
      return undefined;
    case "press":
      await page.locator(action.selector).press(action.key, { timeout });
      return undefined;
    case "select":
      await page.locator(action.selector).selectOption(action.value, { timeout });
      return undefined;
    case "scroll":
      await page.wheel(action.deltaX, action.deltaY);
      return undefined;
    case "elementExists":
      return { exists: (await page.locator(action.selector).count()) > 0 };
    case "textContains": {
      const locator = page.locator(action.selector);
      if (await locator.count() !== 1) {
        throw new RemotePortError("operation_failed", false);
      }
      return {
        contains: await locator.textContains(action.text, action.caseSensitive),
      };
    }
    case "waitFor":
      await page.waitForLoadState(action.state, { timeout });
      return undefined;
    case "waitForSelector":
      await mapTimeout(
        page.locator(action.selector).waitFor({ state: action.state, timeout: waitBudget(timeout) }),
      );
      return undefined;
    case "clickByText": {
      // getByText is visible-only (see the port): hidden duplicates — menus,
      // templates, SSR remnants — must neither hang the wait on a hidden first
      // match nor false-fail the uniqueness count.
      const locator = page.getByText(action.text, action.exact);
      // Wait for the target to exist BEFORE the uniqueness check: lazily
      // mounted controls are the reason this action exists, and count() is a
      // one-shot query that never waits. Ambiguity is still fail-closed.
      await mapTimeout(locator.first().waitFor({ state: "visible", timeout: waitBudget(timeout) }));
      if (await locator.count() !== 1) {
        throw new RemotePortError("operation_failed", false);
      }
      await mapTimeout(locator.click({ button: "left", timeout: waitBudget(timeout) }));
      return undefined;
    }
    case "readValueNearLabel": {
      // The port POLLS until the whole relation resolves. Waiting only for the
      // label is not enough: panels render progressively, so the label can be
      // mounted while the cell beside it is still empty.
      const value = await page.readValueNearLabel(
        action.label,
        action.direction,
        action.exact,
        waitBudget(timeout),
      );
      if (value === undefined) throw new RemotePortError("operation_failed", false);
      return { value };
    }
  }
}

function failure(error: unknown): BridgeFailure {
  if (error instanceof RemotePortError) {
    return { error: { code: error.code, retryable: error.retryable }, ok: false };
  }
  return { error: { code: "operation_failed", retryable: true }, ok: false };
}

export async function handleBridgeRequest(
  request: BridgeRequest,
  port: PlaywrightPort,
): Promise<BridgeResponse> {
  try {
    const session = await port.connect(request.browser, request.endpoint, request.timeoutMs);
    if (session.contexts.length > MAX_CONTEXTS) throw new RemotePortError("response_too_large", false);
    if (request.operation.type === "discover") {
      const pages: PageSnapshot[] = [];
      for (const [contextIndex, context] of session.contexts.entries()) {
        if (context.length > MAX_PAGES_PER_CONTEXT) throw new RemotePortError("response_too_large", false);
        for (const [pageIndex, page] of context.entries()) {
          if (pages.length >= MAX_TOTAL_PAGES) throw new RemotePortError("response_too_large", false);
          pages.push(await snapshot(page, contextIndex, pageIndex, false));
        }
      }
      return { ok: true, result: { pages, type: "pages" } };
    }

    const page = exactPage(session, request.operation.page);
    await requireIdentity(page, request.operation.page);
    if (request.operation.type === "probe") {
      return {
        ok: true,
        result: {
          page: await snapshot(page, request.operation.page.contextIndex, request.operation.page.pageIndex, false),
          type: "page",
        },
      };
    }
    if (request.operation.type === "observe") {
      return {
        ok: true,
        result: {
          page: await snapshot(page, request.operation.page.contextIndex, request.operation.page.pageIndex, true),
          type: "page",
        },
      };
    }
    const output = await perform(page, request.operation.action, request.timeoutMs);
    return {
      ok: true,
      result: {
        page: await snapshot(
          page,
          request.operation.page.contextIndex,
          request.operation.page.pageIndex,
          false,
          request.operation.action.type === "fillSecret",
        ),
        type: "page",
        ...(output === undefined ? {} : { output }),
      },
    };
  } catch (error) {
    return failure(error);
  }
}
