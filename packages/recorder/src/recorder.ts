import { isDeepStrictEqual } from "node:util";

import { RpcRemoteError, type DeviceRailClient } from "@devicerail/client";
import type {
  ProtocolVersion,
  SessionExportResult,
  SessionInfo,
  SystemDescribeResult,
  TestEvent,
} from "@devicerail/protocol";

import { exportAndValidateBundle } from "./bundle-cli.js";
import {
  appendRecorderCheckpointPage,
  commitRecorderCheckpoint,
  loadRecorderCheckpoint,
} from "./checkpoint.js";
import { RecorderError } from "./errors.js";
import { EventLog } from "./event-log.js";
import {
  publishBundleSource,
  readBundleSource,
  type BundleSourceFile,
  validateBundleSourceBounds,
} from "./source-file.js";
import {
  RECORDER_CHECKPOINT_FORMAT,
  RECORDER_CHECKPOINT_VERSION,
  type CompletedCheckpoint,
  type RecorderBundleReceipt,
  type RecorderCheckpoint,
  type RecorderPhase,
  type RecordingCheckpoint,
  type SealedCheckpoint,
} from "./types.js";

const RECORDER_EVENT_PAGE_SIZE = 1_000;
const SESSION_EXPORT_PAGE_FEATURE = "session.export.page.v1";

function isResponseFrameTooLarge(cause: unknown): cause is RpcRemoteError {
  return cause instanceof RpcRemoteError
    && cause.rpcError.data.code === "response_frame_too_large";
}

export interface RecorderEventSource {
  describe(): Promise<SystemDescribeResult>;
  listEvents(
    sessionId: string,
    afterSequence: number | null,
    limit: number,
  ): Promise<readonly unknown[]>;
  exportSession(sessionId: string): Promise<unknown>;
  /** Optional negotiated bounded form of the authoritative Session export. */
  exportSessionPage?(
    sessionId: string,
    afterSequence: number | null,
    limit: number,
  ): Promise<unknown>;
}

/** Public-protocol adapter; Session lifecycle remains owned by the host. */
export class DeviceRailRecorderEventSource implements RecorderEventSource {
  readonly #client: DeviceRailClient;

  constructor(client: DeviceRailClient) {
    this.#client = client;
  }

  async describe(): Promise<SystemDescribeResult> {
    return await this.#client.call("system.describe");
  }

  async listEvents(
    sessionId: string,
    afterSequence: number | null,
    limit: number,
  ): Promise<readonly unknown[]> {
    return await this.#client.call("events.list", {
      sessionId,
      ...(afterSequence === null ? {} : { afterSequence }),
      limit,
    });
  }

  async exportSession(sessionId: string): Promise<unknown> {
    return await this.#client.call("session.export", { sessionId });
  }

  async exportSessionPage(
    sessionId: string,
    afterSequence: number | null,
    limit: number,
  ): Promise<unknown> {
    const params = {
      sessionId,
      ...(afterSequence === null ? {} : { afterSequence }),
      limit,
    };
    return await this.#client.call("session.export", params);
  }
}

interface RecorderOpenBase {
  readonly checkpointPath: string;
  readonly sessionId: string;
  readonly signal?: AbortSignal;
}

export type RecorderOpenOptions = RecorderOpenBase &
  (
    | { readonly client: DeviceRailClient; readonly eventSource?: never }
    | { readonly client?: never; readonly eventSource: RecorderEventSource }
  );

export interface RecorderCaptureResult {
  readonly accepted: number;
  readonly duplicates: number;
  readonly lastSequence: number | null;
  readonly phase: RecorderPhase;
}

export interface RecorderCaptureOptions {
  readonly signal?: AbortSignal;
}

export interface RecorderCaptureUntilOptions extends RecorderCaptureOptions {
  readonly pollIntervalMs?: number;
}

export interface RecorderFinalizeOptions {
  readonly executable: string;
  readonly sourcePath: string;
  readonly evidenceDirectory: string;
  readonly outputDirectory: string;
  readonly signal?: AbortSignal;
}

export interface RecorderOfflineOpenOptions {
  readonly checkpointPath: string;
}

