import assert from "node:assert/strict";
import test from "node:test";

import {
  type ActionProtectionContext,
  compileYamlPlan,
  executeYamlPlan,
  YamlPlanExecutionError,
  YamlPlanValidationError,
} from "../src/index.js";

function standardActions(context: ActionProtectionContext): "standard" | undefined {
  return context.actionName === "tap" && context.deviceId === "fixture-device"
    ? "standard"
    : undefined;
}

test("safe YAML compiles into a deeply immutable public-call plan", () => {
  const plan = compileYamlPlan(
    `version: devicerail/v1
steps:
  - id: connect
    method: device.connect
    params: {}
  - id: tap
    method: device.execute
    timeoutMs: 5000
    params:
      id: 11111111-1111-4111-8111-111111111111
      name: tap
      arguments:
        x: 1e0
        y: 2
`,
    { classifyActionProtection: standardActions, initialDeviceId: "fixture-device" },
  );

  assert.equal(plan.version, "devicerail/v1");
  assert.deepEqual(plan.steps.map((step) => step.method), ["device.connect", "device.execute"]);
  assert.equal(Object.isFrozen(plan), true);
  assert.equal(Object.isFrozen(plan.steps), true);
  assert.equal(Object.isFrozen(plan.steps[1]?.params?.arguments), true);
  assert.throws(() => {
    (plan.steps as unknown[]).push({});
  }, TypeError);
});

test("execution is sequential, passes public parameters, and returns immutable results", async () => {
  const plan = compileYamlPlan(
    `version: devicerail/v1
steps:
  - id: list
    method: devices.list
  - id: observe
    method: device.observe
    timeoutMs: 50
    params: {}
`,
  );
  const calls: unknown[][] = [];
  const client = {
    async call(...args: unknown[]): Promise<unknown> {
      calls.push(args);
      return { call: calls.length };
    },
  };
  const result = await executeYamlPlan(client as never, plan);
  assert.deepEqual(calls, [
    ["devices.list", undefined, undefined],
    ["device.observe", {}, { timeoutMs: 50 }],
  ]);
  assert.deepEqual(result.steps.map((step) => step.result), [{ call: 1 }, { call: 2 }]);
  assert.equal(Object.isFrozen(result.steps), true);
});

test("execution keeps the public client method bound to its instance", async () => {
  const plan = compileYamlPlan(
    "version: devicerail/v1\nsteps:\n  - id: list\n    method: devices.list\n",
  );
  class StatefulClient {
    calls = 0;

    async call(): Promise<unknown> {
      this.calls += 1;
      return { calls: this.calls };
    }
  }
  const client = new StatefulClient();
  const result = await executeYamlPlan(client as never, plan);
  assert.equal(client.calls, 1);
  assert.deepEqual(result.steps[0]?.result, { calls: 1 });
});

test("signal and timeout options follow the public client's cancellable method set", async () => {
  assert.throws(
    () =>
      compileYamlPlan(
        "version: devicerail/v1\nsteps:\n  - id: list\n    method: devices.list\n    timeoutMs: 50\n",
      ),
    (error: unknown) =>
      error instanceof YamlPlanValidationError && error.code === "yaml_timeout_unsupported",
  );

  const plan = compileYamlPlan(
    `version: devicerail/v1
steps:
  - id: list
    method: devices.list
  - id: observe
    method: device.observe
    timeoutMs: 50
    params: {}
`,
  );
  const controller = new AbortController();
  const calls: unknown[][] = [];
  const client = {
    async call(method: string, params?: unknown, options?: unknown): Promise<unknown> {
      calls.push([method, params, options]);
      return {};
    },
  };
  await executeYamlPlan(client as never, plan, { signal: controller.signal });
  assert.deepEqual(calls, [
    ["devices.list", undefined, undefined],
    ["device.observe", {}, { signal: controller.signal, timeoutMs: 50 }],
  ]);
});

test("protected and unclassified actions never compile from persistent YAML", () => {
  const source = `version: devicerail/v1
steps:
  - id: secret
    method: device.execute
    params:
      id: 11111111-1111-4111-8111-111111111111
      name: inputSecret
      arguments:
        text: never-persist-me
`;
  for (const classifyActionProtection of [
    undefined,
    () => "protected" as const,
  ]) {
    assert.throws(
      () =>
        compileYamlPlan(
          source,
          classifyActionProtection === undefined
            ? { initialDeviceId: "fixture-device" }
            : { classifyActionProtection, initialDeviceId: "fixture-device" },
        ),
      (error: unknown) =>
        error instanceof YamlPlanValidationError &&
        ["yaml_action_unknown", "yaml_protected_action_forbidden"].includes(error.code) &&
        !error.message.includes("never-persist-me"),
    );
  }
});

test("unsafe YAML features, duplicate keys, prototype keys, and aliases fail closed", () => {
  const invalidSources = [
    "version: devicerail/v1\nversion: devicerail/v1\nsteps: []\n",
    "version: !!js/function 'function(){}'\nsteps: []\n",
    "version: devicerail/v1\nsteps: &steps [{id: list, method: devices.list}]\ncopy: *steps\n",
    "version: devicerail/v1\nsteps:\n  - id: list\n    method: devices.list\n    params:\n      __proto__: polluted\n",
  ];
  for (const source of invalidSources) {
    assert.throws(() => compileYamlPlan(source), YamlPlanValidationError);
  }
  assert.equal(({} as { polluted?: unknown }).polluted, undefined);
});

