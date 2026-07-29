import { randomUUID } from "node:crypto";
import { constants, type Stats } from "node:fs";
import type { FileHandle } from "node:fs/promises";
import { link, lstat, open, unlink } from "node:fs/promises";
import { basename, dirname, join } from "node:path";

import type {
  ProtocolVersion,
  SessionExport,
  SessionInfo,
  TestEvent,
} from "@devicerail/protocol";

import {
  CanonicalJsonError,
  fromCanonicalJson,
  toCanonicalJson,
} from "./canonical.js";
import { validateRecorderCheckpoint } from "./checkpoint.js";
import { RecorderError } from "./errors.js";
import {
  RECORDER_CHECKPOINT_FORMAT,
  RECORDER_CHECKPOINT_VERSION,
} from "./types.js";

export const BUNDLE_SOURCE_MAX_BYTES = 8 * 1024 * 1024;

const SOURCE_KEYS = ["eventProtocolVersion", "sessionExport"] as const;
const EXPORT_KEYS = ["events", "session"] as const;

/** Strict local input consumed by the Rust Session Bundle CLI. */
export interface BundleSourceFile {
  readonly eventProtocolVersion: ProtocolVersion;
  readonly sessionExport: SessionExport;
}

export interface BundleSourceFileOptions {
  readonly maxBytes?: number;
  readonly signal?: AbortSignal;
}

function isNodeError(error: unknown, code: string): boolean {
  return error instanceof Error && "code" in error && error.code === code;
}

function record(value: unknown, location: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new RecorderError("source_corrupt", `${location} must be an object`);
  }
  return value as Record<string, unknown>;
}

function exactKeys(
  value: Record<string, unknown>,
  required: readonly string[],
  location: string,
): void {
  const allowed = new Set(required);
  for (const key of required) {
    if (!Object.hasOwn(value, key)) {
      throw new RecorderError("source_corrupt", `${location} is missing ${key}`);
    }
  }
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) {
      throw new RecorderError("source_corrupt", `${location} contains unknown field ${key}`);
    }
  }
}

function byteLimit(options: BundleSourceFileOptions): number {
  const limit = options.maxBytes ?? BUNDLE_SOURCE_MAX_BYTES;
  if (!Number.isSafeInteger(limit) || limit <= 0) {
    throw new RecorderError("source_too_large", "source maxBytes must be a positive safe integer");
  }
  if (limit > BUNDLE_SOURCE_MAX_BYTES) {
    throw new RecorderError(
      "source_too_large",
      `source maxBytes cannot exceed ${BUNDLE_SOURCE_MAX_BYTES}`,
    );
  }
  return limit;
}

function requireNotCancelled(signal: AbortSignal | undefined): void {
  if (signal?.aborted) {
    throw new RecorderError("operation_cancelled", "BundleSource publication was cancelled", {
      details: { reason: signal.reason instanceof Error ? signal.reason.name : "aborted" },
    });
  }
}

/** Strictly validate and detach one ended BundleSource. */
export function validateBundleSourceFile(value: unknown): BundleSourceFile {
  try {
    const source = record(value, "BundleSource");
    exactKeys(source, SOURCE_KEYS, "BundleSource");
    const exported = record(source.sessionExport, "BundleSource.sessionExport");
    exactKeys(exported, EXPORT_KEYS, "BundleSource.sessionExport");
    const sessionCandidate = record(exported.session, "BundleSource.sessionExport.session");
    if (typeof sessionCandidate.id !== "string") {
      throw new RecorderError("source_corrupt", "BundleSource Session id must be a string");
    }
    if (!Array.isArray(exported.events)) {
      throw new RecorderError("source_corrupt", "BundleSource events must be an array");
    }

    const checkpoint = validateRecorderCheckpoint({
      format: RECORDER_CHECKPOINT_FORMAT,
      version: RECORDER_CHECKPOINT_VERSION,
      revision: 1,
      phase: "sealed",
      sessionId: sessionCandidate.id,
      eventProtocolVersion: source.eventProtocolVersion,
      events: exported.events,
      session: exported.session,
    });
    if (checkpoint.phase !== "sealed") {
      throw new RecorderError("source_corrupt", "BundleSource did not produce a sealed Session");
    }
    return {
      eventProtocolVersion: checkpoint.eventProtocolVersion,
      sessionExport: {
        session: checkpoint.session,
        events: [...checkpoint.events],
      },
    };
  } catch (cause) {
    if (cause instanceof RecorderError && cause.code === "source_corrupt") {
      throw cause;
    }
    throw new RecorderError("source_corrupt", "BundleSource is invalid", { cause });
  }
}

/** Validate that a strict BundleSource also fits its publication contract. */
export function validateBundleSourceBounds(value: unknown): BundleSourceFile {
  const normalized = validateBundleSourceFile(value);
  try {
    toCanonicalJson(normalized, { maxBytes: BUNDLE_SOURCE_MAX_BYTES });
  } catch (cause) {
    if (cause instanceof CanonicalJsonError) {
      throw new RecorderError(
        "source_too_large",
        `BundleSource exceeds its ${BUNDLE_SOURCE_MAX_BYTES}-byte limit`,
        { cause },
      );
    }
    throw cause;
  }
  return normalized;
}

function requireOwnerOnly(metadata: Stats): void {
  if (process.platform === "win32") {
    return;
  }
  if ((metadata.mode & 0o077) !== 0) {
    throw new RecorderError("source_corrupt", "BundleSource must be owner-only");
  }
  const effectiveUser = process.geteuid?.();
  if (effectiveUser !== undefined && metadata.uid !== effectiveUser) {
    throw new RecorderError("source_corrupt", "BundleSource must be owned by this user");
  }
}

