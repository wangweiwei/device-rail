import {
  NdjsonFrameTooLargeError,
  NdjsonIncompleteFrameError,
  NdjsonInvalidUtf8Error,
} from "./errors.js";

export const DEFAULT_MAX_FRAME_BYTES = 1024 * 1024;

export interface NdjsonDecoderOptions {
  readonly maxFrameBytes?: number;
}

function positiveSafeInteger(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new RangeError(`${name} must be a positive safe integer`);
  }
  return value;
}

/** A byte-bounded, fatal-UTF-8 NDJSON decoder. */
export class NdjsonDecoder {
  readonly maxFrameBytes: number;

  #buffer = Buffer.alloc(0);
  #bufferedBytes = 0;
  #ended = false;
  #failure: Error | undefined;

  constructor(options: NdjsonDecoderOptions = {}) {
    this.maxFrameBytes = positiveSafeInteger(
      options.maxFrameBytes ?? DEFAULT_MAX_FRAME_BYTES,
      "maxFrameBytes",
    );
  }

  get bufferedBytes(): number {
    return this.#bufferedBytes;
  }

  push(chunk: Uint8Array): string[] {
    this.#assertOpen();
    if (chunk.byteLength === 0) {
      return [];
    }

    const incoming = Buffer.from(chunk.buffer, chunk.byteOffset, chunk.byteLength);
    const frames: string[] = [];
    let start = 0;

    for (let index = 0; index < incoming.length; index += 1) {
      if (incoming[index] !== 0x0a) {
        continue;
      }

      const segment = incoming.subarray(start, index);
      this.#checkCompletedBound(segment);
      const frame =
        this.#bufferedBytes === 0
          ? Buffer.from(segment)
          : this.#appendAndView(segment);
      frames.push(this.#decodeFrame(frame));
      this.#bufferedBytes = 0;
      start = index + 1;
    }

    if (start < incoming.length) {
      const remainder = incoming.subarray(start);
      this.#checkPendingSegmentBound(remainder);
      this.#append(remainder);
    }

    return frames;
  }

  end(): void {
    this.#assertOpen();
    this.#ended = true;
    if (this.#bufferedBytes !== 0) {
      const error = new NdjsonIncompleteFrameError(this.#bufferedBytes);
      this.#failure = error;
      throw error;
    }
  }

  #decodeFrame(rawFrame: Buffer): string {
    const frame = rawFrame.at(-1) === 0x0d ? rawFrame.subarray(0, -1) : rawFrame;
    if (frame.length > this.maxFrameBytes) {
      return this.#fail(new NdjsonFrameTooLargeError(this.maxFrameBytes, frame.length));
    }

    try {
      return new TextDecoder("utf-8", { fatal: true }).decode(frame);
    } catch (cause) {
      return this.#fail(new NdjsonInvalidUtf8Error(cause));
    }
  }

  #checkPendingSegmentBound(segment: Buffer): void {
    const length = this.#bufferedBytes + segment.length;
    const finalByte = segment.at(-1) ?? this.#buffer.at(this.#bufferedBytes - 1);
    const mayBeTerminalCarriageReturn =
      length === this.maxFrameBytes + 1 && finalByte === 0x0d;
    if (length > this.maxFrameBytes && !mayBeTerminalCarriageReturn) {
      this.#fail(new NdjsonFrameTooLargeError(this.maxFrameBytes, length));
    }
  }

  #checkCompletedBound(segment: Buffer): void {
    const rawLength = this.#bufferedBytes + segment.length;
    const finalByte = segment.at(-1) ?? this.#buffer.at(this.#bufferedBytes - 1);
    const frameLength = finalByte === 0x0d ? rawLength - 1 : rawLength;
    if (frameLength > this.maxFrameBytes) {
      this.#fail(new NdjsonFrameTooLargeError(this.maxFrameBytes, frameLength));
    }
  }

  #append(segment: Buffer): void {
    const required = this.#bufferedBytes + segment.length;
    this.#ensureCapacity(required);
    segment.copy(this.#buffer, this.#bufferedBytes);
    this.#bufferedBytes = required;
  }

  #appendAndView(segment: Buffer): Buffer {
    this.#append(segment);
    return this.#buffer.subarray(0, this.#bufferedBytes);
  }

  #ensureCapacity(required: number): void {
    if (required <= this.#buffer.length) {
      return;
    }
    const maximum = this.maxFrameBytes + 1;
    let capacity = Math.max(64, this.#buffer.length);
    while (capacity < required) {
      capacity = Math.min(maximum, capacity * 2);
      if (capacity < required && capacity === maximum) {
        capacity = required;
        break;
      }
    }
    const next = Buffer.allocUnsafe(capacity);
    if (this.#bufferedBytes > 0) {
      this.#buffer.copy(next, 0, 0, this.#bufferedBytes);
    }
    this.#buffer = next;
  }

  #assertOpen(): void {
    if (this.#failure) {
      throw this.#failure;
    }
    if (this.#ended) {
      throw new TransportClosedDecoderError();
    }
  }

  #fail<T>(error: Error): T {
    this.#failure = error;
    throw error;
  }
}

class TransportClosedDecoderError extends Error {
  constructor() {
    super("NDJSON decoder has already ended");
    this.name = "TransportClosedDecoderError";
  }
}
