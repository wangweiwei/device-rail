import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import test from "node:test";

import type { RpcError } from "@devicerail/protocol";

import {
  DeviceRailClientError,
  DuplicateRequestIdError,
  EventStreamAbortedError,
  EventStreamClosedError,
  EventStreamCursorError,
  EventStreamQueueOverflowError,
  EventStreamRemoteTerminationError,
  FeatureNotNegotiatedError,
  HandshakeStateError,
  NdjsonFrameTooLargeError,
  NdjsonIncompleteFrameError,
  NdjsonInvalidUtf8Error,
  PendingRequestLimitError,
  ProtocolViolationError,
  RequestAbortedError,
  RpcRemoteError,
  TransportClosedError,
  WriteFrameTooLargeError,
  WriteQueueOverflowError,
} from "../src/errors.js";
import { BoundedStderrTail } from "../src/stderr-tail.js";

test("client errors expose stable codes, class names, causes, and numeric metadata", () => {
  const cause = new Error("root cause");
  const invalidUtf8 = new NdjsonInvalidUtf8Error(cause);
  const transport = new TransportClosedError("closed with cause", { cause });
  const protocol = new ProtocolViolationError("invalid envelope", { cause });
  const errors: readonly DeviceRailClientError[] = [
    new NdjsonFrameTooLargeError(10, 11),
    invalidUtf8,
    new NdjsonIncompleteFrameError(7),
    new WriteFrameTooLargeError(20, 21),
    new WriteQueueOverflowError("queue full"),
    transport,
    protocol,
    new HandshakeStateError("hello required"),
    new DuplicateRequestIdError(42),
    new FeatureNotNegotiatedError("events.list", "events.v1"),
    new PendingRequestLimitError(128),
    new RequestAbortedError(),
    new EventStreamAbortedError(),
    new EventStreamClosedError(1006, "lost"),
    new EventStreamCursorError("cursor mismatch"),
    new EventStreamQueueOverflowError(64, 4_194_304),
    new EventStreamRemoteTerminationError({ reason: "sessionEnded" }),
  ];

  for (const error of errors) {
    assert.ok(error instanceof Error);
    assert.ok(error instanceof DeviceRailClientError);
    assert.equal(error.name, error.constructor.name);
    assert.match(error.message, /\S/);
  }

  assert.equal(invalidUtf8.code, "invalid_ndjson_utf8");
  assert.equal(invalidUtf8.cause, cause);
  assert.equal(transport.code, "transport_closed");
  assert.equal(transport.cause, cause);
  assert.equal(protocol.code, "protocol_violation");
  assert.equal(protocol.cause, cause);

  const inboundSize = errors[0];
  assert.ok(inboundSize instanceof NdjsonFrameTooLargeError);
  assert.equal(inboundSize.limitBytes, 10);
  assert.equal(inboundSize.actualBytes, 11);

  const incomplete = errors[2];
  assert.ok(incomplete instanceof NdjsonIncompleteFrameError);
  assert.equal(incomplete.bufferedBytes, 7);

  const outboundSize = errors[3];
  assert.ok(outboundSize instanceof WriteFrameTooLargeError);
  assert.equal(outboundSize.limitBytes, 20);
  assert.equal(outboundSize.actualBytes, 21);

  const duplicate = errors[8];
  assert.ok(duplicate instanceof DuplicateRequestIdError);
  assert.equal(duplicate.requestId, 42);

  const feature = errors[9];
  assert.ok(feature instanceof FeatureNotNegotiatedError);
  assert.equal(feature.method, "events.list");
  assert.equal(feature.feature, "events.v1");

  const pendingLimit = errors[10];
  assert.ok(pendingLimit instanceof PendingRequestLimitError);
  assert.equal(pendingLimit.limit, 128);
});

test("remote RPC errors retain the exact request id and structured server error", () => {
  const rpcError: RpcError = {
    code: -32_012,
    data: {
      code: "response_frame_too_large",
      details: { actualBytes: 1_048_577, limitBytes: 1_048_576 },
      message: "response exceeds the transport frame limit",
      retryable: false,
    },
    message: "response frame too large",
  };

  const error = new RpcRemoteError("request-7", rpcError);

  assert.equal(error.code, "remote_rpc_error");
  assert.equal(error.name, "RpcRemoteError");
  assert.equal(error.message, rpcError.message);
  assert.equal(error.requestId, "request-7");
  assert.equal(error.rpcError, rpcError);
});

test("bounded stderr retains only the newest bytes across short and oversized chunks", () => {
  const stream = new PassThrough();
  const tail = new BoundedStderrTail(stream, 5);

  stream.emit("data", Buffer.from("ab"));
  stream.emit("data", "cde");
  assert.equal(tail.byteLength, 5);
  assert.equal(tail.text, "abcde");

  stream.emit("data", Buffer.from("fg"));
  assert.equal(tail.byteLength, 5);
  assert.equal(tail.text, "cdefg");

  stream.emit("data", Buffer.from("0123456789"));
  assert.equal(tail.byteLength, 5);
  assert.equal(tail.text, "56789");

  tail.stop();
});

test("bounded stderr counts UTF-8 bytes rather than JavaScript characters", () => {
  const stream = new PassThrough();
  const tail = new BoundedStderrTail(stream, 4);

  stream.emit("data", Buffer.from("AéB"));
  assert.equal(tail.byteLength, 4);
  assert.equal(tail.text, "AéB");

  stream.emit("data", Buffer.from("C"));
  assert.equal(tail.byteLength, 4);
  assert.equal(tail.text, "éBC");

  tail.stop();
});

test("bounded stderr ignores diagnostic errors and stop is idempotent", () => {
  const stream = new PassThrough();
  const dataListeners = stream.listenerCount("data");
  const errorListeners = stream.listenerCount("error");
  const tail = new BoundedStderrTail(stream, 16);

  assert.equal(stream.listenerCount("data"), dataListeners + 1);
  assert.equal(stream.listenerCount("error"), errorListeners + 1);
  stream.emit("data", Buffer.from("before"));
  assert.doesNotThrow(() => stream.emit("error", new Error("diagnostic only")));

  tail.stop();
  tail.stop();
  assert.equal(stream.listenerCount("data"), dataListeners);
  assert.equal(stream.listenerCount("error"), errorListeners);
  stream.emit("data", Buffer.from("-after"));
  assert.equal(tail.text, "before");
});

test("bounded stderr rejects invalid capacities", () => {
  for (const maxBytes of [0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.throws(() => new BoundedStderrTail(undefined, maxBytes), RangeError);
  }
});
