import assert from "node:assert/strict";
import test from "node:test";

import {
  actionToolName,
  DeviceRailToolAdapter,
  InvalidToolArgumentsError,
  InvalidToolOptionsError,
  InvalidToolResultError,
  OBSERVATION_TOOL_NAME,
  UnknownToolError,
} from "../src/index.js";
import {
  action,
  actionResult,
  FakeToolClient,
  observation,
} from "./fake-client.js";

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/iu;

async function tapCatalog(fake: FakeToolClient) {
  const adapter = new DeviceRailToolAdapter(fake.client);
  return await adapter.discover();
}

test("action invocation maps arguments, timeouts, signal, IDs, and structured results", async () => {
  const fake = new FakeToolClient([[action("tap")]]);
  const catalog = await tapCatalog(fake);
  const controller = new AbortController();
  const sourceArguments = { nested: { enabled: true }, x: 10, y: 20 };

  const handle = catalog.beginInvoke(
    {
      arguments: sourceArguments,
      invocationId: "provider-call-7",
      name: actionToolName("tap"),
    },
    {
      actionTimeoutMs: 500,
      requestTimeoutMs: 800,
      signal: controller.signal,
    },
  );

  assert.equal(handle.kind, "action");
  assert.equal(handle.actionName, "tap");
  assert.match(handle.actionCallId, UUID_PATTERN);
  assert.notEqual(handle.actionCallId, "provider-call-7");
  assert.equal(handle.requestId, "fake-request-1");
  const request = fake.beginCalls[0];
  assert.ok(request);
  assert.equal(request.method, "device.execute");
  assert.deepEqual(request.options, { signal: controller.signal, timeoutMs: 800 });
  assert.deepEqual(request.params, {
    actionTimeoutMs: 500,
    arguments: sourceArguments,
    id: handle.actionCallId,
    name: "tap",
  });
  const requestParams = request.params as {
    readonly arguments: Record<string, unknown>;
  };
  assert.notEqual(requestParams.arguments, sourceArguments);
  assert.equal(Object.isFrozen(requestParams.arguments), true);
  assert.equal(Object.isFrozen(requestParams.arguments.nested), true);
  sourceArguments.nested.enabled = false;
  assert.deepEqual(requestParams.arguments, { nested: { enabled: true }, x: 10, y: 20 });

  assert.deepEqual(await handle.cancel(), {
    requestId: "fake-request-1",
    status: "requested",
  });
  assert.deepEqual(await handle.cancel(), {
    requestId: "fake-request-1",
    status: "alreadyRequested",
  });
  assert.equal(request.cancelCount, 2);

  const actionValue = actionResult(handle.actionCallId, { tapped: true });
  request.resolve(actionValue);
  const result = await handle.result;
  assert.deepEqual(result, {
    action: actionValue,
    actionCallId: handle.actionCallId,
    actionName: "tap",
    invocationId: "provider-call-7",
    kind: "action",
    requestId: "fake-request-1",
    toolName: actionToolName("tap"),
  });
  assert.notEqual(result.action, actionValue);
  assert.equal(Object.isFrozen(result.action), true);
});

test("observation invocation has no action identity and preserves the typed observation", async () => {
  const fake = new FakeToolClient([[]]);
  const catalog = await tapCatalog(fake);
  const controller = new AbortController();

  const handle = catalog.beginInvoke(
    { arguments: {}, name: OBSERVATION_TOOL_NAME },
    { requestTimeoutMs: 123, signal: controller.signal },
  );
  assert.equal(handle.kind, "observation");
  assert.equal(handle.requestId, "fake-request-1");
  const request = fake.beginCalls[0];
  assert.ok(request);
  assert.deepEqual(
    { method: request.method, options: request.options, params: request.params },
    {
      method: "device.observe",
      options: { signal: controller.signal, timeoutMs: 123 },
      params: undefined,
    },
  );

  const value = observation(9);
  request.resolve(value);
  const result = await handle.result;
  assert.deepEqual(result, {
    kind: "observation",
    observation: value,
    requestId: "fake-request-1",
    toolName: OBSERVATION_TOOL_NAME,
  });
  assert.equal(Object.hasOwn(result, "invocationId"), false);
});

