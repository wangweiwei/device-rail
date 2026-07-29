import { randomUUID } from "node:crypto";
import { constants, type Stats } from "node:fs";
import type { FileHandle } from "node:fs/promises";
import { link, lstat, open, rename, unlink } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import { isDeepStrictEqual } from "node:util";

import type { ProtocolVersion, SessionInfo, TestEvent } from "@devicerail/protocol";

import {
  fromCanonicalJson,
  sha256Hex,
  sha256Matches,
  toCanonicalJson,
  toCanonicalJsonChecksumEnvelope,
} from "./canonical.js";
import { RecorderError } from "./errors.js";
import { EventLog } from "./event-log.js";
import {
  RECORDER_CHECKPOINT_FORMAT,
  RECORDER_CHECKPOINT_VERSION,
  type RecorderBundleReceipt,
  type RecorderCheckpoint,
  type RecorderPhase,
  type RecordingCheckpoint,
} from "./types.js";

export type { RecorderCheckpoint } from "./types.js";

/** Fixed metadata/checksum room above the complete 8 MiB BundleSource. */
export const RECORDER_CHECKPOINT_HEADROOM_BYTES = 64 * 1024;
export const RECORDER_CHECKPOINT_MAX_BYTES =
  8 * 1024 * 1024 + RECORDER_CHECKPOINT_HEADROOM_BYTES;

const CHECKPOINT_ENVELOPE_KEYS = ["checkpoint", "sha256"] as const;
const CHECKPOINT_BASE_KEYS = [
  "eventProtocolVersion",
  "events",
  "format",
  "phase",
  "revision",
  "sessionId",
  "version",
] as const;
const SESSION_KEYS = [
  "endedAtMs",
  "eventCount",
  "id",
  "lastSequence",
  "startedAtMs",
  "state",
] as const;
const BUNDLE_KEYS = ["assetBytes", "assetCount", "eventCount", "sessionId"] as const;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu;
const LOCK_MAX_BYTES = 1024;
const JOURNAL_HEAD_MAX_BYTES = 4096;
const JOURNAL_FORMAT = "devicerail.execution-recorder-journal" as const;
const JOURNAL_VERSION = 1 as const;
const JOURNAL_SIZE_MULTIPLIER = 4;

interface JournalHead {
  readonly format: typeof JOURNAL_FORMAT;
  readonly version: typeof JOURNAL_VERSION;
  readonly baseRevision: number;
  readonly revision: number;
  readonly sessionId: string;
  readonly eventProtocolVersion: ProtocolVersion;
  readonly committedBytes: number;
  readonly lastSegmentOffset: number;
  readonly lastSegmentLength: number;
  readonly lastSha256: string;
  readonly eventCount: number;
  readonly eventBytes: number;
}

interface JournalSegment {
  readonly format: typeof JOURNAL_FORMAT;
  readonly version: typeof JOURNAL_VERSION;
  readonly revision: number;
  readonly previousSha256: string | null;
  readonly firstSequence: number;
  readonly eventCount: number;
  readonly eventBytes: number;
  readonly events: readonly TestEvent[];
}

export interface RecorderJournalAppendResult {
  readonly revision: number;
  readonly eventCount: number;
}

export interface RecorderCheckpointFileOptions {
  readonly maxBytes?: number;
  readonly signal?: AbortSignal;
}

interface CheckpointLock {
  readonly path: string;
  readonly token: string;
}

interface CheckpointLockRecord {
  readonly pid: number;
  readonly token: string;
}

function isNodeError(error: unknown, code: string): boolean {
  return error instanceof Error && "code" in error && error.code === code;
}

async function pathEntryExists(path: string): Promise<boolean> {
  try {
    await lstat(path);
    return true;
  } catch (cause) {
    if (isNodeError(cause, "ENOENT")) {
      return false;
    }
    throw new RecorderError("checkpoint_corrupt", "checkpoint sidecar metadata could not be read", {
      cause,
    });
  }
}

function record(value: unknown, location: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new RecorderError("checkpoint_corrupt", `${location} must be an object`);
  }
  return value as Record<string, unknown>;
}

function exactKeys(
  value: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[],
  location: string,
): void {
  const allowed = new Set([...required, ...optional]);
  for (const key of required) {
    if (!Object.hasOwn(value, key)) {
      throw new RecorderError("checkpoint_corrupt", `${location} is missing ${key}`);
    }
  }
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) {
      throw new RecorderError("checkpoint_corrupt", `${location} contains unknown field ${key}`);
    }
  }
}

function safeInteger(value: unknown, location: string, minimum = 0): number {
  if (!Number.isSafeInteger(value) || Object.is(value, -0) || (value as number) < minimum) {
    throw new RecorderError(
      "checkpoint_corrupt",
      `${location} must be a safe integer greater than or equal to ${minimum}`,
    );
  }
  return value as number;
}

function maxBytes(options: RecorderCheckpointFileOptions): number {
  const limit = safeInteger(
    options.maxBytes ?? RECORDER_CHECKPOINT_MAX_BYTES,
    "maxBytes",
    1,
  );
  if (limit > RECORDER_CHECKPOINT_MAX_BYTES) {
    throw new RecorderError(
      "checkpoint_corrupt",
      `maxBytes cannot exceed ${RECORDER_CHECKPOINT_MAX_BYTES}`,
    );
  }
  return limit;
}