function throwIfAborted(signal: AbortSignal | undefined): void {
  if (signal?.aborted) {
    throw new RecorderError("operation_cancelled", "Recorder operation was cancelled");
  }
}

function exactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  return isDeepStrictEqual(Object.keys(value).sort(), [...keys].sort());
}

function objectRecord(value: unknown, location: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new RecorderError("session_export_mismatch", `${location} must be an object`);
  }
  return value as Record<string, unknown>;
}

function safeInteger(value: unknown): value is number {
  return (
    typeof value === "number" &&
    Number.isSafeInteger(value) &&
    !Object.is(value, -0) &&
    value >= 0
  );
}

function validateProtocolVersion(value: unknown): ProtocolVersion {
  const record = objectRecord(value, "selected protocol version");
  if (!exactKeys(record, ["major", "minor"]) || !safeInteger(record.major) || !safeInteger(record.minor)) {
    throw new RecorderError(
      "session_export_mismatch",
      "selected protocol version must contain non-negative safe integer major/minor fields",
    );
  }
  if (record.major !== 1 || record.minor > 5) {
    throw new RecorderError(
      "session_export_mismatch",
      "selected protocol version is unsupported by this Recorder",
    );
  }
  return { major: record.major, minor: record.minor };
}

function validateEndedSession(value: unknown, events: readonly TestEvent[]): SessionInfo {
  const record = objectRecord(value, "session.export.session");
  if (
    !exactKeys(record, [
      "endedAtMs",
      "eventCount",
      "id",
      "lastSequence",
      "startedAtMs",
      "state",
    ]) ||
    typeof record.id !== "string" ||
    record.state !== "ended" ||
    !safeInteger(record.startedAtMs) ||
    !safeInteger(record.endedAtMs) ||
    !safeInteger(record.eventCount) ||
    !safeInteger(record.lastSequence)
  ) {
    throw new RecorderError(
      "session_export_mismatch",
      "session.export returned an invalid ended SessionInfo",
    );
  }
  const first = events[0];
  const last = events.at(-1);
  if (
    !first ||
    !last ||
    record.id !== first.sessionId ||
    record.eventCount !== events.length ||
    record.lastSequence !== last.sequence ||
    record.startedAtMs !== first.atMs ||
    record.endedAtMs !== last.atMs
  ) {
    throw new RecorderError(
      "session_export_mismatch",
      "SessionInfo does not match the recorded event lifecycle",
    );
  }
  return structuredClone(record) as unknown as SessionInfo;
}

function validateSessionExport(
  value: unknown,
  sessionId: string,
  recordedEvents: readonly TestEvent[],
  eventProtocolVersion: ProtocolVersion,
): SessionExportResult {
  const record = objectRecord(value, "session.export");
  if (!exactKeys(record, ["events", "session"]) || !Array.isArray(record.events)) {
    throw new RecorderError(
      "session_export_mismatch",
      "session.export must contain exactly session and events",
    );
  }
  const exportedLog = EventLog.replay(sessionId, record.events, eventProtocolVersion);
  if (!exportedLog.terminal || exportedLog.openActionCount !== 0) {
    throw new RecorderError("session_not_ended", "session.export is not terminal");
  }
  if (!isDeepStrictEqual(exportedLog.events, recordedEvents)) {
    throw new RecorderError(
      "session_export_mismatch",
      "session.export events differ from the durable Recorder log",
    );
  }
  const session = validateEndedSession(record.session, exportedLog.events);
  if (session.id !== sessionId) {
    throw new RecorderError("session_mismatch", "session.export identifies another Session");
  }
  return {
    events: structuredClone(exportedLog.events) as TestEvent[],
    session,
  };
}

interface AdaptivePageResult<T> {
  readonly limit: number;
  readonly value: T;
}

interface AdaptivePageOptions<T> {
  readonly afterSequence: number | null;
  readonly initialLimit: number;
  readonly load: (limit: number) => Promise<T>;
  readonly signal?: AbortSignal;
  readonly tooLargeMessage: string;
  readonly unavailableMessage: string;
}