test("custom client results cross the canonical response Schema before adapter semantics", async () => {
  const fake = new FakeToolClient([[]]);
  const catalog = await tapCatalog(fake);
  const handle = catalog.beginInvoke({ name: OBSERVATION_TOOL_NAME });
  const request = fake.beginCalls[0];
  assert.ok(request);
  request.resolve({
    ...observation(12),
    viewport: { height: 800, scaleFactor: 1, width: "600" },
  });
  await assert.rejects(
    handle.result,
    (error: unknown) =>
      error instanceof InvalidToolResultError &&
      /device\.observe response was rejected/u.test(error.message),
  );
});

test("result UUID validation matches Core's non-nil UUID contract", async () => {
  const fake = new FakeToolClient([[]]);
  const catalog = await tapCatalog(fake);
  const handle = catalog.beginInvoke({ name: OBSERVATION_TOOL_NAME });
  const value = observation(10);
  value.id = "ffffffff-ffff-ffff-ffff-ffffffffffff";
  const request = fake.beginCalls[0];
  assert.ok(request);
  request.resolve(value);
  const result = await handle.result;
  assert.equal(result.kind, "observation");
  assert.equal(result.observation.id, value.id);
});

test("unknown tools and malformed invocations are rejected before RPC admission", async () => {
  const fake = new FakeToolClient([[action("tap")]]);
  const catalog = await tapCatalog(fake);

  assert.throws(
    () => catalog.beginInvoke({ name: "missing" }),
    (error: unknown) =>
      error instanceof UnknownToolError &&
      error.code === "unknown_tool" &&
      error.toolName === "missing",
  );
  await assert.rejects(catalog.invoke({ name: "missing" }), UnknownToolError);
  for (const invocation of [
    null,
    {},
    { name: "" },
    { invocationId: 7, name: actionToolName("tap") },
    { name: actionToolName("tap"), unexpected: true },
  ]) {
    assert.throws(
      () => catalog.beginInvoke(invocation as never),
      InvalidToolArgumentsError,
    );
  }
  assert.equal(fake.beginCalls.length, 0);
});

test("tool arguments must be pure JSON objects and accessors are never invoked", async () => {
  const fake = new FakeToolClient([[action("tap")]]);
  const catalog = await tapCatalog(fake);
  const toolName = actionToolName("tap");
  const cyclic: Record<string, unknown> = {};
  cyclic.self = cyclic;
  const shared = { value: true };
  const sparse = new Array<unknown>(2);
  sparse[1] = true;
  let getterCalled = false;
  const accessor: Record<string, unknown> = {};
  Object.defineProperty(accessor, "secret", {
    enumerable: true,
    get() {
      getterCalled = true;
      return "secret";
    },
  });

  const invalidArguments: readonly unknown[] = [
    null,
    [],
    "text",
    7,
    new Date(0),
    { value: undefined },
    { value: 1n },
    { value: Number.NaN },
    { value: Number.POSITIVE_INFINITY },
    { value: Number.MAX_SAFE_INTEGER + 1 },
    { first: shared, second: shared },
    { sparse },
    cyclic,
    accessor,
  ];

  for (const argumentsValue of invalidArguments) {
    assert.throws(
      () => catalog.beginInvoke({ arguments: argumentsValue, name: toolName }),
      InvalidToolArgumentsError,
    );
  }
  assert.equal(getterCalled, false);
  assert.equal(fake.beginCalls.length, 0);

  const accepted = catalog.beginInvoke({ name: toolName });
  assert.ok(accepted.kind === "action");
  const request = fake.beginCalls[0];
  assert.ok(request);
  assert.deepEqual((request.params as { arguments: unknown }).arguments, {});
  request.resolve(actionResult(accepted.actionCallId, null));
  await accepted.result;
});

test("invalid invocation options are rejected locally and action-only options cannot reach observe", async () => {
  const fake = new FakeToolClient([[action("tap")]]);
  const catalog = await tapCatalog(fake);
  const toolName = actionToolName("tap");
  const invalidNumbers = [0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY];

  for (const value of invalidNumbers) {
    assert.throws(
      () => catalog.beginInvoke({ name: toolName }, { requestTimeoutMs: value }),
      InvalidToolOptionsError,
    );
    assert.throws(
      () => catalog.beginInvoke({ name: toolName }, { actionTimeoutMs: value }),
      InvalidToolOptionsError,
    );
  }
  assert.throws(
    () => catalog.beginInvoke({ name: toolName }, { signal: {} as AbortSignal }),
    InvalidToolOptionsError,
  );
  assert.throws(
    () => catalog.beginInvoke({ name: toolName }, { unexpected: true } as never),
    InvalidToolOptionsError,
  );
  assert.throws(
    () =>
      catalog.beginInvoke(
        { name: toolName },
        { actionCallId: "12345678-1234-4123-8123-123456789abc" } as never,
      ),
    InvalidToolOptionsError,
  );
  assert.throws(
    () =>
      catalog.beginInvoke(
        { name: OBSERVATION_TOOL_NAME },
        { actionTimeoutMs: 10 },
      ),
    InvalidToolOptionsError,
  );
  assert.equal(fake.beginCalls.length, 0);
});

