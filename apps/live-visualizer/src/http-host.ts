import { randomBytes } from "node:crypto";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo, Socket } from "node:net";
import { inspect } from "node:util";

import { LiveTimelineError } from "@devicerail/live-visualizer";

import { APP_CSS, APP_JS, documentHtml } from "./assets.js";

const CAPABILITY_PATTERN = /^[0-9a-f]{64}$/u;
const MAX_HEADER_BYTES = 8 * 1024;
const MAX_HEADER_COUNT = 32;
const MAX_TARGET_BYTES = 2 * 1024;
export const DEFAULT_LIVE_VISUALIZER_MAX_API_BYTES = 4 * 1024 * 1024;
const DEFAULT_MAX_CONNECTIONS = 32;
const DEFAULT_MAX_SSE_EVENTS = 8;
const DEFAULT_MAX_SSE_BYTES = 4 * 1024;
const DEFAULT_DRAIN_TIMEOUT_MS = 2_000;
const DEFAULT_SHUTDOWN_TIMEOUT_MS = 3_000;
const FILTERS = new Set<LiveVisualizerFilter>([
  "all",
  "observations",
  "actions",
  "errors",
  "verdicts",
]);

export type LiveVisualizerFilter =
  | "all"
  | "observations"
  | "actions"
  | "errors"
  | "verdicts";

export interface LiveVisualizerPageRequest {
  readonly filter: LiveVisualizerFilter;
  readonly page: number;
  readonly pageSize: 50;
}

/** Narrow port implemented by the transport-neutral presentation controller. */
export interface LiveVisualizerHttpSource {
  currentRevision(): number;
  page(request: LiveVisualizerPageRequest): unknown;
  state(): unknown;
  subscribe(listener: (revision: number) => void): () => void;
}

export interface LiveVisualizerHttpLimits {
  readonly drainTimeoutMs?: number;
  readonly maxApiBytes?: number;
  readonly maxConnections?: number;
  readonly maxSseQueuedBytes?: number;
  readonly maxSseQueuedRevisions?: number;
  readonly shutdownTimeoutMs?: number;
}

interface ValidatedLimits {
  readonly drainTimeoutMs: number;
  readonly maxApiBytes: number;
  readonly maxConnections: number;
  readonly maxSseQueuedBytes: number;
  readonly maxSseQueuedRevisions: number;
  readonly shutdownTimeoutMs: number;
}

export interface LiveVisualizerHttpStats {
  readonly activeRequests: number;
  readonly activeSseConnections: number;
  readonly openSockets: number;
  readonly pendingSseBytes: number;
  readonly pendingSseRevisions: number;
}

export class LiveVisualizerEndpoint {
  readonly #value: string;

  constructor(value: string) {
    this.#value = value;
  }

  exposeSecret(): string {
    return this.#value;
  }

  toJSON(): string {
    return "[REDACTED]";
  }

  toString(): string {
    return "[REDACTED]";
  }

  [inspect.custom](): string {
    return "LiveVisualizerEndpoint([REDACTED])";
  }
}

export interface LiveVisualizerHttpHost {
  readonly endpoint: LiveVisualizerEndpoint;
  close(): Promise<void>;
  stats(): LiveVisualizerHttpStats;
  waitUntilClosed(): Promise<void>;
}

class HttpRequestError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
  ) {
    super(code);
  }
}

function positiveInteger(
  value: number | undefined,
  fallback: number,
  maximum: number,
  name: string,
): number {
  const selected = value ?? fallback;
  if (!Number.isSafeInteger(selected) || selected < 1 || selected > maximum) {
    throw new RangeError(`${name} must be a positive safe integer no greater than ${maximum}`);
  }
  return selected;
}

