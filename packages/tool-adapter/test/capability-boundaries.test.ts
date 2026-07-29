import assert from "node:assert/strict";
import test from "node:test";

import {
  DeviceRailToolAdapter,
  InvalidActionSpaceError,
} from "../src/index.js";
import { action, FakeToolClient } from "./fake-client.js";

interface InvalidCapabilityCase {
  readonly capabilities: unknown;
  readonly label: string;
  readonly message: RegExp;
}

const sharedProperty = { type: "string" };
const cyclicSchema: Record<string, unknown> = { type: "object" };
cyclicSchema.self = cyclicSchema;
let getterCalled = false;
const accessorSchema: Record<string, unknown> = { type: "object" };
Object.defineProperty(accessorSchema, "properties", {
  enumerable: true,
  get() {
    getterCalled = true;
    return {};
  },
});

const invalidCases: readonly InvalidCapabilityCase[] = [
  { capabilities: null, label: "non-array result", message: /must return an array/u },
  { capabilities: [null], label: "non-object entry", message: /capability 0 must be an object/u },
  { capabilities: [action("")], label: "empty action name", message: /invalid action name/u },
  { capabilities: [action("   ")], label: "blank action name", message: /invalid action name/u },
  {
    capabilities: [{ description: 7, inputSchema: { type: "object" }, name: "tap" }],
    label: "non-string description",
    message: /invalid description/u,
  },
  {
    capabilities: [action("tap", { type: "object" }, "   ")],
    label: "blank description",
    message: /invalid description/u,
  },
  {
    capabilities: [action("tap"), action("tap")],
    label: "duplicate action names",
    message: /duplicate action name tap/u,
  },
  {
    capabilities: [action("tap", { type: "array" })],
    label: "non-object schema root",
    message: /type: object/u,
  },
  {
    capabilities: [action("tap", { properties: [], type: "object" })],
    label: "non-object properties",
    message: /properties.*must be an object/u,
  },
  {
    capabilities: [action("tap", { required: "x", type: "object" })],
    label: "non-array required",
    message: /required.*unique strings/u,
  },
  {
    capabilities: [action("tap", { required: ["x", "x"], type: "object" })],
    label: "duplicate required property",
    message: /required.*unique strings/u,
  },
  {
    capabilities: [action("tap", { additionalProperties: 1, type: "object" })],
    label: "invalid additionalProperties",
    message: /additionalProperties.*boolean or object/u,
  },
  {
    capabilities: [action("tap", { $ref: 7, type: "object" })],
    label: "non-string ref",
    message: /\$ref.*must be a string/u,
  },
  {
    capabilities: [action("tap", { $schema: 7, type: "object" })],
    label: "non-string dialect",
    message: /\$schema.*must be a string/u,
  },
  {
    capabilities: [
      action("tap", {
        properties: { value: { enum: [{ a: 1, b: 2 }, { b: 2, a: 1 }] } },
        type: "object",
      }),
    ],
    label: "JSON-deep duplicate enum values",
    message: /enum.*unique JSON values/u,
  },
  {
    capabilities: [
      action("tap", {
        properties: { x: { type: 17 } },
        type: "object",
      }),
    ],
    label: "invalid nested JSON Schema keyword",
    message: /inputSchema|schema/u,
  },
  {
    capabilities: [action("tap", { maximum: Number.MAX_SAFE_INTEGER + 1, type: "object" })],
    label: "unsafe schema number",
    message: /unsafe number/u,
  },
  {
    capabilities: [action("tap", cyclicSchema)],
    label: "cyclic schema",
    message: /repeated or cyclic/u,
  },
  {
    capabilities: [
      action("tap", {
        properties: { first: sharedProperty, second: sharedProperty },
        type: "object",
      }),
    ],
    label: "aliased schema object",
    message: /repeated or cyclic/u,
  },
  {
    capabilities: [action("tap", accessorSchema)],
    label: "accessor schema property",
    message: /not a plain JSON property/u,
  },
];

for (const invalidCase of invalidCases) {
  test(`discovery rejects ${invalidCase.label}`, async () => {
    getterCalled = false;
    const fake = new FakeToolClient([invalidCase.capabilities]);
    const adapter = new DeviceRailToolAdapter(fake.client);

    await assert.rejects(
      adapter.discover(),
      (error: unknown) =>
        error instanceof InvalidActionSpaceError &&
        error.code === "invalid_action_space" &&
        invalidCase.message.test(error.message),
    );
    if (invalidCase.capabilities === invalidCases.at(-1)?.capabilities) {
      assert.equal(getterCalled, false, "schema accessors must not execute");
    }
  });
}

