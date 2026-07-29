import { createHash, timingSafeEqual } from "node:crypto";
import { TextDecoder } from "node:util";

export const DEFAULT_CANONICAL_JSON_MAX_BYTES = 1024 * 1024 + 4 * 1024;

const DEFAULT_MAX_DEPTH = 128;
const DEFAULT_MAX_NODES = 1_000_000;
const SHA256_HEX = /^[0-9a-f]{64}$/u;

export interface CanonicalJsonOptions {
  readonly maxBytes?: number;
  readonly maxDepth?: number;
  readonly maxNodes?: number;
}

export class CanonicalJsonError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = new.target.name;
  }
}

interface EncodeState {
  readonly ancestors: Set<object>;
  readonly maxDepth: number;
  readonly maxNodes: number;
  nodes: number;
}

function positiveSafeInteger(value: number | undefined, fallback: number, name: string): number {
  const selected = value ?? fallback;
  if (!Number.isSafeInteger(selected) || selected <= 0) {
    throw new CanonicalJsonError(`${name} must be a positive safe integer`);
  }
  return selected;
}

function encode(value: unknown, depth: number, state: EncodeState): string {
  if (depth > state.maxDepth) {
    throw new CanonicalJsonError("canonical JSON exceeds its nesting-depth limit");
  }
  state.nodes += 1;
  if (state.nodes > state.maxNodes) {
    throw new CanonicalJsonError("canonical JSON exceeds its node-count limit");
  }

  if (value === null) {
    return "null";
  }
  switch (typeof value) {
    case "boolean":
      return value ? "true" : "false";
    case "string":
      return JSON.stringify(value);
    case "number":
      if (!Number.isFinite(value)) {
        throw new CanonicalJsonError("canonical JSON contains a non-finite number");
      }
      if (Number.isInteger(value) && !Number.isSafeInteger(value)) {
        throw new CanonicalJsonError("canonical JSON contains an unsafe integer");
      }
      // JSON.stringify normalizes -0 to 0, which would change an already
      // confirmed arbitrary JSON value across checkpoint recovery.
      return Object.is(value, -0) ? "-0" : JSON.stringify(value);
    case "object":
      break;
    default:
      throw new CanonicalJsonError(`canonical JSON cannot encode ${typeof value}`);
  }

  if (state.ancestors.has(value)) {
    throw new CanonicalJsonError("canonical JSON cannot encode a cyclic value");
  }
  state.ancestors.add(value);
  try {
    if (Array.isArray(value)) {
      const items: string[] = [];
      for (let index = 0; index < value.length; index += 1) {
        if (!Object.hasOwn(value, index)) {
          throw new CanonicalJsonError("canonical JSON cannot encode a sparse array");
        }
        items.push(encode(value[index], depth + 1, state));
      }
      return `[${items.join(",")}]`;
    }

    const prototype = Object.getPrototypeOf(value) as object | null;
    if (prototype !== Object.prototype && prototype !== null) {
      throw new CanonicalJsonError("canonical JSON objects must have a plain prototype");
    }
    const keys = Reflect.ownKeys(value);
    if (keys.some((key) => typeof key !== "string")) {
      throw new CanonicalJsonError("canonical JSON objects cannot contain symbol keys");
    }
    const stringKeys = keys as string[];
    for (const key of stringKeys) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor?.enumerable || !("value" in descriptor)) {
        throw new CanonicalJsonError(
          "canonical JSON objects must contain enumerable data properties only",
        );
      }
    }
    stringKeys.sort((left, right) => (left < right ? -1 : left > right ? 1 : 0));
    const entries = stringKeys.map((key) => {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor || !("value" in descriptor)) {
        throw new CanonicalJsonError("canonical JSON object changed while it was encoded");
      }
      return `${JSON.stringify(key)}:${encode(descriptor.value, depth + 1, state)}`;
    });
    return `{${entries.join(",")}}`;
  } finally {
    state.ancestors.delete(value);
  }
}