async function requireRealParent(path: string): Promise<string> {
  const parent = dirname(path);
  const name = basename(path);
  if (name.length === 0 || name === "." || name === "..") {
    throw new RecorderError("source_corrupt", "BundleSource path has no safe file name");
  }
  let metadata;
  try {
    metadata = await lstat(parent);
  } catch (cause) {
    throw new RecorderError("source_corrupt", "BundleSource parent does not exist", { cause });
  }
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new RecorderError("source_corrupt", "BundleSource parent must be a real directory");
  }
  return parent;
}

async function readBoundedSource(path: string, limit: number): Promise<Buffer> {
  let pathMetadata;
  try {
    pathMetadata = await lstat(path);
  } catch (cause) {
    throw new RecorderError("source_corrupt", "BundleSource metadata could not be read", {
      cause,
    });
  }
  if (!pathMetadata.isFile() || pathMetadata.isSymbolicLink()) {
    throw new RecorderError("source_corrupt", "BundleSource must be a regular file");
  }
  requireOwnerOnly(pathMetadata);
  if (pathMetadata.size > limit) {
    throw new RecorderError("source_too_large", `BundleSource exceeds its ${limit}-byte limit`);
  }

  let handle: FileHandle;
  try {
    handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  } catch (cause) {
    throw new RecorderError("source_corrupt", "BundleSource could not be opened safely", {
      cause,
    });
  }
  try {
    const opened = await handle.stat();
    if (
      !opened.isFile() ||
      opened.dev !== pathMetadata.dev ||
      opened.ino !== pathMetadata.ino
    ) {
      throw new RecorderError("source_corrupt", "BundleSource changed while it was opened");
    }
    requireOwnerOnly(opened);
    const bytes = Buffer.allocUnsafe(limit + 1);
    let offset = 0;
    while (offset < bytes.length) {
      const { bytesRead } = await handle.read(bytes, offset, bytes.length - offset, null);
      if (bytesRead === 0) {
        break;
      }
      offset += bytesRead;
    }
    if (offset > limit) {
      throw new RecorderError("source_too_large", `BundleSource exceeds its ${limit}-byte limit`);
    }
    return bytes.subarray(0, offset);
  } finally {
    await handle.close().catch(() => {});
  }
}

async function syncParent(parent: string): Promise<void> {
  // Node does not expose a portable Windows directory fsync. Publication is
  // still no-clobber atomic, but power-loss durability is filesystem-defined.
  if (process.platform === "win32") {
    return;
  }
  const handle = await open(parent, "r");
  try {
    await handle.sync();
  } finally {
    await handle.close().catch(() => {});
  }
}

/** Read a strict canonical BundleSource; Unix additionally enforces owner-only metadata. */
export async function readBundleSource(
  path: string,
  options: BundleSourceFileOptions = {},
): Promise<BundleSourceFile> {
  const limit = byteLimit(options);
  const bytes = await readBoundedSource(path, limit);
  try {
    return validateBundleSourceFile(fromCanonicalJson(bytes, { maxBytes: limit }));
  } catch (cause) {
    if (cause instanceof RecorderError && cause.code === "source_too_large") {
      throw cause;
    }
    throw new RecorderError("source_corrupt", "BundleSource is not strict canonical JSON", {
      cause,
    });
  }
}

/**
 * Atomically publish a BundleSource without replacing any existing path.
 * Unix creates it owner-only; Windows relies on the caller's parent ACL. The
 * hard-link operation is the publication linearization point.
 */
export async function publishBundleSource(
  path: string,
  source: BundleSourceFile,
  options: BundleSourceFileOptions = {},
): Promise<void> {
  const limit = byteLimit(options);
  const normalized = validateBundleSourceFile(source);
  let bytes: Buffer;
  try {
    bytes = toCanonicalJson(normalized, { maxBytes: limit });
  } catch (cause) {
    if (cause instanceof CanonicalJsonError) {
      throw new RecorderError("source_too_large", "BundleSource exceeds its encoding limit", {
        cause,
      });
    }
    throw cause;
  }
  const parent = await requireRealParent(path);
  const temporary = join(parent, `.${basename(path)}.${randomUUID()}.tmp`);
  let published = false;
  let operationError: unknown;

  try {
    requireNotCancelled(options.signal);
    const handle = await open(temporary, "wx", 0o600);
    try {
      await handle.writeFile(bytes);
      await handle.sync();
    } finally {
      await handle.close().catch(() => {});
    }
    requireNotCancelled(options.signal);
    try {
      await link(temporary, path);
    } catch (cause) {
      if (isNodeError(cause, "EEXIST")) {
        throw new RecorderError("source_conflict", "BundleSource target already exists");
      }
      throw cause;
    }
    published = true;
    await unlink(temporary);
    await syncParent(parent);
  } catch (cause) {
    operationError =
      cause instanceof RecorderError
        ? cause
        : new RecorderError(
            published ? "source_durability_unknown" : "source_corrupt",
            published
              ? "BundleSource was published but final durability is unknown"
              : "BundleSource publication failed before its linearization point",
            {
              cause,
              ...(published ? { details: { published: true } } : {}),
            },
          );
  }
  if (!published) {
    await unlink(temporary).catch(() => {});
  }
  if (operationError !== undefined) {
    throw operationError;
  }
}

/** Construct the exact local source expected by the Bundle CLI. */
export function bundleSourceFromEndedSession(
  eventProtocolVersion: ProtocolVersion,
  session: SessionInfo,
  events: readonly TestEvent[],
): BundleSourceFile {
  return validateBundleSourceFile({
    eventProtocolVersion,
    sessionExport: { session, events },
  });
}
