import { InvalidActionSpaceError } from "./errors.js";
import { clonePureJson, deepFreezeJson } from "./json.js";
import type { ToolInputSchema } from "./types.js";

type JsonObject = Record<string, unknown>;
type Schema = boolean | JsonObject;

interface SchemaTask {
  readonly path: string;
  readonly schema: Schema;
}

const JSON_TYPES = new Set([
  "array",
  "boolean",
  "integer",
  "null",
  "number",
  "object",
  "string",
]);

function isRecord(value: unknown): value is JsonObject {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function fail(actionName: string, path: string, message: string): never {
  throw new InvalidActionSpaceError(`inputSchema for ${actionName} ${path}: ${message}`);
}

function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== "object") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map((item) => canonicalJson(item)).join(",")}]`;
  }
  const record = value as JsonObject;
  return `{${Object.keys(record)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`)
    .join(",")}}`;
}

function stringArray(
  actionName: string,
  path: string,
  value: unknown,
): readonly string[] {
  if (
    !Array.isArray(value) ||
    value.some((item) => typeof item !== "string") ||
    new Set(value).size !== value.length
  ) {
    fail(actionName, path, "must contain unique strings");
  }
  return value as string[];
}

function schemaAt(
  actionName: string,
  path: string,
  value: unknown,
  pending: SchemaTask[],
): void {
  if (typeof value !== "boolean" && !isRecord(value)) {
    fail(actionName, path, "must be a boolean or object schema");
  }
  pending.push({ path, schema: value as Schema });
}

function schemaMapAt(
  actionName: string,
  path: string,
  value: unknown,
  pending: SchemaTask[],
): void {
  if (!isRecord(value)) {
    fail(actionName, path, "must be an object");
  }
  for (const [key, child] of Object.entries(value)) {
    schemaAt(actionName, `${path}.${key}`, child, pending);
  }
}

function schemaArrayAt(
  actionName: string,
  path: string,
  value: unknown,
  pending: SchemaTask[],
): void {
  if (!Array.isArray(value)) {
    fail(actionName, path, "must be an array of schemas");
  }
  value.forEach((child, index) => schemaAt(actionName, `${path}[${index}]`, child, pending));
}

function nonNegativeInteger(actionName: string, path: string, value: unknown): void {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    fail(actionName, path, "must be a non-negative safe integer");
  }
}

function numberKeyword(
  actionName: string,
  path: string,
  value: unknown,
  positive = false,
): void {
  if (typeof value !== "number" || !Number.isFinite(value) || (positive && value <= 0)) {
    fail(actionName, path, positive ? "must be a positive finite number" : "must be a finite number");
  }
}