test("plan shape, methods, numbers, limits, and unknown fields are closed", () => {
  for (const source of [
    "version: future/v2\nsteps:\n  - id: list\n    method: devices.list\n",
    "version: devicerail/v1\nsteps: []\n",
    "version: devicerail/v1\nsteps:\n  - id: 1bad\n    method: devices.list\n",
    "version: devicerail/v1\nsteps:\n  - id: hello\n    method: system.hello\n",
    "version: devicerail/v1\nsteps:\n  - id: list\n    method: devices.list\n    timeoutMs: 0\n",
    "version: devicerail/v1\nsteps:\n  - id: list\n    method: devices.list\n    surprise: true\n",
  ]) {
    assert.throws(() => compileYamlPlan(source), YamlPlanValidationError);
  }
  assert.throws(
    () => compileYamlPlan("x".repeat(256 * 1024 + 1)),
    (error: unknown) => error instanceof YamlPlanValidationError && error.code === "yaml_source_limit",
  );
});

test("execution errors identify only the step and never echo parameters", async () => {
  const plan = compileYamlPlan(
    "version: devicerail/v1\nsteps:\n  - id: observe\n    method: device.observe\n    params: {}\n",
  );
  const failure = new Error("transport stopped");
  const client = { call: async () => await Promise.reject(failure) };
  await assert.rejects(
    executeYamlPlan(client as never, plan),
    (error: unknown) =>
      error instanceof YamlPlanExecutionError &&
      error.stepId === "observe" &&
      error.cause === failure &&
      !error.message.includes("params"),
  );
});

test("execution rejects a manually constructed plan lookalike", async () => {
  const client = { call: async () => ({}) };
  await assert.rejects(
    executeYamlPlan(
      client as never,
      {
        version: "devicerail/v1",
        steps: [{ id: "describe", method: "system.describe" }],
      } as never,
    ),
    (error: unknown) =>
      error instanceof YamlPlanValidationError && error.code === "yaml_plan_untrusted",
  );

  const legitimate = compileYamlPlan(
    "version: devicerail/v1\nsteps:\n  - id: describe\n    method: system.describe\n",
  );
  const inherited = Object.create(legitimate) as typeof legitimate;
  await assert.rejects(
    executeYamlPlan(client as never, inherited),
    (error: unknown) =>
      error instanceof YamlPlanValidationError && error.code === "yaml_plan_untrusted",
  );
});

test("action protection is device-bound at compile time and rechecked before execution", async () => {
  const source = `version: devicerail/v1
steps:
  - id: select
    method: device.select
    params:
      deviceId: device-b
  - id: tap
    method: device.execute
    params:
      id: 11111111-1111-4111-8111-111111111111
      name: tap
      arguments: {x: 1, y: 2}
`;
  assert.throws(
    () =>
      compileYamlPlan(source, {
        classifyActionProtection: ({ deviceId }) =>
          deviceId === "device-b" ? "protected" : "standard",
      }),
    (error: unknown) =>
      error instanceof YamlPlanValidationError &&
      error.code === "yaml_protected_action_forbidden",
  );

  const plan = compileYamlPlan(source, {
    classifyActionProtection: ({ actionName, deviceId }) =>
      actionName === "tap" && deviceId === "device-b" ? "standard" : undefined,
  });
  assert.equal(plan.steps[1]?.boundDeviceId, "device-b");
  const calls: unknown[][] = [];
  const client = {
    async call(method: string, ...args: unknown[]): Promise<unknown> {
      calls.push([method, ...args]);
      if (method === "devices.list") {
        return { devices: [], selectedDeviceId: "device-b" };
      }
      if (method === "device.capabilities") {
        return [{ name: "tap", description: "Tap", inputSchema: {}, protection: "protected" }];
      }
      return {};
    },
  };
  await assert.rejects(
    executeYamlPlan(client as never, plan),
    (error: unknown) =>
      error instanceof YamlPlanExecutionError &&
      error.stepId === "tap" &&
      error.cause instanceof YamlPlanValidationError &&
      error.cause.code === "yaml_action_runtime_forbidden",
  );
  assert.deepEqual(
    calls.map(([method]) => method),
    ["device.select", "device.select", "devices.list", "device.capabilities"],
  );
  assert.equal(calls.some(([method]) => method === "device.execute"), false);
});

test("verified action starts without a post-capability microtask route window", async () => {
  const plan = compileYamlPlan(
    `version: devicerail/v1
steps:
  - id: tap
    method: device.execute
    params:
      id: 11111111-1111-4111-8111-111111111111
      name: tap
      arguments: {x: 1, y: 2}
`,
    { classifyActionProtection: standardActions, initialDeviceId: "fixture-device" },
  );
  let route = "fixture-device";
  const client = {
    async call(method: string, params?: unknown, options?: unknown): Promise<unknown> {
      if (method === "device.select") {
        route = (params as { deviceId: string }).deviceId;
        assert.equal(options, undefined);
        return {};
      }
      if (method === "devices.list") {
        assert.equal(options, undefined);
        return { devices: [], selectedDeviceId: route };
      }
      if (method === "device.capabilities") {
        // The second microtask runs after the capability continuation but
        // before a caller awaiting a separate verifier can resume.
        queueMicrotask(() => queueMicrotask(() => {
          route = "raced-device";
        }));
        return [{ name: "tap", description: "Tap", inputSchema: {}, protection: "standard" }];
      }
      if (method === "device.execute") {
        assert.equal(route, "fixture-device");
        return { outcome: "succeeded" };
      }
      throw new Error(`unexpected method ${method}`);
    },
  };
  const result = await executeYamlPlan(client as never, plan);
  assert.deepEqual(result.steps[0]?.result, { outcome: "succeeded" });
});
