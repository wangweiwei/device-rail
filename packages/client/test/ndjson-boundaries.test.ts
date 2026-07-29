import assert from "node:assert/strict";
import test from "node:test";

import {
  NdjsonFrameTooLargeError,
  NdjsonIncompleteFrameError,
  NdjsonInvalidUtf8Error,
} from "../src/errors.js";
import { NdjsonDecoder } from "../src/ndjson.js";

test("decoder accepts exact limits and a terminal CR split from its newline", () => {
  const decoder = new NdjsonDecoder({ maxFrameBytes: 3 });

  assert.deepEqual(decoder.push(Buffer.from("123\r")), []);
  assert.equal(decoder.bufferedBytes, 4);
  assert.deepEqual(decoder.push(Buffer.from("\nabc\n")), ["123", "abc"]);
  assert.equal(decoder.bufferedBytes, 0);
  decoder.end();
});

test("decoder preserves empty frames and honors Uint8Array view boundaries", () => {
  const decoder = new NdjsonDecoder({ maxFrameBytes: 8 });
  const backing = Buffer.from("xxone\nyy");
  const view = new Uint8Array(backing.buffer, backing.byteOffset + 2, 4);

  assert.deepEqual(decoder.push(new Uint8Array()), []);
  assert.deepEqual(decoder.push(view), ["one"]);
  assert.deepEqual(decoder.push(Buffer.from("\n\r\n")), ["", ""]);
  decoder.end();
});

test("decoder keeps an oversized-frame failure sticky", () => {
  const decoder = new NdjsonDecoder({ maxFrameBytes: 3 });
  let failure: unknown;

  try {
    decoder.push(Buffer.from("1234"));
    assert.fail("an oversized pending frame must fail");
  } catch (error) {
    failure = error;
  }

  assert.ok(failure instanceof NdjsonFrameTooLargeError);
  assert.equal(failure.limitBytes, 3);
  assert.equal(failure.actualBytes, 4);
  assert.throws(() => decoder.push(Buffer.from("\n")), (error) => error === failure);
  assert.throws(() => decoder.end(), (error) => error === failure);
});

test("decoder keeps malformed UTF-8 and incomplete-EOF failures sticky", () => {
  const malformed = new NdjsonDecoder();
  let utf8Failure: unknown;
  try {
    malformed.push(Uint8Array.from([0xc3, 0x28, 0x0a]));
    assert.fail("malformed UTF-8 must fail");
  } catch (error) {
    utf8Failure = error;
  }
  assert.ok(utf8Failure instanceof NdjsonInvalidUtf8Error);
  assert.throws(() => malformed.push(Buffer.from("ok\n")), (error) => error === utf8Failure);

  const incomplete = new NdjsonDecoder();
  incomplete.push(Buffer.from("partial"));
  let eofFailure: unknown;
  try {
    incomplete.end();
    assert.fail("an incomplete EOF frame must fail");
  } catch (error) {
    eofFailure = error;
  }
  assert.ok(eofFailure instanceof NdjsonIncompleteFrameError);
  assert.equal(eofFailure.bufferedBytes, 7);
  assert.throws(() => incomplete.end(), (error) => error === eofFailure);
});

test("decoder rejects use after a clean end", () => {
  const decoder = new NdjsonDecoder();
  decoder.end();

  assert.throws(
    () => decoder.push(Buffer.from("late\n")),
    (error: unknown) => error instanceof Error && error.name === "TransportClosedDecoderError",
  );
  assert.throws(
    () => decoder.end(),
    (error: unknown) => error instanceof Error && error.name === "TransportClosedDecoderError",
  );
});

test("decoder rejects invalid frame capacities", () => {
  for (const maxFrameBytes of [0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.throws(() => new NdjsonDecoder({ maxFrameBytes }), RangeError);
  }
});

test("decoder handles adversarial one-byte fragmentation without rebuilding the prefix", () => {
  const size = 64 * 1024;
  const decoder = new NdjsonDecoder({ maxFrameBytes: size });
  const byte = Buffer.from("x");
  for (let index = 0; index < size; index += 1) {
    assert.deepEqual(decoder.push(byte), []);
  }
  assert.equal(decoder.bufferedBytes, size);
  const frames = decoder.push(Buffer.from("\n"));
  assert.equal(frames.length, 1);
  assert.equal(frames[0]?.length, size);
  decoder.end();
});
