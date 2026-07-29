import type { ClientRpcMethod, DeviceRailClient } from "@devicerail/client";
import { JSON_SCHEMA, load } from "js-yaml";

export const YAML_PLAN_VERSION = "devicerail/v1";

const MAX_SOURCE_BYTES = 256 * 1024;
const MAX_STEPS = 256;
const MAX_DEPTH = 16;
const MAX_NODES = 4_096;
const MAX_COLLECTION_ITEMS = 512;
const MAX_OBJECT_PROPERTIES = 256;
const MAX_STRING_BYTES = 64 * 1024;
const MAX_KEY_BYTES = 256;
const MAX_TIMEOUT_MS = 24 * 60 * 60 * 1_000;
const STEP_ID = /^[A-Za-z][A-Za-z0-9_-]{0,63}$/u;
const FORBIDDEN_KEYS = new Set(["__proto__", "constructor", "prototype"]);
const TRUSTED_PLANS = new WeakSet<YamlPlan>();
const ACTIVE_CLIENTS = new WeakSet<object>();

const PLAN_METHODS = [
  "device.capabilities",
  "device.connect",
  "device.disconnect",
  "device.execute",
  "device.observe",
  "device.select",
  "devices.list",
  "events.clear",
  "events.list",
  "events.stream.open",
  "session.current",
  "session.end",
  "session.export",
  "session.start",
  "sessions.list",
  "system.describe",
] as const satisfies readonly ClientRpcMethod[];

const PLAN_METHOD_SET = new Set<string>(PLAN_METHODS);
const CANCELLABLE_METHOD_SET = new Set<YamlPlanMethod>([
  "device.capabilities",
  "device.connect",
  "device.disconnect",
  "device.execute",
  "device.observe",
]);

export type YamlPlanMethod = (typeof PLAN_METHODS)[number];

export interface ActionProtectionContext {
  readonly actionName: string;
  readonly deviceId: string;
}

export type ActionProtectionClassifier = (
  context: ActionProtectionContext,
) => "protected" | "standard" | undefined;

export interface CompileYamlPlanOptions {
  /**
   * Required for every `device.execute` step. Unknown and protected actions
   * are rejected so secrets cannot be persisted in a YAML plan by accident.
   */
  readonly classifyActionProtection?: ActionProtectionClassifier;
  /** Device selected before the first YAML step when the plan does not select one itself. */
  readonly initialDeviceId?: string;
}

export interface YamlPlanStep {
  readonly id: string;
  readonly method: YamlPlanMethod;
  readonly params?: Readonly<Record<string, unknown>>;
  readonly timeoutMs?: number;
  /** Host-derived route binding; never read from YAML or sent as action params. */
  readonly boundDeviceId?: string;
}

export interface YamlPlan {
  readonly steps: readonly YamlPlanStep[];
  readonly version: typeof YAML_PLAN_VERSION;
}

export type YamlPlanClient = Pick<DeviceRailClient, "call">;

export interface ExecuteYamlPlanOptions {
  readonly signal?: AbortSignal;
}

export interface YamlStepExecution {
  readonly id: string;
  readonly method: YamlPlanMethod;
  readonly result: unknown;
}

export interface YamlPlanExecution {
  readonly steps: readonly YamlStepExecution[];
}

export class YamlPlanValidationError extends Error {
  readonly code: string;
  readonly path: string;

  constructor(code: string, path: string, message: string) {
    super(message);
    this.name = "YamlPlanValidationError";
    this.code = code;
    this.path = path;
  }
}

export class YamlPlanExecutionError extends Error {
  readonly stepId: string;
  readonly method: YamlPlanMethod;
  override readonly cause: unknown;

  constructor(step: YamlPlanStep, cause: unknown) {
    super(`YAML plan step ${step.id} failed`);
    this.name = "YamlPlanExecutionError";
    this.stepId = step.id;
    this.method = step.method;
    this.cause = cause;
  }
}

