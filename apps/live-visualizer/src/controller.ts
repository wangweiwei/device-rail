import type {
  DeviceRailEventStream,
  EventStreamItem,
  EventStreamOptions,
} from "@devicerail/client";
import {
  DeviceRailClientError,
  EventStreamAbortedError,
  EventStreamClosedError,
  EventStreamCursorError,
  EventStreamQueueOverflowError,
  EventStreamRemoteTerminationError,
  RpcRemoteError,
} from "@devicerail/client";
import {
  LiveTimeline,
  LiveTimelineError,
  type LiveTimelineLimits,
  type LiveTimelineState,
  type TimelinePage,
} from "@devicerail/live-visualizer";
import type {
  EventStreamCursor,
  EventsStreamTerminalParams,
  EventsSubscribeParams,
} from "@devicerail/protocol";

import type {
  LiveVisualizerFilter,
  LiveVisualizerHttpSource,
  LiveVisualizerPageRequest,
} from "./http-host.js";

const DEFAULT_MAX_RESUME_ATTEMPTS = 4;
const DEFAULT_RESUME_INITIAL_DELAY_MS = 100;
const DEFAULT_RESUME_MAX_DELAY_MS = 1_000;
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu;

export interface LiveVisualizerStream extends AsyncIterable<EventStreamItem> {
  readonly terminal: EventsStreamTerminalParams | undefined;
  cancel(): void;
}

/** Already-owned client port. The host never closes or otherwise controls it. */
export interface LiveVisualizerClient {
  readonly state: string;
  openEventStream(
    params: EventsSubscribeParams,
    options?: EventStreamOptions,
  ): Promise<DeviceRailEventStream | LiveVisualizerStream>;
}

export interface LiveVisualizerControllerLimits {
  readonly maxResumeAttempts?: number;
  readonly resumeInitialDelayMs?: number;
  readonly resumeMaxDelayMs?: number;
  readonly streamMaxMessageBytes?: number;
  readonly streamMaxQueuedBytes?: number;
  readonly streamMaxQueuedEvents?: number;
  readonly timeline?: Partial<LiveTimelineLimits>;
}

interface ValidatedControllerLimits {
  readonly maxResumeAttempts: number;
  readonly resumeInitialDelayMs: number;
  readonly resumeMaxDelayMs: number;
  readonly stream: Pick<
    EventStreamOptions,
    "maxMessageBytes" | "maxQueuedBytes" | "maxQueuedEvents"
  >;
  readonly timeline: Partial<LiveTimelineLimits> | undefined;
}

export type LiveVisualizerPumpPhase =
  | "idle"
  | "connecting"
  | "streaming"
  | "backingOff"
  | "sessionEnded"
  | "viewerCapacityExceeded"
  | "failed"
  | "stopped";

export interface LiveVisualizerControllerState extends Omit<LiveTimelineState, "revision"> {
  readonly modelRevision: number;
  readonly revision: number;
  readonly transport: {
    readonly lastErrorCode?: string;
    readonly phase: LiveVisualizerPumpPhase;
    readonly resumeAttempt: number;
  };
}

export interface LiveVisualizerControllerPage extends Omit<TimelinePage, "revision"> {
  readonly modelRevision: number;
  readonly revision: number;
}

function boundedInteger(
  value: number | undefined,
  fallback: number,
  maximum: number,
  name: string,
  allowZero = false,
): number {
  const selected = value ?? fallback;
  if (
    !Number.isSafeInteger(selected) ||
    selected < (allowZero ? 0 : 1) ||
    selected > maximum
  ) {
    throw new RangeError(
      `${name} must be a ${allowZero ? "non-negative" : "positive"} safe integer no greater than ${maximum}`,
    );
  }
  return selected;
}

