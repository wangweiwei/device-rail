import type { SessionOutcome, VerdictStatus } from "@devicerail/protocol";

export type TimelineFilter = "all" | "observations" | "actions" | "errors" | "verdicts";
export type LiveTimelineStatus =
  | "active"
  | "sessionEnded"
  | "viewerCapacityExceeded"
  | "failed"
  | "stopped";

export interface LiveTimelineLimits {
  readonly maxEvents: number;
  readonly maxTotalBytes: number;
  readonly maxEventBytes: number;
  readonly maxInputEventBytes: number;
  readonly maxTextBytes: number;
  readonly maxJsonBytes: number;
  readonly maxJsonDepth: number;
  readonly maxEvidencePerEvent: number;
}

export interface BoundedText {
  readonly text: string;
  readonly truncated: boolean;
}

export interface BoundedJson {
  readonly json: string;
  readonly truncated: boolean;
}

export interface ReferenceOnlyEvidence {
  readonly availability: "referenceOnly";
  readonly id: BoundedText;
  readonly mediaType: BoundedText;
  readonly sha256?: BoundedText;
}

export interface ObservationPresentation {
  readonly capturedAtMs: number;
  readonly deviceId: BoundedText;
  readonly id: BoundedText;
  readonly screenshot?: ReferenceOnlyEvidence;
  readonly screenshotOmission?: "policy" | "protectedAction";
  readonly viewport: {
    readonly height: number;
    readonly scaleFactor: number;
    readonly width: number;
  };
}

export interface ErrorPresentation {
  readonly code: BoundedText;
  readonly details?: BoundedJson;
  readonly message: BoundedText;
  readonly retryable: boolean;
}

export type ActionCompletionPresentation =
  | {
      readonly outcome: "succeeded";
      readonly after?: ObservationPresentation;
      readonly before?: ObservationPresentation;
      readonly evidence: readonly ReferenceOnlyEvidence[];
      readonly evidenceOmitted: number;
      readonly finishedAtMs: number;
      readonly output: BoundedJson;
      readonly startedAtMs: number;
    }
  | { readonly outcome: "failed" | "cancelled"; readonly error: ErrorPresentation }
  | {
      readonly outcome: "timedOut";
      readonly error: ErrorPresentation;
      readonly timeoutMs: number;
    };

export type TimelinePresentation =
  | { readonly type: "sessionStarted" }
  | {
      readonly type: "sessionEnded";
      readonly outcome: SessionOutcome;
      readonly reason?: BoundedText;
    }
  | { readonly type: "observationCaptured"; readonly observation: ObservationPresentation }
  | {
      readonly type: "actionStarted";
      readonly arguments: BoundedJson | { readonly omitted: "protected" };
      readonly callId: BoundedText;
      readonly name: BoundedText;
    }
  | {
      readonly type: "actionCompleted";
      readonly callId: BoundedText;
      readonly completion: ActionCompletionPresentation;
    }
  | {
      readonly type: "mediaStreamStarted";
      readonly streamId: BoundedText;
      readonly kind: "screenshot" | "video";
      readonly mediaType: BoundedText;
      readonly viewport?: { readonly height: number; readonly scaleFactor: number; readonly width: number };
    }
  | {
      readonly type: "mediaFrameCaptured";
      readonly streamId: BoundedText;
      readonly frameIndex: number;
      readonly keyFrame: boolean;
      readonly durationMs?: number;
      readonly evidence: ReferenceOnlyEvidence;
    }
  | {
      readonly type: "mediaStreamEnded";
      readonly streamId: BoundedText;
      readonly frameCount: number;
    }
  | { readonly type: "error"; readonly error: ErrorPresentation }
  | {
      readonly type: "verdictRecorded";
      readonly status: VerdictStatus;
      readonly summary: BoundedText;
      readonly evidence: readonly ReferenceOnlyEvidence[];
      readonly evidenceOmitted: number;
    };

export interface TimelineEntry {
  readonly atMs: number;
  readonly category: "session" | "observation" | "action" | "media" | "error" | "verdict";
  readonly deviceId?: BoundedText;
  readonly eventId: BoundedText;
  readonly presentation: TimelinePresentation;
  readonly sequence: number;
  readonly sessionId: string;
  readonly status?: string;
  readonly title: string;
}

export interface PreparedTimelineEvent {
  readonly byteSize: number;
  readonly fingerprint: string;
  readonly presentation: TimelineEntry;
  readonly sequence: number;
}

export interface TimelineCommit {
  readonly fingerprint: string;
  readonly kind: "committed" | "pendingReplay";
  readonly sequence: number;
}

export interface TimelineConfirmation {
  readonly revision: number;
  readonly sequence: number;
  readonly status: LiveTimelineStatus;
}

export interface LiveTimelineState {
  readonly confirmedSequence?: number;
  readonly eventCount: number;
  readonly pending?: { readonly fingerprint: string; readonly sequence: number };
  readonly revision: number;
  readonly sessionId: string;
  readonly status: LiveTimelineStatus;
  readonly totalBytes: number;
}

export interface TimelinePageRequest {
  readonly filter?: TimelineFilter;
  readonly page?: number;
  readonly pageSize?: number;
}

export interface TimelinePage {
  readonly filter: TimelineFilter;
  readonly items: readonly TimelineEntry[];
  readonly page: number;
  readonly pageSize: number;
  readonly revision: number;
  readonly status: LiveTimelineStatus;
  readonly totalItems: number;
  readonly totalPages: number;
}