test("invocation and discovery options reject proxies, accessors, symbols, and unknown fields", async () => {
  const fake = new FakeToolClient([[action("tap")]]);
  const adapter = new DeviceRailToolAdapter(fake.client);
  let invocationGetterCalled = false;
  const invocationAccessor: Record<string, unknown> = {};
  Object.defineProperty(invocationAccessor, "requestTimeoutMs", {
    enumerable: true,
    get() {
      invocationGetterCalled = true;
      return 100;
    },
  });
  const discoveryAccessor: Record<string, unknown> = {};
  let discoveryGetterCalled = false;
  Object.defineProperty(discoveryAccessor, "requestTimeoutMs", {
    enumerable: true,
    get() {
      discoveryGetterCalled = true;
      return 100;
    },
  });
  const discoverySymbol = { requestTimeoutMs: 100 } as Record<PropertyKey, unknown>;
  discoverySymbol[Symbol("hidden")] = true;

  await assert.rejects(
    adapter.discover(discoveryAccessor as never),
    InvalidToolOptionsError,
  );
  await assert.rejects(
    adapter.discover(new Proxy({}, {}) as never),
    InvalidToolOptionsError,
  );
  await assert.rejects(
    adapter.discover(discoverySymbol as never),
    InvalidToolOptionsError,
  );
  await assert.rejects(
    adapter.discover({ unknown: true } as never),
    InvalidToolOptionsError,
  );
  assert.equal(discoveryGetterCalled, false);
  assert.equal(fake.calls.length, 0);

  const catalog = await adapter.discover();
  assert.throws(
    () =>
      catalog.beginInvoke(
        { name: actionToolName("tap") },
        invocationAccessor as never,
      ),
    InvalidToolOptionsError,
  );
  assert.throws(
    () =>
      catalog.beginInvoke(
        { name: actionToolName("tap") },
        new Proxy({}, {}) as never,
      ),
    InvalidToolOptionsError,
  );
  assert.equal(invocationGetterCalled, false);
  assert.equal(fake.beginCalls.length, 0);
});

test("concurrent invocations preserve catalog mappings and settle out of order", async () => {
  const fake = new FakeToolClient([
    [action("alpha")],
    [action("beta")],
  ]);
  const adapter = new DeviceRailToolAdapter(fake.client, { includeObservation: false });
  const firstCatalog = await adapter.discover();
  const secondCatalog = await adapter.discover();

  const handles = Array.from({ length: 50 }, (_, index) =>
    (index % 2 === 0 ? firstCatalog : secondCatalog).beginInvoke({
      arguments: { index },
      invocationId: `invocation-${index}`,
      name: actionToolName(index % 2 === 0 ? "alpha" : "beta"),
    }),
  );
  assert.equal(new Set(handles.map((handle) => handle.requestId)).size, 50);
  assert.equal(
    new Set(
      handles.map((handle) =>
        handle.kind === "action" ? handle.actionCallId : assert.fail("action expected"),
      ),
    ).size,
    50,
  );

  for (let index = fake.beginCalls.length - 1; index >= 0; index -= 1) {
    const request = fake.beginCalls[index];
    assert.ok(request);
    const params = request.params as { id: string; name: string };
    request.resolve(actionResult(params.id, { settledIndex: index }));
  }
  const results = await Promise.all(handles.map((handle) => handle.result));
  for (let index = 0; index < results.length; index += 1) {
    const result = results[index];
    assert.ok(result?.kind === "action");
    assert.equal(result.invocationId, `invocation-${index}`);
    assert.equal(result.actionName, index % 2 === 0 ? "alpha" : "beta");
    assert.equal(result.action.callId, result.actionCallId);
  }

  assert.throws(
    () => secondCatalog.beginInvoke({ name: actionToolName("alpha") }),
    UnknownToolError,
  );
  const oldCatalogHandle = firstCatalog.beginInvoke({ name: actionToolName("alpha") });
  assert.equal(oldCatalogHandle.kind, "action");
});

