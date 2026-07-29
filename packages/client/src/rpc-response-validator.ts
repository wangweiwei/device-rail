import { types as utilTypes } from "node:util";

import {
  Ajv2020,
  type AnySchema,
  type ErrorObject,
  type ValidateFunction,
} from "ajv/dist/2020.js";

import type { RpcMethod, RpcResponseFor, RpcResultFor } from "@devicerail/protocol";

import { ProtocolViolationError } from "./errors.js";
import { RPC_RESPONSE_SCHEMAS } from "./generated/response-schemas.js";

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu;
const MAX_PUBLIC_JSON_DEPTH = 256;
const MAX_PUBLIC_JSON_NODES = 100_000;

const compiler = new Ajv2020({
  allErrors: false,
  allowUnionTypes: true,
  strict: true,
  validateFormats: true,
});

compiler.addFormat("uuid", { type: "string", validate: (value: string) => UUID_PATTERN.test(value) });
compiler.addFormat("int32", {
  type: "number",
  validate: (value: number) =>
    Number.isInteger(value) && value >= -2_147_483_648 && value <= 2_147_483_647,
});
compiler.addFormat("uint16", {
  type: "number",
  validate: (value: number) => Number.isInteger(value) && value >= 0 && value <= 65_535,
});
compiler.addFormat("uint32", {
  type: "number",
  validate: (value: number) => Number.isInteger(value) && value >= 0 && value <= 4_294_967_295,
});
compiler.addFormat("uint64", {
  type: "number",
  validate: (value: number) => Number.isSafeInteger(value) && value >= 0,
});
compiler.addFormat("double", {
  type: "number",
  validate: (value: number) => Number.isFinite(value),
});

const responseValidators = new Map<RpcMethod, ValidateFunction>();

function responseValidator(method: RpcMethod): ValidateFunction {
  const cached = responseValidators.get(method);
  if (cached) {
    return cached;
  }
  if (typeof method !== "string" || !Object.hasOwn(RPC_RESPONSE_SCHEMAS, method)) {
    throw new ProtocolViolationError("no runtime response Schema is registered for the method");
  }
  const schema = RPC_RESPONSE_SCHEMAS[method];
  try {
    const validator = compiler.compile(schema as AnySchema);
    responseValidators.set(method, validator);
    return validator;
  } catch (cause) {
    throw new Error(`failed to compile the packaged ${method} response Schema`, { cause });
  }
}

function boundedDiagnosticLocation(value: string): string {
  const safe = value.replace(/[^\x20-\x7e]/gu, "?");
  return safe.length <= 256 ? safe : `${safe.slice(0, 253)}...`;
}