function validateLimits(limits: LiveVisualizerHttpLimits = {}): ValidatedLimits {
  return {
    drainTimeoutMs: positiveInteger(
      limits.drainTimeoutMs,
      DEFAULT_DRAIN_TIMEOUT_MS,
      30_000,
      "drainTimeoutMs",
    ),
    maxApiBytes: positiveInteger(
      limits.maxApiBytes,
      DEFAULT_LIVE_VISUALIZER_MAX_API_BYTES,
      4 * 1024 * 1024,
      "maxApiBytes",
    ),
    maxConnections: positiveInteger(
      limits.maxConnections,
      DEFAULT_MAX_CONNECTIONS,
      256,
      "maxConnections",
    ),
    maxSseQueuedBytes: positiveInteger(
      limits.maxSseQueuedBytes,
      DEFAULT_MAX_SSE_BYTES,
      64 * 1024,
      "maxSseQueuedBytes",
    ),
    maxSseQueuedRevisions: positiveInteger(
      limits.maxSseQueuedRevisions,
      DEFAULT_MAX_SSE_EVENTS,
      128,
      "maxSseQueuedRevisions",
    ),
    shutdownTimeoutMs: positiveInteger(
      limits.shutdownTimeoutMs,
      DEFAULT_SHUTDOWN_TIMEOUT_MS,
      30_000,
      "shutdownTimeoutMs",
    ),
  };
}

function safeRevision(value: number, location: string): number {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new TypeError(`${location} must be a non-negative safe integer`);
  }
  return value;
}

function rawHeaderValues(request: IncomingMessage, name: string): string[] {
  const values: string[] = [];
  for (let index = 0; index < request.rawHeaders.length; index += 2) {
    const headerName = request.rawHeaders[index];
    const value = request.rawHeaders[index + 1];
    if (headerName?.toLowerCase() === name && value !== undefined) {
      values.push(value);
    }
  }
  return values;
}

function validateHeaders(request: IncomingMessage, expectedHost: string): void {
  if (request.rawHeaders.length / 2 > MAX_HEADER_COUNT) {
    throw new HttpRequestError(431, "too_many_headers");
  }
  let headerBytes = 2;
  for (let index = 0; index < request.rawHeaders.length; index += 2) {
    headerBytes += Buffer.byteLength(request.rawHeaders[index] ?? "", "latin1");
    headerBytes += Buffer.byteLength(request.rawHeaders[index + 1] ?? "", "latin1") + 4;
  }
  if (headerBytes > MAX_HEADER_BYTES) {
    throw new HttpRequestError(431, "headers_too_large");
  }
  const hosts = rawHeaderValues(request, "host");
  if (hosts.length !== 1 || hosts[0] !== expectedHost) {
    throw new HttpRequestError(403, "invalid_host");
  }
  if (
    rawHeaderValues(request, "content-length").length !== 0 ||
    rawHeaderValues(request, "transfer-encoding").length !== 0
  ) {
    throw new HttpRequestError(400, "request_body_not_allowed");
  }
  const origins = rawHeaderValues(request, "origin");
  if (
    origins.length > 1 ||
    (origins.length === 1 && origins[0] !== `http://${expectedHost}`)
  ) {
    throw new HttpRequestError(403, "invalid_origin");
  }
  for (const name of [
    "sec-fetch-site",
    "sec-fetch-mode",
    "sec-fetch-dest",
    "sec-fetch-user",
    "last-event-id",
  ]) {
    if (rawHeaderValues(request, name).length > 1) {
      throw new HttpRequestError(400, "duplicate_security_header");
    }
  }
  const site = rawHeaderValues(request, "sec-fetch-site")[0];
  if (site !== undefined && site !== "none" && site !== "same-origin") {
    throw new HttpRequestError(403, "cross_site_request_rejected");
  }
}

type RouteKind = "document" | "script" | "style" | "state" | "page" | "sse";