function validateLimits(limits: LiveVisualizerControllerLimits = {}): ValidatedControllerLimits {
  const resumeInitialDelayMs = boundedInteger(
    limits.resumeInitialDelayMs,
    DEFAULT_RESUME_INITIAL_DELAY_MS,
    10_000,
    "resumeInitialDelayMs",
  );
  const resumeMaxDelayMs = boundedInteger(
    limits.resumeMaxDelayMs,
    DEFAULT_RESUME_MAX_DELAY_MS,
    30_000,
    "resumeMaxDelayMs",
  );
  if (resumeMaxDelayMs < resumeInitialDelayMs) {
    throw new RangeError("resumeMaxDelayMs must be no less than resumeInitialDelayMs");
  }
  const stream: ValidatedControllerLimits["stream"] = {
    ...(limits.streamMaxMessageBytes === undefined
      ? {}
      : {
          maxMessageBytes: boundedInteger(
            limits.streamMaxMessageBytes,
            limits.streamMaxMessageBytes,
            4 * 1024 * 1024,
            "streamMaxMessageBytes",
          ),
        }),
    ...(limits.streamMaxQueuedBytes === undefined
      ? {}
      : {
          maxQueuedBytes: boundedInteger(
            limits.streamMaxQueuedBytes,
            limits.streamMaxQueuedBytes,
            16 * 1024 * 1024,
            "streamMaxQueuedBytes",
          ),
        }),
    ...(limits.streamMaxQueuedEvents === undefined
      ? {}
      : {
          maxQueuedEvents: boundedInteger(
            limits.streamMaxQueuedEvents,
            limits.streamMaxQueuedEvents,
            1_024,
            "streamMaxQueuedEvents",
          ),
        }),
  };
  return {
    maxResumeAttempts: boundedInteger(
      limits.maxResumeAttempts,
      DEFAULT_MAX_RESUME_ATTEMPTS,
      16,
      "maxResumeAttempts",
      true,
    ),
    resumeInitialDelayMs,
    resumeMaxDelayMs,
    stream,
    timeline: limits.timeline,
  };
}

function cloneCursor(cursor: EventStreamCursor): EventStreamCursor {
  return { ...cursor };
}

function errorCode(error: unknown): string {
  if (error instanceof LiveTimelineError || error instanceof DeviceRailClientError) {
    return error.code;
  }
  return "live_visualizer_failed";
}

function isRetryable(error: unknown, client: LiveVisualizerClient): boolean {
  if (error instanceof EventStreamRemoteTerminationError) {
    if (error.termination.reason === "slowConsumer") return true;
    return error.termination.reason === "serverShutdown" && client.state === "ready";
  }
  if (error instanceof RpcRemoteError) {
    const code = error.rpcError.data.code;
    return (
      client.state === "ready" &&
      (code === "stream_server_shutdown" || code === "stream_slow_consumer")
    );
  }
  return (
    client.state === "ready" &&
    (error instanceof EventStreamClosedError || error instanceof EventStreamQueueOverflowError)
  );
}

