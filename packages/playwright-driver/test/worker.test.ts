import assert from "node:assert/strict";
import { Readable, Writable } from "node:stream";
import test from "node:test";

import { PLAYWRIGHT_BRIDGE_VERSION, type BridgeRequest, type PageIdentity } from "../src/types.js";
import { serveBridge } from "../src/server.js";
import { parseBridgeRequest } from "../src/validate.js";
import {
  RemotePortError,
  handleBridgeRequest,
  type PlaywrightPort,
  type RemoteLocator,
  type RemotePage,
} from "../src/worker.js";

class FakePage implements RemotePage {
  readonly stableGuid: string;
  currentTitle = "Example";
  currentText = "Example body contains abc";
  currentUrl = "https://example.test/";
  readonly calls: string[] = [];

  constructor(stableGuid = "page@11111111111111111111111111111111") {
    this.stableGuid = stableGuid;
  }

  deviceScaleFactor = async (): Promise<number> => 1;

  goto = async (url: string): Promise<unknown> => {
    this.calls.push("navigate");
    this.currentUrl = url;
    return null;
  };

  locator = (selector: string): RemoteLocator => ({
    click: async () => {
      this.calls.push(`click:${selector}`);
    },
    count: async () => selector.includes("missing") ? 0 : selector.includes("ambiguous") ? 2 : 1,
    fill: async (value) => {
      this.calls.push(`fill:${selector}:${value.length}`);
      if (selector === "#secret") this.currentTitle = value;
    },
    press: async (key) => {
      this.calls.push(`press:${selector}:${key}`);
    },
    selectOption: async (value) => {
      this.calls.push(`select:${selector}:${value}`);
    },
    textContains: async (text, caseSensitive) => {
      const content = caseSensitive ? this.currentText : this.currentText.toLowerCase();
      const expected = caseSensitive ? text : text.toLowerCase();
      return content.includes(expected);
    },
    waitFor: async ({ state }) => {
      this.calls.push(`waitSelector:${selector}:${state}`);
      // model Playwright faithfully: a never-mounting target rejects with a
      // TimeoutError, which the WORKER maps to operation_timed_out — asserting
      // the mapped code exercises the real path, not a fake-only shortcut
      if (selector.includes("missing")) {
        const timeoutError = new Error("Timeout 1000ms exceeded.");
        timeoutError.name = "TimeoutError";
        throw timeoutError;
      }
    },
    first: () => this.locator(selector),
  });

  getByText = (text: string, exact: boolean): RemoteLocator =>
    this.locator(`text:${text}:${exact ? "exact" : "substring"}`);

  readValueNearLabel = async (
    label: string,
    direction: "right" | "below",
    exact: boolean,
    timeout: number,
  ): Promise<string | undefined> => {
    this.calls.push(`readNear:${label}:${direction}:${exact}:${timeout}`);
    return label === "缺失标签" ? undefined : "$76.55B";
  };

  screenshot = async (): Promise<Buffer> => Buffer.from("png");
  title = async (): Promise<string> => this.currentTitle;
  url = (): string => this.currentUrl;
  viewportSize = (): { width: number; height: number } => ({ height: 720, width: 1280 });
  waitForLoadState = async (state: string): Promise<void> => {
    this.calls.push(`wait:${state}`);
  };
  wheel = async (x: number, y: number): Promise<void> => {
    this.calls.push(`scroll:${x}:${y}`);
  };
}

class FakePort implements PlaywrightPort {
  page = new FakePage();
  failure: RemotePortError | undefined;
  connectCalls = 0;

  async connect() {
    this.connectCalls += 1;
    if (this.failure) throw this.failure;
    return { contexts: [[this.page]] };
  }
}

function base(operation: BridgeRequest["operation"]): BridgeRequest {
  return {
    browser: "chromium",
    endpoint: "ws://127.0.0.1:3000/token",
    operation,
    timeoutMs: 1_000,
    version: PLAYWRIGHT_BRIDGE_VERSION,
  } as BridgeRequest;
}

async function identity(port: FakePort): Promise<PageIdentity> {
  const discovered = await handleBridgeRequest(base({ type: "discover" }), port);
  assert.equal(discovered.ok, true);
  if (!discovered.ok || discovered.result.type !== "pages") assert.fail("pages response");
  const page = discovered.result.pages[0];
  assert.ok(page);
  return page;
}