function validateFetchMetadata(request: IncomingMessage, route: RouteKind): void {
  const mode = rawHeaderValues(request, "sec-fetch-mode")[0];
  const destination = rawHeaderValues(request, "sec-fetch-dest")[0];
  const user = rawHeaderValues(request, "sec-fetch-user")[0];
  if ((mode === undefined) !== (destination === undefined)) {
    throw new HttpRequestError(403, "incomplete_fetch_metadata");
  }
  if (mode !== undefined) {
    const accepted =
      (route === "document" && mode === "navigate" && destination === "document") ||
      (route === "script" && mode === "no-cors" && destination === "script") ||
      (route === "style" && mode === "no-cors" && destination === "style") ||
      ((route === "state" || route === "page" || route === "sse") &&
        (mode === "cors" || mode === "same-origin") &&
        destination === "");
    if (!accepted) {
      throw new HttpRequestError(403, "fetch_metadata_mismatch");
    }
  }
  if (user !== undefined && (route !== "document" || user !== "?1")) {
    throw new HttpRequestError(403, "fetch_user_mismatch");
  }
}

interface Route {
  readonly kind: RouteKind;
  readonly pageRequest?: LiveVisualizerPageRequest;
}

function onlyExactQuery(url: URL, allowed: readonly string[]): void {
  const keys = [...url.searchParams.keys()];
  if (keys.some((key) => !allowed.includes(key))) {
    throw new HttpRequestError(400, "unknown_query_parameter");
  }
  for (const key of allowed) {
    if (url.searchParams.getAll(key).length > 1) {
      throw new HttpRequestError(400, "duplicate_query_parameter");
    }
  }
}

export function parsePageRequest(url: URL): LiveVisualizerPageRequest {
  onlyExactQuery(url, ["filter", "page"]);
  const filter = url.searchParams.get("filter") ?? "all";
  const rawPage = url.searchParams.get("page") ?? "1";
  if (!FILTERS.has(filter as LiveVisualizerFilter) || !/^[1-9][0-9]*$/u.test(rawPage)) {
    throw new HttpRequestError(400, "invalid_page_query");
  }
  const page = Number(rawPage);
  if (!Number.isSafeInteger(page)) {
    throw new HttpRequestError(400, "invalid_page_query");
  }
  return { filter: filter as LiveVisualizerFilter, page, pageSize: 50 };
}

function routeRequest(request: IncomingMessage, capability: string, expectedHost: string): Route {
  if (request.httpVersion !== "1.1") {
    throw new HttpRequestError(505, "http_version_not_supported");
  }
  if (request.method !== "GET" && request.method !== "HEAD") {
    throw new HttpRequestError(405, "method_not_allowed");
  }
  validateHeaders(request, expectedHost);
  const target = request.url ?? "";
  if (
    !target.startsWith("/") ||
    target.startsWith("//") ||
    target.includes("\\") ||
    target.includes("%") ||
    target.includes("#") ||
    Buffer.byteLength(target) > MAX_TARGET_BYTES ||
    /[\u0000-\u001f\u007f]/u.test(target)
  ) {
    throw new HttpRequestError(400, "invalid_request_target");
  }
  const rawPath = target.split("?", 1)[0] ?? "";
  if (rawPath.split("/").some((segment) => segment === "." || segment === "..")) {
    throw new HttpRequestError(400, "ambiguous_path");
  }
  const url = new URL(target, `http://${expectedHost}`);
  const prefix = `/${capability}`;
  let route: Route;
  switch (url.pathname) {
    case `${prefix}/`:
      onlyExactQuery(url, ["filter", "page", "follow"]);
      if (
        (url.searchParams.has("filter") &&
          !FILTERS.has(url.searchParams.get("filter") as LiveVisualizerFilter)) ||
        (url.searchParams.has("page") &&
          !/^[1-9][0-9]*$/u.test(url.searchParams.get("page") ?? "")) ||
        (url.searchParams.has("follow") &&
          url.searchParams.get("follow") !== "0" &&
          url.searchParams.get("follow") !== "1")
      ) {
        throw new HttpRequestError(400, "invalid_document_query");
      }
      route = { kind: "document" };
      break;
    case `${prefix}/app.js`:
      onlyExactQuery(url, []);
      route = { kind: "script" };
      break;
    case `${prefix}/app.css`:
      onlyExactQuery(url, []);
      route = { kind: "style" };
      break;
    case `${prefix}/api/state`:
      onlyExactQuery(url, []);
      route = { kind: "state" };
      break;
    case `${prefix}/api/page`:
      route = { kind: "page", pageRequest: parsePageRequest(url) };
      break;
    case `${prefix}/api/revisions`:
      onlyExactQuery(url, []);
      route = { kind: "sse" };
      break;
    default:
      throw new HttpRequestError(404, "not_found");
  }
  validateFetchMetadata(request, route.kind);
  return route;
}

