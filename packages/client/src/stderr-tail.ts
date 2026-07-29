import type { Readable } from "node:stream";

export const DEFAULT_STDERR_TAIL_BYTES = 64 * 1024;

export class BoundedStderrTail {
  readonly maxBytes: number;

  #bytes = Buffer.alloc(0);
  #stopped = false;
  readonly #stream: Readable | undefined;
  readonly #onData = (chunk: unknown): void => {
    const bytes = typeof chunk === "string" ? Buffer.from(chunk) : Buffer.from(chunk as Uint8Array);
    if (bytes.length >= this.maxBytes) {
      this.#bytes = Buffer.from(bytes.subarray(bytes.length - this.maxBytes));
      return;
    }
    const overflow = Math.max(0, this.#bytes.length + bytes.length - this.maxBytes);
    this.#bytes = Buffer.concat([this.#bytes.subarray(overflow), bytes]);
  };
  readonly #onError = (): void => {
    // stderr is diagnostic only; transport health is determined by stdout/stdin.
  };

  constructor(stream?: Readable, maxBytes = DEFAULT_STDERR_TAIL_BYTES) {
    if (!Number.isSafeInteger(maxBytes) || maxBytes <= 0) {
      throw new RangeError("maxBytes must be a positive safe integer");
    }
    this.maxBytes = maxBytes;
    this.#stream = stream;
    stream?.on("data", this.#onData);
    stream?.on("error", this.#onError);
  }

  get byteLength(): number {
    return this.#bytes.length;
  }

  get text(): string {
    return this.#bytes.toString("utf8");
  }

  stop(): void {
    if (this.#stopped) {
      return;
    }
    this.#stopped = true;
    this.#stream?.off("data", this.#onData);
    this.#stream?.off("error", this.#onError);
  }
}