test("request failures pass through without being converted to textual tool successes", async () => {
  const fake = new FakeToolClient([[action("tap")]]);
  const catalog = await tapCatalog(fake);
  const failure = new Error("driver failed");

  const result = catalog.invoke({ name: actionToolName("tap") });
  const request = fake.beginCalls[0];
  assert.ok(request);
  request.reject(failure);
  await assert.rejects(result, (error) => error === failure);
});

test("successful envelopes with mismatched result identity are rejected", async () => {
  const fake = new FakeToolClient([[action("tap")]]);
  const catalog = await tapCatalog(fake);

  const actionHandle = catalog.beginInvoke({ name: actionToolName("tap") });
  assert.ok(actionHandle.kind === "action");
  const actionRequest = fake.beginCalls[0];
  assert.ok(actionRequest);
  actionRequest.resolve(
    actionResult("00000000-0000-4000-8000-000000000099", { tapped: true }),
  );
  await assert.rejects(actionHandle.result, InvalidToolResultError);

  const observationHandle = catalog.beginInvoke({ name: OBSERVATION_TOOL_NAME });
  const observationRequest = fake.beginCalls[1];
  assert.ok(observationRequest);
  observationRequest.resolve(null);
  await assert.rejects(observationHandle.result, InvalidToolResultError);

  const incompleteAction = catalog.beginInvoke({ name: actionToolName("tap") });
  assert.ok(incompleteAction.kind === "action");
  const incompleteActionRequest = fake.beginCalls[2];
  assert.ok(incompleteActionRequest);
  incompleteActionRequest.resolve({ callId: incompleteAction.actionCallId });
  await assert.rejects(incompleteAction.result, InvalidToolResultError);

  const incompleteObservation = catalog.beginInvoke({ name: OBSERVATION_TOOL_NAME });
  const incompleteObservationRequest = fake.beginCalls[3];
  assert.ok(incompleteObservationRequest);
  incompleteObservationRequest.resolve({});
  await assert.rejects(incompleteObservation.result, InvalidToolResultError);
});

test("malformed successful results never become typed tool success", async () => {
  const fake = new FakeToolClient([[action("tap")]]);
  const catalog = await tapCatalog(fake);
  let getterCalled = false;
  const cases: Array<(callId: string) => unknown> = [
    (callId) => ({ ...actionResult(callId, null), startedAtMs: 0 }),
    (callId) => ({
      ...actionResult(callId, null),
      finishedAtMs: 9,
      startedAtMs: 10,
    }),
    (callId) => {
      const value = actionResult(callId, null) as unknown as Record<string, unknown>;
      delete value.output;
      return value;
    },
    (callId) => ({ ...actionResult(callId, null), after: null }),
    (callId) => ({ ...actionResult(callId, null), evidence: [] }),
    (callId) => {
      const value = actionResult(callId, null);
      const asset = value.evidence?.[0];
      assert.ok(asset);
      return { ...value, evidence: [{ ...asset }, { ...asset }] };
    },
    (callId) => {
      const value = actionResult(callId, null);
      const asset = value.evidence?.[0];
      assert.ok(asset);
      return { ...value, evidence: [{ ...asset, uri: "" }] };
    },
    (callId) => {
      const value = actionResult(callId, null);
      assert.ok(value.before);
      return {
        ...value,
        before: { ...value.before, deviceId: "another-device" },
      };
    },
    (callId) => {
      const value = actionResult(callId, null) as unknown as Record<string, unknown>;
      const cycle: Record<string, unknown> = {};
      cycle.self = cycle;
      value.output = cycle;
      return value;
    },
    (callId) => {
      const value = actionResult(callId, null) as unknown as Record<string, unknown>;
      Object.defineProperty(value, "output", {
        enumerable: true,
        get() {
          getterCalled = true;
          return "secret";
        },
      });
      return value;
    },
  ];

  for (const build of cases) {
    const handle = catalog.beginInvoke({ name: actionToolName("tap") });
    assert.ok(handle.kind === "action");
    const request = fake.beginCalls.at(-1);
    assert.ok(request);
    request.resolve(build(handle.actionCallId));
    await assert.rejects(handle.result, InvalidToolResultError);
  }
  assert.equal(getterCalled, false);

  const observationHandle = catalog.beginInvoke({ name: OBSERVATION_TOOL_NAME });
  const invalidObservation = observation(11);
  invalidObservation.id = "00000000-0000-0000-0000-000000000000";
  const observationRequest = fake.beginCalls.at(-1);
  assert.ok(observationRequest);
  observationRequest.resolve(invalidObservation);
  await assert.rejects(observationHandle.result, InvalidToolResultError);
});
