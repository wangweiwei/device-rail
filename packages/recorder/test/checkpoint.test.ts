import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import {
  appendFile,
  chmod,
  mkdtemp,
  readFile,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import type { SessionInfo, TestEvent } from "@devicerail/protocol";

import {
  fromCanonicalJson,
  sha256Hex,
  toCanonicalJson,
} from "../src/canonical.js";
import {
  appendRecorderCheckpointPage,
  commitRecorderCheckpoint,
  loadRecorderCheckpoint,
  RECORDER_CHECKPOINT_MAX_BYTES,
} from "../src/checkpoint.js";
import { RecorderError, type RecorderErrorCode } from "../src/errors.js";
import {
  RECORDER_CHECKPOINT_FORMAT,
  RECORDER_CHECKPOINT_VERSION,
  type RecorderCheckpoint,
  type SealedCheckpoint,
} from "../src/types.js";

const SESSION_ID = "11111111-1111-4111-8111-111111111111";

const started = {
  eventId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1",
  sessionId: SESSION_ID,
  sequence: 1,
  atMs: 100,
  payload: { type: "sessionStarted" },
} satisfies TestEvent;

const ended = {
  eventId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa2",
  sessionId: SESSION_ID,
  sequence: 2,
  atMs: 200,
  payload: { type: "sessionEnded", outcome: "completed", reason: null },
} satisfies TestEvent;

const session = {
  id: SESSION_ID,
  state: "ended",
  startedAtMs: 100,
  endedAtMs: 200,
  eventCount: 2,
  lastSequence: 2,
} satisfies SessionInfo;

function recording(revision: number): RecorderCheckpoint {
  return {
    format: RECORDER_CHECKPOINT_FORMAT,
    version: RECORDER_CHECKPOINT_VERSION,
    revision,
    phase: "recording",
    sessionId: SESSION_ID,
    eventProtocolVersion: { major: 1, minor: 2 },
    events: [started],
  };
}

function sealed(revision: number): SealedCheckpoint {
  return {
    format: RECORDER_CHECKPOINT_FORMAT,
    version: RECORDER_CHECKPOINT_VERSION,
    revision,
    phase: "sealed",
    sessionId: SESSION_ID,
    eventProtocolVersion: { major: 1, minor: 2 },
    events: [started, ended],
    session,
  };
}

function hasCode(code: RecorderErrorCode): (error: unknown) => boolean {
  return (error: unknown) => {
    assert.ok(error instanceof RecorderError);
    assert.equal(error.code, code);
    return true;
  };
}

async function temporaryDirectory(): Promise<string> {
  return await mkdtemp(join(tmpdir(), "devicerail-recorder-checkpoint-"));
}

test("canonical JSON is deterministic and rejects alternate encodings", () => {
  const canonical = toCanonicalJson({ z: 1, a: { y: true, x: null } });
  assert.equal(canonical.toString("utf8"), '{"a":{"x":null,"y":true},"z":1}\n');
  assert.deepEqual(fromCanonicalJson(canonical), { a: { x: null, y: true }, z: 1 });
  assert.throws(() => fromCanonicalJson(Buffer.from('{"z":1,"a":2}\n')));
  assert.throws(() => fromCanonicalJson(Buffer.from('{"a":1,"a":1}\n')));
  assert.throws(() => fromCanonicalJson(Buffer.from('{"a":1}')));
  assert.throws(() => toCanonicalJson({ unsafe: Number.MAX_SAFE_INTEGER + 1 }));
  const negativeZero = toCanonicalJson({ value: -0 });
  assert.equal(negativeZero.toString("utf8"), '{"value":-0}\n');
  assert.ok(Object.is((fromCanonicalJson(negativeZero) as { value: number }).value, -0));
});

test("checkpoint commit is revision-CAS, monotonic, and owner-only on Unix", async (context) => {
  const directory = await temporaryDirectory();
  context.after(async () => await rm(directory, { force: true, recursive: true }));
  const path = join(directory, "recording.checkpoint.json");

  const first = await commitRecorderCheckpoint(path, 0, recording(1));
  assert.deepEqual(first, recording(1));
  assert.deepEqual(await loadRecorderCheckpoint(path), recording(1));
  const payload = toCanonicalJson(first);
  assert.deepEqual(
    await readFile(path),
    toCanonicalJson({ checkpoint: first, sha256: sha256Hex(payload) }),
    "the single-pass envelope must retain the exact canonical checkpoint bytes",
  );
  if (process.platform !== "win32") {
    const { mode } = await import("node:fs/promises").then(async ({ stat }) => await stat(path));
    assert.equal(mode & 0o077, 0);
  }

  await commitRecorderCheckpoint(path, 1, sealed(2));
  assert.deepEqual(await loadRecorderCheckpoint(path), sealed(2));
  await assert.rejects(commitRecorderCheckpoint(path, 1, sealed(2)), hasCode("checkpoint_conflict"));
  await assert.rejects(commitRecorderCheckpoint(path, 2, sealed(3)), hasCode("checkpoint_conflict"));

  const changedEvents = [
    { ...started, atMs: 101 },
    ended,
  ] as const satisfies readonly TestEvent[];
  const changed = {
    ...sealed(3),
    session: { ...session, startedAtMs: 101 },
  } satisfies RecorderCheckpoint;
  await assert.rejects(
    commitRecorderCheckpoint(path, 2, { ...changed, events: changedEvents }),
    hasCode("checkpoint_conflict"),
  );
  assert.deepEqual(await loadRecorderCheckpoint(path), sealed(2));
});

test("recording journal appends pages linearly and recovers only the published prefix", async (context) => {
  const directory = await temporaryDirectory();
  context.after(async () => await rm(directory, { force: true, recursive: true }));
  const path = join(directory, "journal.checkpoint.json");
  const initial: RecorderCheckpoint = {
    ...recording(1),
    events: [],
  };
  await commitRecorderCheckpoint(path, 0, initial);
  const baseBytes = await readFile(path);

  await appendRecorderCheckpointPage(path, 1, initial, [started]);
  await appendFile(`${path}.journal`, Buffer.from("unpublished crash tail", "utf8"));
  await appendRecorderCheckpointPage(path, 2, initial, [ended]);

  assert.deepEqual(await readFile(path), baseBytes, "page commits must not rewrite the base snapshot");
  const recovered = await loadRecorderCheckpoint(path);
  assert.equal(recovered?.revision, 3);
  assert.deepEqual(recovered?.events, [started, ended]);
  const journalBytes = await readFile(`${path}.journal`);
  assert.equal(
    journalBytes.filter((byte) => byte === 0x0a).length,
    2,
    "one bounded record must be retained per published page",
  );

  await commitRecorderCheckpoint(path, 3, sealed(4));
  assert.deepEqual(await loadRecorderCheckpoint(path), sealed(4));
  await assert.rejects(readFile(`${path}.journal`), { code: "ENOENT" });
  await assert.rejects(readFile(`${path}.journal-head`), { code: "ENOENT" });
});

test("checkpoint rejects truncation, non-canonical bytes, bad checksums, and unknown fields", async (context) => {
  const directory = await temporaryDirectory();
  context.after(async () => await rm(directory, { force: true, recursive: true }));
  const path = join(directory, "recording.checkpoint.json");
  await commitRecorderCheckpoint(path, 0, recording(1));
  const original = await readFile(path);
  const envelope = JSON.parse(original.toString("utf8")) as {
    checkpoint: Record<string, unknown>;
    sha256: string;
  };

  await writeFile(path, original.subarray(0, original.length - 3), { mode: 0o600 });
  await assert.rejects(loadRecorderCheckpoint(path), hasCode("checkpoint_corrupt"));

  await writeFile(path, `${JSON.stringify(envelope, null, 2)}\n`, { mode: 0o600 });
  await assert.rejects(loadRecorderCheckpoint(path), hasCode("checkpoint_corrupt"));

  await writeFile(path, toCanonicalJson({ ...envelope, sha256: "0".repeat(64) }), {
    mode: 0o600,
  });
  await assert.rejects(loadRecorderCheckpoint(path), hasCode("checkpoint_corrupt"));

  const checkpoint = { ...envelope.checkpoint, unexpected: true };
  const payload = toCanonicalJson(checkpoint);
  await writeFile(
    path,
    toCanonicalJson({ checkpoint, sha256: sha256Hex(payload) }),
    { mode: 0o600 },
  );
  await assert.rejects(loadRecorderCheckpoint(path), hasCode("checkpoint_corrupt"));

  await writeFile(path, Buffer.alloc(RECORDER_CHECKPOINT_MAX_BYTES + 1, 0x20), { mode: 0o600 });
  await assert.rejects(loadRecorderCheckpoint(path), hasCode("checkpoint_corrupt"));
});

test("checkpoint refuses live locks and reclaims only an OS-confirmed dead owner", async (context) => {
  const directory = await temporaryDirectory();
  context.after(async () => await rm(directory, { force: true, recursive: true }));

  const livePath = join(directory, "live.checkpoint.json");
  await writeFile(
    `${livePath}.lock`,
    toCanonicalJson(
      { pid: process.pid, token: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa" },
      { maxBytes: 1024 },
    ),
    { mode: 0o600 },
  );
  await assert.rejects(
    commitRecorderCheckpoint(livePath, 0, recording(1)),
    hasCode("checkpoint_locked"),
  );

  const child = spawn(process.execPath, ["-e", ""], { stdio: "ignore" });
  const deadPid = child.pid;
  assert.ok(deadPid !== undefined);
  await once(child, "exit");
  const stalePath = join(directory, "stale.checkpoint.json");
  await writeFile(
    `${stalePath}.lock`,
    toCanonicalJson(
      { pid: deadPid, token: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb" },
      { maxBytes: 1024 },
    ),
    { mode: 0o600 },
  );
  await commitRecorderCheckpoint(stalePath, 0, recording(1));
  assert.deepEqual(await loadRecorderCheckpoint(stalePath), recording(1));
});

test("checkpoint cancellation and unsafe paths fail before publication", async (context) => {
  const directory = await temporaryDirectory();
  context.after(async () => await rm(directory, { force: true, recursive: true }));
  const path = join(directory, "cancelled.checkpoint.json");
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    commitRecorderCheckpoint(path, 0, recording(1), { signal: controller.signal }),
    hasCode("operation_cancelled"),
  );
  assert.equal(await loadRecorderCheckpoint(path), null);

  const directoryTarget = join(directory, "directory-target");
  await import("node:fs/promises").then(async ({ mkdir }) => await mkdir(directoryTarget));
  await assert.rejects(loadRecorderCheckpoint(directoryTarget), hasCode("checkpoint_corrupt"));

  if (process.platform !== "win32") {
    const real = join(directory, "real.json");
    await commitRecorderCheckpoint(real, 0, recording(1));
    const linked = join(directory, "linked.json");
    await symlink(real, linked);
    await assert.rejects(loadRecorderCheckpoint(linked), hasCode("checkpoint_corrupt"));
    await chmod(real, 0o644);
    await assert.rejects(loadRecorderCheckpoint(real), hasCode("checkpoint_corrupt"));
  }
});

test("checkpoint creation rejects orphaned journal sidecars", async (context) => {
  const directory = await temporaryDirectory();
  context.after(async () => await rm(directory, { force: true, recursive: true }));
  const path = join(directory, "orphan.checkpoint.json");
  await writeFile(`${path}.journal`, "orphan", { mode: 0o600 });

  await assert.rejects(
    commitRecorderCheckpoint(path, 0, { ...recording(1), events: [] }),
    hasCode("checkpoint_corrupt"),
  );
  await assert.rejects(readFile(path), { code: "ENOENT" });
});
