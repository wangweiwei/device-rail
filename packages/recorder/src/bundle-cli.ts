import { spawn } from "node:child_process";
import { constants } from "node:fs";
import { lstat, open } from "node:fs/promises";
import { isDeepStrictEqual } from "node:util";
import { dirname, isAbsolute, join } from "node:path";

import { RecorderError } from "./errors.js";
import {
  validateBundleSourceFile,
  type BundleSourceFile,
} from "./source-file.js";
import type { RecorderBundleReceipt } from "./types.js";

const COMMAND_OUTPUT_MAX_BYTES = 64 * 1024;
const MANIFEST_MAX_BYTES = 16 * 1024 * 1024;

export type BundleSource = BundleSourceFile;

export interface BundleFinalizeOptions {
  readonly executable: string;
  readonly sourcePath: string;
  readonly evidenceDirectory: string;
  readonly outputDirectory: string;
  readonly source: BundleSource;
  readonly signal?: AbortSignal;
}

interface CliSummary extends RecorderBundleReceipt {
  readonly ok: true;
  readonly operation: "export" | "validate";
}

interface ProcessResult {
  readonly exitCode: number;
  readonly stdout: Buffer;
  readonly stderr: Buffer;
}

function cancelled(): never {
  throw new RecorderError("operation_cancelled", "Recorder operation was cancelled");
}

function appendBounded(
  chunks: Buffer[],
  chunk: Buffer | string,
  total: { value: number },
): boolean {
  const bytes = typeof chunk === "string" ? Buffer.from(chunk) : chunk;
  total.value += bytes.length;
  if (total.value > COMMAND_OUTPUT_MAX_BYTES) {
    return false;
  }
  chunks.push(bytes);
  return true;
}

