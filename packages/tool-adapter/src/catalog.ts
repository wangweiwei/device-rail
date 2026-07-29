import { randomUUID } from "node:crypto";
import { types as utilTypes } from "node:util";

import {
  ProtocolViolationError,
  validateRpcResult,
  type CallOptions,
} from "@devicerail/client";
import type { ActionDefinition, RpcId } from "@devicerail/protocol";

import {
  InvalidActionSpaceError,
  InvalidToolArgumentsError,
  InvalidToolOptionsError,
  UnknownToolError,
} from "./errors.js";
import { clonePureJson, deepFreezeJson } from "./json.js";
import { actionToolName, OBSERVATION_TOOL_NAME } from "./naming.js";
import { validateActionResult, validateObservationResult } from "./result.js";
import { validateToolInputSchema } from "./schema.js";
import type {
  ActionToolDefinition,
  ActionToolInvocationHandle,
  ActionToolResult,
  DeviceRailToolAdapterOptions,
  DeviceRailToolCatalog,
  DeviceRailToolClient,
  DeviceRailToolDefinition,
  ObservationToolDefinition,
  ObservationToolInvocationHandle,
  ObservationToolResult,
  ToolDiscoveryOptions,
  ToolInputSchema,
  ToolInvocation,
  ToolInvocationHandle,
  ToolInvocationOptions,
  ToolInvocationResult,
} from "./types.js";

const DEFAULT_MAX_ACTIONS = 256;
const ACTION_PROTECTED_FEATURE = "action.protected.v1";
interface CatalogEntry {
  readonly definition: DeviceRailToolDefinition;
}

interface NormalizedInvocation {
  readonly arguments: Record<string, unknown>;
  readonly invocationId?: string;
  readonly name: string;
}

interface NormalizedOptions {
  readonly actionTimeoutMs?: number;
  readonly requestTimeoutMs?: number;
  readonly signal?: AbortSignal;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function positiveSafeInteger(value: unknown, name: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) {
    throw new InvalidToolOptionsError(`${name} must be a positive safe integer`);
  }
  return value;
}

function isAbortSignal(value: unknown): value is AbortSignal {
  return value instanceof AbortSignal;
}

function protectedFeatureEnabled(client: DeviceRailToolClient): boolean {
  return client.enabledFeatures?.has(ACTION_PROTECTED_FEATURE) ?? false;
}

function plainOptionRecord(value: unknown, context: string): Record<string, unknown> {
  if (!isRecord(value) || utilTypes.isProxy(value)) {
    throw new InvalidToolOptionsError(`${context} must be a plain object`);
  }
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) {
    throw new InvalidToolOptionsError(`${context} must be a plain object`);
  }
  const keys = Reflect.ownKeys(value);
  if (keys.some((key) => typeof key !== "string")) {
    throw new InvalidToolOptionsError(`${context} must not contain symbol properties`);
  }
  const descriptors = Object.getOwnPropertyDescriptors(value);
  const record: Record<string, unknown> = {};
  for (const key of keys as string[]) {
    const descriptor = descriptors[key];
    if (!descriptor?.enumerable || !("value" in descriptor)) {
      throw new InvalidToolOptionsError(`${context}.${key} must be a plain data property`);
    }
    Object.defineProperty(record, key, {
      configurable: true,
      enumerable: true,
      value: descriptor.value,
      writable: true,
    });
  }
  return record;
}

function normalizeOptions(
  value: ToolInvocationOptions | ToolDiscoveryOptions | undefined,
  context: "tool discovery options" | "tool invocation options",
  allowActionTimeout: boolean,
): NormalizedOptions {
  if (value === undefined) {
    return {};
  }
  const record = plainOptionRecord(value, context);
  const allowed = new Set([
    ...(allowActionTimeout ? ["actionTimeoutMs"] : []),
    "requestTimeoutMs",
    "signal",
  ]);
  for (const key of Object.keys(record)) {
    if (!allowed.has(key)) {
      throw new InvalidToolOptionsError(`unknown ${context.slice(0, -1)} ${key}`);
    }
  }
  const actionTimeoutMs =
    record.actionTimeoutMs === undefined
      ? undefined
      : positiveSafeInteger(record.actionTimeoutMs, "actionTimeoutMs");
  const requestTimeoutMs =
    record.requestTimeoutMs === undefined
      ? undefined
      : positiveSafeInteger(record.requestTimeoutMs, "requestTimeoutMs");
  const signal = record.signal;
  if (signal !== undefined && !isAbortSignal(signal)) {
    throw new InvalidToolOptionsError("signal must be an AbortSignal");
  }
  return {
    ...(actionTimeoutMs === undefined ? {} : { actionTimeoutMs }),
    ...(requestTimeoutMs === undefined ? {} : { requestTimeoutMs }),
    ...(signal === undefined ? {} : { signal }),
  };
}

