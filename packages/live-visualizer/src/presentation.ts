import type {
  ActionOutcome,
  AssetRef,
  ErrorInfo,
  Observation,
  RecordedActionCall,
  TestEvent,
  Verdict,
} from "@devicerail/protocol";

import { LiveTimelineError } from "./errors.js";
import { boundedJson, boundedText, deepFreeze } from "./sanitize.js";
import type {
  ActionCompletionPresentation,
  ErrorPresentation,
  LiveTimelineLimits,
  ObservationPresentation,
  ReferenceOnlyEvidence,
  TimelineEntry,
  TimelinePresentation,
} from "./types.js";

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu;

function record(value: unknown, location: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new LiveTimelineError("invalid_event", `${location} must be an object`);
  }
  return value as Record<string, unknown>;
}

function stringValue(value: unknown, location: string): string {
  if (typeof value !== "string") {
    throw new LiveTimelineError("invalid_event", `${location} must be a string`);
  }
  return value;
}

function uuid(value: unknown, location: string): string {
  const text = stringValue(value, location);
  if (!UUID_PATTERN.test(text)) {
    throw new LiveTimelineError("invalid_event", `${location} must be a UUID`);
  }
  return text;
}

function safeInteger(value: unknown, location: string, minimum = 0): number {
  if (!Number.isSafeInteger(value) || (value as number) < minimum) {
    throw new LiveTimelineError("invalid_event", `${location} must be a safe integer`);
  }
  return value as number;
}

function finiteNumber(value: unknown, location: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new LiveTimelineError("invalid_event", `${location} must be finite`);
  }
  return value;
}

function evidence(asset: AssetRef, limits: LiveTimelineLimits): ReferenceOnlyEvidence {
  const value = record(asset, "Evidence");
  // URI is validated as a wire field, then deliberately discarded. It must
  // never cross the live presentation boundary.
  stringValue(value.uri, "Evidence.uri");
  const sha256 = value.sha256;
  return deepFreeze({
    availability: "referenceOnly" as const,
    id: boundedText(stringValue(value.id, "Evidence.id"), limits.maxTextBytes),
    mediaType: boundedText(
      stringValue(value.mediaType, "Evidence.mediaType"),
      limits.maxTextBytes,
    ),
    ...(sha256 === undefined || sha256 === null
      ? {}
      : { sha256: boundedText(stringValue(sha256, "Evidence.sha256"), limits.maxTextBytes) }),
  });
}

function evidenceList(
  value: unknown,
  limits: LiveTimelineLimits,
  location: string,
): { readonly evidence: readonly ReferenceOnlyEvidence[]; readonly omitted: number } {
  if (value === undefined) return Object.freeze({ evidence: Object.freeze([]), omitted: 0 });
  if (!Array.isArray(value)) {
    throw new LiveTimelineError("invalid_event", `${location} must be an array`);
  }
  const kept = value.slice(0, limits.maxEvidencePerEvent).map((asset) =>
    evidence(asset as AssetRef, limits),
  );
  return Object.freeze({
    evidence: Object.freeze(kept),
    omitted: Math.max(0, value.length - kept.length),
  });
}

