import {
  PLAYWRIGHT_BRIDGE_VERSION,
  type BridgeRequest,
  type BrowserKind,
  type PageIdentity,
  type RemoteAction,
} from "./types.js";

const MAX_ENDPOINT_BYTES = 4_096;
const MAX_SELECTOR_BYTES = 2_048;
const MAX_TEXT_BYTES = 64 * 1_024;
const MAX_URL_BYTES = 8 * 1_024;
const MAX_KEY_BYTES = 128;
const MAX_TIMEOUT_MS = 120_000;
const MAX_INDEX = 1_023;
const MAX_DELTA = 1_000_000;
const FINGERPRINT = /^[0-9a-f]{64}$/u;

type JsonObject = Record<string, unknown>;

function object(value: unknown, keys: readonly string[]): JsonObject {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("expected object");
  }
  const record = value as JsonObject;
  const actual = Object.keys(record).sort();
  const expected = [...keys].sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new Error("unexpected object fields");
  }
  return record;
}

function string(value: unknown, maximum: number, allowEmpty = false): string {
  if (typeof value !== "string" || (!allowEmpty && value.length === 0) || Buffer.byteLength(value) > maximum) {
    throw new Error("invalid string");
  }
  if (/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/u.test(value)) {
    throw new Error("control characters are not allowed");
  }
  return value;
}

function integer(value: unknown, maximum: number, allowZero = true): number {
  if (!Number.isSafeInteger(value) || typeof value !== "number" || value < (allowZero ? 0 : 1) || value > maximum) {
    throw new Error("invalid integer");
  }
  return value;
}

function boolean(value: unknown): boolean {
  if (typeof value !== "boolean") throw new Error("invalid boolean");
  return value;
}

function browser(value: unknown): BrowserKind {
  if (value !== "chromium" && value !== "firefox" && value !== "webkit") {
    throw new Error("invalid browser kind");
  }
  return value;
}

function endpoint(value: unknown): string {
  const parsed = new URL(string(value, MAX_ENDPOINT_BYTES));
  if ((parsed.protocol !== "ws:" && parsed.protocol !== "wss:") || parsed.username || parsed.password || parsed.hash) {
    throw new Error("invalid Playwright endpoint");
  }
  return parsed.href;
}

function pageIdentity(value: unknown): PageIdentity {
  const record = object(value, ["contextIndex", "fingerprint", "pageIndex"]);
  const fingerprint = string(record.fingerprint, 64);
  if (!FINGERPRINT.test(fingerprint)) throw new Error("invalid page fingerprint");
  return {
    contextIndex: integer(record.contextIndex, MAX_INDEX),
    pageIndex: integer(record.pageIndex, MAX_INDEX),
    fingerprint,
  };
}

function selector(value: unknown): string {
  const selected = string(value, MAX_SELECTOR_BYTES);
  if (selected.includes(">>") || selected.startsWith("css=")) {
    throw new Error("only the CSS selector dialect is accepted");
  }
  return selected;
}

function finiteNumber(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value) || !Number.isInteger(value) || Math.abs(value) > MAX_DELTA) {
    throw new Error("invalid finite integer");
  }
  return value;
}