function normalizeDiscoveryOptions(value: ToolDiscoveryOptions | undefined): NormalizedOptions {
  return normalizeOptions(value, "tool discovery options", false);
}

function normalizeInvocation(value: ToolInvocation): NormalizedInvocation {
  let errorToolName = "<unknown>";
  if (isRecord(value) && !utilTypes.isProxy(value)) {
    const nameDescriptor = Object.getOwnPropertyDescriptor(value, "name");
    if (
      nameDescriptor?.enumerable &&
      "value" in nameDescriptor &&
      typeof nameDescriptor.value === "string" &&
      nameDescriptor.value.length > 0
    ) {
      errorToolName = nameDescriptor.value;
    }
  }
  const cloned = clonePureJson(
    value,
    (message) => new InvalidToolArgumentsError(errorToolName, message),
  );
  if (!isRecord(cloned)) {
    throw new InvalidToolArgumentsError("<unknown>", "tool invocation must be an object");
  }
  const allowed = new Set(["arguments", "invocationId", "name"]);
  for (const key of Object.keys(cloned)) {
    if (!allowed.has(key)) {
      throw new InvalidToolArgumentsError("<unknown>", `unknown invocation field ${key}`);
    }
  }
  if (typeof cloned.name !== "string" || cloned.name.length === 0) {
    throw new InvalidToolArgumentsError("<unknown>", "tool name must be a non-empty string");
  }
  if (cloned.invocationId !== undefined && typeof cloned.invocationId !== "string") {
    throw new InvalidToolArgumentsError(cloned.name, "invocationId must be a string");
  }
  const argumentsValue = cloned.arguments === undefined ? {} : cloned.arguments;
  if (!isRecord(argumentsValue)) {
    throw new InvalidToolArgumentsError(cloned.name, "arguments must be a JSON object");
  }
  return {
    arguments: deepFreezeJson(argumentsValue),
    ...(cloned.invocationId === undefined ? {} : { invocationId: cloned.invocationId }),
    name: cloned.name,
  };
}

function callOptions<M extends "device.capabilities" | "device.execute" | "device.observe">(
  options: NormalizedOptions,
): CallOptions<M> {
  return {
    ...(options.requestTimeoutMs === undefined ? {} : { timeoutMs: options.requestTimeoutMs }),
    ...(options.signal === undefined ? {} : { signal: options.signal }),
  } as CallOptions<M>;
}

