import assert from "node:assert/strict";
import { constants } from "node:fs";
import {
  access,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  exportAndValidateBundle,
  type BundleSource,
} from "../src/bundle-cli.js";
import { RecorderError } from "../src/errors.js";

type CommandName = "export" | "validate";

interface FakeCommand {
  readonly createBundle?: boolean;
  readonly exitCode?: number;
  readonly hang?: boolean;
  readonly readyPath?: string;
  readonly stderr?: string;
  readonly stderrBytes?: number;
  readonly stdout?: string;
}

interface FakeCli {
  readonly commands: Partial<Record<CommandName, FakeCommand>>;
  readonly logPath: string;
  readonly manifest?: unknown;
}

interface Invocation {
  readonly args: string[];
  readonly command: CommandName;
}

const SESSION_ID = "11111111-1111-4111-8111-111111111111";

function bundleSource(): BundleSource {
  return {
    eventProtocolVersion: { major: 1, minor: 2 },
    sessionExport: {
      events: [
        {
          atMs: 1_000,
          eventId: "22222222-2222-4222-8222-222222222221",
          payload: { type: "sessionStarted" },
          sequence: 1,
          sessionId: SESSION_ID,
        },
        {
          atMs: 2_000,
          eventId: "22222222-2222-4222-8222-222222222222",
          payload: { outcome: "completed", reason: null, type: "sessionEnded" },
          sequence: 2,
          sessionId: SESSION_ID,
        },
      ],
      session: {
        endedAtMs: 2_000,
        eventCount: 2,
        id: SESSION_ID,
        lastSequence: 2,
        startedAtMs: 1_000,
        state: "ended",
      },
    },
  };
}

function manifestFor(source: BundleSource): Record<string, unknown> {
  return {
    bundleVersion: 1,
    eventProtocolVersion: source.eventProtocolVersion,
    events: source.sessionExport.events,
    magic: "devicerail.session-bundle",
    session: source.sessionExport.session,
  };
}

function summary(
  operation: CommandName,
  source: BundleSource,
  overrides: Readonly<Record<string, unknown>> = {},
): Record<string, unknown> {
  return {
    assetBytes: 0,
    assetCount: 0,
    eventCount: source.sessionExport.events.length,
    ok: true,
    operation,
    sessionId: source.sessionExport.session.id,
    ...overrides,
  };
}

function summaryLine(
  operation: CommandName,
  source: BundleSource,
  overrides: Readonly<Record<string, unknown>> = {},
): string {
  return `${JSON.stringify(summary(operation, source, overrides))}\n`;
}

async function exists(path: string): Promise<boolean> {
  try {
    await access(path, constants.F_OK);
    return true;
  } catch {
    return false;
  }
}

async function writeFakeCli(directory: string, config: FakeCli): Promise<string> {
  const hook = join(directory, "fake-bundle-cli.cjs");
  const encoded = JSON.stringify(config);
  await writeFile(
    hook,
    [
      'const fs = require("node:fs");',
      'const path = require("node:path");',
      `const config = ${encoded};`,
      'const sourceFlag = process.argv.indexOf("--source");',
      'const command = sourceFlag >= 0 ? "export" : "validate";',
      'const args = command === "export" ? process.argv.slice(sourceFlag) : [process.argv.at(-1)];',
      'fs.appendFileSync(config.logPath, JSON.stringify({ command, args }) + "\\n");',
      'const behavior = config.commands[command] || {};',
      'const writeAll = (descriptor, value) => {',
      '  const bytes = Buffer.from(value);',
      '  let offset = 0;',
      '  while (offset < bytes.length) {',
      '    offset += fs.writeSync(descriptor, bytes, offset, bytes.length - offset);',
      '  }',
      '};',
      'if (behavior.createBundle) {',
      '  const outputIndex = args.indexOf("--output");',
      '  if (outputIndex < 0 || typeof args[outputIndex + 1] !== "string") process.exit(96);',
      '  const output = args[outputIndex + 1];',
      '  fs.mkdirSync(output, { recursive: true });',
      '  fs.writeFileSync(path.join(output, "manifest.json"), JSON.stringify(config.manifest));',
      '}',
      'if (behavior.readyPath) fs.writeFileSync(behavior.readyPath, "ready");',
      'if (behavior.stderr) writeAll(2, behavior.stderr);',
      'if (behavior.stderrBytes) writeAll(2, "x".repeat(behavior.stderrBytes));',
      'if (behavior.stdout) writeAll(1, behavior.stdout);',
      'if (behavior.hang) {',
      '  const cell = new Int32Array(new SharedArrayBuffer(4));',
      '  for (;;) Atomics.wait(cell, 0, 0, 60_000);',
      '}',
      'process.exit(behavior.exitCode || 0);',
    ].join("\n"),
    { mode: 0o600 },
  );
  return hook;
}