const CSP = [
  "default-src 'none'",
  "script-src 'self'",
  "style-src 'self'",
  "connect-src 'self'",
  "img-src 'none'",
  "media-src 'none'",
  "font-src 'none'",
  "object-src 'none'",
  "base-uri 'none'",
  "frame-ancestors 'none'",
  "form-action 'none'",
].join("; ");

function commonHeaders(contentType: string): Record<string, string> {
  return {
    "Cache-Control": "no-store",
    "Content-Security-Policy": CSP,
    "Content-Type": contentType,
    "Cross-Origin-Opener-Policy": "same-origin",
    "Cross-Origin-Resource-Policy": "same-origin",
    "Permissions-Policy": "camera=(), display-capture=(), geolocation=(), microphone=(), usb=()",
    "Referrer-Policy": "no-referrer",
    "X-Content-Type-Options": "nosniff",
    "X-Frame-Options": "DENY",
  };
}

function writeBytes(
  request: IncomingMessage,
  response: ServerResponse,
  status: number,
  contentType: string,
  bytes: Buffer,
  extraHeaders: Record<string, string> = {},
): void {
  response.writeHead(status, {
    ...commonHeaders(contentType),
    "Content-Length": String(bytes.byteLength),
    ...extraHeaders,
  });
  if (request.method === "HEAD") {
    response.end();
  } else {
    response.end(bytes);
  }
}

function jsonBytes(value: unknown, limit: number): Buffer {
  let serialized: string;
  try {
    serialized = JSON.stringify(value);
  } catch {
    throw new HttpRequestError(500, "state_serialization_failed");
  }
  const bytes = Buffer.from(serialized);
  if (bytes.byteLength > limit) {
    throw new HttpRequestError(500, "state_response_too_large");
  }
  return bytes;
}

function validatePageResponse(value: unknown, request: LiveVisualizerPageRequest): unknown {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new HttpRequestError(500, "invalid_page_response");
  }
  const page = Reflect.get(value, "page");
  const totalPages = Reflect.get(value, "totalPages");
  const items = Reflect.get(value, "items");
  if (
    page !== request.page ||
    !Number.isSafeInteger(totalPages) ||
    (totalPages as number) < 1 ||
    !Array.isArray(items) ||
    items.length > request.pageSize
  ) {
    throw new HttpRequestError(500, "invalid_page_response");
  }
  if (request.page > Math.max(1, totalPages as number)) {
    throw new HttpRequestError(400, "page_out_of_range");
  }
  return value;
}

class SseClient {
  readonly #limits: ValidatedLimits;
  readonly #onClose: (client: SseClient) => void;
  readonly #request: IncomingMessage;
  readonly #response: ServerResponse;
  #closed = false;
  #inFlightBytes = 0;
  #inFlightRevisions = 0;
  #pendingBytes = 0;
  #pumping = false;
  #queue: Buffer[] = [];

  constructor(
    request: IncomingMessage,
    response: ServerResponse,
    limits: ValidatedLimits,
    onClose: (client: SseClient) => void,
  ) {
    this.#request = request;
    this.#response = response;
    this.#limits = limits;
    this.#onClose = onClose;
    request.once("aborted", () => this.close());
    response.once("close", () => this.close());
  }

