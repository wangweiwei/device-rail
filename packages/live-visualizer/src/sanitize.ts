import { createHash } from "node:crypto";

import { LiveTimelineError } from "./errors.js";
import type { BoundedJson, BoundedText, LiveTimelineLimits } from "./types.js";

const DANGEROUS_KEYS = new Set(["__proto__", "constructor", "prototype"]);

function escapeCodePoint(codePoint: number): string {
  return `\\u{${codePoint.toString(16).padStart(4, "0")}}`;
}

function escapedCharacter(character: string, codePoint: number): string {
  const isControl =
    codePoint <= 0x1f ||
    (codePoint >= 0x7f && codePoint <= 0x9f) ||
    (codePoint >= 0xd800 && codePoint <= 0xdfff);
  const isBidi =
    codePoint === 0x061c ||
    codePoint === 0x200e ||
    codePoint === 0x200f ||
    (codePoint >= 0x202a && codePoint <= 0x202e) ||
    (codePoint >= 0x2066 && codePoint <= 0x2069);
  const isMarkup = character === "<" || character === ">" || character === "&";
  return isControl || isBidi || isMarkup ? escapeCodePoint(codePoint) : character;
}

export function boundedText(value: string, maxBytes: number): BoundedText {
  let text = "";
  let bytes = 0;
  let truncated = false;
  for (const character of value) {
    const codePoint = character.codePointAt(0);
    if (codePoint === undefined) continue;
    const escaped = escapedCharacter(character, codePoint);
    const addition = Buffer.byteLength(escaped);
    if (bytes + addition > maxBytes) {
      truncated = true;
      break;
    }
    text += escaped;
    bytes += addition;
  }
  return Object.freeze({ text, truncated });
}

class CanonicalWriter {
  readonly #blocks: string[] = [];
  readonly #limit: number;
  #bytes = 0;
  #pending: string[] = [];
  #pendingBytes = 0;

  constructor(limit: number) {
    this.#limit = limit;
  }

  push(value: string): void {
    const bytes = Buffer.byteLength(value);
    if (this.#bytes + bytes > this.#limit) {
      throw new LiveTimelineError(
        "viewer_capacity_exceeded",
        "input event exceeds the canonical fingerprint byte limit",
        { details: { limitBytes: this.#limit, observedBytesAtLeast: this.#bytes + bytes } },
      );
    }
    if (this.#pendingBytes + bytes > 8 * 1024 || this.#pending.length >= 256) {
      this.#flush();
    }
    this.#pending.push(value);
    this.#pendingBytes += bytes;
    this.#bytes += bytes;
  }

  finish(): string {
    this.#flush();
    return this.#blocks.join("");
  }

  #flush(): void {
    if (this.#pending.length === 0) return;
    this.#blocks.push(this.#pending.join(""));
    this.#pending = [];
    this.#pendingBytes = 0;
  }
}

function jsonEncode(value: unknown): string {
  const encoded = JSON.stringify(value);
  if (encoded === undefined) {
    throw new LiveTimelineError("invalid_event", "input event contains a non-JSON value");
  }
  return encoded;
}

function writeJsonString(value: string, writer: CanonicalWriter): void {
  writer.push('"');
  let plain = "";
  const flushPlain = (): void => {
    if (plain.length === 0) return;
    writer.push(plain);
    plain = "";
  };
  for (const character of value) {
    const encoded = jsonEncode(character).slice(1, -1);
    if (encoded === character) {
      plain += character;
      if (plain.length >= 1_024) flushPlain();
    } else {
      flushPlain();
      writer.push(encoded);
    }
  }
  flushPlain();
  writer.push('"');
}

function canonicalVisit(
  value: unknown,
  writer: CanonicalWriter,
  depth: number,
  maxDepth: number,
  ancestors: Set<object>,
): void {
  if (depth > maxDepth) {
    throw new LiveTimelineError("invalid_event", "input event exceeds the JSON depth limit", {
      details: { maxDepth },
    });
  }
  if (value === null || typeof value === "boolean") {
    writer.push(String(value));
    return;
  }
  if (typeof value === "string") {
    writeJsonString(value, writer);
    return;
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value))) {
      throw new LiveTimelineError("invalid_event", "input event contains an unsafe number");
    }
    writer.push(jsonEncode(value));
    return;
  }
  if (typeof value !== "object") {
    throw new LiveTimelineError("invalid_event", "input event contains a non-JSON value");
  }
  const prototype = Object.getPrototypeOf(value);
  const isArray = Array.isArray(value);
  if (
    (isArray && prototype !== Array.prototype) ||
    (!isArray && prototype !== Object.prototype && prototype !== null)
  ) {
    throw new LiveTimelineError("invalid_event", "input event contains a non-JSON prototype");
  }
  if (ancestors.has(value)) {
    throw new LiveTimelineError("invalid_event", "input event contains a cycle");
  }
  ancestors.add(value);
  if (isArray) {
    if (Object.getOwnPropertySymbols(value).length > 0) {
      throw new LiveTimelineError("invalid_event", "input event contains symbol keys");
    }
    writer.push("[");
    for (let index = 0; index < value.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (!descriptor) {
        throw new LiveTimelineError("invalid_event", "input event contains a sparse array");
      }
      if (!("value" in descriptor) || !descriptor.enumerable) {
        throw new LiveTimelineError("invalid_event", "input event contains array accessors");
      }
      if (index > 0) writer.push(",");
      canonicalVisit(descriptor.value, writer, depth + 1, maxDepth, ancestors);
    }
    writer.push("]");
  } else {
    if (Object.getOwnPropertySymbols(value).length > 0) {
      throw new LiveTimelineError("invalid_event", "input event contains symbol keys");
    }
    writer.push("{");
    const record = value as Record<string, unknown>;
    const keys = Object.keys(record).sort();
    keys.forEach((key, index) => {
      if (index > 0) writer.push(",");
      writeJsonString(key, writer);
      writer.push(":");
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor || !("value" in descriptor)) {
        throw new LiveTimelineError(
          "invalid_event",
          "input event contains an accessor or changed during fingerprinting",
        );
      }
      canonicalVisit(descriptor.value, writer, depth + 1, maxDepth, ancestors);
    });
    writer.push("}");
  }
  ancestors.delete(value);
}

