import type { TestEvent } from "@devicerail/protocol";

import { LiveTimelineError } from "./errors.js";
import {
  LIVE_TIMELINE_MAX_PAGE_SIZE,
  normalizeLiveTimelineLimits,
} from "./limits.js";
import { presentEvent } from "./presentation.js";
import { canonicalFingerprint, deepFreeze } from "./sanitize.js";
import type {
  LiveTimelineLimits,
  LiveTimelineState,
  LiveTimelineStatus,
  PreparedTimelineEvent,
  TimelineCommit,
  TimelineConfirmation,
  TimelineEntry,
  TimelineFilter,
  TimelinePage,
  TimelinePageRequest,
} from "./types.js";

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu;

interface EventLifecycle {
  readonly callId?: string;
  readonly eventId: string;
  readonly frameCount?: number;
  readonly frameIndex?: number;
  readonly mediaType?: string;
  readonly streamId?: string;
  readonly type: TestEvent["payload"]["type"];
}

interface PreparedMetadata {
  readonly byteSize: number;
  readonly entry: TimelineEntry;
  readonly fingerprint: string;
  readonly lifecycle: EventLifecycle;
  readonly sequence: number;
}

interface PendingEvent extends PreparedMetadata {
  readonly generation: number;
}

interface CommitMetadata {
  readonly fingerprint: string;
  readonly generation: number;
  readonly sequence: number;
}

export interface LiveTimelineOptions {
  readonly limits?: Partial<LiveTimelineLimits>;
}

function positiveInteger(value: number, name: string, maximum?: number): number {
  if (!Number.isSafeInteger(value) || value <= 0 || (maximum !== undefined && value > maximum)) {
    throw new LiveTimelineError("invalid_page", `${name} is outside its supported range`, {
      details: { ...(maximum === undefined ? {} : { maximum }), value },
    });
  }
  return value;
}

function entryMatchesFilter(entry: TimelineEntry, filter: TimelineFilter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "observations":
      return (
        entry.category === "observation" ||
        entry.presentation.type === "mediaFrameCaptured"
      );
    case "actions":
      return entry.category === "action";
    case "errors":
      return entry.category === "error";
    case "verdicts":
      return entry.category === "verdict";
  }
}

function eventLifecycle(event: TestEvent): EventLifecycle {
  const payload = event.payload;
  if (payload.type === "actionStarted") {
    return Object.freeze({ callId: payload.call.id, eventId: event.eventId, type: payload.type });
  }
  if (payload.type === "actionCompleted") {
    if (
      payload.outcome.outcome === "succeeded" &&
      payload.outcome.result.callId !== payload.callId
    ) {
      throw new LiveTimelineError(
        "invalid_event",
        "Action result.callId does not match the completed Action callId",
      );
    }
    return Object.freeze({ callId: payload.callId, eventId: event.eventId, type: payload.type });
  }
  if (payload.type === "mediaStreamStarted") {
    return Object.freeze({
      eventId: event.eventId,
      mediaType: payload.stream.mediaType,
      streamId: payload.stream.id,
      type: payload.type,
    });
  }
  if (payload.type === "mediaFrameCaptured") {
    return Object.freeze({
      eventId: event.eventId,
      frameIndex: payload.frame.frameIndex,
      mediaType: payload.frame.evidence.mediaType,
      streamId: payload.frame.streamId,
      type: payload.type,
    });
  }
  if (payload.type === "mediaStreamEnded") {
    return Object.freeze({
      eventId: event.eventId,
      frameCount: payload.frameCount,
      streamId: payload.streamId,
      type: payload.type,
    });
  }
  return Object.freeze({ eventId: event.eventId, type: payload.type });
}

/**
 * Bounded, protocol-only model for a live Session timeline.
 *
 * `commit` reserves a sanitized presentation but does not publish it. The
 * host must durably confirm the daemon item and then call `confirm`; only that
 * final call advances the public revision and confirmed sequence.
 */
export class LiveTimeline {
  readonly #actionEntries: TimelineEntry[] = [];
  readonly #commits = new WeakMap<object, CommitMetadata>();
  readonly #entries: TimelineEntry[] = [];
  readonly #errorEntries: TimelineEntry[] = [];
  readonly #eventIds = new Set<string>();
  readonly #inFlightActionIds = new Set<string>();
  readonly #mediaStreams = new Map<string, { readonly mediaType: string; readonly nextFrameIndex: number }>();
  readonly #limits: LiveTimelineLimits;
  readonly #prepared = new WeakMap<object, PreparedMetadata>();
  readonly #observationEntries: TimelineEntry[] = [];
  readonly #seenActionIds = new Set<string>();
  readonly #seenMediaStreamIds = new Set<string>();
  readonly #sessionId: string;
  readonly #verdictEntries: TimelineEntry[] = [];
  #confirmedSequence: number | undefined;
  #generation = 0;
  #pending: PendingEvent | undefined;
  #revision = 0;
  #sessionStarted = false;
  #status: LiveTimelineStatus = "active";
  #totalBytes = 0;

