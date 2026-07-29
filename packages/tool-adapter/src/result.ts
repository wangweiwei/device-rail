import { ProtocolViolationError, validateRpcResult } from "@devicerail/client";
import type { ActionResult, AssetRef, Observation } from "@devicerail/protocol";

import { InvalidToolResultError } from "./errors.js";
import { clonePureJson, deepFreezeJson } from "./json.js";

type JsonObject = Record<string, unknown>;

interface ValidatedObservation {
  readonly hasScreenshot: boolean;
  readonly omission: "policy" | "protectedAction" | undefined;
  readonly value: Observation;
}

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu;
const NIL_UUID = "00000000-0000-0000-0000-000000000000";
const SHA256_PATTERN = /^[0-9a-f]{64}$/iu;
const MAX_U32 = 4_294_967_295;

function isRecord(value: unknown): value is JsonObject {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function fail(toolName: string, message: string): never {
  throw new InvalidToolResultError(toolName, message);
}

function requiredSafeInteger(
  toolName: string,
  value: unknown,
  name: string,
  minimum: number,
  maximum = Number.MAX_SAFE_INTEGER,
): number {
  if (
    typeof value !== "number" ||
    !Number.isSafeInteger(value) ||
    value < minimum ||
    value > maximum
  ) {
    fail(toolName, `${name} must be a safe integer between ${minimum} and ${maximum}`);
  }
  return value;
}

function validateAsset(toolName: string, value: unknown, name: string): AssetRef {
  if (!isRecord(value)) {
    fail(toolName, `${name} must be an evidence object`);
  }
  if (typeof value.id !== "string" || value.id.trim().length === 0) {
    fail(toolName, `${name}.id must be a non-empty string`);
  }
  if (
    typeof value.mediaType !== "string" ||
    !value.mediaType.includes("/") ||
    /\s/u.test(value.mediaType)
  ) {
    fail(toolName, `${name}.mediaType must be a valid media type`);
  }
  if (typeof value.uri !== "string" || value.uri.trim().length === 0) {
    fail(toolName, `${name}.uri must be a non-empty string`);
  }
  if (
    value.sha256 !== undefined &&
    value.sha256 !== null &&
    (typeof value.sha256 !== "string" || !SHA256_PATTERN.test(value.sha256))
  ) {
    fail(toolName, `${name}.sha256 must be 64 hexadecimal characters when present`);
  }
  return value as AssetRef;
}

function validateObservation(
  toolName: string,
  value: unknown,
  name: string,
): ValidatedObservation {
  if (!isRecord(value)) {
    fail(toolName, `${name} must be an observation object`);
  }
  if (
    typeof value.id !== "string" ||
    !UUID_PATTERN.test(value.id) ||
    value.id.toLowerCase() === NIL_UUID
  ) {
    fail(toolName, `${name}.id must be a non-nil UUID`);
  }
  if (typeof value.deviceId !== "string" || value.deviceId.trim().length === 0) {
    fail(toolName, `${name}.deviceId must be a non-empty string`);
  }
  requiredSafeInteger(toolName, value.capturedAtMs, `${name}.capturedAtMs`, 1);
  if (!isRecord(value.viewport)) {
    fail(toolName, `${name}.viewport must be an object`);
  }
  requiredSafeInteger(toolName, value.viewport.width, `${name}.viewport.width`, 1, MAX_U32);
  requiredSafeInteger(toolName, value.viewport.height, `${name}.viewport.height`, 1, MAX_U32);
  if (
    typeof value.viewport.scaleFactor !== "number" ||
    !Number.isFinite(value.viewport.scaleFactor) ||
    value.viewport.scaleFactor <= 0
  ) {
    fail(toolName, `${name}.viewport.scaleFactor must be a positive finite number`);
  }
  if (value.metadata !== undefined && !isRecord(value.metadata)) {
    fail(toolName, `${name}.metadata must be an object when present`);
  }
  const hasScreenshot = value.screenshot !== undefined && value.screenshot !== null;
  if (hasScreenshot) {
    validateAsset(toolName, value.screenshot, `${name}.screenshot`);
  }
  const omission = value.screenshotOmission;
  if (
    omission !== undefined &&
    omission !== "policy" &&
    omission !== "protectedAction"
  ) {
    fail(
      toolName,
      `${name}.screenshotOmission must be policy or protectedAction when present`,
    );
  }
  if (hasScreenshot && omission !== undefined) {
    fail(toolName, `${name} must not contain both a screenshot and screenshotOmission`);
  }
  return {
    hasScreenshot,
    omission,
    value: value as Observation,
  };
}

function pureResult(
  toolName: string,
  method: "device.execute" | "device.observe",
  value: unknown,
): unknown {
  const cloned = deepFreezeJson(
    clonePureJson(
      value,
      (message) => new InvalidToolResultError(toolName, `result ${message}`),
    ),
  );
  try {
    validateRpcResult(method, cloned);
  } catch (cause) {
    if (cause instanceof ProtocolViolationError) {
      throw new InvalidToolResultError(toolName, cause.message);
    }
    throw cause;
  }
  return cloned;
}

export function validateObservationResult(toolName: string, value: unknown): Observation {
  const cloned = pureResult(toolName, "device.observe", value);
  return validateObservation(toolName, cloned, "observation").value;
}

export function validateActionResult(
  toolName: string,
  actionCallId: string,
  value: unknown,
  protection?: "protected",
): ActionResult {
  const cloned = pureResult(toolName, "device.execute", value);
  if (!isRecord(cloned)) {
    fail(toolName, "device.execute must return an ActionResult object");
  }
  if (cloned.callId !== actionCallId) {
    fail(toolName, "device.execute returned a result for a different Action call");
  }
  const startedAtMs = requiredSafeInteger(
    toolName,
    cloned.startedAtMs,
    "action.startedAtMs",
    1,
  );
  const finishedAtMs = requiredSafeInteger(
    toolName,
    cloned.finishedAtMs,
    "action.finishedAtMs",
    1,
  );
  if (finishedAtMs < startedAtMs) {
    fail(toolName, "action.finishedAtMs must not precede action.startedAtMs");
  }
  if (!Object.hasOwn(cloned, "output")) {
    fail(toolName, "action.output is required");
  }
  const before =
    cloned.before === undefined || cloned.before === null
      ? undefined
      : validateObservation(toolName, cloned.before, "action.before");
  if (cloned.after === undefined || cloned.after === null) {
    fail(toolName, "action.after observation is required");
  }
  const after = validateObservation(toolName, cloned.after, "action.after");
  if (before !== undefined && before.value.deviceId !== after.value.deviceId) {
    fail(toolName, "action.before and action.after must identify the same device");
  }
  if (protection === "protected") {
    if (before === undefined) {
      fail(toolName, "protected action.before observation is required");
    }
    const observations = [before, after];
    if (observations.some((observation) => observation.omission !== "protectedAction")) {
      fail(
        toolName,
        "protected action observations must explicitly omit screenshots as protectedAction",
      );
    }
  }
  if (!Array.isArray(cloned.evidence)) {
    fail(toolName, "action.evidence must be an array");
  }
  if (protection === "protected" && cloned.evidence.length !== 0) {
    fail(toolName, "protected action.evidence must be empty");
  }
  if (cloned.evidence.length === 0) {
    const observations = before === undefined ? [after] : [before, after];
    if (observations.some((observation) => observation.omission === undefined)) {
      fail(
        toolName,
        "action.evidence may be empty only when every observation explicitly omits its screenshot",
      );
    }
  }
  const evidenceIds = new Set<string>();
  cloned.evidence.forEach((asset, index) => {
    const validated = validateAsset(toolName, asset, `action.evidence[${index}]`);
    if (evidenceIds.has(validated.id)) {
      fail(toolName, `action.evidence contains duplicate id ${validated.id}`);
    }
    evidenceIds.add(validated.id);
  });
  return cloned as unknown as ActionResult;
}