export function canonicalFingerprint(
  value: unknown,
  limits: LiveTimelineLimits,
): { readonly canonical: string; readonly fingerprint: string } {
  const writer = new CanonicalWriter(limits.maxInputEventBytes);
  canonicalVisit(value, writer, 0, limits.maxJsonDepth, new Set());
  const canonical = writer.finish();
  return Object.freeze({
    canonical,
    fingerprint: createHash("sha256").update(canonical, "utf8").digest("hex"),
  });
}

function presentationValue(
  value: unknown,
  limits: LiveTimelineLimits,
  depth: number,
  state: { truncated: boolean },
): unknown {
  if (depth > limits.maxJsonDepth) {
    state.truncated = true;
    return "[depth limit]";
  }
  if (typeof value === "string") {
    const text = boundedText(value, limits.maxTextBytes);
    if (text.truncated) state.truncated = true;
    return text.text;
  }
  if (value === null || typeof value === "boolean" || typeof value === "number") return value;
  if (Array.isArray(value)) {
    const count = Math.min(value.length, 128);
    if (count < value.length) state.truncated = true;
    return value
      .slice(0, count)
      .map((child) => presentationValue(child, limits, depth + 1, state));
  }
  if (typeof value === "object") {
    const entries = Object.entries(value).sort(([left], [right]) =>
      left < right ? -1 : left > right ? 1 : 0,
    );
    const count = Math.min(entries.length, 128);
    if (count < entries.length) state.truncated = true;
    const pairs: Array<readonly [string, unknown]> = [];
    const seen = new Set<string>();
    let collision = false;
    for (const [rawKey, child] of entries.slice(0, count)) {
      const labelled = DANGEROUS_KEYS.has(rawKey) ? `[unsafe-key:${rawKey}]` : rawKey;
      const boundedKey = boundedText(labelled, limits.maxTextBytes);
      if (boundedKey.truncated) state.truncated = true;
      if (seen.has(boundedKey.text)) collision = true;
      seen.add(boundedKey.text);
      pairs.push([
        boundedKey.text,
        presentationValue(child, limits, depth + 1, state),
      ] as const);
    }
    if (collision) {
      state.truncated = true;
      const collisionSafe: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
      collisionSafe["[collision-safe object entries]"] = pairs;
      return collisionSafe;
    }
    const result: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    for (const [key, child] of pairs) result[key] = child;
    return result;
  }
  state.truncated = true;
  return "[non-JSON value]";
}

export function boundedJson(value: unknown, limits: LiveTimelineLimits): BoundedJson {
  const state = { truncated: false };
  const safe = presentationValue(value, limits, 0, state);
  const json = jsonEncode(safe);
  if (Buffer.byteLength(json) <= limits.maxJsonBytes) {
    return Object.freeze({ json, truncated: state.truncated });
  }
  return Object.freeze({
    json: jsonEncode(`[JSON exceeds ${limits.maxJsonBytes}-byte presentation limit]`),
    truncated: true,
  });
}

export function deepFreeze<T>(value: T): T {
  if (value !== null && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const child of Object.values(value)) deepFreeze(child);
  }
  return value;
}