function fail(code: string, path: string, message: string): never {
  throw new YamlPlanValidationError(code, path, message);
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

interface JsonBudget {
  nodes: number;
  readonly seen: WeakSet<object>;
}

function cloneBoundedJson(value: unknown, path: string, depth: number, budget: JsonBudget): unknown {
  budget.nodes += 1;
  if (budget.nodes > MAX_NODES) {
    fail("yaml_node_limit", path, "YAML plan exceeds the node limit");
  }
  if (depth > MAX_DEPTH) {
    fail("yaml_depth_limit", path, "YAML plan exceeds the nesting limit");
  }
  if (value === null || typeof value === "boolean") {
    return value;
  }
  if (typeof value === "string") {
    if (Buffer.byteLength(value, "utf8") > MAX_STRING_BYTES) {
      fail("yaml_string_limit", path, "YAML string exceeds the byte limit");
    }
    return value;
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value) || !Number.isSafeInteger(value) && value % 1 === 0) {
      fail("yaml_number_invalid", path, "YAML number is not safely representable as JSON");
    }
    return value;
  }
  if (typeof value !== "object") {
    fail("yaml_type_invalid", path, "YAML contains a non-JSON value");
  }
  if (budget.seen.has(value)) {
    fail("yaml_alias_forbidden", path, "YAML aliases and cyclic objects are not supported");
  }
  budget.seen.add(value);
  if (Array.isArray(value)) {
    if (value.length > MAX_COLLECTION_ITEMS) {
      fail("yaml_collection_limit", path, "YAML sequence exceeds the item limit");
    }
    return Object.freeze(
      value.map((entry, index) => cloneBoundedJson(entry, `${path}[${index}]`, depth + 1, budget)),
    );
  }
  const entries = Object.entries(value);
  if (entries.length > MAX_OBJECT_PROPERTIES) {
    fail("yaml_object_limit", path, "YAML mapping exceeds the property limit");
  }
  const clone: Record<string, unknown> = {};
  for (const [key, entry] of entries) {
    if (FORBIDDEN_KEYS.has(key)) {
      fail("yaml_key_forbidden", `${path}.${key}`, "YAML mapping contains a forbidden key");
    }
    if (Buffer.byteLength(key, "utf8") > MAX_KEY_BYTES) {
      fail("yaml_key_limit", path, "YAML mapping key exceeds the byte limit");
    }
    clone[key] = cloneBoundedJson(entry, `${path}.${key}`, depth + 1, budget);
  }
  return Object.freeze(clone);
}

function exactKeys(
  value: Record<string, unknown>,
  allowed: ReadonlySet<string>,
  path: string,
): void {
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) {
      fail("yaml_unknown_field", `${path}.${key}`, "YAML plan contains an unknown field");
    }
  }
}

function positiveTimeout(value: unknown, path: string): number | undefined {
  if (value === undefined) {
    return undefined;
  }
  if (!Number.isSafeInteger(value) || (value as number) <= 0 || (value as number) > MAX_TIMEOUT_MS) {
    fail("yaml_timeout_invalid", path, "timeoutMs must be a positive bounded safe integer");
  }
  return value as number;
}

function requestOptions(
  method: YamlPlanMethod,
  timeoutMs: number | undefined,
  signal: AbortSignal | undefined,
): { readonly signal?: AbortSignal; readonly timeoutMs?: number } | undefined {
  if (!CANCELLABLE_METHOD_SET.has(method)) {
    return undefined;
  }
  if (timeoutMs === undefined && signal === undefined) {
    return undefined;
  }
  return {
    ...(signal === undefined ? {} : { signal }),
    ...(timeoutMs === undefined ? {} : { timeoutMs }),
  };
}

function throwAbortCauseIfNeeded(signal: AbortSignal | undefined): void {
  if (signal?.aborted) {
    throw signal.reason;
  }
}

function requireStandardAction(
  params: Readonly<Record<string, unknown>> | undefined,
  options: CompileYamlPlanOptions,
  path: string,
  deviceId: string | undefined,
): string {
  const name = params?.name;
  if (typeof name !== "string" || name.length === 0 || Buffer.byteLength(name, "utf8") > 128) {
    fail("yaml_action_name_invalid", `${path}.params.name`, "device.execute requires a bounded action name");
  }
  if (deviceId === undefined) {
    fail(
      "yaml_action_device_unbound",
      `${path}.params.name`,
      "device.execute requires a fixed selected device",
    );
  }
  const protection = options.classifyActionProtection?.({ actionName: name, deviceId });
  if (protection === "protected") {
    fail("yaml_protected_action_forbidden", `${path}.params.name`, "protected actions cannot be persisted in YAML");
  }
  if (protection !== "standard") {
    fail("yaml_action_unknown", `${path}.params.name`, "device.execute action must be confirmed as standard");
  }
  return deviceId;
}