async function loadAdaptivePage<T>(
  options: AdaptivePageOptions<T>,
): Promise<AdaptivePageResult<T>> {
  let pageLimit = options.initialLimit;
  while (true) {
    throwIfAborted(options.signal);
    try {
      return { limit: pageLimit, value: await options.load(pageLimit) };
    } catch (cause) {
      throwIfAborted(options.signal);
      if (!isResponseFrameTooLarge(cause)) {
        throw new RecorderError("upstream_unavailable", options.unavailableMessage, { cause });
      }
      if (pageLimit === 1) {
        throw new RecorderError("event_too_large", options.tooLargeMessage, {
          cause,
          details: {
            afterSequence: options.afterSequence,
            pageLimit,
            upstreamCode: cause.rpcError.data.code,
          },
        });
      }
      pageLimit = Math.max(1, Math.floor(pageLimit / 2));
    }
  }
}

interface ValidatedExportPage {
  readonly eventCount: number;
  readonly nextAfterSequence: number | null;
  readonly session: SessionInfo;
}

function validateSessionExportPage(
  value: unknown,
  recordedEvents: readonly TestEvent[],
  offset: number,
  pageLimit: number,
  priorSession: SessionInfo | undefined,
): ValidatedExportPage {
  const record = objectRecord(value, "session.export page");
  const hasNext = Object.hasOwn(record, "nextAfterSequence");
  if (
    !exactKeys(record, hasNext ? ["events", "nextAfterSequence", "session"] : ["events", "session"])
    || !Array.isArray(record.events)
  ) {
    throw new RecorderError(
      "session_export_mismatch",
      "session.export page must contain exactly session and events",
    );
  }
  if (record.events.length > pageLimit) {
    throw new RecorderError(
      "session_export_mismatch",
      `session.export returned more than its ${pageLimit}-event page limit`,
    );
  }
  const session = validateEndedSession(record.session, recordedEvents);
  if (priorSession && !isDeepStrictEqual(session, priorSession)) {
    throw new RecorderError(
      "session_export_mismatch",
      "session.export SessionInfo changed between pages",
    );
  }
  if (offset + record.events.length > recordedEvents.length) {
    throw new RecorderError(
      "session_export_mismatch",
      "session.export contains events beyond the durable Recorder log",
    );
  }
  for (const [pageIndex, event] of record.events.entries()) {
    if (!isDeepStrictEqual(event, recordedEvents[offset + pageIndex])) {
      throw new RecorderError(
        "session_export_mismatch",
        "session.export events differ from the durable Recorder log",
        { details: { sequence: offset + pageIndex + 1 } },
      );
    }
  }
  const nextAfterSequence = hasNext ? record.nextAfterSequence : null;
  const remaining = offset + record.events.length < recordedEvents.length;
  const lastPageEvent = record.events.at(-1) as TestEvent | undefined;
  if (
    remaining
      ? !safeInteger(nextAfterSequence)
        || nextAfterSequence < 1
        || nextAfterSequence !== lastPageEvent?.sequence
      : hasNext
  ) {
    throw new RecorderError(
      "session_export_mismatch",
      remaining
        ? "session.export must continue at the exact final sequence of a non-terminal page"
        : "session.export final page must not contain nextAfterSequence",
    );
  }
  return {
    eventCount: record.events.length,
    nextAfterSequence: nextAfterSequence as number | null,
    session,
  };
}

async function waitForPoll(milliseconds: number, signal: AbortSignal | undefined): Promise<void> {
  throwIfAborted(signal);
  await new Promise<void>((resolve, reject) => {
    let settled = false;
    const finish = (callback: () => void): void => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener("abort", abortListener);
      callback();
    };
    const abortListener = (): void => {
      finish(() =>
        reject(new RecorderError("operation_cancelled", "Recorder operation was cancelled")),
      );
    };
    const timer = setTimeout(() => finish(resolve), milliseconds);
    signal?.addEventListener("abort", abortListener, { once: true });
    if (signal?.aborted) {
      abortListener();
    }
  });
}