/** Encode one value as compact recursively key-sorted UTF-8 JSON plus one LF. */
export function toCanonicalJson(value: unknown, options: CanonicalJsonOptions = {}): Buffer {
  const maxBytes = positiveSafeInteger(
    options.maxBytes,
    DEFAULT_CANONICAL_JSON_MAX_BYTES,
    "maxBytes",
  );
  const state: EncodeState = {
    ancestors: new Set(),
    maxDepth: positiveSafeInteger(options.maxDepth, DEFAULT_MAX_DEPTH, "maxDepth"),
    maxNodes: positiveSafeInteger(options.maxNodes, DEFAULT_MAX_NODES, "maxNodes"),
    nodes: 0,
  };
  const bytes = Buffer.from(`${encode(value, 0, state)}\n`, "utf8");
  if (bytes.length > maxBytes) {
    throw new CanonicalJsonError(`canonical JSON exceeds its ${maxBytes}-byte limit`);
  }
  return bytes;
}

/**
 * Encode `{ checkpoint, sha256 }` while visiting the checkpoint value once.
 * The checksum covers the checkpoint's standalone canonical bytes, including
 * its LF, while depth/node limits still apply to the complete envelope.
 */
export function toCanonicalJsonChecksumEnvelope(
  checkpoint: unknown,
  options: CanonicalJsonOptions = {},
): Buffer {
  const maxBytes = positiveSafeInteger(
    options.maxBytes,
    DEFAULT_CANONICAL_JSON_MAX_BYTES,
    "maxBytes",
  );
  const state: EncodeState = {
    ancestors: new Set(),
    maxDepth: positiveSafeInteger(options.maxDepth, DEFAULT_MAX_DEPTH, "maxDepth"),
    maxNodes: positiveSafeInteger(options.maxNodes, DEFAULT_MAX_NODES, "maxNodes"),
    nodes: 1,
  };
  const checkpointJson = encode(checkpoint, 1, state);
  const payloadBytes = Buffer.from(`${checkpointJson}\n`, "utf8");
  const checksumJson = encode(sha256Hex(payloadBytes), 1, state);
  const bytes = Buffer.from(
    `{"checkpoint":${checkpointJson},"sha256":${checksumJson}}\n`,
    "utf8",
  );
  if (bytes.length > maxBytes) {
    throw new CanonicalJsonError(`canonical JSON exceeds its ${maxBytes}-byte limit`);
  }
  return bytes;
}

/** Parse JSON only when its bytes are already in the canonical representation. */
export function fromCanonicalJson(
  bytes: Uint8Array,
  options: CanonicalJsonOptions = {},
): unknown {
  const maxBytes = positiveSafeInteger(
    options.maxBytes,
    DEFAULT_CANONICAL_JSON_MAX_BYTES,
    "maxBytes",
  );
  if (bytes.byteLength === 0 || bytes.byteLength > maxBytes) {
    throw new CanonicalJsonError(`canonical JSON must contain 1-${maxBytes} bytes`);
  }

  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch (cause) {
    throw new CanonicalJsonError("canonical JSON is not valid UTF-8", { cause });
  }
  if (!text.endsWith("\n") || text.endsWith("\n\n")) {
    throw new CanonicalJsonError("canonical JSON must end in exactly one LF");
  }

  let value: unknown;
  try {
    value = JSON.parse(text.slice(0, -1)) as unknown;
  } catch (cause) {
    throw new CanonicalJsonError("canonical JSON is malformed or truncated", { cause });
  }
  const canonical = toCanonicalJson(value, options);
  if (!canonical.equals(Buffer.from(bytes))) {
    throw new CanonicalJsonError("JSON bytes are not canonical");
  }
  return value;
}

export function sha256Hex(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

export function canonicalSha256(value: unknown, options: CanonicalJsonOptions = {}): string {
  return sha256Hex(toCanonicalJson(value, options));
}

export function sha256Matches(actualBytes: Uint8Array, expectedHex: string): boolean {
  if (!SHA256_HEX.test(expectedHex)) {
    return false;
  }
  const actual = createHash("sha256").update(actualBytes).digest();
  const expected = Buffer.from(expectedHex, "hex");
  return timingSafeEqual(actual, expected);
}