function parseProtocolVersion(value: unknown): ProtocolVersion {
  const candidate = record(value, "checkpoint.eventProtocolVersion");
  exactKeys(candidate, ["major", "minor"], [], "checkpoint.eventProtocolVersion");
  const major = safeInteger(candidate.major, "checkpoint.eventProtocolVersion.major");
  const minor = safeInteger(candidate.minor, "checkpoint.eventProtocolVersion.minor");
  if (major > 65_535 || minor > 65_535 || major !== 1 || minor > 5) {
    throw new RecorderError(
      "checkpoint_corrupt",
      "checkpoint event protocol version is unsupported",
    );
  }
  return { major, minor };
}

function parseSession(
  value: unknown,
  sessionId: string,
  events: readonly TestEvent[],
): SessionInfo {
  const candidate = record(value, "checkpoint.session");
  exactKeys(candidate, SESSION_KEYS, [], "checkpoint.session");
  if (candidate.id !== sessionId || candidate.state !== "ended") {
    throw new RecorderError(
      "checkpoint_corrupt",
      "checkpoint Session identity or state is inconsistent",
    );
  }
  const startedAtMs = safeInteger(candidate.startedAtMs, "checkpoint.session.startedAtMs");
  const endedAtMs = safeInteger(candidate.endedAtMs, "checkpoint.session.endedAtMs");
  const eventCount = safeInteger(candidate.eventCount, "checkpoint.session.eventCount", 1);
  const lastSequence = safeInteger(
    candidate.lastSequence,
    "checkpoint.session.lastSequence",
    1,
  );
  if (
    events.length === 0 ||
    eventCount !== events.length ||
    lastSequence !== events.length ||
    events[0]?.atMs !== startedAtMs ||
    events.at(-1)?.atMs !== endedAtMs
  ) {
    throw new RecorderError(
      "checkpoint_corrupt",
      "checkpoint ended Session metadata does not match its events",
    );
  }
  return {
    id: sessionId,
    state: "ended",
    startedAtMs,
    endedAtMs,
    eventCount,
    lastSequence,
  };
}

function parseBundle(
  value: unknown,
  sessionId: string,
  events: readonly TestEvent[],
): RecorderBundleReceipt {
  const candidate = record(value, "checkpoint.bundle");
  exactKeys(candidate, BUNDLE_KEYS, [], "checkpoint.bundle");
  const eventCount = safeInteger(candidate.eventCount, "checkpoint.bundle.eventCount", 1);
  if (candidate.sessionId !== sessionId || eventCount !== events.length) {
    throw new RecorderError(
      "checkpoint_corrupt",
      "checkpoint Bundle receipt does not match its Session events",
    );
  }
  return {
    sessionId,
    eventCount,
    assetCount: safeInteger(candidate.assetCount, "checkpoint.bundle.assetCount"),
    assetBytes: safeInteger(candidate.assetBytes, "checkpoint.bundle.assetBytes"),
  };
}

/** Strictly validate and detach one in-memory checkpoint. */
export function validateRecorderCheckpoint(value: unknown): RecorderCheckpoint {
  const candidate = record(value, "checkpoint");
  const phase = candidate.phase;
  if (phase !== "recording" && phase !== "sealed" && phase !== "completed") {
    throw new RecorderError("checkpoint_corrupt", "checkpoint phase is invalid");
  }
  const optional = phase === "recording" ? [] : phase === "sealed" ? ["session"] : ["bundle", "session"];
  exactKeys(candidate, CHECKPOINT_BASE_KEYS, optional, "checkpoint");
  if (
    candidate.format !== RECORDER_CHECKPOINT_FORMAT ||
    candidate.version !== RECORDER_CHECKPOINT_VERSION
  ) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint format or version is unsupported");
  }
  const revision = safeInteger(candidate.revision, "checkpoint.revision", 1);
  if (typeof candidate.sessionId !== "string" || !UUID.test(candidate.sessionId)) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint sessionId must be a UUID");
  }
  if (!Array.isArray(candidate.events)) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint events must be an array");
  }
  const protocol = parseProtocolVersion(candidate.eventProtocolVersion);
  const eventLog = EventLog.replay(candidate.sessionId, candidate.events, protocol);
  const events = eventLog.events;

  const base = {
    format: RECORDER_CHECKPOINT_FORMAT,
    version: RECORDER_CHECKPOINT_VERSION,
    revision,
    sessionId: candidate.sessionId,
    eventProtocolVersion: protocol,
    events,
  } as const;
  if (phase === "recording") {
    return { ...base, phase };
  }
  if (!eventLog.terminal || eventLog.openActionCount !== 0) {
    throw new RecorderError(
      "checkpoint_corrupt",
      "sealed checkpoint must contain one closed terminal Session",
    );
  }
  const session = parseSession(candidate.session, candidate.sessionId, events);
  if (phase === "sealed") {
    return { ...base, phase, session };
  }
  return {
    ...base,
    phase,
    session,
    bundle: parseBundle(candidate.bundle, candidate.sessionId, events),
  };
}