async function awaitSealOperation(
  operation: Promise<BundleSourceFile>,
  signal: AbortSignal | undefined,
): Promise<BundleSourceFile> {
  throwIfAborted(signal);
  if (!signal) {
    return await operation;
  }
  return await new Promise<BundleSourceFile>((resolve, reject) => {
    let settled = false;
    const finish = (callback: () => void): void => {
      if (settled) {
        return;
      }
      settled = true;
      signal.removeEventListener("abort", abortListener);
      callback();
    };
    const abortListener = (): void => {
      finish(() =>
        reject(new RecorderError("operation_cancelled", "Recorder operation was cancelled")),
      );
    };
    signal.addEventListener("abort", abortListener, { once: true });
    operation.then(
      (source) => finish(() => resolve(source)),
      (error: unknown) => finish(() => reject(error)),
    );
    if (signal.aborted) {
      abortListener();
    }
  });
}

/** Durable, resumable consumer of one public Session event stream. */
export class ExecutionRecorder {
  readonly #checkpointPath: string;
  readonly #eventSource: RecorderEventSource | undefined;
  readonly #supportsPaginatedSessionExport: boolean;
  #checkpoint: RecorderCheckpoint;
  #log: EventLog;
  #sealInFlight: Promise<BundleSourceFile> | undefined;

