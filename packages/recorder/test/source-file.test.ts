import assert from "node:assert/strict";
import {
  chmod,
  mkdtemp,
  readFile,
  rm,
  stat,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import type { SessionInfo, TestEvent } from "@devicerail/protocol";

import { toCanonicalJson } from "../src/canonical.js";
import { RecorderError, type RecorderErrorCode } from "../src/errors.js";
import {
  BUNDLE_SOURCE_MAX_BYTES,
  bundleSourceFromEndedSession,
  publishBundleSource,
  readBundleSource,
  type BundleSourceFile,
} from "../src/source-file.js";

const SESSION_ID = "11111111-1111-4111-8111-111111111111";
const events = [
  {
    eventId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1",
    sessionId: SESSION_ID,
    sequence: 1,
    atMs: 100,
    payload: { type: "sessionStarted" },
  },
  {
    eventId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa2",
    sessionId: SESSION_ID,
    sequence: 2,
    atMs: 200,
    payload: { type: "sessionEnded", outcome: "completed", reason: null },
  },
] as const satisfies readonly TestEvent[];
const session = {
  id: SESSION_ID,
  state: "ended",
  startedAtMs: 100,
  endedAtMs: 200,
  eventCount: 2,
  lastSequence: 2,
} satisfies SessionInfo;

function source(): BundleSourceFile {
  return bundleSourceFromEndedSession({ major: 1, minor: 2 }, session, events);
}

function hasCode(code: RecorderErrorCode): (error: unknown) => boolean {
  return (error: unknown) => {
    assert.ok(error instanceof RecorderError);
    assert.equal(error.code, code);
    return true;
  };
}

async function temporaryDirectory(): Promise<string> {
  return await mkdtemp(join(tmpdir(), "devicerail-recorder-source-"));
}

test("BundleSource is canonical, readable, no-clobber, and owner-only on Unix", async (context) => {
  const directory = await temporaryDirectory();
  context.after(async () => await rm(directory, { force: true, recursive: true }));
  const path = join(directory, "source.json");

  await publishBundleSource(path, source());
  assert.deepEqual(await readBundleSource(path), source());
  const bytes = await readFile(path);
  assert.ok(bytes.equals(toCanonicalJson(source())));
  if (process.platform !== "win32") {
    assert.equal((await stat(path)).mode & 0o077, 0);
  }

  await assert.rejects(publishBundleSource(path, source()), hasCode("source_conflict"));
  assert.ok((await readFile(path)).equals(bytes));
});

test("BundleSource reader rejects unknown fields, non-canonical JSON, and oversized input", async (context) => {
  const directory = await temporaryDirectory();
  context.after(async () => await rm(directory, { force: true, recursive: true }));
  const path = join(directory, "source.json");

  await writeFile(path, toCanonicalJson({ ...source(), unknown: true }), { mode: 0o600 });
  await assert.rejects(readBundleSource(path), hasCode("source_corrupt"));

  await writeFile(path, `${JSON.stringify(source(), null, 2)}\n`, { mode: 0o600 });
  await assert.rejects(readBundleSource(path), hasCode("source_corrupt"));

  await writeFile(path, Buffer.alloc(129, 0x20), { mode: 0o600 });
  await assert.rejects(readBundleSource(path, { maxBytes: 128 }), hasCode("source_too_large"));

  await writeFile(path, Buffer.alloc(BUNDLE_SOURCE_MAX_BYTES + 1, 0x20), { mode: 0o600 });
  await assert.rejects(readBundleSource(path), hasCode("source_too_large"));
});

test("BundleSource cancellation and unsafe target types never overwrite a target", async (context) => {
  const directory = await temporaryDirectory();
  context.after(async () => await rm(directory, { force: true, recursive: true }));
  const cancelled = join(directory, "cancelled.json");
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    publishBundleSource(cancelled, source(), { signal: controller.signal }),
    hasCode("operation_cancelled"),
  );
  await assert.rejects(readFile(cancelled), { code: "ENOENT" });

  if (process.platform !== "win32") {
    const victim = join(directory, "victim.txt");
    const linked = join(directory, "source-link.json");
    await writeFile(victim, "do not replace", { mode: 0o600 });
    await symlink(victim, linked);
    await assert.rejects(publishBundleSource(linked, source()), hasCode("source_conflict"));
    assert.equal(await readFile(victim, "utf8"), "do not replace");

    const privateSource = join(directory, "private-source.json");
    await publishBundleSource(privateSource, source());
    await chmod(privateSource, 0o644);
    await assert.rejects(readBundleSource(privateSource), hasCode("source_corrupt"));
  }
});

test("BundleSource strict model rejects active and internally inconsistent Sessions", () => {
  assert.throws(
    () =>
      bundleSourceFromEndedSession(
        { major: 1, minor: 2 },
        { ...session, state: "active", endedAtMs: null },
        events,
      ),
    hasCode("source_corrupt"),
  );
  assert.throws(
    () =>
      bundleSourceFromEndedSession(
        { major: 1, minor: 2 },
        { ...session, eventCount: 3 },
        events,
      ),
    hasCode("source_corrupt"),
  );
});