function boundedDeviceId(value: unknown, path: string): string {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    Buffer.byteLength(value, "utf8") > MAX_KEY_BYTES ||
    [...value].some((character) => character <= "\u001f" || character === "\u007f")
  ) {
    fail("yaml_device_id_invalid", path, "selected device id must be a bounded printable string");
  }
  return value;
}

export function compileYamlPlan(source: string, options: CompileYamlPlanOptions = {}): YamlPlan {
  if (typeof source !== "string" || Buffer.byteLength(source, "utf8") > MAX_SOURCE_BYTES) {
    fail("yaml_source_limit", "$", "YAML source exceeds the byte limit");
  }
  let decoded: unknown;
  try {
    decoded = load(source, {
      filename: "devicerail-plan.yaml",
      json: false,
      maxDepth: MAX_DEPTH + 2,
      maxTotalMergeKeys: 0,
      schema: JSON_SCHEMA,
    });
  } catch {
    fail("yaml_parse_failed", "$", "YAML source is not valid safe YAML");
  }
  const cloned = cloneBoundedJson(decoded, "$", 0, { nodes: 0, seen: new WeakSet() });
  if (!isObject(cloned)) {
    fail("yaml_plan_invalid", "$", "YAML plan must be a mapping");
  }
  exactKeys(cloned, new Set(["version", "steps"]), "$");
  if (cloned.version !== YAML_PLAN_VERSION) {
    fail("yaml_version_unsupported", "$.version", "YAML plan version is unsupported");
  }
  if (!Array.isArray(cloned.steps) || cloned.steps.length === 0 || cloned.steps.length > MAX_STEPS) {
    fail("yaml_steps_invalid", "$.steps", "YAML plan must contain a bounded non-empty steps sequence");
  }

  const ids = new Set<string>();
  let selectedDeviceId =
    options.initialDeviceId === undefined
      ? undefined
      : boundedDeviceId(options.initialDeviceId, "$.initialDeviceId");
  const steps = cloned.steps.map((rawStep, index): YamlPlanStep => {
    const path = `$.steps[${index}]`;
    if (!isObject(rawStep)) {
      fail("yaml_step_invalid", path, "YAML plan step must be a mapping");
    }
    exactKeys(rawStep, new Set(["id", "method", "params", "timeoutMs"]), path);
    if (typeof rawStep.id !== "string" || !STEP_ID.test(rawStep.id) || ids.has(rawStep.id)) {
      fail("yaml_step_id_invalid", `${path}.id`, "step id must be unique and portable");
    }
    ids.add(rawStep.id);
    if (typeof rawStep.method !== "string" || !PLAN_METHOD_SET.has(rawStep.method)) {
      fail("yaml_method_forbidden", `${path}.method`, "method is not available to YAML plans");
    }
    const method = rawStep.method as YamlPlanMethod;
    const params = rawStep.params;
    if (params !== undefined && !isObject(params)) {
      fail("yaml_params_invalid", `${path}.params`, "params must be a JSON mapping");
    }
    if (method === "device.select") {
      if (params === undefined) {
        fail("yaml_device_select_invalid", `${path}.params`, "device.select requires params");
      }
      exactKeys(params, new Set(["deviceId"]), `${path}.params`);
      selectedDeviceId = boundedDeviceId(params.deviceId, `${path}.params.deviceId`);
    }
    let boundDeviceId: string | undefined;
    if (method === "device.execute") {
      boundDeviceId = requireStandardAction(params, options, path, selectedDeviceId);
    }
    const timeoutMs = positiveTimeout(rawStep.timeoutMs, `${path}.timeoutMs`);
    if (timeoutMs !== undefined && !CANCELLABLE_METHOD_SET.has(method)) {
      fail(
        "yaml_timeout_unsupported",
        `${path}.timeoutMs`,
        `${method} does not support timeoutMs`,
      );
    }
    return Object.freeze({
      id: rawStep.id,
      method,
      ...(params === undefined ? {} : { params }),
      ...(timeoutMs === undefined ? {} : { timeoutMs }),
      ...(boundDeviceId === undefined ? {} : { boundDeviceId }),
    });
  });

  const plan = Object.freeze({
    version: YAML_PLAN_VERSION,
    steps: Object.freeze(steps),
  });
  TRUSTED_PLANS.add(plan);
  return plan;
}