function parseEnvelope(bytes: Uint8Array, limit: number): RecorderCheckpoint {
  let value: unknown;
  try {
    value = fromCanonicalJson(bytes, { maxBytes: limit });
  } catch (cause) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint JSON is not canonical", {
      cause,
    });
  }
  const envelope = record(value, "checkpoint envelope");
  exactKeys(envelope, CHECKPOINT_ENVELOPE_KEYS, [], "checkpoint envelope");
  if (typeof envelope.sha256 !== "string") {
    throw new RecorderError("checkpoint_corrupt", "checkpoint checksum is invalid");
  }
  let payloadBytes: Buffer;
  try {
    payloadBytes = toCanonicalJson(envelope.checkpoint, { maxBytes: limit });
  } catch (cause) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint payload is invalid", { cause });
  }
  if (!sha256Matches(payloadBytes, envelope.sha256)) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint checksum does not match");
  }
  return validateRecorderCheckpoint(envelope.checkpoint);
}

function serializeNormalizedEnvelope(normalized: RecorderCheckpoint, limit: number): Buffer {
  try {
    return toCanonicalJsonChecksumEnvelope(normalized, { maxBytes: limit });
  } catch (cause) {
    if (cause instanceof RecorderError) {
      throw cause;
    }
    throw new RecorderError("checkpoint_corrupt", "checkpoint exceeds its encoding limits", {
      cause,
    });
  }
}

function parseChecksummedValue(
  bytes: Uint8Array,
  limit: number,
  location: string,
): { readonly value: unknown; readonly sha256: string } {
  let parsed: unknown;
  try {
    parsed = fromCanonicalJson(bytes, { maxBytes: limit });
  } catch (cause) {
    throw new RecorderError("checkpoint_corrupt", `${location} JSON is not canonical`, { cause });
  }
  const envelope = record(parsed, `${location} envelope`);
  exactKeys(envelope, CHECKPOINT_ENVELOPE_KEYS, [], `${location} envelope`);
  if (typeof envelope.sha256 !== "string" || !/^[0-9a-f]{64}$/u.test(envelope.sha256)) {
    throw new RecorderError("checkpoint_corrupt", `${location} checksum is invalid`);
  }
  let payloadBytes: Buffer;
  try {
    payloadBytes = toCanonicalJson(envelope.checkpoint, { maxBytes: limit });
  } catch (cause) {
    throw new RecorderError("checkpoint_corrupt", `${location} payload is invalid`, { cause });
  }
  if (!sha256Matches(payloadBytes, envelope.sha256)) {
    throw new RecorderError("checkpoint_corrupt", `${location} checksum does not match`);
  }
  return { value: envelope.checkpoint, sha256: envelope.sha256 };
}

function journalHeadPath(path: string): string {
  return `${path}.journal-head`;
}

function journalPath(path: string): string {
  return `${path}.journal`;
}

function parseJournalHead(bytes: Uint8Array): JournalHead {
  const { value } = parseChecksummedValue(bytes, JOURNAL_HEAD_MAX_BYTES, "checkpoint journal head");
  const candidate = record(value, "checkpoint journal head");
  exactKeys(
    candidate,
    [
      "baseRevision",
      "committedBytes",
      "eventBytes",
      "eventCount",
      "eventProtocolVersion",
      "format",
      "lastSegmentLength",
      "lastSegmentOffset",
      "lastSha256",
      "revision",
      "sessionId",
      "version",
    ],
    [],
    "checkpoint journal head",
  );
  if (
    candidate.format !== JOURNAL_FORMAT
    || candidate.version !== JOURNAL_VERSION
    || typeof candidate.sessionId !== "string"
    || !UUID.test(candidate.sessionId)
    || typeof candidate.lastSha256 !== "string"
    || !/^[0-9a-f]{64}$/u.test(candidate.lastSha256)
  ) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint journal head identity is invalid");
  }
  const committedBytes = safeInteger(candidate.committedBytes, "journal committedBytes", 1);
  const lastSegmentOffset = safeInteger(candidate.lastSegmentOffset, "journal lastSegmentOffset");
  const lastSegmentLength = safeInteger(candidate.lastSegmentLength, "journal lastSegmentLength", 1);
  if (lastSegmentOffset + lastSegmentLength !== committedBytes) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint journal head offsets are inconsistent");
  }
  return {
    format: JOURNAL_FORMAT,
    version: JOURNAL_VERSION,
    baseRevision: safeInteger(candidate.baseRevision, "journal baseRevision", 1),
    revision: safeInteger(candidate.revision, "journal revision", 2),
    sessionId: candidate.sessionId,
    eventProtocolVersion: parseProtocolVersion(candidate.eventProtocolVersion),
    committedBytes,
    lastSegmentOffset,
    lastSegmentLength,
    lastSha256: candidate.lastSha256,
    eventCount: safeInteger(candidate.eventCount, "journal eventCount", 1),
    eventBytes: safeInteger(candidate.eventBytes, "journal eventBytes", 1),
  };
}

function parseJournalSegment(bytes: Uint8Array, limit: number): {
  readonly segment: JournalSegment;
  readonly sha256: string;
} {
  const { value, sha256 } = parseChecksummedValue(bytes, limit, "checkpoint journal segment");
  const candidate = record(value, "checkpoint journal segment");
  exactKeys(
    candidate,
    [
      "eventBytes",
      "eventCount",
      "events",
      "firstSequence",
      "format",
      "previousSha256",
      "revision",
      "version",
    ],
    [],
    "checkpoint journal segment",
  );
  if (
    candidate.format !== JOURNAL_FORMAT
    || candidate.version !== JOURNAL_VERSION
    || (candidate.previousSha256 !== null
      && (typeof candidate.previousSha256 !== "string"
        || !/^[0-9a-f]{64}$/u.test(candidate.previousSha256)))
    || !Array.isArray(candidate.events)
    || candidate.events.length === 0
  ) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint journal segment is invalid");
  }
  return {
    segment: {
      format: JOURNAL_FORMAT,
      version: JOURNAL_VERSION,
      revision: safeInteger(candidate.revision, "journal segment revision", 2),
      previousSha256: candidate.previousSha256,
      firstSequence: safeInteger(candidate.firstSequence, "journal firstSequence", 1),
      eventCount: safeInteger(candidate.eventCount, "journal eventCount", 1),
      eventBytes: safeInteger(candidate.eventBytes, "journal eventBytes", 1),
      events: candidate.events as readonly TestEvent[],
    },
    sha256,
  };
}