function validateSchemaNode(
  actionName: string,
  task: SchemaTask,
  pending: SchemaTask[],
): void {
  if (typeof task.schema === "boolean") {
    return;
  }
  const schema = task.schema;

  for (const keyword of [
    "$schema",
    "$id",
    "$anchor",
    "$dynamicAnchor",
    "$ref",
    "$dynamicRef",
    "$recursiveRef",
  ] as const) {
    if (schema[keyword] !== undefined && typeof schema[keyword] !== "string") {
      fail(actionName, `${task.path}.${keyword}`, "must be a string");
    }
  }
  if (schema.$recursiveAnchor !== undefined && typeof schema.$recursiveAnchor !== "boolean") {
    fail(actionName, `${task.path}.$recursiveAnchor`, "must be a boolean");
  }

  if (schema.type !== undefined) {
    const types = Array.isArray(schema.type) ? schema.type : [schema.type];
    if (
      types.length === 0 ||
      types.some((type) => typeof type !== "string" || !JSON_TYPES.has(type)) ||
      new Set(types).size !== types.length
    ) {
      fail(actionName, `${task.path}.type`, "must be a JSON Schema type or unique type array");
    }
  }

  if (schema.required !== undefined) {
    stringArray(actionName, `${task.path}.required`, schema.required);
  }
  if (schema.enum !== undefined) {
    if (!Array.isArray(schema.enum) || schema.enum.length === 0) {
      fail(actionName, `${task.path}.enum`, "must be a non-empty array");
    }
    const values = new Set(schema.enum.map((value) => canonicalJson(value)));
    if (values.size !== schema.enum.length) {
      fail(actionName, `${task.path}.enum`, "must contain unique JSON values");
    }
  }
  if (schema.examples !== undefined && !Array.isArray(schema.examples)) {
    fail(actionName, `${task.path}.examples`, "must be an array");
  }

  for (const keyword of [
    "$comment",
    "title",
    "description",
    "format",
    "contentEncoding",
    "contentMediaType",
    "pattern",
  ] as const) {
    if (schema[keyword] !== undefined && typeof schema[keyword] !== "string") {
      fail(actionName, `${task.path}.${keyword}`, "must be a string");
    }
  }

  for (const keyword of [
    "readOnly",
    "writeOnly",
    "deprecated",
    "uniqueItems",
  ] as const) {
    if (schema[keyword] !== undefined && typeof schema[keyword] !== "boolean") {
      fail(actionName, `${task.path}.${keyword}`, "must be a boolean");
    }
  }
  for (const keyword of ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] as const) {
    if (schema[keyword] !== undefined) {
      numberKeyword(actionName, `${task.path}.${keyword}`, schema[keyword]);
    }
  }
  if (schema.multipleOf !== undefined) {
    numberKeyword(actionName, `${task.path}.multipleOf`, schema.multipleOf, true);
  }
  for (const keyword of [
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "minContains",
    "maxContains",
    "minProperties",
    "maxProperties",
  ] as const) {
    if (schema[keyword] !== undefined) {
      nonNegativeInteger(actionName, `${task.path}.${keyword}`, schema[keyword]);
    }
  }

  for (const keyword of [
    "$defs",
    "definitions",
    "properties",
    "patternProperties",
    "dependentSchemas",
  ] as const) {
    if (schema[keyword] !== undefined) {
      schemaMapAt(actionName, `${task.path}.${keyword}`, schema[keyword], pending);
    }
  }
  for (const keyword of ["allOf", "anyOf", "oneOf", "prefixItems"] as const) {
    if (schema[keyword] !== undefined) {
      schemaArrayAt(actionName, `${task.path}.${keyword}`, schema[keyword], pending);
    }
  }
  for (const keyword of [
    "additionalItems",
    "additionalProperties",
    "unevaluatedProperties",
    "propertyNames",
    "contains",
    "unevaluatedItems",
    "not",
    "if",
    "then",
    "else",
    "contentSchema",
  ] as const) {
    if (schema[keyword] !== undefined) {
      schemaAt(actionName, `${task.path}.${keyword}`, schema[keyword], pending);
    }
  }
  if (schema.items !== undefined) {
    if (Array.isArray(schema.items)) {
      schemaArrayAt(actionName, `${task.path}.items`, schema.items, pending);
    } else {
      schemaAt(actionName, `${task.path}.items`, schema.items, pending);
    }
  }

  if (schema.dependencies !== undefined) {
    if (!isRecord(schema.dependencies)) {
      fail(actionName, `${task.path}.dependencies`, "must be an object");
    }
    for (const [key, dependency] of Object.entries(schema.dependencies)) {
      if (Array.isArray(dependency)) {
        stringArray(actionName, `${task.path}.dependencies.${key}`, dependency);
      } else {
        schemaAt(actionName, `${task.path}.dependencies.${key}`, dependency, pending);
      }
    }
  }
  if (schema.dependentRequired !== undefined) {
    if (!isRecord(schema.dependentRequired)) {
      fail(actionName, `${task.path}.dependentRequired`, "must be an object");
    }
    for (const [key, required] of Object.entries(schema.dependentRequired)) {
      stringArray(actionName, `${task.path}.dependentRequired.${key}`, required);
    }
  }
  if (schema.$vocabulary !== undefined) {
    if (
      !isRecord(schema.$vocabulary) ||
      Object.values(schema.$vocabulary).some((required) => typeof required !== "boolean")
    ) {
      fail(actionName, `${task.path}.$vocabulary`, "must map vocabulary URIs to booleans");
    }
  }
}

export function validateToolInputSchema(actionName: string, value: unknown): ToolInputSchema {
  const cloned = clonePureJson(
    value,
    (message) => new InvalidActionSpaceError(`inputSchema for ${actionName}: ${message}`),
  );
  if (!isRecord(cloned) || cloned.type !== "object") {
    throw new InvalidActionSpaceError(
      `inputSchema for ${actionName} must be a JSON object with type: object`,
    );
  }

  const pending: SchemaTask[] = [{ path: "$", schema: cloned }];
  while (pending.length > 0) {
    const task = pending.pop();
    if (task) {
      validateSchemaNode(actionName, task, pending);
    }
  }

  return deepFreezeJson(cloned);
}