function validateTrustedPlan(plan: YamlPlan): void {
  if (!TRUSTED_PLANS.has(plan)) {
    fail("yaml_plan_untrusted", "$", "YAML plan must be produced by compileYamlPlan");
  }
  if (
    plan.version !== YAML_PLAN_VERSION ||
    !Object.isFrozen(plan) ||
    !Object.isFrozen(plan.steps) ||
    plan.steps.length === 0 ||
    plan.steps.length > MAX_STEPS
  ) {
    fail("yaml_plan_corrupt", "$", "compiled YAML plan integrity check failed");
  }
  const ids = new Set<string>();
  for (const [index, step] of plan.steps.entries()) {
    const path = `$.steps[${index}]`;
    if (
      !Object.isFrozen(step) ||
      !STEP_ID.test(step.id) ||
      ids.has(step.id) ||
      !PLAN_METHOD_SET.has(step.method)
    ) {
      fail("yaml_plan_corrupt", path, "compiled YAML plan integrity check failed");
    }
    ids.add(step.id);
    if (step.method === "device.execute" && step.boundDeviceId === undefined) {
      fail("yaml_plan_corrupt", path, "compiled action route binding is missing");
    }
  }
}

function actionName(step: YamlPlanStep): string {
  const name = step.params?.name;
  if (typeof name !== "string") {
    fail("yaml_plan_corrupt", `$.steps.${step.id}`, "compiled action name is missing");
  }
  return name;
}

type PublicCall = (
  method: ClientRpcMethod,
  params?: unknown,
  options?: { readonly signal?: AbortSignal; readonly timeoutMs?: number },
) => Promise<unknown>;

async function executeVerifiedAction(
  call: PublicCall,
  step: YamlPlanStep,
  signal: AbortSignal | undefined,
): Promise<unknown> {
  const deviceId = step.boundDeviceId;
  if (deviceId === undefined) {
    fail("yaml_plan_corrupt", `$.steps.${step.id}`, "compiled action route binding is missing");
  }
  // Re-select and then re-read the public capability immediately before the
  // action. This catches route drift and a standard -> protected transition.
  await call("device.select", { deviceId });
  throwAbortCauseIfNeeded(signal);
  const inventory = await call("devices.list");
  if (!isObject(inventory) || inventory.selectedDeviceId !== deviceId) {
    fail("yaml_action_route_changed", `$.steps.${step.id}`, "selected device route changed");
  }
  throwAbortCauseIfNeeded(signal);
  const capabilities = await call(
    "device.capabilities",
    undefined,
    requestOptions("device.capabilities", step.timeoutMs, signal),
  );
  const name = actionName(step);
  const definition = Array.isArray(capabilities)
    ? capabilities.find((candidate) => isObject(candidate) && candidate.name === name)
    : undefined;
  if (!isObject(definition) || (definition.protection ?? "standard") !== "standard") {
    fail(
      "yaml_action_runtime_forbidden",
      `$.steps.${step.id}`,
      "action is missing or protected on the selected device",
    );
  }
  throwAbortCauseIfNeeded(signal);
  // Start the action in this same continuation. Returning to executeYamlPlan
  // here would introduce a microtask window in which another caller could
  // change the connection-local route after the capability check.
  return await call(
    "device.execute",
    step.params,
    requestOptions("device.execute", step.timeoutMs, signal),
  );
}

export async function executeYamlPlan(
  client: YamlPlanClient,
  plan: YamlPlan,
  options: ExecuteYamlPlanOptions = {},
): Promise<YamlPlanExecution> {
  validateTrustedPlan(plan);
  if (ACTIVE_CLIENTS.has(client)) {
    fail("yaml_client_busy", "$", "this client is already executing a YAML plan");
  }
  ACTIVE_CLIENTS.add(client);
  const completed: YamlStepExecution[] = [];
  try {
    const call = client.call.bind(client) as unknown as PublicCall;
    for (const step of plan.steps) {
      if (options.signal?.aborted) {
        throw new YamlPlanExecutionError(step, options.signal.reason);
      }
      try {
        const result =
          step.method === "device.execute"
            ? await executeVerifiedAction(call, step, options.signal)
            : await call(
                step.method,
                step.params,
                requestOptions(step.method, step.timeoutMs, options.signal),
              );
        completed.push(Object.freeze({ id: step.id, method: step.method, result }));
      } catch (cause) {
        throw new YamlPlanExecutionError(step, cause);
      }
    }
    return Object.freeze({ steps: Object.freeze(completed) });
  } finally {
    ACTIVE_CLIENTS.delete(client);
  }
}