function encodedEventsBytes(events: readonly TestEvent[]): number {
  let total = 0;
  for (const event of events) {
    total += toCanonicalJson(event, { maxBytes: RECORDER_CHECKPOINT_MAX_BYTES }).length - 1;
  }
  return total;
}

function requireNotCancelled(signal: AbortSignal | undefined): void {
  if (signal?.aborted) {
    throw new RecorderError("operation_cancelled", "checkpoint operation was cancelled", {
      details: { reason: signal.reason instanceof Error ? signal.reason.name : "aborted" },
    });
  }
}

async function requireRealParent(path: string): Promise<string> {
  const parent = dirname(path);
  const name = basename(path);
  if (name.length === 0 || name === "." || name === "..") {
    throw new RecorderError("checkpoint_corrupt", "checkpoint path has no safe file name");
  }
  let metadata;
  try {
    metadata = await lstat(parent);
  } catch (cause) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint parent does not exist", { cause });
  }
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint parent must be a real directory");
  }
  return parent;
}

function requireOwnerOnly(metadata: Stats): void {
  if (process.platform === "win32") {
    return;
  }
  if ((metadata.mode & 0o077) !== 0) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint file must be owner-only");
  }
  const effectiveUser = process.geteuid?.();
  if (effectiveUser !== undefined && metadata.uid !== effectiveUser) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint file must be owned by this user");
  }
}

async function readBoundedPrivateFile(path: string, limit: number): Promise<Buffer | null> {
  let pathMetadata;
  try {
    pathMetadata = await lstat(path);
  } catch (cause) {
    if (isNodeError(cause, "ENOENT")) {
      return null;
    }
    throw new RecorderError("checkpoint_corrupt", "checkpoint metadata could not be read", {
      cause,
    });
  }
  if (!pathMetadata.isFile() || pathMetadata.isSymbolicLink()) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint must be a regular file");
  }
  requireOwnerOnly(pathMetadata);
  if (pathMetadata.size > limit) {
    throw new RecorderError("checkpoint_corrupt", `checkpoint exceeds its ${limit}-byte limit`);
  }

  let handle: FileHandle;
  try {
    handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  } catch (cause) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint could not be opened safely", {
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
      throw new RecorderError("checkpoint_corrupt", "checkpoint changed while it was opened");
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
      throw new RecorderError("checkpoint_corrupt", `checkpoint exceeds its ${limit}-byte limit`);
    }
    return bytes.subarray(0, offset);
  } finally {
    await handle.close().catch(() => {});
  }
}

async function readJournalHead(path: string): Promise<JournalHead | null> {
  const bytes = await readBoundedPrivateFile(journalHeadPath(path), JOURNAL_HEAD_MAX_BYTES);
  return bytes === null ? null : parseJournalHead(bytes);
}

function validateJournalIdentity(
  base: RecordingCheckpoint,
  head: JournalHead,
): void {
  if (
    head.baseRevision !== base.revision
    || head.sessionId !== base.sessionId
    || !isDeepStrictEqual(head.eventProtocolVersion, base.eventProtocolVersion)
    || head.revision <= base.revision
    || head.eventCount <= base.events.length
  ) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint journal identity is inconsistent");
  }
}

async function loadCheckpointWithJournal(
  path: string,
  limit: number,
): Promise<RecorderCheckpoint | null> {
  const baseBytes = await readBoundedPrivateFile(path, limit);
  if (baseBytes === null) {
    return null;
  }
  const base = parseEnvelope(baseBytes, limit);
  if (base.phase !== "recording") {
    // A sealed/completed base is the compaction boundary. Journal sidecars
    // from an interrupted best-effort cleanup are obsolete and untrusted.
    return base;
  }
  const head = await readJournalHead(path);
  if (head === null || head.revision <= base.revision) {
    return base;
  }
  validateJournalIdentity(base, head);
  const journalLimit = Math.min(
    Number.MAX_SAFE_INTEGER,
    limit * JOURNAL_SIZE_MULTIPLIER,
  );
  if (head.committedBytes > journalLimit) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint journal exceeds its size limit");
  }
  const journal = await readBoundedPrivateFile(journalPath(path), journalLimit);
  if (journal === null || journal.length < head.committedBytes) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint journal is missing or truncated");
  }

  const log = EventLog.replay(base.sessionId, base.events, base.eventProtocolVersion);
  let offset = 0;
  let previousSha256: string | null = null;
  let expectedRevision = base.revision + 1;
  let eventBytes = encodedEventsBytes(base.events);
  while (offset < head.committedBytes) {
    const newline = journal.indexOf(0x0a, offset);
    if (newline < offset || newline >= head.committedBytes) {
      throw new RecorderError("checkpoint_corrupt", "checkpoint journal record is truncated");
    }
    const end = newline + 1;
    const { segment, sha256 } = parseJournalSegment(
      journal.subarray(offset, end),
      Math.min(limit, end - offset),
    );
    if (
      segment.revision !== expectedRevision
      || segment.previousSha256 !== previousSha256
      || segment.firstSequence !== log.nextSequence
    ) {
      throw new RecorderError("checkpoint_corrupt", "checkpoint journal chain is inconsistent");
    }
    const accepted = log.acceptBatch(segment.events);
    eventBytes += encodedEventsBytes(segment.events);
    if (
      accepted.accepted !== segment.events.length
      || accepted.duplicates !== 0
      || segment.eventCount !== (log.lastSequence ?? 0)
      || segment.eventBytes !== eventBytes
    ) {
      throw new RecorderError("checkpoint_corrupt", "checkpoint journal counters are inconsistent");
    }
    previousSha256 = sha256;
    expectedRevision += 1;
    offset = end;
  }
  if (
    offset !== head.committedBytes
    || head.lastSegmentOffset + head.lastSegmentLength !== offset
    || head.lastSha256 !== previousSha256
    || head.revision !== expectedRevision - 1
    || head.eventCount !== (log.lastSequence ?? 0)
    || head.eventBytes !== eventBytes
  ) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint journal head does not match its records");
  }
  const logical: RecordingCheckpoint = {
    ...base,
    revision: head.revision,
    events: log.events,
  };
  // Preserve the existing exact snapshot bound at recovery and compaction.
  serializeNormalizedEnvelope(logical, limit);
  return logical;
}