function observation(value: Observation, limits: LiveTimelineLimits): ObservationPresentation {
  const source = record(value, "Observation");
  const viewport = record(source.viewport, "Observation.viewport");
  const screenshotOmission = source.screenshotOmission;
  if (
    screenshotOmission !== undefined &&
    screenshotOmission !== null &&
    screenshotOmission !== "policy" &&
    screenshotOmission !== "protectedAction"
  ) {
    throw new LiveTimelineError("invalid_event", "Observation.screenshotOmission is invalid");
  }
  if (screenshotOmission && source.screenshot !== undefined && source.screenshot !== null) {
    throw new LiveTimelineError(
      "invalid_event",
      "an omitted Observation cannot also expose screenshot Evidence",
    );
  }
  const width = safeInteger(viewport.width, "Observation.viewport.width");
  const height = safeInteger(viewport.height, "Observation.viewport.height");
  if (width > 0xffff_ffff || height > 0xffff_ffff) {
    throw new LiveTimelineError("invalid_event", "Observation viewport exceeds u32");
  }
  return deepFreeze({
    capturedAtMs: safeInteger(source.capturedAtMs, "Observation.capturedAtMs"),
    deviceId: boundedText(
      stringValue(source.deviceId, "Observation.deviceId"),
      limits.maxTextBytes,
    ),
    id: boundedText(uuid(source.id, "Observation.id"), limits.maxTextBytes),
    ...(source.screenshot === undefined || source.screenshot === null
      ? {}
      : { screenshot: evidence(source.screenshot as AssetRef, limits) }),
    ...(screenshotOmission ? { screenshotOmission } : {}),
    viewport: {
      height,
      scaleFactor: finiteNumber(viewport.scaleFactor, "Observation.viewport.scaleFactor"),
      width,
    },
  });
}

function errorInfo(value: ErrorInfo, limits: LiveTimelineLimits): ErrorPresentation {
  const source = record(value, "ErrorInfo");
  if (typeof source.retryable !== "boolean") {
    throw new LiveTimelineError("invalid_event", "ErrorInfo.retryable must be boolean");
  }
  return deepFreeze({
    code: boundedText(stringValue(source.code, "ErrorInfo.code"), limits.maxTextBytes),
    ...(source.details === undefined || source.details === null
      ? {}
      : { details: boundedJson(source.details, limits) }),
    message: boundedText(stringValue(source.message, "ErrorInfo.message"), limits.maxTextBytes),
    retryable: source.retryable,
  });
}

function completion(
  value: ActionOutcome,
  limits: LiveTimelineLimits,
): ActionCompletionPresentation {
  const source = record(value, "Action outcome");
  switch (source.outcome) {
    case "succeeded": {
      const result = record(source.result, "Action result");
      uuid(result.callId, "Action result.callId");
      if (!Object.hasOwn(result, "output")) {
        throw new LiveTimelineError("invalid_event", "Action result.output is required");
      }
      const references = evidenceList(result.evidence, limits, "Action result.evidence");
      return deepFreeze({
        outcome: "succeeded" as const,
        ...(result.after === undefined || result.after === null
          ? {}
          : { after: observation(result.after as Observation, limits) }),
        ...(result.before === undefined || result.before === null
          ? {}
          : { before: observation(result.before as Observation, limits) }),
        evidence: references.evidence,
        evidenceOmitted: references.omitted,
        finishedAtMs: safeInteger(result.finishedAtMs, "Action result.finishedAtMs"),
        output: boundedJson(result.output, limits),
        startedAtMs: safeInteger(result.startedAtMs, "Action result.startedAtMs"),
      });
    }
    case "failed":
    case "cancelled":
      return deepFreeze({
        outcome: source.outcome,
        error: errorInfo(source.error as ErrorInfo, limits),
      });
    case "timedOut":
      return deepFreeze({
        outcome: "timedOut" as const,
        error: errorInfo(source.error as ErrorInfo, limits),
        timeoutMs: safeInteger(source.timeoutMs, "Action timeoutMs"),
      });
    default:
      throw new LiveTimelineError("invalid_event", "Action outcome is unknown");
  }
}

function actionStarted(
  value: RecordedActionCall,
  limits: LiveTimelineLimits,
): Extract<TimelinePresentation, { readonly type: "actionStarted" }> {
  const source = record(value, "RecordedActionCall");
  if (source.argumentsRedacted !== undefined && typeof source.argumentsRedacted !== "boolean") {
    throw new LiveTimelineError(
      "invalid_event",
      "RecordedActionCall.argumentsRedacted must be boolean",
    );
  }
  return deepFreeze({
    type: "actionStarted" as const,
    arguments:
      source.argumentsRedacted === true
        ? { omitted: "protected" as const }
        : boundedJson(source.arguments ?? null, limits),
    callId: boundedText(uuid(source.id, "RecordedActionCall.id"), limits.maxTextBytes),
    name: boundedText(
      stringValue(source.name, "RecordedActionCall.name"),
      limits.maxTextBytes,
    ),
  });
}