test("request validation is closed, bounded, and names one CSS selector dialect", () => {
  assert.throws(() =>
    parseBridgeRequest({
      browser: "chromium",
      endpoint: "http://example.test",
      operation: { type: "discover" },
      timeoutMs: 1_000,
      version: PLAYWRIGHT_BRIDGE_VERSION,
    }),
  );
  assert.throws(() =>
    parseBridgeRequest({
      browser: "chromium",
      endpoint: "ws://127.0.0.1/token",
      extra: true,
      operation: { type: "discover" },
      timeoutMs: 1_000,
      version: PLAYWRIGHT_BRIDGE_VERSION,
    }),
  );
  assert.throws(() =>
    parseBridgeRequest({
      browser: "chromium",
      endpoint: "ws://127.0.0.1/token",
      operation: {
        action: { button: "left", selector: "css=button >> text=Go", type: "click" },
        page: { contextIndex: 0, fingerprint: "a".repeat(64), pageIndex: 0 },
        type: "action",
      },
      timeoutMs: 1_000,
      version: PLAYWRIGHT_BRIDGE_VERSION,
    }),
  );
  for (const url of ["https://user:secret@example.test/", "https://example.test/#secret"]) {
    assert.throws(() =>
      parseBridgeRequest({
        browser: "chromium",
        endpoint: "ws://127.0.0.1/token",
        operation: {
          action: { type: "navigate", url },
          page: { contextIndex: 0, fingerprint: "a".repeat(64), pageIndex: 0 },
          type: "action",
        },
        timeoutMs: 1_000,
        version: PLAYWRIGHT_BRIDGE_VERSION,
      }),
    );
  }
  for (const action of [
    { selector: "css=#panel", type: "waitForSelector" },
    { selector: "#panel", state: "load", type: "waitForSelector" },
    { text: "", type: "clickByText" },
    { exact: "yes", text: "Go", type: "clickByText" },
    { label: "", type: "readValueNearLabel" },
    { direction: "above", label: "Total", type: "readValueNearLabel" },
    { label: "Total", selector: "#x", type: "readValueNearLabel" },
  ]) {
    assert.throws(() =>
      parseBridgeRequest({
        browser: "chromium",
        endpoint: "ws://127.0.0.1/token",
        operation: {
          action,
          page: { contextIndex: 0, fingerprint: "a".repeat(64), pageIndex: 0 },
          type: "action",
        },
        timeoutMs: 1_000,
        version: PLAYWRIGHT_BRIDGE_VERSION,
      }),
    );
  }
});

test("waitForSelector defaults to visible and clickByText defaults to exact", () => {
  const parsed = parseBridgeRequest({
    browser: "chromium",
    endpoint: "ws://127.0.0.1/token",
    operation: {
      action: { selector: "#panel", type: "waitForSelector" },
      page: { contextIndex: 0, fingerprint: "a".repeat(64), pageIndex: 0 },
      type: "action",
    },
    timeoutMs: 1_000,
    version: PLAYWRIGHT_BRIDGE_VERSION,
  });
  assert.equal(parsed.operation.type, "action");
  if (parsed.operation.type !== "action") assert.fail("action request");
  assert.deepEqual(parsed.operation.action, { selector: "#panel", state: "visible", type: "waitForSelector" });

  const clicked = parseBridgeRequest({
    browser: "chromium",
    endpoint: "ws://127.0.0.1/token",
    operation: {
      action: { text: "币种信息", type: "clickByText" },
      page: { contextIndex: 0, fingerprint: "a".repeat(64), pageIndex: 0 },
      type: "action",
    },
    timeoutMs: 1_000,
    version: PLAYWRIGHT_BRIDGE_VERSION,
  });
  if (clicked.operation.type !== "action") assert.fail("action request");
  assert.deepEqual(clicked.operation.action, { exact: true, text: "币种信息", type: "clickByText" });

  const read = parseBridgeRequest({
    browser: "chromium",
    endpoint: "ws://127.0.0.1/token",
    operation: {
      action: { label: "完全稀释市值", type: "readValueNearLabel" },
      page: { contextIndex: 0, fingerprint: "a".repeat(64), pageIndex: 0 },
      type: "action",
    },
    timeoutMs: 1_000,
    version: PLAYWRIGHT_BRIDGE_VERSION,
  });
  if (read.operation.type !== "action") assert.fail("action request");
  assert.deepEqual(read.operation.action, {
    direction: "right",
    exact: true,
    label: "完全稀释市值",
    type: "readValueNearLabel",
  });
});