function parseLockRecord(value: unknown): CheckpointLockRecord {
  const candidate = record(value, "checkpoint lock");
  exactKeys(candidate, ["pid", "token"], [], "checkpoint lock");
  const pid = safeInteger(candidate.pid, "checkpoint lock pid", 1);
  if (pid > 2_147_483_647 || typeof candidate.token !== "string" || !UUID.test(candidate.token)) {
    throw new RecorderError("checkpoint_locked", "checkpoint lock owner is invalid");
  }
  return { pid, token: candidate.token };
}

async function readLockRecord(path: string): Promise<CheckpointLockRecord> {
  try {
    const bytes = await readBoundedPrivateFile(path, LOCK_MAX_BYTES);
    if (bytes === null) {
      throw new RecorderError("checkpoint_locked", "checkpoint lock disappeared");
    }
    return parseLockRecord(fromCanonicalJson(bytes, { maxBytes: LOCK_MAX_BYTES }));
  } catch (cause) {
    if (cause instanceof RecorderError && cause.code === "checkpoint_locked") {
      throw cause;
    }
    throw new RecorderError(
      "checkpoint_locked",
      "checkpoint lock is damaged and cannot be reclaimed automatically",
      { cause },
    );
  }
}

function processIsAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (cause) {
    if (isNodeError(cause, "ESRCH")) {
      return false;
    }
    // EPERM and every ambiguous platform failure are treated as a live owner.
    return true;
  }
}

async function acquireLock(path: string, signal: AbortSignal | undefined): Promise<CheckpointLock> {
  const lockPath = `${path}.lock`;
  const parent = dirname(path);
  for (let attempt = 0; attempt < 3; attempt += 1) {
    requireNotCancelled(signal);
    const token = randomUUID();
    const temporary = join(parent, `.${basename(path)}.lock.${token}.tmp`);
    const bytes = toCanonicalJson({ pid: process.pid, token }, { maxBytes: LOCK_MAX_BYTES });
    let handle: FileHandle | undefined;
    try {
      handle = await open(temporary, "wx", 0o600);
      await handle.writeFile(bytes);
      await handle.sync();
      await handle.close();
      handle = undefined;
      try {
        await link(temporary, lockPath);
      } catch (cause) {
        if (!isNodeError(cause, "EEXIST")) {
          throw cause;
        }
        const owner = await readLockRecord(lockPath);
        if (processIsAlive(owner.pid)) {
          throw new RecorderError("checkpoint_locked", "checkpoint is locked by a live writer", {
            details: { pid: owner.pid },
          });
        }
        // No age heuristic is used: only an OS-confirmed ESRCH owner is stale.
        await unlink(lockPath);
        continue;
      }
      await unlink(temporary);
      try {
        await syncParent(parent);
      } catch (cause) {
        await unlink(lockPath).catch(() => {});
        throw new RecorderError("checkpoint_locked", "checkpoint lock durability is unknown", {
          cause,
        });
      }
      return { path: lockPath, token };
    } catch (cause) {
      if (cause instanceof RecorderError) {
        throw cause;
      }
      throw new RecorderError("checkpoint_corrupt", "checkpoint lock could not be published", {
        cause,
      });
    } finally {
      await handle?.close().catch(() => {});
      await unlink(temporary).catch(() => {});
    }
  }
  throw new RecorderError("checkpoint_locked", "checkpoint stale-lock recovery did not converge");
}

async function releaseLock(lock: CheckpointLock): Promise<void> {
  const current = await readLockRecord(lock.path);
  if (current.pid !== process.pid || current.token !== lock.token) {
    throw new RecorderError("checkpoint_locked", "checkpoint writer no longer owns its lock");
  }
  await unlink(lock.path);
}

