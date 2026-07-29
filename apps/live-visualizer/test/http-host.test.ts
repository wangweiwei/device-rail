import assert from "node:assert/strict";
import { request as httpRequest, type IncomingHttpHeaders } from "node:http";
import { connect } from "node:net";
import test, { type TestContext } from "node:test";
import { inspect } from "node:util";

import {
  bindLiveVisualizerHttp,
  type LiveVisualizerHttpHost,
  type LiveVisualizerHttpSource,
  type LiveVisualizerPageRequest,
} from "../src/http-host.js";

class FakeSource implements LiveVisualizerHttpSource {
  readonly #listeners = new Set<(revision: number) => void>();
  revision = 40;

  currentRevision(): number {
    return this.revision;
  }

  page(request: LiveVisualizerPageRequest): unknown {
    return {
      filter: request.filter,
      items:
        request.page === 1
          ? [
              {
                atMs: 10,
                category: "session",
                eventId: "event-1",
                presentation: { type: "sessionStarted" },
                sequence: 1,
                sessionId: "session-1",
                title: "Session started",
              },
            ]
          : [],
      page: request.page,
      pageSize: request.pageSize,
      revision: this.revision,
      status: "active",
      totalItems: 1,
      totalPages: 2,
    };
  }

  state(): unknown {
    return { revision: this.revision, status: "active" };
  }