  constructor(sessionId: string, options: LiveTimelineOptions = {}) {
    if (typeof sessionId !== "string" || !UUID_PATTERN.test(sessionId)) {
      throw new LiveTimelineError("invalid_event", "timeline sessionId is invalid");
    }
    this.#sessionId = sessionId;
    this.#limits = normalizeLiveTimelineLimits(options.limits);
  }

  get limits(): LiveTimelineLimits {
    return this.#limits;
  }

  get revision(): number {
    return this.#revision;
  }

  get status(): LiveTimelineStatus {
    return this.#status;
  }

  prepare(event: TestEvent): PreparedTimelineEvent {
    if (this.#status !== "active") {
      throw new LiveTimelineError("timeline_closed", `timeline is ${this.#status}`);
    }
    let canonical: string;
    let fingerprint: string;
    try {
      ({ canonical, fingerprint } = canonicalFingerprint(event, this.#limits));
    } catch (error) {
      if (
        error instanceof LiveTimelineError &&
        error.code === "viewer_capacity_exceeded" &&
        this.#status === "active"
      ) {
        this.#status = "viewerCapacityExceeded";
      }
      throw error;
    }
    // Present the exact canonical snapshot that was fingerprinted. The caller
    // may mutate its input immediately after this method returns, and hostile
    // accessor/proxy state must never make fingerprint and presentation drift.
    const snapshot = JSON.parse(canonical) as TestEvent;
    const presentation = presentEvent(snapshot, this.#limits);
    if (presentation.sessionId !== this.#sessionId) {
      throw new LiveTimelineError("invalid_event", "event belongs to another Session");
    }
    const byteSize = Buffer.byteLength(JSON.stringify(presentation));
    const prepared: PreparedTimelineEvent = {
      byteSize,
      fingerprint,
      presentation,
      sequence: presentation.sequence,
    };
    const frozen = deepFreeze(prepared);
    this.#prepared.set(frozen, Object.freeze({
      byteSize,
      entry: presentation,
      fingerprint,
      lifecycle: eventLifecycle(snapshot),
      sequence: presentation.sequence,
    }));
    return frozen;
  }

