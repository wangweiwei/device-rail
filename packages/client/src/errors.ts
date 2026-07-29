import type { EventsStreamTermination, RpcError, RpcId } from "@devicerail/protocol";

export type ClientErrorCode =
  | "duplicate_request_id"
  | "event_stream_aborted"
  | "event_stream_closed"
  | "event_stream_cursor"
  | "event_stream_queue_overflow"
  | "event_stream_remote_termination"
  | "feature_not_negotiated"
  | "handshake_state"
  | "incomplete_ndjson_frame"
  | "invalid_ndjson_utf8"
  | "ndjson_frame_too_large"
  | "protocol_violation"
  | "pending_request_limit"
  | "request_aborted"
  | "remote_rpc_error"
  | "transport_closed"
  | "write_frame_too_large"
  | "write_queue_overflow";

export class DeviceRailClientError extends Error {
  readonly code: ClientErrorCode;

  constructor(code: ClientErrorCode, message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = new.target.name;
    this.code = code;
  }
}

export class NdjsonFrameTooLargeError extends DeviceRailClientError {
  readonly actualBytes: number;
  readonly limitBytes: number;

  constructor(limitBytes: number, actualBytes: number) {
    super(
      "ndjson_frame_too_large",
      `NDJSON frame is ${actualBytes} bytes; the limit is ${limitBytes} bytes`,
    );
    this.limitBytes = limitBytes;
    this.actualBytes = actualBytes;
  }
}

export class NdjsonInvalidUtf8Error extends DeviceRailClientError {
  constructor(cause: unknown) {
    super("invalid_ndjson_utf8", "NDJSON frame is not valid UTF-8", {
      cause,
    });
  }
}

export class NdjsonIncompleteFrameError extends DeviceRailClientError {
  readonly bufferedBytes: number;

  constructor(bufferedBytes: number) {
    super(
      "incomplete_ndjson_frame",
      `transport ended with an incomplete ${bufferedBytes}-byte NDJSON frame`,
    );
    this.bufferedBytes = bufferedBytes;
  }
}

export class WriteFrameTooLargeError extends DeviceRailClientError {
  readonly actualBytes: number;
  readonly limitBytes: number;

  constructor(limitBytes: number, actualBytes: number) {
    super(
      "write_frame_too_large",
      `outbound NDJSON frame is ${actualBytes} bytes; the limit is ${limitBytes} bytes`,
    );
    this.limitBytes = limitBytes;
    this.actualBytes = actualBytes;
  }
}

export class WriteQueueOverflowError extends DeviceRailClientError {
  constructor(message: string) {
    super("write_queue_overflow", message);
  }
}

export class TransportClosedError extends DeviceRailClientError {
  constructor(message = "DeviceRail transport is closed", options?: ErrorOptions) {
    super("transport_closed", message, options);
  }
}

export class ProtocolViolationError extends DeviceRailClientError {
  constructor(message: string, options?: ErrorOptions) {
    super("protocol_violation", message, options);
  }
}

export class HandshakeStateError extends DeviceRailClientError {
  constructor(message: string) {
    super("handshake_state", message);
  }
}

export class DuplicateRequestIdError extends DeviceRailClientError {
  readonly requestId: RpcId;

  constructor(requestId: RpcId) {
    super("duplicate_request_id", `RPC request id is already pending: ${String(requestId)}`);
    this.requestId = requestId;
  }
}

export class RpcRemoteError extends DeviceRailClientError {
  readonly requestId: RpcId;
  readonly rpcError: RpcError;

  constructor(requestId: RpcId, rpcError: RpcError) {
    super("remote_rpc_error", rpcError.message);
    this.requestId = requestId;
    this.rpcError = rpcError;
  }
}

export class FeatureNotNegotiatedError extends DeviceRailClientError {
  readonly feature: string;
  readonly method: string;

  constructor(method: string, feature: string) {
    super(
      "feature_not_negotiated",
      `${method} requires negotiated protocol feature ${feature}`,
    );
    this.method = method;
    this.feature = feature;
  }
}

export class PendingRequestLimitError extends DeviceRailClientError {
  readonly limit: number;

  constructor(limit: number) {
    super("pending_request_limit", `the client already has ${limit} pending requests`);
    this.limit = limit;
  }
}

export class RequestAbortedError extends DeviceRailClientError {
  constructor() {
    super("request_aborted", "the request AbortSignal was already aborted");
  }
}

export class EventStreamAbortedError extends DeviceRailClientError {
  constructor(message = "the event stream was aborted") {
    super("event_stream_aborted", message);
  }
}

export class EventStreamClosedError extends DeviceRailClientError {
  readonly closeCode: number;
  readonly closeReason: string;

  constructor(closeCode: number, closeReason: string) {
    super(
      "event_stream_closed",
      `the WebSocket closed before an events.stream.terminal notification (code ${closeCode}${
        closeReason ? `: ${closeReason}` : ""
      })`,
    );
    this.closeCode = closeCode;
    this.closeReason = closeReason;
  }
}

export class EventStreamCursorError extends DeviceRailClientError {
  constructor(message: string) {
    super("event_stream_cursor", message);
  }
}

export class EventStreamQueueOverflowError extends DeviceRailClientError {
  readonly limitBytes: number;
  readonly limitEvents: number;

  constructor(limitEvents: number, limitBytes: number) {
    super(
      "event_stream_queue_overflow",
      `the event stream receive queue exceeded ${limitEvents} events or ${limitBytes} bytes`,
    );
    this.limitEvents = limitEvents;
    this.limitBytes = limitBytes;
  }
}

export class EventStreamRemoteTerminationError extends DeviceRailClientError {
  readonly termination: EventsStreamTermination;

  constructor(termination: EventsStreamTermination) {
    super(
      "event_stream_remote_termination",
      `the event stream terminated remotely: ${termination.reason}`,
    );
    this.termination = structuredClone(termination);
  }
}
