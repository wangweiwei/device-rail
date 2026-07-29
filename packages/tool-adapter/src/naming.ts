import { createHash } from "node:crypto";

import { InvalidActionSpaceError } from "./errors.js";

const ACTION_TOOL_PREFIX = "devicerail_action_";
const MAX_TOOL_NAME_LENGTH = 64;

export const OBSERVATION_TOOL_NAME = "devicerail_observe";

export function actionToolName(actionName: string): string {
  if (typeof actionName !== "string" || actionName.trim().length === 0) {
    throw new InvalidActionSpaceError("action names must be non-empty strings");
  }
  const raw = `${ACTION_TOOL_PREFIX}raw_${actionName}`;
  if (/^[A-Za-z0-9_-]+$/u.test(actionName) && raw.length <= MAX_TOOL_NAME_LENGTH) {
    return raw;
  }
  const encoded = Buffer.from(actionName, "utf8").toString("base64url");
  const base64 = `${ACTION_TOOL_PREFIX}b64_${encoded}`;
  if (base64.length <= MAX_TOOL_NAME_LENGTH) {
    return base64;
  }
  const digest = createHash("sha256").update(actionName, "utf8").digest("hex").slice(0, 32);
  return `${ACTION_TOOL_PREFIX}sha256_${digest}`;
}