async function withFakeCli<T>(
  directory: string,
  config: FakeCli,
  operation: () => Promise<T>,
): Promise<T> {
  const hook = await writeFakeCli(directory, config);
  const previous = process.env.NODE_OPTIONS;
  const preload = `--require ${JSON.stringify(hook.replaceAll("\\", "/"))}`;
  process.env.NODE_OPTIONS = previous ? `${previous} ${preload}` : preload;
  try {
    return await operation();
  } finally {
    if (previous === undefined) {
      delete process.env.NODE_OPTIONS;
    } else {
      process.env.NODE_OPTIONS = previous;
    }
  }
}

async function invocations(path: string): Promise<Invocation[]> {
  const lines = (await readFile(path, "utf8")).trim().split("\n");
  return lines.filter(Boolean).map((line) => JSON.parse(line) as Invocation);
}

async function writeExistingBundle(
  output: string,
  manifest: Readonly<Record<string, unknown>>,
): Promise<void> {
  await mkdir(output, { recursive: true });
  await writeFile(join(output, "manifest.json"), JSON.stringify(manifest));
}

async function expectRecorderError(
  operation: Promise<unknown>,
  code: string,
): Promise<RecorderError> {
  let caught: unknown;
  try {
    await operation;
  } catch (error) {
    caught = error;
  }
  assert.ok(caught instanceof RecorderError);
  assert.equal(caught.code, code);
  return caught;
}

async function waitForFile(path: string): Promise<void> {
  const deadline = Date.now() + 5_000;
  while (!(await exists(path))) {
    if (Date.now() >= deadline) {
      assert.fail(`timed out waiting for ${path}`);
    }
    await new Promise<void>((resolve) => setTimeout(resolve, 10));
  }
}