function verdict(value: Verdict, limits: LiveTimelineLimits): TimelinePresentation {
  const source = record(value, "Verdict");
  if (source.status !== "pass" && source.status !== "fail" && source.status !== "unknown") {
    throw new LiveTimelineError("invalid_event", "Verdict.status is invalid");
  }
  const references = evidenceList(source.evidence, limits, "Verdict.evidence");
  return deepFreeze({
    type: "verdictRecorded" as const,
    status: source.status,
    summary: boundedText(stringValue(source.summary, "Verdict.summary"), limits.maxTextBytes),
    evidence: references.evidence,
    evidenceOmitted: references.omitted,
  });
}

export function presentEvent(event: TestEvent, limits: LiveTimelineLimits): TimelineEntry {
  const source = record(event, "TestEvent");
  const payload = record(source.payload, "TestEvent.payload");
  const sessionId = uuid(source.sessionId, "TestEvent.sessionId");
  const common = {
    atMs: safeInteger(source.atMs, "TestEvent.atMs"),
    ...(source.deviceId === undefined || source.deviceId === null
      ? {}
      : {
          deviceId: boundedText(
            stringValue(source.deviceId, "TestEvent.deviceId"),
            limits.maxTextBytes,
          ),
        }),
    eventId: boundedText(uuid(source.eventId, "TestEvent.eventId"), limits.maxTextBytes),
    sequence: safeInteger(source.sequence, "TestEvent.sequence", 1),
    sessionId,
  };
  let category: TimelineEntry["category"];
  let title: string;
  let status: string | undefined;
  let presentation: TimelinePresentation;
  switch (payload.type) {
    case "sessionStarted":
      category = "session";
      title = "Session started";
      presentation = { type: "sessionStarted" };
      break;
    case "sessionEnded":
      if (
        payload.outcome !== "completed" &&
        payload.outcome !== "failed" &&
        payload.outcome !== "cancelled" &&
        payload.outcome !== "shutdown"
      ) {
        throw new LiveTimelineError("invalid_event", "Session outcome is invalid");
      }
      category = "session";
      title = "Session ended";
      status = payload.outcome[0]?.toUpperCase() + payload.outcome.slice(1);
      presentation = deepFreeze({
        type: "sessionEnded" as const,
        outcome: payload.outcome,
        ...(payload.reason === undefined || payload.reason === null
          ? {}
          : {
              reason: boundedText(
                stringValue(payload.reason, "Session end reason"),
                limits.maxTextBytes,
              ),
            }),
      });
      break;
    case "observationCaptured":
      category = "observation";
      title = "Observation captured";
      presentation = deepFreeze({
        type: "observationCaptured" as const,
        observation: observation(payload.observation as Observation, limits),
      });
      break;
    case "actionStarted":
      category = "action";
      title = "Action started";
      presentation = actionStarted(payload.call as RecordedActionCall, limits);
      break;
    case "actionCompleted": {
      category = "action";
      title = "Action completed";
      const callId = uuid(payload.callId, "Action callId");
      const outcomeSource = record(payload.outcome, "Action outcome");
      if (outcomeSource.outcome === "succeeded") {
        const result = record(outcomeSource.result, "Action result");
        if (uuid(result.callId, "Action result.callId") !== callId) {
          throw new LiveTimelineError(
            "invalid_event",
            "Action result callId does not match its completed event",
          );
        }
      }
      const completed = completion(payload.outcome as ActionOutcome, limits);
      status =
        completed.outcome === "timedOut"
          ? "Timed out"
          : completed.outcome[0]?.toUpperCase() + completed.outcome.slice(1);
      presentation = deepFreeze({
        type: "actionCompleted" as const,
        callId: boundedText(callId, limits.maxTextBytes),
        completion: completed,
      });
      break;
    }
    case "mediaStreamStarted": {
      const stream = record(payload.stream, "Media stream");
      const streamId = uuid(stream.id, "Media stream.id");
      const kind = stringValue(stream.kind, "Media stream.kind");
      if (kind !== "screenshot" && kind !== "video") {
        throw new LiveTimelineError("invalid_event", "Media stream.kind is invalid");
      }
      let viewport: { readonly height: number; readonly scaleFactor: number; readonly width: number } | undefined;
      if (stream.viewport !== undefined && stream.viewport !== null) {
        const sourceViewport = record(stream.viewport, "Media stream.viewport");
        const width = safeInteger(sourceViewport.width, "Media stream.viewport.width", 1);
        const height = safeInteger(sourceViewport.height, "Media stream.viewport.height", 1);
        const scaleFactor = finiteNumber(sourceViewport.scaleFactor, "Media stream.viewport.scaleFactor");
        if (width > 0xffff_ffff || height > 0xffff_ffff || scaleFactor <= 0) {
          throw new LiveTimelineError("invalid_event", "Media stream viewport is invalid");
        }
        viewport = Object.freeze({ height, scaleFactor, width });
      }
      category = "media";
      title = "Media stream started";
      presentation = deepFreeze({
        type: "mediaStreamStarted" as const,
        streamId: boundedText(streamId, limits.maxTextBytes),
        kind,
        mediaType: boundedText(
          stringValue(stream.mediaType, "Media stream.mediaType"),
          limits.maxTextBytes,
        ),
        ...(viewport === undefined ? {} : { viewport }),
      });
      break;
    }
    case "mediaFrameCaptured": {
      const frame = record(payload.frame, "Media frame");
      category = "media";
      title = "Media frame captured";
      presentation = deepFreeze({
        type: "mediaFrameCaptured" as const,
        streamId: boundedText(uuid(frame.streamId, "Media frame.streamId"), limits.maxTextBytes),
        frameIndex: safeInteger(frame.frameIndex, "Media frame.frameIndex", 1),
        keyFrame: frame.keyFrame === true,
        ...(frame.durationMs === undefined || frame.durationMs === null
          ? {}
          : { durationMs: safeInteger(frame.durationMs, "Media frame.durationMs") }),
        evidence: evidence(frame.evidence as AssetRef, limits),
      });
      break;
    }
    case "mediaStreamEnded":
      category = "media";
      title = "Media stream ended";
      presentation = deepFreeze({
        type: "mediaStreamEnded" as const,
        streamId: boundedText(
          uuid(payload.streamId, "Media stream terminal.streamId"),
          limits.maxTextBytes,
        ),
        frameCount: safeInteger(payload.frameCount, "Media stream terminal.frameCount"),
      });
      break;
    case "error":
      category = "error";
      title = "Error";
      presentation = deepFreeze({
        type: "error" as const,
        error: errorInfo(payload.error as ErrorInfo, limits),
      });
      break;
    case "verdictRecorded": {
      category = "verdict";
      title = "Verdict recorded";
      presentation = verdict(payload.verdict as Verdict, limits);
      if (presentation.type !== "verdictRecorded") {
        throw new LiveTimelineError("invalid_event", "Verdict presentation is invalid");
      }
      status = presentation.status[0]?.toUpperCase() + presentation.status.slice(1);
      break;
    }
    default:
      throw new LiveTimelineError("invalid_event", "TestEvent payload type is unknown");
  }
  return deepFreeze({
    ...common,
    category,
    presentation,
    ...(status ? { status } : {}),
    title,
  });
}