async function syncParent(parent: string): Promise<void> {
  // Node cannot portably open and fsync directory handles on Windows. The
  // atomic rename still defines publication there, but crash-power durability
  // is limited to the file flush and filesystem implementation.
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

function validateTransition(
  current: RecorderCheckpoint | null,
  next: RecorderCheckpoint,
): void {
  if (current === null) {
    if (next.phase !== "recording") {
      throw new RecorderError(
        "checkpoint_conflict",
        "the first durable checkpoint revision must be recording",
      );
    }
    return;
  }
  if (
    current.sessionId !== next.sessionId ||
    !isDeepStrictEqual(current.eventProtocolVersion, next.eventProtocolVersion)
  ) {
    throw new RecorderError(
      "checkpoint_conflict",
      "checkpoint identity or event protocol version cannot change",
    );
  }
  if (current.phase === "completed") {
    throw new RecorderError("checkpoint_conflict", "completed checkpoint is immutable");
  }
  if (current.phase === "sealed" && next.phase === "sealed") {
    throw new RecorderError("checkpoint_conflict", "sealed checkpoint cannot be sealed again");
  }
  const phaseOrder: Record<RecorderPhase, number> = { recording: 0, sealed: 1, completed: 2 };
  const phaseDelta = phaseOrder[next.phase] - phaseOrder[current.phase];
  if (phaseDelta < 0 || phaseDelta > 1) {
    throw new RecorderError(
      "checkpoint_conflict",
      "checkpoint phase must advance one step at a time",
    );
  }
  if (current.events.length > next.events.length) {
    throw new RecorderError("checkpoint_conflict", "checkpoint events cannot be removed");
  }
  for (let index = 0; index < current.events.length; index += 1) {
    if (!isDeepStrictEqual(current.events[index], next.events[index])) {
      throw new RecorderError("checkpoint_conflict", "confirmed checkpoint events cannot change");
    }
  }
  if (current.phase === "sealed") {
    if (
      next.events.length !== current.events.length ||
      !isDeepStrictEqual(current.session, next.session)
    ) {
      throw new RecorderError("checkpoint_conflict", "sealed Session data is immutable");
    }
  }
}

/** Load a canonical checkpoint; Unix additionally enforces owner-only file metadata. */
export async function loadRecorderCheckpoint(
  path: string,
  options: RecorderCheckpointFileOptions = {},
): Promise<RecorderCheckpoint | null> {
  const limit = maxBytes(options);
  return await loadCheckpointWithJournal(path, limit);
}

async function readPrivateRange(
  path: string,
  offset: number,
  length: number,
  sizeLimit: number,
): Promise<Buffer> {
  const metadata = await lstat(path).catch((cause: unknown) => {
    throw new RecorderError("checkpoint_corrupt", "checkpoint journal metadata could not be read", {
      cause,
    });
  });
  if (
    !metadata.isFile()
    || metadata.isSymbolicLink()
    || metadata.size > sizeLimit
    || offset + length > metadata.size
  ) {
    throw new RecorderError("checkpoint_corrupt", "checkpoint journal range is invalid");
  }
  requireOwnerOnly(metadata);
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const opened = await handle.stat();
    if (opened.dev !== metadata.dev || opened.ino !== metadata.ino || !opened.isFile()) {
      throw new RecorderError("checkpoint_corrupt", "checkpoint journal changed while opening");
    }
    requireOwnerOnly(opened);
    const bytes = Buffer.allocUnsafe(length);
    let read = 0;
    while (read < length) {
      const result = await handle.read(bytes, read, length - read, offset + read);
      if (result.bytesRead === 0) {
        throw new RecorderError("checkpoint_corrupt", "checkpoint journal range is truncated");
      }
      read += result.bytesRead;
    }
    return bytes;
  } finally {
    await handle.close().catch(() => {});
  }
}

async function openJournalForAppend(path: string): Promise<FileHandle> {
  try {
    const metadata = await lstat(path);
    if (!metadata.isFile() || metadata.isSymbolicLink()) {
      throw new RecorderError("checkpoint_corrupt", "checkpoint journal must be a regular file");
    }
    requireOwnerOnly(metadata);
    const handle = await open(path, constants.O_RDWR | constants.O_NOFOLLOW);
    const opened = await handle.stat();
    if (opened.dev !== metadata.dev || opened.ino !== metadata.ino || !opened.isFile()) {
      await handle.close().catch(() => {});
      throw new RecorderError("checkpoint_corrupt", "checkpoint journal changed while opening");
    }
    requireOwnerOnly(opened);
    return handle;
  } catch (cause) {
    if (!isNodeError(cause, "ENOENT")) {
      throw cause;
    }
    return await open(path, "wx+", 0o600);
  }
}

async function writeAllAt(handle: FileHandle, bytes: Uint8Array, offset: number): Promise<void> {
  let written = 0;
  while (written < bytes.length) {
    const result = await handle.write(bytes, written, bytes.length - written, offset + written);
    if (result.bytesWritten === 0) {
      throw new RecorderError("checkpoint_corrupt", "checkpoint journal write made no progress");
    }
    written += result.bytesWritten;
  }
}