async function runCommand(
  executable: string,
  arguments_: readonly string[],
  signal: AbortSignal | undefined,
): Promise<ProcessResult> {
  if (signal?.aborted) {
    cancelled();
  }
  if (typeof executable !== "string" || !isAbsolute(executable)) {
    throw new RecorderError("bundle_cli_failed", "Bundle CLI executable must be an absolute path");
  }

  return await new Promise<ProcessResult>((resolve, reject) => {
    const child = spawn(executable, [...arguments_], {
      shell: false,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    const stdoutBytes = { value: 0 };
    const stderrBytes = { value: 0 };
    let overflow = false;
    let aborted = false;
    let spawnError: Error | undefined;
    let forceKillTimer: ReturnType<typeof setTimeout> | undefined;

    const terminate = (): void => {
      child.kill(process.platform === "win32" ? undefined : "SIGINT");
      forceKillTimer ??= setTimeout(() => child.kill("SIGKILL"), 2_000);
      forceKillTimer.unref();
    };

    const abortListener = (): void => {
      aborted = true;
      terminate();
    };
    signal?.addEventListener("abort", abortListener, { once: true });

    child.stdout.on("data", (chunk: Buffer | string) => {
      if (!appendBounded(stdout, chunk, stdoutBytes)) {
        overflow = true;
        terminate();
      }
    });
    child.stderr.on("data", (chunk: Buffer | string) => {
      if (!appendBounded(stderr, chunk, stderrBytes)) {
        overflow = true;
        terminate();
      }
    });
    child.once("error", (error) => {
      spawnError = error;
    });
    child.once("close", (code) => {
      if (forceKillTimer) {
        clearTimeout(forceKillTimer);
      }
      signal?.removeEventListener("abort", abortListener);
      if (aborted || signal?.aborted) {
        reject(new RecorderError("operation_cancelled", "Bundle CLI operation was cancelled"));
        return;
      }
      if (overflow) {
        reject(
          new RecorderError(
            "bundle_cli_failed",
            "Bundle CLI output exceeded the bounded diagnostic limit",
          ),
        );
        return;
      }
      if (spawnError) {
        reject(
          new RecorderError("bundle_cli_failed", "Bundle CLI could not be started", {
            cause: spawnError,
          }),
        );
        return;
      }
      resolve({
        exitCode: code ?? 1,
        stdout: Buffer.concat(stdout),
        stderr: Buffer.concat(stderr),
      });
    });
  });
}

function parseSummary(bytes: Buffer, operation: CliSummary["operation"]): CliSummary {
  let value: unknown;
  try {
    const text = bytes.toString("utf8");
    if (!text.endsWith("\n") || text.indexOf("\n") !== text.length - 1) {
      throw new Error("summary must be exactly one newline-terminated JSON value");
    }
    value = JSON.parse(text.slice(0, -1));
  } catch (cause) {
    throw new RecorderError("bundle_summary_invalid", "Bundle CLI returned invalid JSON", {
      cause,
    });
  }
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new RecorderError("bundle_summary_invalid", "Bundle CLI summary must be an object");
  }
  const summary = value as Record<string, unknown>;
  const keys = Object.keys(summary).sort();
  const expectedKeys = [
    "assetBytes",
    "assetCount",
    "eventCount",
    "ok",
    "operation",
    "sessionId",
  ];
  if (!isDeepStrictEqual(keys, expectedKeys)) {
    throw new RecorderError("bundle_summary_invalid", "Bundle CLI summary fields are invalid");
  }
  if (summary.ok !== true || summary.operation !== operation) {
    throw new RecorderError("bundle_summary_invalid", "Bundle CLI summary operation is invalid");
  }
  if (typeof summary.sessionId !== "string" || summary.sessionId.length === 0) {
    throw new RecorderError("bundle_summary_invalid", "Bundle CLI sessionId is invalid");
  }
  for (const field of ["eventCount", "assetCount", "assetBytes"] as const) {
    if (
      !Number.isSafeInteger(summary[field]) ||
      Object.is(summary[field], -0) ||
      (summary[field] as number) < 0
    ) {
      throw new RecorderError(
        "bundle_summary_invalid",
        `Bundle CLI ${field} must be a non-negative safe integer`,
      );
    }
  }
  return summary as unknown as CliSummary;
}

async function executeCli(
  executable: string,
  arguments_: readonly string[],
  operation: CliSummary["operation"],
  signal: AbortSignal | undefined,
): Promise<CliSummary> {
  const result = await runCommand(executable, arguments_, signal);
  if (result.exitCode !== 0) {
    const diagnostic = result.stderr.toString("utf8").trim();
    throw new RecorderError("bundle_cli_failed", `Bundle CLI ${operation} failed`, {
      details: {
        exitCode: result.exitCode,
        ...(diagnostic ? { diagnostic } : {}),
      },
    });
  }
  return parseSummary(result.stdout, operation);
}

async function pathExists(path: string): Promise<boolean> {
  try {
    await lstat(path);
    return true;
  } catch (cause) {
    if (cause instanceof Error && "code" in cause && cause.code === "ENOENT") {
      return false;
    }
    throw new RecorderError("bundle_cli_failed", "Bundle output metadata is inaccessible", {
      cause,
    });
  }
}

function expectedReceipt(source: BundleSource): Pick<RecorderBundleReceipt, "sessionId" | "eventCount"> {
  return {
    sessionId: source.sessionExport.session.id,
    eventCount: source.sessionExport.events.length,
  };
}

function verifySummary(summary: CliSummary, source: BundleSource): void {
  const expected = expectedReceipt(source);
  if (summary.sessionId !== expected.sessionId || summary.eventCount !== expected.eventCount) {
    throw new RecorderError(
      "bundle_summary_mismatch",
      "Bundle CLI summary does not identify the sealed recording",
      { details: { expected, actual: summary } },
    );
  }
}

async function verifyManifestIdentity(outputDirectory: string, source: BundleSource): Promise<void> {
  let bytes: Buffer;
  try {
    const path = join(outputDirectory, "manifest.json");
    const pathMetadata = await lstat(path);
    if (
      !pathMetadata.isFile() ||
      pathMetadata.isSymbolicLink() ||
      pathMetadata.size > MANIFEST_MAX_BYTES
    ) {
      throw new Error("manifest is not a bounded regular file");
    }
    const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const opened = await handle.stat();
      if (
        !opened.isFile() ||
        opened.dev !== pathMetadata.dev ||
        opened.ino !== pathMetadata.ino
      ) {
        throw new Error("manifest changed while it was opened");
      }
      const buffer = Buffer.allocUnsafe(MANIFEST_MAX_BYTES + 1);
      let offset = 0;
      while (offset < buffer.length) {
        const { bytesRead } = await handle.read(buffer, offset, buffer.length - offset, null);
        if (bytesRead === 0) {
          break;
        }
        offset += bytesRead;
      }
      if (offset > MANIFEST_MAX_BYTES) {
        throw new Error("manifest exceeds its bounded read limit");
      }
      bytes = buffer.subarray(0, offset);
    } finally {
      await handle.close().catch(() => {});
    }
  } catch (cause) {
    throw new RecorderError("bundle_summary_mismatch", "Validated Bundle manifest is unreadable", {
      cause,
    });
  }
  let manifest: unknown;
  try {
    manifest = JSON.parse(bytes.toString("utf8"));
  } catch (cause) {
    throw new RecorderError("bundle_summary_mismatch", "Validated Bundle manifest is invalid", {
      cause,
    });
  }
  if (manifest === null || typeof manifest !== "object" || Array.isArray(manifest)) {
    throw new RecorderError("bundle_summary_mismatch", "Validated Bundle manifest is invalid");
  }
  const record = manifest as Record<string, unknown>;
  if (
    !isDeepStrictEqual(record.eventProtocolVersion, source.eventProtocolVersion) ||
    !isDeepStrictEqual(record.session, source.sessionExport.session) ||
    !isDeepStrictEqual(record.events, source.sessionExport.events)
  ) {
    throw new RecorderError(
      "bundle_summary_mismatch",
      "Validated Bundle content does not match the sealed recording",
    );
  }
}

