/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

/**
 * The terminal reason is a closed tagged union so reason/error combinations
 * cannot become ambiguous on the wire.
 */
export type EventsStreamTermination =
  | {
      reason: "sessionEnded";
    }
  | {
      reason: "cancelled";
    }
  | {
      error: ErrorInfo;
      reason: "slowConsumer";
    }
  | {
      error: ErrorInfo;
      reason: "sessionDeleted";
    }
  | {
      error: ErrorInfo;
      reason: "serverShutdown";
    }
  | {
      error: ErrorInfo;
      reason: "sequenceGap";
    }
  | {
      error: ErrorInfo;
      reason: "eventTooLarge";
    }
  | {
      error: ErrorInfo;
      reason: "internalError";
    };

export interface ErrorInfo {
  code: string;
  details?: unknown;
  message: string;
  retryable: boolean;
}