/** @internal Append one already-validated Recorder page without rewriting its prefix. */
export async function appendRecorderCheckpointPage(
  path: string,
  expectedRevision: number,
  identity: Pick<RecordingCheckpoint, "sessionId" | "eventProtocolVersion">,
  events: readonly TestEvent[],
  options: RecorderCheckpointFileOptions = {},
): Promise<RecorderJournalAppendResult> {
  const expected = safeInteger(expectedRevision, "expectedRevision", 1);
  if (expected === Number.MAX_SAFE_INTEGER || events.length === 0) {
    throw new RecorderError(
      "checkpoint_conflict",
      events.length === 0 ? "journal page must contain events" : "checkpoint revision is exhausted",
    );
  }
  const limit = maxBytes(options);
  const parent = await requireRealParent(path);
  const lock = await acquireLock(path, options.signal);
  const journal = journalPath(path);
  const headPath = journalHeadPath(path);
  const temporaryHead = join(parent, `.${basename(path)}.journal-head.${randomUUID()}.tmp`);
  let headPublished = false;
  let operationError: unknown;
  let result: RecorderJournalAppendResult | undefined;

  try {
    requireNotCancelled(options.signal);
    const baseBytes = await readBoundedPrivateFile(path, limit);
    if (baseBytes === null) {
      throw new RecorderError("checkpoint_conflict", "checkpoint does not exist");
    }
    const base = parseEnvelope(baseBytes, limit);
    if (
      base.phase !== "recording"
      || base.sessionId !== identity.sessionId
      || !isDeepStrictEqual(base.eventProtocolVersion, identity.eventProtocolVersion)
    ) {
      throw new RecorderError("checkpoint_conflict", "checkpoint recording identity changed");
    }
    let head = await readJournalHead(path);
    if (head !== null && head.revision <= base.revision) {
      head = null;
    }
    if (head !== null) {
      validateJournalIdentity(base, head);
    }
    const currentRevision = head?.revision ?? base.revision;
    const currentEventCount = head?.eventCount ?? base.events.length;
    const currentEventBytes = head?.eventBytes ?? encodedEventsBytes(base.events);
    const committedBytes = head?.committedBytes ?? 0;
    const journalLimit = Math.min(Number.MAX_SAFE_INTEGER, limit * JOURNAL_SIZE_MULTIPLIER);
    if (currentRevision !== expected) {
      throw new RecorderError(
        "checkpoint_conflict",
        `checkpoint revision is ${currentRevision}, expected ${expected}`,
        { details: { actualRevision: currentRevision, expectedRevision: expected } },
      );
    }
    if (head !== null) {
      const lastBytes = await readPrivateRange(
        journal,
        head.lastSegmentOffset,
        head.lastSegmentLength,
        journalLimit,
      );
      const last = parseJournalSegment(lastBytes, Math.min(limit, lastBytes.length));
      if (last.sha256 !== head.lastSha256 || last.segment.revision !== head.revision) {
        throw new RecorderError("checkpoint_corrupt", "checkpoint journal head is not bound to its tail");
      }
    }
    for (const [index, event] of events.entries()) {
      if (
        event.sessionId !== base.sessionId
        || event.sequence !== currentEventCount + index + 1
      ) {
        throw new RecorderError("checkpoint_conflict", "journal page sequence or Session is invalid");
      }
    }
    const eventBytes = currentEventBytes + encodedEventsBytes(events);
    const eventCount = currentEventCount + events.length;
    const emptyEnvelopeBytes = serializeNormalizedEnvelope(
      { ...base, revision: expected + 1, events: [] },
      limit,
    ).length;
    const projectedSnapshotBytes = emptyEnvelopeBytes + eventBytes + Math.max(0, eventCount - 1);
    if (projectedSnapshotBytes > limit) {
      throw new RecorderError("checkpoint_corrupt", "checkpoint exceeds its encoding limits");
    }
    const segment: JournalSegment = {
      format: JOURNAL_FORMAT,
      version: JOURNAL_VERSION,
      revision: expected + 1,
      previousSha256: head?.lastSha256 ?? null,
      firstSequence: currentEventCount + 1,
      eventCount,
      eventBytes,
      events,
    };
    const segmentBytes = toCanonicalJsonChecksumEnvelope(segment, { maxBytes: limit });
    if (committedBytes + segmentBytes.length > journalLimit) {
      throw new RecorderError("checkpoint_corrupt", "checkpoint journal exceeds its size limit");
    }
    const segmentSha256 = sha256Hex(toCanonicalJson(segment, { maxBytes: limit }));

    requireNotCancelled(options.signal);
    const journalHandle = await openJournalForAppend(journal);
    try {
      const metadata = await journalHandle.stat();
      if (metadata.size < committedBytes || metadata.size > journalLimit) {
        throw new RecorderError("checkpoint_corrupt", "checkpoint journal size is inconsistent");
      }
      if (metadata.size !== committedBytes) {
        await journalHandle.truncate(committedBytes);
      }
      await writeAllAt(journalHandle, segmentBytes, committedBytes);
      await journalHandle.sync();
    } finally {
      await journalHandle.close().catch(() => {});
    }

    const nextHead: JournalHead = {
      format: JOURNAL_FORMAT,
      version: JOURNAL_VERSION,
      baseRevision: base.revision,
      revision: expected + 1,
      sessionId: base.sessionId,
      eventProtocolVersion: base.eventProtocolVersion,
      committedBytes: committedBytes + segmentBytes.length,
      lastSegmentOffset: committedBytes,
      lastSegmentLength: segmentBytes.length,
      lastSha256: segmentSha256,
      eventCount,
      eventBytes,
    };
    const headBytes = toCanonicalJsonChecksumEnvelope(nextHead, {
      maxBytes: JOURNAL_HEAD_MAX_BYTES,
    });
    requireNotCancelled(options.signal);
    const headHandle = await open(temporaryHead, "wx", 0o600);
    try {
      await headHandle.writeFile(headBytes);
      await headHandle.sync();
    } finally {
      await headHandle.close().catch(() => {});
    }
    requireNotCancelled(options.signal);
    await rename(temporaryHead, headPath);
    headPublished = true;
    try {
      await syncParent(parent);
    } catch (cause) {
      throw new RecorderError(
        "checkpoint_durability_unknown",
        "checkpoint journal head was published but directory durability is unknown",
        { cause, details: { committed: true, revision: nextHead.revision } },
      );
    }
    result = Object.freeze({ revision: nextHead.revision, eventCount });
  } catch (cause) {
    operationError = cause instanceof RecorderError
      ? cause
      : new RecorderError(
          headPublished ? "checkpoint_durability_unknown" : "checkpoint_corrupt",
          headPublished
            ? "checkpoint journal was published but final durability is unknown"
            : "checkpoint journal append failed before publication",
          { cause },
        );
  }

  await unlink(temporaryHead).catch(() => {});
  try {
    await releaseLock(lock);
  } catch (cause) {
    operationError ??= new RecorderError(
      headPublished ? "checkpoint_durability_unknown" : "checkpoint_locked",
      "checkpoint journal writer lock could not be released",
      { cause },
    );
  }
  if (operationError !== undefined) {
    throw operationError;
  }
  return result!;
}