  commit(prepared: PreparedTimelineEvent): TimelineCommit {
    if (prepared === null || typeof prepared !== "object") {
      throw new LiveTimelineError("stale_prepared_event", "prepared event is invalid");
    }
    const internal = this.#prepared.get(prepared);
    if (!internal) {
      throw new LiveTimelineError(
        "stale_prepared_event",
        "prepared event was not produced by this timeline",
      );
    }
    if (this.#status !== "active") {
      throw new LiveTimelineError("timeline_closed", `timeline is ${this.#status}`);
    }
    if (this.#pending) {
      if (
        internal.sequence === this.#pending.sequence &&
        internal.fingerprint === this.#pending.fingerprint
      ) {
        return this.#commitToken(this.#pending, "pendingReplay");
      }
      if (internal.sequence === this.#pending.sequence) {
        throw new LiveTimelineError(
          "event_conflict",
          "an unconfirmed sequence was replayed with different content",
          { details: { sequence: internal.sequence } },
        );
      }
      throw new LiveTimelineError(
        "pending_confirmation",
        "the previous committed event must be confirmed before another sequence",
        {
          details: {
            pendingSequence: this.#pending.sequence,
            receivedSequence: internal.sequence,
          },
        },
      );
    }

    const expected = (this.#confirmedSequence ?? 0) + 1;
    if (internal.sequence !== expected) {
      throw new LiveTimelineError("sequence_gap", "event sequence is not contiguous", {
        details: { expected, received: internal.sequence },
      });
    }
    this.#validateLifecycle(internal.lifecycle, internal.sequence);
    const capacityExceeded =
      internal.byteSize > this.#limits.maxEventBytes ||
      this.#entries.length >= this.#limits.maxEvents ||
      this.#totalBytes + internal.byteSize > this.#limits.maxTotalBytes;
    if (capacityExceeded) {
      this.#status = "viewerCapacityExceeded";
      throw new LiveTimelineError(
        "viewer_capacity_exceeded",
        "live timeline capacity was reached; use the offline Bundle Viewer",
        {
          details: {
            eventBytes: internal.byteSize,
            eventCount: this.#entries.length,
            totalBytes: this.#totalBytes,
          },
        },
      );
    }

    this.#generation += 1;
    this.#pending = Object.freeze({
      byteSize: internal.byteSize,
      entry: internal.entry,
      fingerprint: internal.fingerprint,
      generation: this.#generation,
      lifecycle: internal.lifecycle,
      sequence: internal.sequence,
    });
    return this.#commitToken(this.#pending, "committed");
  }

  confirm(commit: TimelineCommit): TimelineConfirmation {
    if (commit === null || typeof commit !== "object") {
      throw new LiveTimelineError("invalid_confirmation", "commit token is invalid");
    }
    const internal = this.#commits.get(commit);
    if (!internal || !this.#pending) {
      throw new LiveTimelineError("invalid_confirmation", "commit token is not pending here");
    }
    if (
      internal.generation !== this.#pending.generation ||
      internal.sequence !== this.#pending.sequence ||
      internal.fingerprint !== this.#pending.fingerprint
    ) {
      throw new LiveTimelineError("invalid_confirmation", "commit token is stale or mismatched");
    }

    const pending = this.#pending;
    this.#applyLifecycle(pending.lifecycle);
    this.#entries.push(pending.entry);
    this.#indexEntry(pending.entry);
    this.#totalBytes += pending.byteSize;
    this.#confirmedSequence = pending.sequence;
    this.#pending = undefined;
    this.#commits.delete(commit);
    this.#revision += 1;
    if (pending.entry.presentation.type === "sessionEnded") {
      this.#status = "sessionEnded";
    }
    return Object.freeze({
      revision: this.#revision,
      sequence: pending.sequence,
      status: this.#status,
    });
  }

  fail(): LiveTimelineState {
    if (this.#status === "active") {
      this.#pending = undefined;
      this.#status = "failed";
    }
    return this.state();
  }

  stop(): LiveTimelineState {
    if (this.#status === "active") {
      this.#pending = undefined;
      this.#status = "stopped";
    }
    return this.state();
  }

  state(): LiveTimelineState {
    return deepFreeze({
      ...(this.#confirmedSequence === undefined
        ? {}
        : { confirmedSequence: this.#confirmedSequence }),
      eventCount: this.#entries.length,
      ...(this.#pending
        ? {
            pending: {
              fingerprint: this.#pending.fingerprint,
              sequence: this.#pending.sequence,
            },
          }
        : {}),
      revision: this.#revision,
      sessionId: this.#sessionId,
      status: this.#status,
      totalBytes: this.#totalBytes,
    });
  }

  page(request: TimelinePageRequest = {}): TimelinePage {
    const filter = request.filter ?? "all";
    if (
      filter !== "all" &&
      filter !== "observations" &&
      filter !== "actions" &&
      filter !== "errors" &&
      filter !== "verdicts"
    ) {
      throw new LiveTimelineError("invalid_page", "timeline filter is invalid");
    }
    const page = positiveInteger(request.page ?? 1, "page");
    const pageSize = positiveInteger(
      request.pageSize ?? LIVE_TIMELINE_MAX_PAGE_SIZE,
      "pageSize",
      LIVE_TIMELINE_MAX_PAGE_SIZE,
    );
    const entries = this.#entriesForFilter(filter);
    const totalItems = entries.length;
    const totalPages = Math.max(1, Math.ceil(totalItems / pageSize));
    if (page > totalPages) {
      throw new LiveTimelineError("invalid_page", "page exceeds the available page count", {
        details: { page, totalPages },
      });
    }
    const start = (page - 1) * pageSize;
    if (!Number.isSafeInteger(start)) {
      throw new LiveTimelineError("invalid_page", "page offset exceeds safe integer range");
    }
    const items = Object.freeze(entries.slice(start, start + pageSize));
    return Object.freeze({
      filter,
      items,
      page,
      pageSize,
      revision: this.#revision,
      status: this.#status,
      totalItems,
      totalPages,
    });
  }

  #indexEntry(entry: TimelineEntry): void {
    if (entryMatchesFilter(entry, "observations")) {
      this.#observationEntries.push(entry);
    } else if (entryMatchesFilter(entry, "actions")) {
      this.#actionEntries.push(entry);
    } else if (entryMatchesFilter(entry, "errors")) {
      this.#errorEntries.push(entry);
    } else if (entryMatchesFilter(entry, "verdicts")) {
      this.#verdictEntries.push(entry);
    }
  }

  #entriesForFilter(filter: TimelineFilter): readonly TimelineEntry[] {
    switch (filter) {
      case "all":
        return this.#entries;
      case "observations":
        return this.#observationEntries;
      case "actions":
        return this.#actionEntries;
      case "errors":
        return this.#errorEntries;
      case "verdicts":
        return this.#verdictEntries;
    }
  }

  #applyLifecycle(lifecycle: EventLifecycle): void {
    this.#eventIds.add(lifecycle.eventId);
    switch (lifecycle.type) {
      case "sessionStarted":
        this.#sessionStarted = true;
        break;
      case "actionStarted":
        this.#seenActionIds.add(lifecycle.callId as string);
        this.#inFlightActionIds.add(lifecycle.callId as string);
        break;
      case "actionCompleted":
        this.#inFlightActionIds.delete(lifecycle.callId as string);
        break;
      case "mediaStreamStarted":
        this.#seenMediaStreamIds.add(lifecycle.streamId as string);
        this.#mediaStreams.set(lifecycle.streamId as string, {
          mediaType: lifecycle.mediaType as string,
          nextFrameIndex: 1,
        });
        break;
      case "mediaFrameCaptured": {
        const stream = this.#mediaStreams.get(lifecycle.streamId as string) as {
          readonly mediaType: string;
          readonly nextFrameIndex: number;
        };
        this.#mediaStreams.set(lifecycle.streamId as string, {
          mediaType: stream.mediaType,
          nextFrameIndex: stream.nextFrameIndex + 1,
        });
        break;
      }
      case "mediaStreamEnded":
        this.#mediaStreams.delete(lifecycle.streamId as string);
        break;
      case "sessionEnded":
      case "observationCaptured":
      case "error":
      case "verdictRecorded":
        break;
    }
  }

  #commitToken(pending: PendingEvent, kind: TimelineCommit["kind"]): TimelineCommit {
    const token: TimelineCommit = Object.freeze({
      fingerprint: pending.fingerprint,
      kind,
      sequence: pending.sequence,
    });
    this.#commits.set(token, Object.freeze({
      fingerprint: pending.fingerprint,
      generation: pending.generation,
      sequence: pending.sequence,
    }));
    return token;
  }

  #validateLifecycle(lifecycle: EventLifecycle, sequence: number): void {
    if (this.#eventIds.has(lifecycle.eventId)) {
      throw new LiveTimelineError("invalid_event", "TestEvent.eventId is duplicated");
    }
    if (sequence === 1 && lifecycle.type !== "sessionStarted") {
      throw new LiveTimelineError("invalid_event", "the first Session event must be sessionStarted");
    }
    if (sequence !== 1 && lifecycle.type === "sessionStarted") {
      throw new LiveTimelineError("invalid_event", "sessionStarted may only be the first event");
    }
    if (!this.#sessionStarted && lifecycle.type !== "sessionStarted") {
      throw new LiveTimelineError("invalid_event", "Session events require a confirmed sessionStarted");
    }
    if (lifecycle.type === "actionStarted") {
      const callId = lifecycle.callId as string;
      if (this.#seenActionIds.has(callId)) {
        throw new LiveTimelineError("invalid_event", "Action callId is duplicated");
      }
    } else if (lifecycle.type === "actionCompleted") {
      const callId = lifecycle.callId as string;
      if (!this.#inFlightActionIds.has(callId)) {
        throw new LiveTimelineError(
          "invalid_event",
          "Action completion has no confirmed matching Action start",
        );
      }
    } else if (lifecycle.type === "mediaStreamStarted") {
      if (this.#seenMediaStreamIds.has(lifecycle.streamId as string)) {
        throw new LiveTimelineError("invalid_event", "media stream id is duplicated");
      }
    } else if (lifecycle.type === "mediaFrameCaptured") {
      const stream = this.#mediaStreams.get(lifecycle.streamId as string);
      if (
        !stream
        || lifecycle.frameIndex !== stream.nextFrameIndex
        || lifecycle.mediaType !== stream.mediaType
      ) {
        throw new LiveTimelineError("invalid_event", "media frame does not match an active stream");
      }
    } else if (lifecycle.type === "mediaStreamEnded") {
      const stream = this.#mediaStreams.get(lifecycle.streamId as string);
      if (!stream || lifecycle.frameCount !== stream.nextFrameIndex - 1) {
        throw new LiveTimelineError("invalid_event", "media stream terminal is inconsistent");
      }
    } else if (
      lifecycle.type === "sessionEnded"
      && (this.#inFlightActionIds.size > 0 || this.#mediaStreams.size > 0)
    ) {
      throw new LiveTimelineError(
        "invalid_event",
        "Session cannot end while Actions or media streams remain in flight",
      );
    }
  }
}