function action(value: unknown): RemoteAction {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error("invalid action");
  const type = (value as JsonObject).type;
  switch (type) {
    case "navigate": {
      const record = object(value, ["type", "url"]);
      const target = new URL(string(record.url, MAX_URL_BYTES));
      if (
        (target.protocol !== "http:" && target.protocol !== "https:")
        || target.username
        || target.password
        || target.hash
      ) {
        throw new Error("invalid navigation URL");
      }
      return { type, url: target.href };
    }
    case "click": {
      const record = object(value, ["button", "selector", "type"]);
      if (record.button !== "left" && record.button !== "middle" && record.button !== "right") throw new Error("invalid button");
      return { button: record.button, selector: selector(record.selector), type };
    }
    case "fill":
    case "fillSecret": {
      const record = object(value, ["selector", "text", "type"]);
      return { selector: selector(record.selector), text: string(record.text, MAX_TEXT_BYTES, true), type };
    }
    case "press": {
      const record = object(value, ["key", "selector", "type"]);
      return { key: string(record.key, MAX_KEY_BYTES), selector: selector(record.selector), type };
    }
    case "select": {
      const record = object(value, ["selector", "type", "value"]);
      return { selector: selector(record.selector), type, value: string(record.value, MAX_TEXT_BYTES, true) };
    }
    case "scroll": {
      const record = object(value, ["deltaX", "deltaY", "type"]);
      const deltaX = finiteNumber(record.deltaX);
      const deltaY = finiteNumber(record.deltaY);
      if (deltaX === 0 && deltaY === 0) throw new Error("scroll delta must be nonzero");
      return { deltaX, deltaY, type };
    }
    case "elementExists": {
      const record = object(value, ["selector", "type"]);
      return { selector: selector(record.selector), type };
    }
    case "textContains": {
      const hasCaseSensitive = Object.hasOwn(value, "caseSensitive");
      const record = object(
        value,
        hasCaseSensitive
          ? ["caseSensitive", "selector", "text", "type"]
          : ["selector", "text", "type"],
      );
      return {
        caseSensitive: hasCaseSensitive ? boolean(record.caseSensitive) : true,
        selector: selector(record.selector),
        text: string(record.text, MAX_TEXT_BYTES, true),
        type,
      };
    }
    case "waitFor": {
      const record = object(value, ["state", "type"]);
      if (record.state !== "load" && record.state !== "domcontentloaded" && record.state !== "networkidle") {
        throw new Error("invalid wait state");
      }
      return { state: record.state, type };
    }
    case "waitForSelector": {
      const hasState = Object.hasOwn(value, "state");
      const record = object(value, hasState ? ["selector", "state", "type"] : ["selector", "type"]);
      let state: "attached" | "detached" | "visible" | "hidden" = "visible";
      if (hasState) {
        if (
          record.state !== "attached"
          && record.state !== "detached"
          && record.state !== "visible"
          && record.state !== "hidden"
        ) {
          throw new Error("invalid selector wait state");
        }
        state = record.state;
      }
      return { selector: selector(record.selector), state, type };
    }
    case "clickByText": {
      const hasExact = Object.hasOwn(value, "exact");
      const record = object(value, hasExact ? ["exact", "text", "type"] : ["text", "type"]);
      return {
        exact: hasExact ? boolean(record.exact) : true,
        text: string(record.text, MAX_TEXT_BYTES),
        type,
      };
    }
    case "readValueNearLabel": {
      const hasExact = Object.hasOwn(value, "exact");
      const hasDirection = Object.hasOwn(value, "direction");
      const keys = ["label", "type"];
      if (hasDirection) keys.push("direction");
      if (hasExact) keys.push("exact");
      const record = object(value, keys);
      let direction: "right" | "below" = "right";
      if (hasDirection) {
        if (record.direction !== "right" && record.direction !== "below") {
          throw new Error("invalid direction");
        }
        direction = record.direction;
      }
      return {
        direction,
        exact: hasExact ? boolean(record.exact) : true,
        label: string(record.label, MAX_TEXT_BYTES),
        type,
      };
    }
    default:
      throw new Error("unknown action");
  }
}

export function parseBridgeRequest(value: unknown): BridgeRequest {
  const record = object(value, ["browser", "endpoint", "operation", "timeoutMs", "version"]);
  if (record.version !== PLAYWRIGHT_BRIDGE_VERSION) throw new Error("unsupported bridge version");
  const base = {
    browser: browser(record.browser),
    endpoint: endpoint(record.endpoint),
    timeoutMs: integer(record.timeoutMs, MAX_TIMEOUT_MS, false),
    version: PLAYWRIGHT_BRIDGE_VERSION,
  } as const;
  if (record.operation === null || typeof record.operation !== "object" || Array.isArray(record.operation)) {
    throw new Error("invalid operation");
  }
  const type = (record.operation as JsonObject).type;
  if (type === "discover") {
    object(record.operation, ["type"]);
    return { ...base, operation: { type } };
  }
  if (type === "probe" || type === "observe") {
    const operation = object(record.operation, ["page", "type"]);
    return { ...base, operation: { page: pageIdentity(operation.page), type } };
  }
  if (type === "action") {
    const operation = object(record.operation, ["action", "page", "type"]);
    return {
      ...base,
      operation: { action: action(operation.action), page: pageIdentity(operation.page), type },
    };
  }
  throw new Error("unknown operation");
}