function delay(ms: number, signal: AbortSignal): Promise<boolean> {
  if (signal.aborted) return Promise.resolve(false);
  return new Promise<boolean>((resolve) => {
    const finish = (elapsed: boolean): void => {
      clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
      resolve(elapsed);
    };
    const onAbort = (): void => finish(false);
    const timer = setTimeout(() => finish(true), ms);
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

export class LiveVisualizerController implements LiveVisualizerHttpSource {
  readonly #abort = new AbortController();
  readonly #client: LiveVisualizerClient;
  readonly #limits: ValidatedControllerLimits;
  readonly #listeners = new Set<(revision: number) => void>();
  readonly #model: LiveTimeline;
  readonly #sessionId: string;

  #activeStream: LiveVisualizerStream | undefined;
  #confirmedCursor: EventStreamCursor | undefined;
  #streamEpoch: string | undefined;
  #lastErrorCode: string | undefined;
  #phase: LiveVisualizerPumpPhase = "idle";
  #pump: Promise<void> | undefined;
  #resumeAttempt = 0;
  #revision = 0;
  #stopPromise: Promise<void> | undefined;

  constructor(
    client: LiveVisualizerClient,
    sessionId: string,
    limits: LiveVisualizerControllerLimits = {},
  ) {
    this.#client = client;
    this.#sessionId = sessionId;
    this.#limits = validateLimits(limits);
    this.#model = new LiveTimeline(
      sessionId,
      this.#limits.timeline === undefined ? {} : { limits: this.#limits.timeline },
    );
  }

  currentRevision(): number {
    return this.#revision;
  }

  page(request: LiveVisualizerPageRequest): LiveVisualizerControllerPage {
    const page = this.#model.page({
      filter: request.filter,
      page: request.page,
      pageSize: request.pageSize,
    });
    return Object.freeze({
      ...page,
      modelRevision: page.revision,
      revision: this.#revision,
    });
  }

  start(): void {
    if (this.#pump) return;
    if (this.#abort.signal.aborted) {
      throw new Error("a stopped live visualizer controller cannot be started");
    }
    this.#setPhase("connecting");
    this.#pump = this.#run();
  }

  state(): LiveVisualizerControllerState {
    const state = this.#model.state();
    return Object.freeze({
      ...state,
      modelRevision: state.revision,
      revision: this.#revision,
      transport: Object.freeze({
        ...(this.#lastErrorCode === undefined ? {} : { lastErrorCode: this.#lastErrorCode }),
        phase: this.#phase,
        resumeAttempt: this.#resumeAttempt,
      }),
    });
  }

  subscribe(listener: (revision: number) => void): () => void {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  async stop(): Promise<void> {
    if (this.#stopPromise) return await this.#stopPromise;
    this.#stopPromise = (async () => {
      this.#abort.abort();
      try {
        this.#activeStream?.cancel();
      } catch {
        // The shared AbortSignal remains authoritative for teardown.
      }
      if (this.#pump) await this.#pump;
      if (this.#phase !== "sessionEnded" && this.#phase !== "viewerCapacityExceeded" && this.#phase !== "failed") {
        this.#model.stop();
        this.#setPhase("stopped");
      }
      this.#listeners.clear();
    })();
    return await this.#stopPromise;
  }

  async waitUntilStopped(): Promise<void> {
    if (this.#pump) await this.#pump;
  }

  async #run(): Promise<void> {
    try {
      while (!this.#abort.signal.aborted) {
        try {
          const stream = await this.#client.openEventStream(
            {
              sessionId: this.#sessionId,
              ...(this.#confirmedCursor
                ? { afterCursor: cloneCursor(this.#confirmedCursor) }
                : {}),
            },
            {
              ...this.#limits.stream,
              originPolicy: { kind: "absent" },
              signal: this.#abort.signal,
            },
          );
          this.#activeStream = stream;
          this.#lastErrorCode = undefined;
          this.#setPhase("streaming");
          for await (const item of stream) {
            this.#accept(item);
            this.#resumeAttempt = 0;
          }
          this.#activeStream = undefined;
          if (this.#abort.signal.aborted) break;
          if (stream.terminal?.termination.reason !== "sessionEnded") {
            throw new EventStreamClosedError(1006, "event iterator ended without sessionEnded");
          }
          this.#setPhase("sessionEnded");
          return;
        } catch (error) {
          this.#activeStream = undefined;
          if (this.#abort.signal.aborted || error instanceof EventStreamAbortedError) break;
          if (
            error instanceof LiveTimelineError &&
            error.code === "viewer_capacity_exceeded"
          ) {
            this.#lastErrorCode = error.code;
            this.#setPhase("viewerCapacityExceeded");
            return;
          }
          if (!isRetryable(error, this.#client) || this.#resumeAttempt >= this.#limits.maxResumeAttempts) {
            this.#lastErrorCode = errorCode(error);
            this.#model.fail();
            this.#setPhase("failed");
            return;
          }
          this.#resumeAttempt += 1;
          this.#lastErrorCode = errorCode(error);
          this.#setPhase("backingOff");
          const backoff = Math.min(
            this.#limits.resumeMaxDelayMs,
            this.#limits.resumeInitialDelayMs * 2 ** (this.#resumeAttempt - 1),
          );
          if (!(await delay(backoff, this.#abort.signal))) break;
          this.#setPhase("connecting");
        }
      }
    } finally {
      this.#activeStream = undefined;
    }
  }

  #accept(item: EventStreamItem): void {
    const inputCursor = cloneCursor(item.cursor);
    if (
      !Number.isSafeInteger(inputCursor.sequence) ||
      inputCursor.sequence < 1 ||
      inputCursor.sequence !== item.event.sequence ||
      inputCursor.sessionId !== this.#sessionId ||
      !UUID_PATTERN.test(inputCursor.streamEpoch) ||
      item.event.sessionId !== this.#sessionId ||
      (this.#streamEpoch !== undefined && inputCursor.streamEpoch !== this.#streamEpoch)
    ) {
      throw new EventStreamCursorError(
        "event stream item cursor does not identify its exact Session event",
      );
    }
    // This ordering is the acknowledgement boundary. Do not combine or reorder.
    const prepared = this.#model.prepare(item.event);
    const commit = this.#model.commit(prepared);
    const cursor = item.confirm();
    if (
      cursor.sequence !== inputCursor.sequence ||
      cursor.sessionId !== inputCursor.sessionId ||
      cursor.streamEpoch !== inputCursor.streamEpoch
    ) {
      throw new EventStreamCursorError(
        "event stream confirmation returned a cursor different from the committed event",
      );
    }
    this.#model.confirm(commit);
    this.#streamEpoch = cursor.streamEpoch;
    this.#confirmedCursor = cloneCursor(cursor);
    this.#publish();
  }

  #publish(): void {
    if (this.#revision >= Number.MAX_SAFE_INTEGER) {
      this.#lastErrorCode = "viewer_revision_exhausted";
      this.#model.fail();
      this.#phase = "failed";
      return;
    }
    this.#revision += 1;
    for (const listener of this.#listeners) {
      try {
        listener(this.#revision);
      } catch {
        // A browser invalidation consumer cannot affect model confirmation.
      }
    }
  }

  #setPhase(phase: LiveVisualizerPumpPhase): void {
    if (this.#phase === phase) return;
    this.#phase = phase;
    this.#publish();
  }
}