test("Bundle CLI runner is shell-free, strict, bounded, cancellable, and identity-safe", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "devicerail-recorder-cli-"));
  t.after(async () => rm(root, { force: true, recursive: true }));

  await t.test("uses the fixed export/validate argv without a shell", async () => {
    const directory = join(root, "fixed-argv");
    await mkdir(directory);
    const source = bundleSource();
    const logPath = join(directory, "calls.ndjson");
    const marker = join(directory, "shell-was-used");
    const sourcePath = `${join(directory, "source.json")};touch ${marker}`;
    const evidenceDirectory = join(directory, "evidence $literal");
    const outputDirectory = join(directory, "bundle output");

    const receipt = await withFakeCli(
      directory,
      {
        commands: {
          export: {
            createBundle: true,
            stdout: summaryLine("export", source),
          },
          validate: { stdout: summaryLine("validate", source) },
        },
        logPath,
        manifest: manifestFor(source),
      },
      () =>
        exportAndValidateBundle({
          evidenceDirectory,
          executable: process.execPath,
          outputDirectory,
          source,
          sourcePath,
        }),
    );

    assert.deepEqual(receipt, {
      assetBytes: 0,
      assetCount: 0,
      eventCount: 2,
      sessionId: SESSION_ID,
    });
    assert.deepEqual(await invocations(logPath), [
      {
        args: [
          "--source",
          sourcePath,
          "--evidence-dir",
          evidenceDirectory,
          "--output",
          outputDirectory,
        ],
        command: "export",
      },
      { args: [outputDirectory], command: "validate" },
    ]);
    assert.equal(await exists(marker), false);
  });

  await t.test("rejects lossy serde-default DTOs before starting the CLI", async () => {
    const directory = join(root, "lossy-source");
    await mkdir(directory);
    const source = structuredClone(bundleSource());
    const terminal = source.sessionExport.events[1];
    assert.equal(terminal?.payload.type, "sessionEnded");
    if (terminal?.payload.type === "sessionEnded") {
      delete terminal.payload.reason;
    }
    const logPath = join(directory, "calls.ndjson");
    await expectRecorderError(
      withFakeCli(
        directory,
        {
          commands: { validate: { stdout: summaryLine("validate", source) } },
          logPath,
        },
        () =>
          exportAndValidateBundle({
            evidenceDirectory: join(directory, "evidence"),
            executable: process.execPath,
            outputDirectory: join(directory, "bundle"),
            source,
            sourcePath: join(directory, "source.json"),
          }),
      ),
      "source_corrupt",
    );
    assert.equal(await exists(logPath), false);
  });

  await t.test("rejects non-exact or malformed success summaries", async () => {
    const source = bundleSource();
    const cases = [
      {
        name: "extra-field",
        stdout: summaryLine("validate", source, { extra: true }),
      },
      {
        name: "wrong-operation",
        stdout: summaryLine("export", source),
      },
      {
        name: "unsafe-count",
        stdout: summaryLine("validate", source, { eventCount: 9_007_199_254_740_992 }),
      },
      {
        name: "missing-final-newline",
        stdout: JSON.stringify(summary("validate", source)),
      },
      {
        name: "multiple-values",
        stdout: `${summaryLine("validate", source)}${summaryLine("validate", source)}`,
      },
    ] as const;

    for (const candidate of cases) {
      const directory = join(root, `summary-${candidate.name}`);
      const outputDirectory = join(directory, "bundle");
      await mkdir(directory);
      await writeExistingBundle(outputDirectory, manifestFor(source));
      await expectRecorderError(
        withFakeCli(
          directory,
          {
            commands: { validate: { stdout: candidate.stdout } },
            logPath: join(directory, "calls.ndjson"),
          },
          () =>
            exportAndValidateBundle({
              evidenceDirectory: join(directory, "evidence"),
              executable: process.execPath,
              outputDirectory,
              source,
              sourcePath: join(directory, "source.json"),
            }),
        ),
        "bundle_summary_invalid",
      );
    }
  });

  await t.test("reports non-zero exits while bounding child diagnostics", async () => {
    const source = bundleSource();
    const failureDirectory = join(root, "non-zero");
    const failureOutput = join(failureDirectory, "bundle");
    await mkdir(failureDirectory);
    await writeExistingBundle(failureOutput, manifestFor(source));
    const failure = await expectRecorderError(
      withFakeCli(
        failureDirectory,
        {
          commands: {
            validate: {
              exitCode: 23,
              stderr: "bounded fake diagnostic\n",
            },
          },
          logPath: join(failureDirectory, "calls.ndjson"),
        },
        () =>
          exportAndValidateBundle({
            evidenceDirectory: join(failureDirectory, "evidence"),
            executable: process.execPath,
            outputDirectory: failureOutput,
            source,
            sourcePath: join(failureDirectory, "source.json"),
          }),
      ),
      "bundle_cli_failed",
    );
    assert.deepEqual(failure.details, {
      diagnostic: "bounded fake diagnostic",
      exitCode: 23,
    });

    const overflowDirectory = join(root, "overflow");
    const overflowOutput = join(overflowDirectory, "bundle");
    await mkdir(overflowDirectory);
    await writeExistingBundle(overflowOutput, manifestFor(source));
    const overflow = await expectRecorderError(
      withFakeCli(
        overflowDirectory,
        {
          commands: { validate: { exitCode: 1, stderrBytes: 64 * 1024 + 1 } },
          logPath: join(overflowDirectory, "calls.ndjson"),
        },
        () =>
          exportAndValidateBundle({
            evidenceDirectory: join(overflowDirectory, "evidence"),
            executable: process.execPath,
            outputDirectory: overflowOutput,
            source,
            sourcePath: join(overflowDirectory, "source.json"),
          }),
      ),
      "bundle_cli_failed",
    );
    assert.match(overflow.message, /bounded diagnostic limit/u);
    assert.equal(overflow.details, undefined);
  });

  await t.test("kills an in-flight validator when cancelled", async () => {
    const directory = join(root, "cancel");
    const outputDirectory = join(directory, "bundle");
    const readyPath = join(directory, "ready");
    const logPath = join(directory, "calls.ndjson");
    const source = bundleSource();
    await mkdir(directory);
    await writeExistingBundle(outputDirectory, manifestFor(source));
    const controller = new AbortController();

    await withFakeCli(
      directory,
      {
        commands: { validate: { hang: true, readyPath } },
        logPath,
      },
      async () => {
        const pending = exportAndValidateBundle({
          evidenceDirectory: join(directory, "evidence"),
          executable: process.execPath,
          outputDirectory,
          signal: controller.signal,
          source,
          sourcePath: join(directory, "source.json"),
        });
        try {
          await waitForFile(readyPath);
          controller.abort();
          await expectRecorderError(pending, "operation_cancelled");
        } finally {
          controller.abort();
          await pending.catch(() => {});
        }
      },
    );
    assert.deepEqual(await invocations(logPath), [
      { args: [outputDirectory], command: "validate" },
    ]);
  });

  await t.test("never accepts a pre-existing Bundle with another identity", async () => {
    const directory = join(root, "identity-mismatch");
    const outputDirectory = join(directory, "bundle");
    const logPath = join(directory, "calls.ndjson");
    const source = bundleSource();
    const wrongManifest = manifestFor(source);
    wrongManifest.session = {
      ...source.sessionExport.session,
      id: "33333333-3333-4333-8333-333333333333",
    };
    await mkdir(directory);
    await writeExistingBundle(outputDirectory, wrongManifest);

    await expectRecorderError(
      withFakeCli(
        directory,
        {
          commands: { validate: { stdout: summaryLine("validate", source) } },
          logPath,
        },
        () =>
          exportAndValidateBundle({
            evidenceDirectory: join(directory, "evidence"),
            executable: process.execPath,
            outputDirectory,
            source,
            sourcePath: join(directory, "source.json"),
          }),
      ),
      "bundle_summary_mismatch",
    );
    assert.deepEqual(await invocations(logPath), [
      { args: [outputDirectory], command: "validate" },
    ]);
  });
});
