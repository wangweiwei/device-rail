/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type TestEventPayload =
  | {
      type: "sessionStarted";
    }
  | {
      outcome: SessionOutcome;
      reason?: string | null;
      type: "sessionEnded";
    }
  | {
      observation: Observation;
      type: "observationCaptured";
    }
  | {
      call: RecordedActionCall;
      type: "actionStarted";
    }
  | {
      callId: string;
      outcome: ActionOutcome;
      type: "actionCompleted";
    }
  | {
      stream: MediaStreamInfo;
      type: "mediaStreamStarted";
    }
  | {
      frame: MediaFrame;
      type: "mediaFrameCaptured";
    }
  | {
      frameCount: number;
      streamId: string;
      type: "mediaStreamEnded";
    }
  | {
      type: "verdictRecorded";
      verdict: Verdict;
    }
  | {
      error: ErrorInfo;
      type: "error";
    };
export type SessionOutcome = "completed" | "failed" | "cancelled" | "shutdown";
export type ScreenshotOmissionReason = "policy" | "protectedAction";
export type UiContextKind = "native" | "web";
export type UiSnapshotOmissionReason = "driverUnsupported" | "policy" | "protectedAction";
/**
 * The terminal outcome for one action call.
 *
 * Keeping the four outcomes structurally distinct prevents clients from
 * having to infer timeout or cancellation from human-readable error text.
 */
export type ActionOutcome =
  | {
      outcome: "succeeded";
      result: ActionResult;
    }
  | {
      error: ErrorInfo;
      outcome: "failed";
    }
  | {
      error: ErrorInfo;
      outcome: "cancelled";
    }
  | {
      error: ErrorInfo;
      outcome: "timedOut";
      timeoutMs: number;
    };
export type ActionExecution =
  | {
      context: UiContextRef;
      mode: "nativeSemantic";
    }
  | {
      context: UiContextRef;
      mode: "webSemantic";
    }
  | {
      context: UiContextRef;
      fallbackReason: CoordinateFallbackReason;
      mode: "coordinateFallback";
    };
export type CoordinateFallbackReason = "semanticInteractionUnavailable" | "platformLimitation";
export type MediaStreamKind = "screenshot" | "video";
/**
 * A one-based sequence number within one session.
 *
 * The wire value is capped at JavaScript's maximum safe integer so generated
 * clients can sort and resume event streams without losing precision.
 */
export type EventSequence = number;
export type VerdictStatus = "pass" | "fail" | "unknown";
export type RpcIdSchema = string | number;
export type EventsListResult = TestEvent[];

export interface TestEvent {
  atMs: number;
  deviceId?: string | null;
  eventId: string;
  payload: TestEventPayload;
  requestId?: RpcIdSchema | null;
  sequence: EventSequence;
  sessionId: string;
}
export interface Observation {
  capturedAtMs: number;
  deviceId: string;
  id: string;
  metadata?: {
    [k: string]: unknown;
  };
  screenshot?: AssetRef | null;
  screenshotOmission?: ScreenshotOmissionReason | null;
  uiSnapshot?: UiSnapshotRef | null;
  uiSnapshotOmission?: UiSnapshotOmissionReason | null;
  viewport: Viewport;
  [k: string]: unknown;
}
export interface AssetRef {
  id: string;
  mediaType: string;
  sha256?: string | null;
  uri: string;
  [k: string]: unknown;
}
/**
 * Small Observation-side reference to a UI Tree Evidence object.
 */
export interface UiSnapshotRef {
  byteLength: number;
  context: UiContextRef;
  evidence: AssetRef;
  formatVersion: number;
  nodeCount: number;
}
/**
 * Full identity of one native accessibility or web-document context.
 * `documentEpoch` is required for both channels and changes after reconnect,
 * navigation, or any replacement that invalidates prior node references.
 */
export interface UiContextRef {
  contextId: string;
  contextKind: UiContextKind;
  documentEpoch: string;
}
export interface Viewport {
  height: number;
  scaleFactor: number;
  width: number;
  [k: string]: unknown;
}
/**
 * Durable representation of an Action invocation.
 *
 * Standard calls preserve the historical wire shape. Protected and unknown
 * calls retain only correlation fields and serialize `arguments` as `null`
 * with an explicit `argumentsRedacted` marker.
 */
export interface RecordedActionCall {
  arguments?: unknown;
  argumentsRedacted?: boolean;
  id: string;
  name: string;
  [k: string]: unknown;
}
export interface ActionResult {
  after?: Observation | null;
  before?: Observation | null;
  callId: string;
  evidence?: AssetRef[];
  execution?: ActionExecution | null;
  finishedAtMs: number;
  output: unknown;
  startedAtMs: number;
  [k: string]: unknown;
}
export interface ErrorInfo {
  code: string;
  details?: unknown;
  message: string;
  retryable: boolean;
}
export interface MediaStreamInfo {
  id: string;
  kind: MediaStreamKind;
  mediaType: string;
  viewport?: Viewport | null;
}
export interface MediaFrame {
  durationMs?: number | null;
  evidence: AssetRef;
  frameIndex: EventSequence;
  keyFrame?: boolean;
  streamId: string;
}
export interface Verdict {
  /**
   * @maxItems 64
   */
  evidence?: AssetRef[];
  status: VerdictStatus;
  summary: string;
}