test("discover, observe, actions, and stable page fingerprints remain explicit", async () => {
  const port = new FakePort();
  let page = await identity(port);
  const observation = await handleBridgeRequest(base({ page, type: "observe" }), port);
  assert.equal(observation.ok, true);
  if (!observation.ok || observation.result.type !== "page") assert.fail("page response");
  assert.equal(observation.result.page.screenshotBase64, Buffer.from("png").toString("base64"));

  const navigated = await handleBridgeRequest(
    base({ action: { type: "navigate", url: "https://next.test/" }, page, type: "action" }),
    port,
  );
  assert.equal(navigated.ok, true);
  if (!navigated.ok || navigated.result.type !== "page") assert.fail("page response");
  assert.equal(navigated.result.page.url, "https://next.test/");
  assert.equal(navigated.result.page.fingerprint, page.fingerprint);
  page = navigated.result.page;

  const stale = await handleBridgeRequest(
    base({ page: { ...page, fingerprint: "0".repeat(64) }, type: "probe" }),
    port,
  );
  assert.deepEqual(stale, {
    error: { code: "page_identity_changed", retryable: false },
    ok: false,
  });
});

test("same-state page replacement cannot receive a protected fill", async () => {
  const port = new FakePort();
  const page = await identity(port);
  const replacement = new FakePage("page@22222222222222222222222222222222");
  port.page = replacement;

  const response = await handleBridgeRequest(
    base({
      action: { selector: "#secret", text: "DO-NOT-REDIRECT", type: "fillSecret" },
      page,
      type: "action",
    }),
    port,
  );
  assert.deepEqual(response, {
    error: { code: "page_identity_changed", retryable: false },
    ok: false,
  });
  assert.deepEqual(replacement.calls, []);
  assert.equal(replacement.currentTitle, "Example");
});

test("a reconnected wrapper for the same Playwright page keeps its identity", async () => {
  const port = new FakePort();
  const page = await identity(port);
  port.page = new FakePage(port.page.stableGuid);
  const response = await handleBridgeRequest(base({ page, type: "probe" }), port);
  assert.equal(response.ok, true);
  if (!response.ok || response.result.type !== "page") assert.fail("page response");
  assert.equal(response.result.page.fingerprint, page.fingerprint);
});

test("protected fill never returns secret-derived URL or title", async () => {
  const port = new FakePort();
  const page = await identity(port);
  const secret = "DO-NOT-ECHO";
  const response = await handleBridgeRequest(
    base({
      action: { selector: "#secret", text: secret, type: "fillSecret" },
      page,
      type: "action",
    }),
    port,
  );
  assert.equal(response.ok, true);
  if (!response.ok || response.result.type !== "page") assert.fail("page response");
  assert.equal(response.result.page.title, "");
  assert.equal(response.result.page.url, "");
  assert.equal(JSON.stringify(response).includes(secret), false);
});

test("bounded read-only queries return machine-checkable outputs", async () => {
  const port = new FakePort();
  const page = await identity(port);

  const exists = await handleBridgeRequest(
    base({ action: { selector: "#result", type: "elementExists" }, page, type: "action" }),
    port,
  );
  assert.equal(exists.ok, true);
  if (!exists.ok || exists.result.type !== "page") assert.fail("page response");
  assert.deepEqual(exists.result.output, { exists: true });

  const missing = await handleBridgeRequest(
    base({ action: { selector: "#missing", type: "elementExists" }, page, type: "action" }),
    port,
  );
  assert.equal(missing.ok, true);
  if (!missing.ok || missing.result.type !== "page") assert.fail("page response");
  assert.deepEqual(missing.result.output, { exists: false });

  const contains = await handleBridgeRequest(
    base({
      action: { caseSensitive: false, selector: "body", text: "ABC", type: "textContains" },
      page,
      type: "action",
    }),
    port,
  );
  assert.equal(contains.ok, true);
  if (!contains.ok || contains.result.type !== "page") assert.fail("page response");
  assert.deepEqual(contains.result.output, { contains: true });
});