test("constructor validates the fake client and bounded options", () => {
  const fake = new FakeToolClient();
  let getterCalled = false;
  const accessorOptions: Record<string, unknown> = {};
  Object.defineProperty(accessorOptions, "maxActions", {
    enumerable: true,
    get() {
      getterCalled = true;
      return 1;
    },
  });
  assert.throws(
    () => new DeviceRailToolAdapter({} as never),
    InvalidActionSpaceError,
  );
  for (const maxActions of [0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.throws(
      () => new DeviceRailToolAdapter(fake.client, { maxActions }),
      InvalidActionSpaceError,
    );
  }
  assert.throws(
    () =>
      new DeviceRailToolAdapter(fake.client, {
        includeObservation: "yes" as unknown as boolean,
      }),
    InvalidActionSpaceError,
  );
  assert.throws(
    () =>
      new DeviceRailToolAdapter(fake.client, {
        includeProtectedActions: "yes" as unknown as boolean,
      }),
    InvalidActionSpaceError,
  );
  assert.throws(
    () => new DeviceRailToolAdapter(fake.client, { unknown: true } as never),
    InvalidActionSpaceError,
  );
  assert.throws(
    () => new DeviceRailToolAdapter(fake.client, accessorOptions as never),
    InvalidActionSpaceError,
  );
  assert.equal(getterCalled, false);
});

test("schema-shaped keys inside const remain literal JSON data", async () => {
  const literal = {
    $dynamicRef: "https://example.test/dynamic-literal",
    $recursiveRef: "https://example.test/recursive-literal",
    $ref: "https://example.test/ref-literal",
    $schema: 17,
  };
  const fake = new FakeToolClient([
    [
      action("literal", {
        properties: {
          payload: { const: literal },
        },
        type: "object",
      }),
    ],
  ]);

  const catalog = await new DeviceRailToolAdapter(fake.client).discover();
  const definition = catalog.tools.find(
    (candidate) => candidate.kind === "action" && candidate.actionName === "literal",
  );
  assert.ok(definition?.kind === "action");
  const properties = definition.inputSchema.properties as Record<string, unknown>;
  const payload = properties.payload as Record<string, unknown>;
  assert.deepEqual(payload.const, literal);
});

test("Draft 7 schema with a nested $id anchor is preserved as a frozen snapshot", async () => {
  const schema = {
    $id: "https://example.test/actions/input.json",
    $schema: "http://json-schema.org/draft-07/schema#",
    definitions: {
      coordinate: {
        $id: "#coordinate",
        minimum: 0,
        type: "integer",
      },
    },
    properties: {
      point: {
        additionalItems: false,
        items: [{ $ref: "#coordinate" }, { $ref: "#coordinate" }],
        type: "array",
      },
      shared: { $ref: "https://schemas.example.test/shared.json#pointer" },
    },
    type: "object",
  };
  const fake = new FakeToolClient([[action("draft7", schema)]]);

  const catalog = await new DeviceRailToolAdapter(fake.client).discover();
  const definition = catalog.tools.find(
    (candidate) => candidate.kind === "action" && candidate.actionName === "draft7",
  );
  assert.ok(definition?.kind === "action");
  assert.deepEqual(definition.inputSchema, schema);
  assert.equal(Object.isFrozen(definition.inputSchema), true);
  assert.equal(Object.isFrozen(definition.inputSchema.definitions), true);
  assert.equal(Object.isFrozen(definition.inputSchema.properties), true);
});

test("Draft 2020-12 compound resources are preserved without dereferencing", async () => {
  const schema = {
    $defs: {
      embedded: {
        $anchor: "payload",
        $dynamicAnchor: "dynamicPayload",
        $id: "embedded.json",
        $schema: "https://json-schema.org/draft/2020-12/schema",
        properties: {
          value: { type: "string" },
        },
        type: "object",
      },
    },
    $id: "https://example.test/actions/root.json",
    $schema: "https://json-schema.org/draft/2020-12/schema",
    properties: {
      dynamic: { $dynamicRef: "embedded.json#dynamicPayload" },
      local: { $ref: "embedded.json#payload" },
      unresolved: { $ref: "https://schemas.example.test/not-fetched.json" },
    },
    type: "object",
  };
  const fake = new FakeToolClient([[action("compound", schema)]]);

  const catalog = await new DeviceRailToolAdapter(fake.client).discover();
  const definition = catalog.tools.find(
    (candidate) => candidate.kind === "action" && candidate.actionName === "compound",
  );
  assert.ok(definition?.kind === "action");
  assert.deepEqual(definition.inputSchema, schema);
  const defs = definition.inputSchema.$defs as Record<string, unknown>;
  assert.equal(Object.isFrozen(defs), true);
  assert.equal(Object.isFrozen(defs.embedded), true);
});