export function assertPureJsonValue(value: unknown, location = "$"): void {
  const pending: Array<{
    readonly depth: number;
    readonly location: string;
    readonly value: unknown;
  }> = [
    { depth: 0, location, value },
  ];
  const seen = new Set<object>();
  let nodes = 0;
  while (pending.length > 0) {
    const current = pending.pop();
    if (!current) {
      continue;
    }
    nodes += 1;
    if (nodes > MAX_PUBLIC_JSON_NODES || current.depth > MAX_PUBLIC_JSON_DEPTH) {
      throw new ProtocolViolationError("response exceeds the pure-JSON validation budget");
    }
    if (
      current.value === null ||
      typeof current.value === "string" ||
      typeof current.value === "boolean"
    ) {
      continue;
    }
    if (typeof current.value === "number") {
      if (
        !Number.isFinite(current.value) ||
        (Number.isInteger(current.value) && !Number.isSafeInteger(current.value))
      ) {
        throw new ProtocolViolationError(
          `${boundedDiagnosticLocation(current.location)} contains an unsafe JSON number`,
        );
      }
      continue;
    }
    if (typeof current.value !== "object") {
      throw new ProtocolViolationError(
        `${boundedDiagnosticLocation(current.location)} contains a non-JSON value`,
      );
    }
    if (utilTypes.isProxy(current.value)) {
      throw new ProtocolViolationError(
        `${boundedDiagnosticLocation(current.location)} contains a non-JSON proxy`,
      );
    }
    if (seen.has(current.value)) {
      throw new ProtocolViolationError(
        `${boundedDiagnosticLocation(current.location)} contains a repeated or cyclic value`,
      );
    }
    seen.add(current.value);
    if (Array.isArray(current.value)) {
      const array = current.value;
      if (Object.getPrototypeOf(array) !== Array.prototype) {
        throw new ProtocolViolationError(
          `${boundedDiagnosticLocation(current.location)} contains a non-JSON array`,
        );
      }
      const ownKeys = Reflect.ownKeys(array);
      if (
        array.length > MAX_PUBLIC_JSON_NODES ||
        ownKeys.length > MAX_PUBLIC_JSON_NODES + 1
      ) {
        throw new ProtocolViolationError("response exceeds the pure-JSON validation budget");
      }
      if (
        ownKeys.some(
          (key) =>
            typeof key !== "string" ||
            (key !== "length" &&
              (!/^(0|[1-9][0-9]*)$/u.test(key) ||
                !Number.isSafeInteger(Number(key)) ||
                Number(key) >= array.length)),
        )
      ) {
        throw new ProtocolViolationError(
          `${boundedDiagnosticLocation(current.location)} contains a non-JSON array property`,
        );
      }
      const descriptors = Object.getOwnPropertyDescriptors(array);
      for (let index = array.length - 1; index >= 0; index -= 1) {
        const descriptor = descriptors[String(index)];
        if (!descriptor?.enumerable || !("value" in descriptor)) {
          throw new ProtocolViolationError(
            `${boundedDiagnosticLocation(current.location)} contains a sparse or accessor array slot`,
          );
        }
        pending.push({
          depth: current.depth + 1,
          location: boundedDiagnosticLocation(`${current.location}[${index}]`),
          value: descriptor.value,
        });
      }
    } else {
      const prototype = Object.getPrototypeOf(current.value);
      if (prototype !== Object.prototype && prototype !== null) {
        throw new ProtocolViolationError(
          `${boundedDiagnosticLocation(current.location)} contains a non-JSON object`,
        );
      }
      const ownKeys = Reflect.ownKeys(current.value);
      if (ownKeys.length > MAX_PUBLIC_JSON_NODES) {
        throw new ProtocolViolationError("response exceeds the pure-JSON validation budget");
      }
      const descriptors = Object.getOwnPropertyDescriptors(current.value);
      for (const key of ownKeys) {
        if (typeof key !== "string") {
          throw new ProtocolViolationError(
            `${boundedDiagnosticLocation(current.location)} contains a symbol property`,
          );
        }
        const descriptor = descriptors[key];
        if (!descriptor?.enumerable || !("value" in descriptor)) {
          throw new ProtocolViolationError(
            `${boundedDiagnosticLocation(current.location)} contains a non-JSON property`,
          );
        }
        pending.push({
          depth: current.depth + 1,
          location: boundedDiagnosticLocation(`${current.location}.*`),
          value: descriptor.value,
        });
      }
    }
  }
}

function redactedInstanceLocation(instancePath: string): string {
  const segments = instancePath.split("/").slice(1);
  let location = "$";
  for (const segment of segments) {
    location += /^(0|[1-9][0-9]{0,11})$/u.test(segment) ? `[${segment}]` : ".*";
    if (location.length > 256) {
      return `${location.slice(0, 253)}...`;
    }
  }
  return location;
}

function validationError(method: RpcMethod, errors: readonly ErrorObject[] | null | undefined): Error {
  const candidates = [...(errors ?? [])].sort((left, right) => {
    const pathDifference = right.instancePath.length - left.instancePath.length;
    if (pathDifference !== 0) {
      return pathDifference;
    }
    const leftAggregate = left.keyword === "anyOf" || left.keyword === "oneOf";
    const rightAggregate = right.keyword === "anyOf" || right.keyword === "oneOf";
    return Number(leftAggregate) - Number(rightAggregate);
  });
  const error = candidates[0];
  if (!error) {
    return new ProtocolViolationError(`${method} response was rejected by its JSON Schema`);
  }
  const location = redactedInstanceLocation(error.instancePath);
  return new ProtocolViolationError(
    `${method} response was rejected at ${location}: ${error.message ?? error.keyword}`,
  );
}

export function validateRpcResponse<M extends RpcMethod>(
  method: M,
  response: unknown,
): asserts response is RpcResponseFor<M> {
  const validator = responseValidator(method);
  assertPureJsonValue(response);
  if (!validator(response)) {
    throw validationError(method, validator.errors);
  }
}

/** Validates a method result for adapters that inject a client-compatible transport. */
export function validateRpcResult<M extends RpcMethod>(
  method: M,
  result: unknown,
): asserts result is RpcResultFor<M> {
  validateRpcResponse(method, { id: 0, jsonrpc: "2.0", result });
}