test("waitForSelector waits on the element and clickByText enforces a unique match", async () => {
  const port = new FakePort();
  const page = await identity(port);

  const waited = await handleBridgeRequest(
    base({ action: { selector: "#panel", state: "visible", type: "waitForSelector" }, page, type: "action" }),
    port,
  );
  assert.equal(waited.ok, true);
  assert.ok(port.page.calls.includes("waitSelector:#panel:visible"));

  const clicked = await handleBridgeRequest(
    base({ action: { exact: true, text: "Go", type: "clickByText" }, page, type: "action" }),
    port,
  );
  assert.equal(clicked.ok, true);
  assert.ok(port.page.calls.includes("click:text:Go:exact"));

  // ambiguity is fail-closed; a never-mounting target times out rather than
  // being reported as "not found" from a one-shot count()
  const ambiguous = await handleBridgeRequest(
    base({ action: { exact: true, text: "ambiguous label", type: "clickByText" }, page, type: "action" }),
    port,
  );
  assert.deepEqual(ambiguous, { error: { code: "operation_failed", retryable: false }, ok: false });
  assert.equal(port.page.calls.some((c) => c === "click:text:ambiguous label:exact"), false);

  const neverMounts = await handleBridgeRequest(
    base({ action: { exact: true, text: "missing label", type: "clickByText" }, page, type: "action" }),
    port,
  );
  assert.deepEqual(neverMounts, { error: { code: "operation_timed_out", retryable: true }, ok: false });
  assert.equal(port.page.calls.some((c) => c === "click:text:missing label:exact"), false);
});

test("clickByText waits for a lazily mounted target before the uniqueness check", async () => {
  const port = new FakePort();
  const page = await identity(port);
  const clicked = await handleBridgeRequest(
    base({ action: { exact: true, text: "币种信息", type: "clickByText" }, page, type: "action" }),
    port,
  );
  assert.equal(clicked.ok, true);
  // the wait must happen BEFORE the click, otherwise a not-yet-mounted target
  // fails with count()===0 instead of being waited for
  const waitAt = port.page.calls.indexOf("waitSelector:text:币种信息:exact:visible");
  const clickAt = port.page.calls.indexOf("click:text:币种信息:exact");
  assert.ok(waitAt >= 0, "expected a waitFor on the text locator");
  assert.ok(clickAt > waitAt, "expected the wait to precede the click");
});

test("readValueNearLabel returns the neighbouring value and defaults to right/exact", async () => {
  const port = new FakePort();
  const page = await identity(port);

  const read = await handleBridgeRequest(
    base({
      action: { direction: "right", exact: true, label: "完全稀释市值", type: "readValueNearLabel" },
      page,
      type: "action",
    }),
    port,
  );
  assert.equal(read.ok, true);
  if (!read.ok || read.result.type !== "page") assert.fail("page response");
  assert.deepEqual(read.result.output, { value: "$76.55B" });
  // the request timeout must reach the port: it is the polling budget
  assert.ok(port.page.calls.includes("readNear:完全稀释市值:right:true:1000"));

  // a label that cannot be resolved is a failed step, never a guessed value
  const missing = await handleBridgeRequest(
    base({
      action: { direction: "right", exact: true, label: "缺失标签", type: "readValueNearLabel" },
      page,
      type: "action",
    }),
    port,
  );
  assert.deepEqual(missing, { error: { code: "operation_failed", retryable: false }, ok: false });
});

test("persistent NDJSON server handles multiple requests before stdin closes", async () => {
  const port = new FakePort();
  const request = `${JSON.stringify(base({ type: "discover" }))}\n`;
  const chunks: Buffer[] = [];
  const sink = new Writable({
    write(chunk: Buffer, _encoding, callback) {
      chunks.push(Buffer.from(chunk));
      callback();
    },
  });

  await serveBridge(Readable.from([request, request]), sink, port);

  const responses = Buffer.concat(chunks)
    .toString("utf8")
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line) as unknown);
  assert.equal(responses.length, 2);
  assert.equal(port.connectCalls, 2);
});

test("connect failures have stable redacted classifications", async () => {
  const port = new FakePort();
  port.failure = new RemotePortError("connect_failed", true);
  assert.deepEqual(await handleBridgeRequest(base({ type: "discover" }), port), {
    error: { code: "connect_failed", retryable: true },
    ok: false,
  });
});