function compileDefinitions(
  capabilities: unknown,
  includeObservation: boolean,
  includeProtectedActions: boolean,
  maxActions: number,
): readonly DeviceRailToolDefinition[] {
  const clonedCapabilities = clonePureJson(
    capabilities,
    (message) => new InvalidActionSpaceError(`device.capabilities: ${message}`),
  );
  if (!Array.isArray(clonedCapabilities)) {
    throw new InvalidActionSpaceError("device.capabilities must return an array");
  }
  if (clonedCapabilities.length > maxActions) {
    throw new InvalidActionSpaceError(
      `device.capabilities returned ${clonedCapabilities.length} actions; the limit is ${maxActions}`,
    );
  }

  const actions: Array<{
    readonly description: string;
    readonly inputSchema: ToolInputSchema;
    readonly name: string;
    readonly protection: "protected" | "standard";
  }> = [];
  const actionNames = new Set<string>();
  for (let index = 0; index < clonedCapabilities.length; index += 1) {
    const capability = clonedCapabilities[index] as ActionDefinition | unknown;
    if (!isRecord(capability)) {
      throw new InvalidActionSpaceError(`capability ${index} must be an object`);
    }
    if (typeof capability.name !== "string" || capability.name.trim().length === 0) {
      throw new InvalidActionSpaceError(`capability ${index} has an invalid action name`);
    }
    if (
      typeof capability.description !== "string" ||
      capability.description.trim().length === 0
    ) {
      throw new InvalidActionSpaceError(`capability ${capability.name} has an invalid description`);
    }
    if (actionNames.has(capability.name)) {
      throw new InvalidActionSpaceError(`duplicate action name ${capability.name}`);
    }
    const protection = capability.protection ?? "standard";
    if (protection !== "standard" && protection !== "protected") {
      throw new InvalidActionSpaceError(
        `capability ${capability.name} has an invalid protection classification`,
      );
    }
    actionNames.add(capability.name);
    actions.push({
      description: capability.description,
      inputSchema: validateToolInputSchema(capability.name, capability.inputSchema),
      name: capability.name,
      protection,
    });
  }
  try {
    validateRpcResult("device.capabilities", clonedCapabilities);
  } catch (cause) {
    if (cause instanceof ProtocolViolationError) {
      throw new InvalidActionSpaceError(`device.capabilities: ${cause.message}`, { cause });
    }
    throw cause;
  }
  actions.sort((left, right) => (left.name < right.name ? -1 : left.name > right.name ? 1 : 0));

  const definitions: DeviceRailToolDefinition[] = [];
  if (includeObservation) {
    const observation: ObservationToolDefinition = Object.freeze({
      description: "Capture the current DeviceRail device observation",
      inputSchema: deepFreezeJson({ additionalProperties: false, type: "object" }),
      kind: "observation",
      name: OBSERVATION_TOOL_NAME,
    });
    definitions.push(observation);
  }
  const toolNames = new Set(definitions.map((definition) => definition.name));
  for (const action of actions) {
    if (action.protection === "protected" && !includeProtectedActions) {
      continue;
    }
    const name = actionToolName(action.name);
    if (toolNames.has(name)) {
      throw new InvalidActionSpaceError(`portable tool name collision for action ${action.name}`);
    }
    toolNames.add(name);
    const definition: ActionToolDefinition = Object.freeze({
      actionName: action.name,
      description: action.description,
      inputSchema: action.inputSchema,
      kind: "action",
      name,
      ...(action.protection === "protected" ? { protection: "protected" as const } : {}),
    });
    definitions.push(definition);
  }
  return Object.freeze(definitions);
}

class ToolCatalog implements DeviceRailToolCatalog {
  readonly id = randomUUID();
  readonly revision: number;
  readonly tools: readonly DeviceRailToolDefinition[];

  readonly #client: DeviceRailToolClient;
  readonly #entries: ReadonlyMap<string, CatalogEntry>;

  constructor(
    client: DeviceRailToolClient,
    revision: number,
    tools: readonly DeviceRailToolDefinition[],
  ) {
    this.#client = client;
    this.revision = revision;
    this.tools = tools;
    this.#entries = new Map(tools.map((definition) => [definition.name, { definition }]));
    Object.freeze(this);
  }

  beginInvoke(
    rawInvocation: ToolInvocation,
    rawOptions?: ToolInvocationOptions,
  ): ToolInvocationHandle {
    const invocation = normalizeInvocation(rawInvocation);
    const options = normalizeOptions(rawOptions, "tool invocation options", true);
    const entry = this.#entries.get(invocation.name);
    if (!entry) {
      throw new UnknownToolError(invocation.name);
    }
    if (entry.definition.kind === "observation") {
      if (Object.keys(invocation.arguments).length !== 0) {
        throw new InvalidToolArgumentsError(
          invocation.name,
          "the observation tool accepts no arguments",
        );
      }
      if (options.actionTimeoutMs !== undefined) {
        throw new InvalidToolOptionsError(
          "actionTimeoutMs is only valid for action tools",
        );
      }
      const request = this.#client.beginCall(
        "device.observe",
        undefined,
        callOptions<"device.observe">(options),
      );
      const result = request.result.then<ObservationToolResult>((observation) => {
        const validated = validateObservationResult(invocation.name, observation);
        return {
          ...(invocation.invocationId === undefined
            ? {}
            : { invocationId: invocation.invocationId }),
          kind: "observation",
          observation: validated,
          requestId: request.id,
          toolName: invocation.name,
        };
      });
      const handle: ObservationToolInvocationHandle = {
        cancel: () => request.cancel(),
        kind: "observation",
        requestId: request.id,
        result,
      };
      return handle;
    }