  get pendingBytes(): number {
    return this.#pendingBytes + this.#inFlightBytes;
  }

  get pendingRevisions(): number {
    return this.#queue.length + this.#inFlightRevisions;
  }

  enqueue(eventId: number, revision: number): void {
    if (this.#closed) return;
    const frame = Buffer.from(`id: ${eventId}\nevent: revision\ndata: ${revision}\n\n`);
    if (
      this.pendingRevisions >= this.#limits.maxSseQueuedRevisions ||
      this.pendingBytes + frame.byteLength > this.#limits.maxSseQueuedBytes
    ) {
      this.close(true);
      return;
    }
    this.#queue.push(frame);
    this.#pendingBytes += frame.byteLength;
    if (!this.#pumping) {
      this.#pumping = true;
      void this.#pump();
    }
  }

  close(force = false): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#queue = [];
    this.#pendingBytes = 0;
    this.#inFlightBytes = 0;
    this.#inFlightRevisions = 0;
    if (force) {
      this.#response.destroy();
    } else {
      this.#response.end();
    }
    this.#onClose(this);
  }

  async #pump(): Promise<void> {
    try {
      while (!this.#closed) {
        const frame = this.#queue.shift();
        if (!frame) return;
        this.#pendingBytes -= frame.byteLength;
        this.#inFlightBytes += frame.byteLength;
        this.#inFlightRevisions += 1;
        const accepted = this.#response.write(frame, (error?: Error | null) => {
          if (this.#closed) return;
          this.#inFlightBytes = Math.max(0, this.#inFlightBytes - frame.byteLength);
          this.#inFlightRevisions = Math.max(0, this.#inFlightRevisions - 1);
          if (error) this.close(true);
        });
        if (!accepted) {
          const drained = await new Promise<boolean>((resolve) => {
            let settled = false;
            const settle = (value: boolean): void => {
              if (settled) return;
              settled = true;
              clearTimeout(timer);
              this.#response.off("drain", onDrain);
              this.#response.off("close", onClose);
              resolve(value);
            };
            const onDrain = (): void => settle(true);
            const onClose = (): void => settle(false);
            const timer = setTimeout(() => settle(false), this.#limits.drainTimeoutMs);
            timer.unref();
            this.#response.once("drain", onDrain);
            this.#response.once("close", onClose);
          });
          if (!drained) {
            this.close(true);
            return;
          }
        }
      }
    } catch {
      this.close(true);
    } finally {
      this.#pumping = false;
      if (!this.#closed && this.#queue.length > 0) {
        this.#pumping = true;
        void this.#pump();
      }
    }
  }
}

class RevisionHub {
  readonly #clients = new Set<SseClient>();
  readonly #limits: ValidatedLimits;
  #closed = false;
  #nextEventId = 1;

  constructor(limits: ValidatedLimits) {
    this.#limits = limits;
  }

  get size(): number {
    return this.#clients.size;
  }