/**
 * CAS-commit one checkpoint revision through a same-directory atomic replace.
 *
 * `expectedRevision === 0` means the target must not exist and `next.revision`
 * must be 1. The rename is the publication linearization point; cancellation
 * is no longer observed after it.
 */
export async function commitRecorderCheckpoint(
  path: string,
  expectedRevision: number,
  next: RecorderCheckpoint,
  options: RecorderCheckpointFileOptions = {},
): Promise<RecorderCheckpoint> {
  const expected = safeInteger(expectedRevision, "expectedRevision");
  if (expected === Number.MAX_SAFE_INTEGER) {
    throw new RecorderError("checkpoint_conflict", "checkpoint revision is exhausted");
  }
  const normalized = validateRecorderCheckpoint(next);
  if (normalized.revision !== expected + 1) {
    throw new RecorderError(
      "checkpoint_conflict",
      "next checkpoint revision must equal expectedRevision + 1",
    );
  }
  const limit = maxBytes(options);
  const bytes = serializeNormalizedEnvelope(normalized, limit);
  const parent = await requireRealParent(path);
  const lock = await acquireLock(path, options.signal);
  const temporary = join(parent, `.${basename(path)}.${randomUUID()}.tmp`);
  let published = false;
  let operationError: unknown;

  try {
    requireNotCancelled(options.signal);
    const current = await loadCheckpointWithJournal(path, limit);
    if (
      current === null
      && (await pathEntryExists(journalHeadPath(path))
        || await pathEntryExists(journalPath(path)))
    ) {
      throw new RecorderError(
        "checkpoint_corrupt",
        "checkpoint sidecars exist without their base checkpoint",
      );
    }
    const currentRevision = current?.revision ?? 0;
    if (currentRevision !== expected) {
      throw new RecorderError(
        "checkpoint_conflict",
        `checkpoint revision is ${currentRevision}, expected ${expected}`,
        { details: { actualRevision: currentRevision, expectedRevision: expected } },
      );
    }
    validateTransition(current, normalized);

    requireNotCancelled(options.signal);
    const temporaryHandle = await open(temporary, "wx", 0o600);
    try {
      await temporaryHandle.writeFile(bytes);
      await temporaryHandle.sync();
    } finally {
      await temporaryHandle.close().catch(() => {});
    }
    requireNotCancelled(options.signal);
    await rename(temporary, path);
    published = true;
    try {
      await syncParent(parent);
    } catch (cause) {
      throw new RecorderError(
        "checkpoint_durability_unknown",
        "checkpoint was published but parent-directory durability is unknown",
        { cause, details: { committed: true, revision: normalized.revision } },
      );
    }
    // The full snapshot now supersedes every sidecar revision. Cleanup is
    // intentionally best-effort: the checkpoint is already durable and loads
    // ignore obsolete sidecars at or below its revision.
    const removedHead = expected > 0 && await unlink(journalHeadPath(path)).then(
      () => true,
      () => false,
    );
    const removedJournal = expected > 0 && await unlink(journalPath(path)).then(
      () => true,
      () => false,
    );
    if (removedHead || removedJournal) {
      await syncParent(parent).catch(() => {});
    }
  } catch (cause) {
    operationError =
      cause instanceof RecorderError
        ? cause
        : new RecorderError(
            published ? "checkpoint_durability_unknown" : "checkpoint_corrupt",
            published
              ? "checkpoint was published but final durability is unknown"
              : "checkpoint commit failed before publication",
            {
              cause,
              ...(published
                ? { details: { committed: true, revision: normalized.revision } }
                : {}),
            },
          );
  }

  if (!published) {
    await unlink(temporary).catch(() => {});
  }
  try {
    await releaseLock(lock);
  } catch (cause) {
    operationError ??= new RecorderError(
      published ? "checkpoint_durability_unknown" : "checkpoint_locked",
      published
        ? "checkpoint was published but its writer lock could not be released"
        : "checkpoint writer lock could not be released",
      {
        cause,
        ...(published
          ? { details: { committed: true, revision: normalized.revision } }
          : {}),
      },
    );
  }
  if (operationError !== undefined) {
    throw operationError;
  }
  return normalized;
}