    const definition = entry.definition;
    if (
      definition.protection === "protected" &&
      !protectedFeatureEnabled(this.#client)
    ) {
      throw new InvalidActionSpaceError(
        `protected action ${definition.actionName} requires negotiated ${ACTION_PROTECTED_FEATURE}`,
      );
    }
    const actionCallId = randomUUID();
    const request = this.#client.beginCall(
      "device.execute",
      {
        ...(options.actionTimeoutMs === undefined
          ? {}
          : { actionTimeoutMs: options.actionTimeoutMs }),
        arguments: invocation.arguments,
        id: actionCallId,
        name: definition.actionName,
      },
      callOptions<"device.execute">(options),
    );
    const result = request.result.then<ActionToolResult>((action) => {
      const validated = validateActionResult(
        invocation.name,
        actionCallId,
        action,
        definition.protection,
      );
      return {
        action: validated,
        actionCallId,
        actionName: definition.actionName,
        ...(invocation.invocationId === undefined
          ? {}
          : { invocationId: invocation.invocationId }),
        kind: "action",
        requestId: request.id,
        toolName: invocation.name,
      };
    });
    const handle: ActionToolInvocationHandle = {
      actionCallId,
      actionName: definition.actionName,
      cancel: () => request.cancel(),
      kind: "action",
      requestId: request.id,
      result,
    };
    return handle;
  }

  async invoke(
    invocation: ToolInvocation,
    options?: ToolInvocationOptions,
  ): Promise<ToolInvocationResult> {
    return await this.beginInvoke(invocation, options).result;
  }
}

export class DeviceRailToolAdapter {
  readonly #client: DeviceRailToolClient;
  readonly #includeObservation: boolean;
  readonly #includeProtectedActions: boolean;
  readonly #maxActions: number;
  #revision = 0;

  constructor(client: DeviceRailToolClient, options: DeviceRailToolAdapterOptions = {}) {
    if (
      !isRecord(client) ||
      typeof client.call !== "function" ||
      typeof client.beginCall !== "function" ||
      (client.enabledFeatures !== undefined &&
        (!isRecord(client.enabledFeatures) || typeof client.enabledFeatures.has !== "function"))
    ) {
      throw new InvalidActionSpaceError(
        "tool adapter client must provide call() and beginCall(); enabledFeatures must be set-like when present",
      );
    }
    const clonedOptions = clonePureJson(
      options,
      (message) => new InvalidActionSpaceError(`tool adapter options: ${message}`),
    );
    if (!isRecord(clonedOptions)) {
      throw new InvalidActionSpaceError("tool adapter options must be an object");
    }
    const allowed = new Set(["includeObservation", "includeProtectedActions", "maxActions"]);
    for (const key of Object.keys(clonedOptions)) {
      if (!allowed.has(key)) {
        throw new InvalidActionSpaceError(`unknown tool adapter option ${key}`);
      }
    }
    if (
      clonedOptions.includeObservation !== undefined &&
      typeof clonedOptions.includeObservation !== "boolean"
    ) {
      throw new InvalidActionSpaceError("includeObservation must be a boolean");
    }
    if (
      clonedOptions.includeProtectedActions !== undefined &&
      typeof clonedOptions.includeProtectedActions !== "boolean"
    ) {
      throw new InvalidActionSpaceError("includeProtectedActions must be a boolean");
    }
    const maxActions = clonedOptions.maxActions ?? DEFAULT_MAX_ACTIONS;
    if (
      typeof maxActions !== "number" ||
      !Number.isSafeInteger(maxActions) ||
      maxActions <= 0
    ) {
      throw new InvalidActionSpaceError("maxActions must be a positive safe integer");
    }
    this.#client = client;
    this.#includeObservation = clonedOptions.includeObservation ?? true;
    this.#includeProtectedActions = clonedOptions.includeProtectedActions ?? false;
    if (
      this.#includeProtectedActions &&
      !protectedFeatureEnabled(this.#client)
    ) {
      throw new InvalidActionSpaceError(
        `includeProtectedActions requires negotiated ${ACTION_PROTECTED_FEATURE}`,
      );
    }
    this.#maxActions = maxActions;
  }

  async discover(options: ToolDiscoveryOptions = {}): Promise<DeviceRailToolCatalog> {
    const normalized = normalizeDiscoveryOptions(options);
    const capabilities = await this.#client.call(
      "device.capabilities",
      undefined,
      callOptions<"device.capabilities">(normalized),
    );
    const tools = compileDefinitions(
      capabilities,
      this.#includeObservation,
      this.#includeProtectedActions,
      this.#maxActions,
    );
    if (this.#revision >= Number.MAX_SAFE_INTEGER) {
      throw new InvalidActionSpaceError("tool catalog revision is exhausted");
    }
    this.#revision += 1;
    return new ToolCatalog(this.#client, this.#revision, tools);
  }
}

export function requestIdOf(handle: ToolInvocationHandle): RpcId {
  return handle.requestId;
}