  private constructor(
    eventSource: RecorderEventSource | undefined,
    checkpointPath: string,
    checkpoint: RecorderCheckpoint,
    supportsPaginatedSessionExport: boolean,
  ) {
    this.#eventSource = eventSource;
    this.#checkpointPath = checkpointPath;
    this.#checkpoint = checkpoint;
    this.#supportsPaginatedSessionExport = supportsPaginatedSessionExport;
    this.#log = EventLog.replay(
      checkpoint.sessionId,
      checkpoint.events,
      checkpoint.eventProtocolVersion,
    );
    if (checkpoint.phase !== "recording" && !this.#log.terminal) {
      throw new RecorderError("checkpoint_corrupt", "sealed checkpoint is not terminal");
    }
  }

  static async open(options: RecorderOpenOptions): Promise<ExecutionRecorder> {
    throwIfAborted(options.signal);
    const eventSource = options.eventSource ?? new DeviceRailRecorderEventSource(options.client);
    let description: SystemDescribeResult;
    try {
      description = await eventSource.describe();
    } catch (cause) {
      throw new RecorderError("upstream_unavailable", "Recorder could not describe the daemon", {
        cause,
      });
    }
    throwIfAborted(options.signal);
    const eventProtocolVersion = validateProtocolVersion(
      description.connection.protocol.selected,
    );
    const loaded = await loadRecorderCheckpoint(options.checkpointPath);
    let checkpoint: RecorderCheckpoint;
    if (loaded) {
      if (loaded.sessionId !== options.sessionId) {
        throw new RecorderError("session_mismatch", "checkpoint belongs to another Session");
      }
      if (!isDeepStrictEqual(loaded.eventProtocolVersion, eventProtocolVersion)) {
        throw new RecorderError(
          "checkpoint_conflict",
          "checkpoint protocol version differs from the live connection",
        );
      }
      checkpoint = loaded;
    } else {
      const initial: RecordingCheckpoint = {
        format: RECORDER_CHECKPOINT_FORMAT,
        version: RECORDER_CHECKPOINT_VERSION,
        revision: 1,
        phase: "recording",
        sessionId: options.sessionId,
        eventProtocolVersion,
        events: [],
      };
      checkpoint = await commitRecorderCheckpoint(options.checkpointPath, 0, initial, {
        ...(options.signal ? { signal: options.signal } : {}),
      });
    }
    const supportsPaginatedSessionExport =
      description.connection.features.enabled.includes(SESSION_EXPORT_PAGE_FEATURE)
      && typeof eventSource.exportSessionPage === "function";
    return new ExecutionRecorder(
      eventSource,
      options.checkpointPath,
      checkpoint,
      supportsPaginatedSessionExport,
    );
  }

  /** Reopen a sealed/completed checkpoint after the daemon has stopped. */
  static async openOffline(options: RecorderOfflineOpenOptions): Promise<ExecutionRecorder> {
    const checkpoint = await loadRecorderCheckpoint(options.checkpointPath);
    if (!checkpoint) {
      throw new RecorderError("checkpoint_corrupt", "Recorder checkpoint does not exist");
    }
    if (checkpoint.phase === "recording") {
      throw new RecorderError(
        "upstream_unavailable",
        "an active recording requires its original live daemon connection",
      );
    }
    return new ExecutionRecorder(undefined, options.checkpointPath, checkpoint, false);
  }

  get phase(): RecorderPhase {
    return this.#checkpoint.phase;
  }

  get lastSequence(): number | null {
    return this.#log.lastSequence;
  }

  get checkpoint(): RecorderCheckpoint {
    const checkpoint = this.#checkpoint.phase === "recording"
      ? { ...this.#checkpoint, events: this.#log.events }
      : this.#checkpoint;
    return structuredClone(checkpoint);
  }

  bundleSource(): BundleSourceFile {
    if (this.#checkpoint.phase === "recording") {
      throw new RecorderError("session_not_ended", "recording has not been sealed");
    }
    return {
      eventProtocolVersion: structuredClone(this.#checkpoint.eventProtocolVersion),
      sessionExport: {
        events: structuredClone(this.#checkpoint.events) as TestEvent[],
        session: structuredClone(this.#checkpoint.session),
      },
    };
  }

  async captureOnce(options: RecorderCaptureOptions = {}): Promise<RecorderCaptureResult> {
    throwIfAborted(options.signal);
    if (this.#checkpoint.phase !== "recording") {
      return {
        accepted: 0,
        duplicates: 0,
        lastSequence: this.#log.lastSequence,
        phase: this.#checkpoint.phase,
      };
    }
    if (this.#log.terminal) {
      await this.seal(options);
      return {
        accepted: 0,
        duplicates: 0,
        lastSequence: this.#log.lastSequence,
        phase: this.#checkpoint.phase,
      };
    }

    if (!this.#eventSource) {
      throw new RecorderError("upstream_unavailable", "Recorder has no live event source");
    }
    let acceptedTotal = 0;
    let duplicateTotal = 0;
    let pageLimit = RECORDER_EVENT_PAGE_SIZE;
    while (!this.#log.terminal) {
      const afterSequence = this.#log.lastSequence;
      const page = await loadAdaptivePage({
        afterSequence,
        initialLimit: pageLimit,
        load: async (limit) =>
          await this.#eventSource!.listEvents(
            this.#checkpoint.sessionId,
            afterSequence,
            limit,
          ),
        ...(options.signal ? { signal: options.signal } : {}),
        tooLargeMessage: "one Session event cannot fit in the bounded events.list response",
        unavailableMessage: "Recorder could not list Session events",
      });
      pageLimit = page.limit;
      const values = page.value;
      throwIfAborted(options.signal);
      if (!Array.isArray(values) || values.length > pageLimit) {
        throw new RecorderError(
          "invalid_event",
          `events.list result must be an array of at most ${pageLimit} events`,
        );
      }
      const prepared = this.#log.prepareBatch(values);
      const accepted = prepared.result;
      acceptedTotal += accepted.accepted;
      duplicateTotal += accepted.duplicates;
      if (accepted.accepted > 0) {
        const appended = await appendRecorderCheckpointPage(
          this.#checkpointPath,
          this.#checkpoint.revision,
          this.#checkpoint,
          prepared.acceptedEvents,
          { ...(options.signal ? { signal: options.signal } : {}) },
        );
        try {
          this.#log.commitPreparedBatch(prepared);
        } catch (cause) {
          const recovered = await loadRecorderCheckpoint(this.#checkpointPath);
          if (recovered === null) {
            throw cause;
          }
          this.#checkpoint = recovered;
          this.#log = EventLog.replay(
            recovered.sessionId,
            recovered.events,
            recovered.eventProtocolVersion,
          );
          throw cause;
        }
        // Recording pages live in the append-only journal. Avoid materializing
        // a growing checkpoint snapshot until a caller requests it or sealing
        // performs the one compaction write.
        this.#checkpoint = {
          ...this.#checkpoint,
          revision: appended.revision,
        };
      }
      if (values.length < pageLimit || this.#log.terminal) {
        break;
      }
      if (this.#log.lastSequence === afterSequence) {
        throw new RecorderError(
          "invalid_event",
          "a full events.list page did not advance the Recorder sequence",
        );
      }
    }
    if (this.#log.terminal) {
      await this.seal(options);
    }
    return {
      accepted: acceptedTotal,
      duplicates: duplicateTotal,
      lastSequence: this.#log.lastSequence,
      phase: this.#checkpoint.phase,
    };
  }

  async captureUntilSealed(
    options: RecorderCaptureUntilOptions = {},
  ): Promise<BundleSourceFile> {
    const pollIntervalMs = options.pollIntervalMs ?? 50;
    if (!Number.isSafeInteger(pollIntervalMs) || pollIntervalMs < 1 || pollIntervalMs > 60_000) {
      throw new RangeError("pollIntervalMs must be a safe integer between 1 and 60000");
    }
    while (this.#checkpoint.phase === "recording") {
      const result = await this.captureOnce(options);
      if (result.phase === "recording" && result.accepted === 0) {
        await waitForPoll(pollIntervalMs, options.signal);
      }
    }
    return this.bundleSource();
  }

  async #validatePaginatedSessionExport(
    exportSessionPage: NonNullable<RecorderEventSource["exportSessionPage"]>,
    recording: RecordingCheckpoint,
    signal: AbortSignal | undefined,
  ): Promise<SessionInfo> {
    let afterSequence: number | null = null;
    let offset = 0;
    let pageLimit = RECORDER_EVENT_PAGE_SIZE;
    let authoritativeSession: SessionInfo | undefined;
    while (true) {
      const page = await loadAdaptivePage({
        afterSequence,
        initialLimit: pageLimit,
        load: async (limit) =>
          await exportSessionPage.call(
            this.#eventSource,
            recording.sessionId,
            afterSequence,
            limit,
          ),
        ...(signal ? { signal } : {}),
        tooLargeMessage:
          "one Session event cannot fit in the bounded session.export response",
        unavailableMessage: "Recorder could not export the Session page",
      });
      pageLimit = page.limit;
      throwIfAborted(signal);
      const validated = validateSessionExportPage(
        page.value,
        recording.events,
        offset,
        pageLimit,
        authoritativeSession,
      );
      authoritativeSession ??= validated.session;
      offset += validated.eventCount;
      if (validated.nextAfterSequence === null) {
        if (offset !== recording.events.length) {
          throw new RecorderError(
            "session_export_mismatch",
            "session.export ended before the durable Recorder log",
          );
        }
        return authoritativeSession;
      }
      afterSequence = validated.nextAfterSequence;
    }
  }

  async seal(options: RecorderCaptureOptions = {}): Promise<BundleSourceFile> {
    let mayRetryForeignFailure = true;
    while (true) {
      throwIfAborted(options.signal);
      let operation = this.#sealInFlight;
      const ownsOperation = operation === undefined;
      if (!operation) {
        operation = this.#sealOnce(options);
        this.#sealInFlight = operation;
        const clear = (): void => {
          if (this.#sealInFlight === operation) {
            this.#sealInFlight = undefined;
          }
        };
        operation.then(clear, clear);
      }
      try {
        return await awaitSealOperation(operation, options.signal);
      } catch (error) {
        throwIfAborted(options.signal);
        if (
          !ownsOperation
          && mayRetryForeignFailure
          && this.#checkpoint.phase === "recording"
        ) {
          mayRetryForeignFailure = false;
          if (this.#sealInFlight === operation) {
            this.#sealInFlight = undefined;
          }
          continue;
        }
        throw error;
      }
    }
  }

  async #sealOnce(options: RecorderCaptureOptions): Promise<BundleSourceFile> {
    if (this.#checkpoint.phase !== "recording") {
      return this.bundleSource();
    }
    const recording: RecordingCheckpoint = {
      ...this.#checkpoint,
      events: this.#log.events,
    };
    if (!this.#log.terminal || this.#log.openActionCount !== 0) {
      throw new RecorderError("session_not_ended", "Session event stream is not terminal");
    }
    if (!this.#eventSource) {
      throw new RecorderError("upstream_unavailable", "Recorder has no live event source");
    }
    let authoritativeSession: SessionInfo;
    const exportSessionPage = this.#eventSource.exportSessionPage;
    if (this.#supportsPaginatedSessionExport && exportSessionPage) {
      authoritativeSession = await this.#validatePaginatedSessionExport(
        exportSessionPage,
        recording,
        options.signal,
      );
    } else {
      let value: unknown;
      try {
        value = await this.#eventSource.exportSession(recording.sessionId);
      } catch (cause) {
        throwIfAborted(options.signal);
        throw new RecorderError("upstream_unavailable", "Recorder could not export the Session", {
          cause,
        });
      }
      throwIfAborted(options.signal);
      authoritativeSession = validateSessionExport(
        value,
        recording.sessionId,
        recording.events,
        recording.eventProtocolVersion,
      ).session;
    }
    const source = validateBundleSourceBounds({
      eventProtocolVersion: recording.eventProtocolVersion,
      sessionExport: {
        events: recording.events,
        session: authoritativeSession,
      },
    });
    if (
      this.#checkpoint.phase !== "recording"
      || this.#checkpoint.revision !== recording.revision
    ) {
      if (
        this.#checkpoint.phase !== "recording"
        && isDeepStrictEqual(this.bundleSource(), source)
      ) {
        return this.bundleSource();
      }
      throw new RecorderError(
        "checkpoint_conflict",
        "Recorder checkpoint changed while the Session export was being verified",
      );
    }
    const next: SealedCheckpoint = {
      format: RECORDER_CHECKPOINT_FORMAT,
      version: RECORDER_CHECKPOINT_VERSION,
      revision: recording.revision + 1,
      phase: "sealed",
      sessionId: recording.sessionId,
      eventProtocolVersion: recording.eventProtocolVersion,
      events: recording.events,
      session: authoritativeSession,
    };
    this.#checkpoint = await commitRecorderCheckpoint(
      this.#checkpointPath,
      recording.revision,
      next,
      { ...(options.signal ? { signal: options.signal } : {}) },
    );
    return this.bundleSource();
  }

  async publishSource(path: string, options: RecorderCaptureOptions = {}): Promise<void> {
    const source = this.bundleSource() as BundleSourceFile;
    try {
      await publishBundleSource(path, source, {
        ...(options.signal ? { signal: options.signal } : {}),
      });
    } catch (error) {
      if (!(error instanceof RecorderError) || error.code !== "source_conflict") {
        throw error;
      }
      const existing = await readBundleSource(path);
      if (!isDeepStrictEqual(existing, source)) {
        throw error;
      }
    }
  }

  async finalize(options: RecorderFinalizeOptions): Promise<RecorderBundleReceipt> {
    const completedReceipt =
      this.#checkpoint.phase === "completed"
        ? structuredClone(this.#checkpoint.bundle)
        : undefined;
    const source = this.bundleSource();
    await this.publishSource(options.sourcePath, {
      ...(options.signal ? { signal: options.signal } : {}),
    });
    const receipt = await exportAndValidateBundle({
      ...options,
      source,
    });
    if (completedReceipt) {
      if (!isDeepStrictEqual(receipt, completedReceipt)) {
        throw new RecorderError(
          "bundle_summary_mismatch",
          "validated Bundle differs from the completed checkpoint receipt",
        );
      }
      return structuredClone(receipt);
    }
    if (this.#checkpoint.phase !== "sealed") {
      throw new RecorderError("checkpoint_conflict", "Recorder checkpoint changed during finalize");
    }
    const next: CompletedCheckpoint = {
      format: RECORDER_CHECKPOINT_FORMAT,
      version: RECORDER_CHECKPOINT_VERSION,
      revision: this.#checkpoint.revision + 1,
      phase: "completed",
      sessionId: this.#checkpoint.sessionId,
      eventProtocolVersion: this.#checkpoint.eventProtocolVersion,
      events: this.#checkpoint.events,
      session: this.#checkpoint.session,
      bundle: receipt,
    };
    this.#checkpoint = await commitRecorderCheckpoint(
      this.#checkpointPath,
      this.#checkpoint.revision,
      next,
      { ...(options.signal ? { signal: options.signal } : {}) },
    );
    return structuredClone(receipt);
  }
}