  get pendingBytes(): number {
    let result = 0;
    for (const client of this.#clients) result += client.pendingBytes;
    return result;
  }

  get pendingRevisions(): number {
    let result = 0;
    for (const client of this.#clients) result += client.pendingRevisions;
    return result;
  }

  add(request: IncomingMessage, response: ServerResponse, revision: number): void {
    if (this.#closed) {
      response.end();
      return;
    }
    const client = new SseClient(request, response, this.#limits, (closed) => {
      this.#clients.delete(closed);
    });
    this.#clients.add(client);
    client.enqueue(this.#takeEventId(), revision);
  }

  publish(revision: number): void {
    if (this.#closed) return;
    const eventId = this.#takeEventId();
    for (const client of this.#clients) client.enqueue(eventId, revision);
  }

  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    for (const client of [...this.#clients]) client.close();
  }

  #takeEventId(): number {
    const result = this.#nextEventId;
    if (this.#nextEventId < Number.MAX_SAFE_INTEGER) {
      this.#nextEventId += 1;
    }
    return result;
  }
}

function listen(server: ReturnType<typeof createServer>): Promise<AddressInfo> {
  return new Promise<AddressInfo>((resolve, reject) => {
    const onError = (error: Error): void => {
      server.off("listening", onListening);
      reject(error);
    };
    const onListening = (): void => {
      server.off("error", onError);
      const address = server.address();
      if (!address || typeof address === "string" || address.address !== "127.0.0.1") {
        reject(new Error("live visualizer did not bind numeric IPv4 loopback"));
        return;
      }
      resolve(address);
    };
    server.once("error", onError);
    server.once("listening", onListening);
    server.listen({ exclusive: true, host: "127.0.0.1", port: 0 });
  });
}

export async function bindLiveVisualizerHttp(options: {
  readonly limits?: LiveVisualizerHttpLimits;
  readonly signal?: AbortSignal;
  readonly source: LiveVisualizerHttpSource;
}): Promise<LiveVisualizerHttpHost> {
  if (options.signal?.aborted) {
    throw new DOMException("live visualizer binding was aborted", "AbortError");
  }
  const limits = validateLimits(options.limits);
  const capability = randomBytes(32).toString("hex");
  if (!CAPABILITY_PATTERN.test(capability)) {
    throw new Error("failed to create a live visualizer capability");
  }
  const sockets = new Set<Socket>();
  const hub = new RevisionHub(limits);
  let activeRequests = 0;
  let expectedHost = "";
  let closed = false;
  let runtimeError: Error | undefined;
  let requestRuntimeClose: (() => void) | undefined;
  let resolveClosed!: () => void;
  const closedPromise = new Promise<void>((resolve) => {
    resolveClosed = resolve;
  });

  const server = createServer(
    { joinDuplicateHeaders: false, maxHeaderSize: MAX_HEADER_BYTES, requireHostHeader: true },
    (request, response) => {
      activeRequests += 1;
      let finished = false;
      const finish = (): void => {
        if (finished) return;
        finished = true;
        activeRequests -= 1;
      };
      response.once("close", finish);
      response.once("finish", finish);
      try {
        const route = routeRequest(request, capability, expectedHost);
        if (route.kind === "document") {
          writeBytes(
            request,
            response,
            200,
            "text/html; charset=utf-8",
            Buffer.from(documentHtml(capability)),
          );
        } else if (route.kind === "script") {
          writeBytes(
            request,
            response,
            200,
            "text/javascript; charset=utf-8",
            Buffer.from(APP_JS),
          );
        } else if (route.kind === "style") {
          writeBytes(request, response, 200, "text/css; charset=utf-8", Buffer.from(APP_CSS));
        } else if (route.kind === "state") {
          writeBytes(
            request,
            response,
            200,
            "application/json; charset=utf-8",
            jsonBytes(options.source.state(), limits.maxApiBytes),
          );
        } else if (route.kind === "page") {
          const pageRequest =
            route.pageRequest ?? { filter: "all", page: 1, pageSize: 50 };
          writeBytes(
            request,
            response,
            200,
            "application/json; charset=utf-8",
            jsonBytes(
              validatePageResponse(options.source.page(pageRequest), pageRequest),
              limits.maxApiBytes,
            ),
          );
        } else if (request.method === "HEAD") {
          writeBytes(
            request,
            response,
            200,
            "text/event-stream; charset=utf-8",
            Buffer.alloc(0),
          );
        } else {
          const lastEventId = rawHeaderValues(request, "last-event-id")[0];
          if (lastEventId !== undefined && !/^(0|[1-9][0-9]*)$/u.test(lastEventId)) {
            throw new HttpRequestError(400, "invalid_last_event_id");
          }
          if (lastEventId !== undefined && !Number.isSafeInteger(Number(lastEventId))) {
            throw new HttpRequestError(400, "invalid_last_event_id");
          }
          response.writeHead(200, {
            ...commonHeaders("text/event-stream; charset=utf-8"),
            Connection: "keep-alive",
            "X-Accel-Buffering": "no",
          });
          response.flushHeaders();
          hub.add(request, response, safeRevision(options.source.currentRevision(), "revision"));
        }
      } catch (cause) {
        const error =
          cause instanceof HttpRequestError
            ? cause
            : cause instanceof LiveTimelineError && cause.code === "invalid_page"
              ? new HttpRequestError(400, "page_out_of_range")
              : new HttpRequestError(500, "internal_error");
        if (!response.headersSent) {
          writeBytes(
            request,
            response,
            error.status,
            "application/json; charset=utf-8",
            Buffer.from(JSON.stringify({ error: { code: error.code } })),
            {
              Connection: "close",
              ...(error.status === 405 ? { Allow: "GET, HEAD" } : {}),
            },
          );
        } else {
          response.destroy();
        }
      }
    },
  );
  server.maxConnections = limits.maxConnections;
  server.maxHeadersCount = MAX_HEADER_COUNT;
  server.headersTimeout = 5_000;
  server.requestTimeout = 5_000;
  server.keepAliveTimeout = 2_000;
  server.maxRequestsPerSocket = 32;
  const onRuntimeError = (error: Error): void => {
    runtimeError ??= error;
    requestRuntimeClose?.();
  };
  server.on("error", onRuntimeError);
  server.on("connection", (socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
  });
  const address = await listen(server);
  expectedHost = `127.0.0.1:${address.port}`;
  let unsubscribe = (): void => {};

  let signalListener: (() => void) | undefined;
  const close = async (): Promise<void> => {
    if (closed) return await closedPromise;
    closed = true;
    if (signalListener) options.signal?.removeEventListener("abort", signalListener);
    try {
      unsubscribe();
    } catch {
      // Listener teardown cannot prevent socket and queue reclamation.
    }
    hub.close();
    const trackedSocketsClosed = Promise.all(
      [...sockets].map(
        (socket) =>
          new Promise<void>((resolve) => {
            if (!sockets.has(socket)) {
              resolve();
              return;
            }
            socket.once("close", resolve);
          }),
      ),
    );
    const closing = new Promise<void>((resolve) => {
      try {
        server.close(() => resolve());
      } catch {
        resolve();
      }
    });
    server.closeIdleConnections();
    const timer = setTimeout(() => {
      server.closeAllConnections();
      for (const socket of sockets) socket.destroy();
    }, limits.shutdownTimeoutMs);
    timer.unref();
    await Promise.all([closing, trackedSocketsClosed]);
    clearTimeout(timer);
    server.off("error", onRuntimeError);
    resolveClosed();
  };
  requestRuntimeClose = () => {
    void close();
  };
  if (runtimeError) {
    await close();
    throw new Error("live visualizer HTTP server failed after binding");
  }
  try {
    unsubscribe = options.source.subscribe((revision) => {
      hub.publish(safeRevision(revision, "published revision"));
    });
  } catch (error) {
    await close();
    throw error;
  }
  if (options.signal) {
    signalListener = () => {
      void close();
    };
    options.signal.addEventListener("abort", signalListener, { once: true });
    if (options.signal.aborted) await close();
  }

  const endpoint = new LiveVisualizerEndpoint(`http://${expectedHost}/${capability}/`);
  return {
    endpoint,
    close,
    stats: () => ({
      activeRequests,
      activeSseConnections: hub.size,
      openSockets: sockets.size,
      pendingSseBytes: hub.pendingBytes,
      pendingSseRevisions: hub.pendingRevisions,
    }),
    waitUntilClosed: async () => await closedPromise,
  };
}