  subscribe(listener: (revision: number) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  publish(): void {
    this.revision += 1;
    for (const listener of this.#listeners) listener(this.revision);
  }
}

function isPermissionDenied(error: unknown): boolean {
  return (
    error !== null &&
    typeof error === "object" &&
    "code" in error &&
    (error.code === "EACCES" || error.code === "EPERM")
  );
}

async function bindOrSkip(
  context: TestContext,
  source: FakeSource,
  limits: Parameters<typeof bindLiveVisualizerHttp>[0]["limits"] = {},
): Promise<LiveVisualizerHttpHost | undefined> {
  try {
    return await bindLiveVisualizerHttp({ limits, source });
  } catch (error) {
    if (
      isPermissionDenied(error) &&
      process.env.DEVICERAIL_ALLOW_NO_LOOPBACK === "1"
    ) {
      context.skip("runner explicitly permits skipping forbidden AF_INET loopback binds");
      return undefined;
    }
    throw error;
  }
}

interface ResponseData {
  readonly body: Buffer;
  readonly headers: IncomingHttpHeaders;
  readonly status: number;
}

async function request(
  endpoint: URL,
  options: {
    readonly headers?: Readonly<Record<string, string>>;
    readonly method?: string;
    readonly path?: string;
  } = {},
): Promise<ResponseData> {
  return await new Promise<ResponseData>((resolve, reject) => {
    const operation = httpRequest(
      {
        agent: false,
        headers: { Host: endpoint.host, ...options.headers },
        host: "127.0.0.1",
        method: options.method ?? "GET",
        path: options.path ?? endpoint.pathname,
        port: endpoint.port,
      },
      (response) => {
        const chunks: Buffer[] = [];
        response.on("data", (chunk: Buffer) => chunks.push(Buffer.from(chunk)));
        response.once("end", () => {
          resolve({
            body: Buffer.concat(chunks),
            headers: response.headers,
            status: response.statusCode ?? 0,
          });
        });
      },
    );
    operation.once("error", reject);
    operation.end();
  });
}

async function rawRequest(endpoint: URL, raw: string): Promise<string> {
  return await new Promise<string>((resolve, reject) => {
    const socket = connect(Number(endpoint.port), "127.0.0.1");
    const chunks: Buffer[] = [];
    socket.once("connect", () => socket.end(raw));
    socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
    socket.once("error", reject);
    socket.once("close", () => resolve(Buffer.concat(chunks).toString("latin1")));
  });
}

interface OpenSse {
  readonly close: () => void;
  readonly firstFrame: string;
  readonly pause: () => void;
  readonly waitForClose: () => Promise<void>;
}

async function openSse(endpoint: URL, lastEventId?: string): Promise<OpenSse> {
  return await new Promise<OpenSse>((resolve, reject) => {
    const operation = httpRequest(
      {
        agent: false,
        headers: {
          Host: endpoint.host,
          Accept: "text/event-stream",
          ...(lastEventId === undefined ? {} : { "Last-Event-ID": lastEventId }),
        },
        host: "127.0.0.1",
        method: "GET",
        path: `${endpoint.pathname}api/revisions`,
        port: endpoint.port,
      },
      (response) => {
        let text = "";
        let settled = false;
        let resolveClosed!: () => void;
        const closed = new Promise<void>((closedResolve) => {
          resolveClosed = closedResolve;
        });
        response.once("close", resolveClosed);
        response.on("data", (chunk: Buffer) => {
          text += chunk.toString("utf8");
          if (!settled && text.includes("\n\n")) {
            settled = true;
            resolve({
              close: () => operation.destroy(),
              firstFrame: text.slice(0, text.indexOf("\n\n") + 2),
              pause: () => response.pause(),
              waitForClose: async () => await closed,
            });
          }
        });
        if (response.statusCode !== 200) {
          reject(new Error(`SSE returned ${response.statusCode}`));
        }
      },
    );
    operation.once("error", (error) => {
      if ((error as NodeJS.ErrnoException).code !== "ECONNRESET") reject(error);
    });
    operation.end();
  });
}

function sseField(frame: string, field: string): string {
  const prefix = `${field}: `;
  return frame
    .split("\n")
    .find((line) => line.startsWith(prefix))
    ?.slice(prefix.length) ?? "";
}

async function eventually(block: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (block()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.fail("condition did not become true");
}

test("capability routes enforce Host, Origin, methods, body, target, and security headers", async (context) => {
  const source = new FakeSource();
  const host = await bindOrSkip(context, source);
  if (!host) return;
  context.after(async () => await host.close());
  const endpoint = new URL(host.endpoint.exposeSecret());

  assert.equal(String(host.endpoint), "[REDACTED]");
  assert.equal(JSON.stringify(host.endpoint), '"[REDACTED]"');
  assert.equal(inspect(host.endpoint).includes(endpoint.pathname), false);

  const document = await request(endpoint);
  assert.equal(document.status, 200);
  assert.equal(document.headers["access-control-allow-origin"], undefined);
  const contentSecurityPolicy = document.headers["content-security-policy"];
  assert.ok(typeof contentSecurityPolicy === "string");
  assert.match(contentSecurityPolicy, /script-src 'self'/u);
  assert.equal(document.headers["x-frame-options"], "DENY");
  assert.match(document.body.toString("utf8"), /<script[^>]+src=/u);

  const wrongTokenCharacter = endpoint.pathname[2] === "a" ? "b" : "a";
  const wrongToken = `${endpoint.pathname.slice(0, 2)}${wrongTokenCharacter}${endpoint.pathname.slice(3)}`;
  assert.equal((await request(endpoint, { path: wrongToken })).status, 404);
  assert.equal(
    (await request(endpoint, { headers: { Host: `localhost:${endpoint.port}` } })).status,
    403,
  );
  assert.equal(
    (
      await request(endpoint, {
        headers: { Origin: "http://127.0.0.1:9" },
        path: `${endpoint.pathname}api/state`,
      })
    ).status,
    403,
  );
  assert.equal(
    (
      await request(endpoint, {
        headers: { Origin: endpoint.origin },
        path: `${endpoint.pathname}api/state`,
      })
    ).status,
    200,
  );
  const method = await request(endpoint, { method: "POST" });
  assert.equal(method.status, 405);
  assert.equal(method.headers.allow, "GET, HEAD");
  assert.equal(
    (
      await request(endpoint, {
        headers: { "Content-Length": "0" },
        path: `${endpoint.pathname}api/state`,
      })
    ).status,
    400,
  );
  assert.equal(
    (
      await request(endpoint, {
        path: `${endpoint.pathname}api/../api/state`,
      })
    ).status,
    400,
  );
  assert.equal(
    (
      await request(endpoint, {
        headers: { "Sec-Fetch-Dest": "script", "Sec-Fetch-Mode": "no-cors" },
        path: `${endpoint.pathname}api/state`,
      })
    ).status,
    403,
  );
  const duplicateHost = await rawRequest(
    endpoint,
    `GET ${endpoint.pathname} HTTP/1.1\r\nHost: ${endpoint.host}\r\nHost: ${endpoint.host}\r\nConnection: close\r\n\r\n`,
  );
  assert.match(duplicateHost, /^HTTP\/1\.1 (400|403) /u);
  const absolute = await rawRequest(
    endpoint,
    `GET ${endpoint.origin}${endpoint.pathname} HTTP/1.1\r\nHost: ${endpoint.host}\r\nConnection: close\r\n\r\n`,
  );
  assert.match(absolute, /^HTTP\/1\.1 400 /u);
  const oversized = await rawRequest(
    endpoint,
    `GET ${endpoint.pathname} HTTP/1.1\r\nHost: ${endpoint.host}\r\nX-Fill: ${"x".repeat(9 * 1024)}\r\nConnection: close\r\n\r\n`,
  );
  assert.match(oversized, /^HTTP\/1\.1 431 /u);
});

test("state/page/HEAD stay bounded and page overflow is explicit", async (context) => {
  const source = new FakeSource();
  const host = await bindOrSkip(context, source, { maxApiBytes: 16 * 1024 });
  if (!host) return;
  context.after(async () => await host.close());
  const endpoint = new URL(host.endpoint.exposeSecret());

  const state = await request(endpoint, { path: `${endpoint.pathname}api/state` });
  assert.equal(state.status, 200);
  assert.deepEqual(JSON.parse(state.body.toString("utf8")), source.state());
  const page = await request(endpoint, {
    path: `${endpoint.pathname}api/page?filter=all&page=1`,
  });
  assert.equal(page.status, 200);
  assert.equal(JSON.parse(page.body.toString("utf8")).items.length, 1);
  assert.equal(
    (
      await request(endpoint, {
        path: `${endpoint.pathname}api/page?filter=all&page=3`,
      })
    ).status,
    400,
  );
  const head = await request(endpoint, {
    method: "HEAD",
    path: `${endpoint.pathname}app.js`,
  });
  assert.equal(head.status, 200);
  assert.equal(head.body.byteLength, 0);
  assert.ok(Number(head.headers["content-length"]) > 0);
  const sseHead = await request(endpoint, {
    method: "HEAD",
    path: `${endpoint.pathname}api/revisions`,
  });
  assert.equal(sseHead.status, 200);
  assert.equal(sseHead.body.byteLength, 0);
  assert.equal(host.stats().activeSseConnections, 0);
  assert.equal(
    (
      await request(endpoint, {
        headers: { "Last-Event-ID": "daemon-cursor-not-an-sse-id" },
        path: `${endpoint.pathname}api/revisions`,
      })
    ).status,
    400,
  );
});

test("SSE revisions reconnect by invalidation and a bounded slow tab is isolated", async (context) => {
  const source = new FakeSource();
  const host = await bindOrSkip(context, source, {
    drainTimeoutMs: 20,
    maxSseQueuedBytes: 512,
    maxSseQueuedRevisions: 2,
  });
  if (!host) return;
  context.after(async () => await host.close());
  const endpoint = new URL(host.endpoint.exposeSecret());

  const first = await openSse(endpoint);
  assert.equal(sseField(first.firstFrame, "event"), "revision");
  assert.equal(sseField(first.firstFrame, "data"), "40");
  const firstId = sseField(first.firstFrame, "id");
  assert.notEqual(firstId, "40", "SSE event ID and UI revision are separate namespaces");
  first.close();
  await first.waitForClose();

  source.publish();
  const reconnected = await openSse(endpoint, firstId);
  assert.equal(sseField(reconnected.firstFrame, "data"), "41");
  reconnected.pause();
  source.publish();
  source.publish();
  source.publish();
  source.publish();
  await Promise.race([
    reconnected.waitForClose(),
    new Promise<never>((_, reject) =>
      setTimeout(() => reject(new Error("bounded SSE subscriber did not close")), 1_000),
    ),
  ]);
  await eventually(() => host.stats().activeSseConnections === 0);
  assert.equal(host.stats().pendingSseBytes, 0);
  assert.equal(host.stats().pendingSseRevisions, 0);

  const state = await request(endpoint, { path: `${endpoint.pathname}api/state` });
  assert.equal(state.status, 200, "one slow tab must not stop the viewer host");
  await host.close();
  assert.deepEqual(host.stats(), {
    activeRequests: 0,
    activeSseConnections: 0,
    openSockets: 0,
    pendingSseBytes: 0,
    pendingSseRevisions: 0,
  });
});