async function syncOutputParent(outputDirectory: string): Promise<void> {
  // Rust publication uses MoveFileExW(MOVEFILE_WRITE_THROUGH) on Windows.
  // On Unix this closes the durability-unknown recovery case before the
  // Recorder marks a previously published target complete.
  if (process.platform === "win32") {
    return;
  }
  try {
    const handle = await open(dirname(outputDirectory), "r");
    try {
      await handle.sync();
    } finally {
      await handle.close().catch(() => {});
    }
  } catch (cause) {
    throw new RecorderError(
      "bundle_cli_failed",
      "validated Bundle parent-directory durability could not be confirmed",
      { cause },
    );
  }
}

/** Runs the existing Rust Bundle writer and validator without a shell. */
export async function exportAndValidateBundle(
  options: BundleFinalizeOptions,
): Promise<RecorderBundleReceipt> {
  // The generated DTO types contain serde-default optionals that are wider
  // than the daemon's canonical output. Reject that lossy shape before the
  // Rust CLI can publish a target that would compare differently afterward.
  const source = validateBundleSourceFile(options.source);
  let exportSummary: CliSummary | undefined;
  if (!(await pathExists(options.outputDirectory))) {
    try {
      exportSummary = await executeCli(
        options.executable,
        [
          "export",
          "--source",
          options.sourcePath,
          "--evidence-dir",
          options.evidenceDirectory,
          "--output",
          options.outputDirectory,
        ],
        "export",
        options.signal,
      );
      verifySummary(exportSummary, source);
    } catch (error) {
      // Publication is the Rust writer's linearization point. If it raced the
      // process failure, converge through the independent validator below.
      if (!(await pathExists(options.outputDirectory))) {
        throw error;
      }
    }
  }

  const validateSummary = await executeCli(
    options.executable,
    ["validate", options.outputDirectory],
    "validate",
    options.signal,
  );
  verifySummary(validateSummary, source);
  if (
    exportSummary &&
    !isDeepStrictEqual(
      {
        sessionId: exportSummary.sessionId,
        eventCount: exportSummary.eventCount,
        assetCount: exportSummary.assetCount,
        assetBytes: exportSummary.assetBytes,
      },
      {
        sessionId: validateSummary.sessionId,
        eventCount: validateSummary.eventCount,
        assetCount: validateSummary.assetCount,
        assetBytes: validateSummary.assetBytes,
      },
    )
  ) {
    throw new RecorderError(
      "bundle_summary_mismatch",
      "Bundle export and validation summaries disagree",
    );
  }
  await verifyManifestIdentity(options.outputDirectory, source);
  await syncOutputParent(options.outputDirectory);
  return {
    sessionId: validateSummary.sessionId,
    eventCount: validateSummary.eventCount,
    assetCount: validateSummary.assetCount,
    assetBytes: validateSummary.assetBytes,
  };
}
