import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";

import {
  actionToolName,
  DeviceRailToolAdapter,
  InvalidActionSpaceError,
  OBSERVATION_TOOL_NAME,
} from "../src/index.js";
import { action, FakeToolClient } from "./fake-client.js";

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/iu;

test("discovery produces deterministic immutable snapshots without retaining capability objects", async () => {
  const tapSchema = {
    $defs: { coordinate: { minimum: 0, type: "number" } },
    additionalProperties: false,
    properties: {
      x: { $ref: "#/$defs/coordinate" },
      y: { $ref: "#/$defs/coordinate" },
    },
    required: ["x", "y"],
    type: "object",
  };
  const firstCapabilities = [
    action("输入 文本", {
      additionalProperties: false,
      properties: { text: { type: "string" } },
      required: ["text"],
      type: "object",
    }),
    action("tap", tapSchema),
    action("z".repeat(100)),
  ];
  const fake = new FakeToolClient([
    firstCapabilities,
    [action("replacement")],
  ]);
  const adapter = new DeviceRailToolAdapter(fake.client);
  const abort = new AbortController();

  const first = await adapter.discover({ requestTimeoutMs: 321, signal: abort.signal });

  assert.match(first.id, UUID_PATTERN);
  assert.equal(first.revision, 1);
  assert.deepEqual(fake.calls, [
    {
      method: "device.capabilities",
      options: { signal: abort.signal, timeoutMs: 321 },
      params: undefined,
    },
  ]);
  assert.equal(first.tools[0]?.name, OBSERVATION_TOOL_NAME);
  const actionDefinitions = first.tools.slice(1);
  assert.deepEqual(
    actionDefinitions.map((definition) =>
      definition.kind === "action"
        ? [definition.actionName, definition.name]
        : assert.fail("only action definitions follow observation"),
    ),
    [...firstCapabilities]
      .sort((left, right) => (left.name < right.name ? -1 : left.name > right.name ? 1 : 0))
      .map((definition) => [definition.name, actionToolName(definition.name)]),
  );

  assert.equal(Object.isFrozen(first), true);
  assert.equal(Object.isFrozen(first.tools), true);
  for (const definition of first.tools) {
    assert.equal(Object.isFrozen(definition), true);
    assert.equal(Object.isFrozen(definition.inputSchema), true);
  }
  const tapTool = first.tools.find(
    (definition) => definition.kind === "action" && definition.actionName === "tap",
  );
  assert.ok(tapTool?.kind === "action");
  assert.notEqual(tapTool.inputSchema, tapSchema);
  assert.equal(Object.isFrozen(tapTool.inputSchema.properties), true);
  tapSchema.$defs.coordinate.minimum = 99;
  assert.equal(
    ((tapTool.inputSchema.$defs as Record<string, unknown>).coordinate as Record<string, unknown>)
      .minimum,
    0,
  );
  assert.throws(
    () => (first.tools as unknown[]).push({}),
    TypeError,
  );

  const second = await adapter.discover();
  assert.equal(second.revision, 2);
  assert.notEqual(second.id, first.id);
  assert.equal(first.tools.length, 4, "the first snapshot remains isolated");
  assert.deepEqual(
    second.tools.map((definition) => definition.name),
    [OBSERVATION_TOOL_NAME, actionToolName("replacement")],
  );
});

test("portable action names are deterministic, bounded, and provider-safe", () => {
  const raw = actionToolName("tap-v2_1");
  const unicode = actionToolName("输入 文本");
  const longAction = "action/".repeat(40);
  const hashed = actionToolName(longAction);

  assert.equal(raw, "devicerail_action_raw_tap-v2_1");
  assert.equal(
    unicode,
    `devicerail_action_b64_${Buffer.from("输入 文本", "utf8").toString("base64url")}`,
  );
  assert.equal(
    hashed,
    `devicerail_action_sha256_${createHash("sha256").update(longAction).digest("hex").slice(0, 32)}`,
  );
  for (const name of [raw, unicode, hashed]) {
    assert.match(name, /^[A-Za-z0-9_-]+$/u);
    assert.ok(name.length <= 64);
    assert.notEqual(name, OBSERVATION_TOOL_NAME);
  }
  assert.equal(actionToolName(longAction), hashed);
  assert.notEqual(actionToolName("é"), actionToolName("e\u0301"));
  assert.throws(() => actionToolName(""), InvalidActionSpaceError);
  assert.throws(() => actionToolName("   "), InvalidActionSpaceError);
  assert.throws(() => actionToolName(7 as never), InvalidActionSpaceError);
});

test("observation inclusion and action count limits are explicit", async () => {
  const fake = new FakeToolClient([
    [action("tap")],
    [action("tap"), action("scroll")],
  ]);
  const adapter = new DeviceRailToolAdapter(fake.client, {
    includeObservation: false,
    maxActions: 1,
  });

  const catalog = await adapter.discover();
  assert.deepEqual(catalog.tools.map((definition) => definition.name), [actionToolName("tap")]);
  await assert.rejects(
    adapter.discover(),
    (error: unknown) =>
      error instanceof InvalidActionSpaceError &&
      error.code === "invalid_action_space" &&
      /limit is 1/u.test(error.message),
  );
});

test("failed discoveries do not consume catalog revisions", async () => {
  const fake = new FakeToolClient([
    [action("first")],
    [action("broken", { type: "array" })],
    [action("second")],
  ]);
  const adapter = new DeviceRailToolAdapter(fake.client);

  assert.equal((await adapter.discover()).revision, 1);
  await assert.rejects(adapter.discover(), InvalidActionSpaceError);
  assert.equal((await adapter.discover()).revision, 2);
});
