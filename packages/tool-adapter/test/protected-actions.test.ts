import assert from "node:assert/strict";
import test from "node:test";

import {
  actionToolName,
  DeviceRailToolAdapter,
  InvalidActionSpaceError,
  InvalidToolResultError,
} from "../src/index.js";
import { action, FakeToolClient, observation } from "./fake-client.js";

const PROTECTED_FEATURE = "action.protected.v1";

function omittedObservation(sequence: number, reason: "policy" | "protectedAction") {
  return {
    ...observation(sequence),
    screenshotOmission: reason,
  };
}

function omittedActionResult(callId: string, reason: "policy" | "protectedAction") {
  return {
    after: omittedObservation(2, reason),
    before: omittedObservation(1, reason),
    callId,
    evidence: [],
    finishedAtMs: 20,
    output: { accepted: true },
    startedAtMs: 10,
  };
}

test("protected capabilities require both adapter opt-in and a negotiated feature", async () => {
  const capabilities = [
    action("tap"),
    action("inputSecret", undefined, "Type a protected secret", "protected"),
  ];

  const noFeature = new FakeToolClient([capabilities, capabilities]);
  const defaultCatalog = await new DeviceRailToolAdapter(noFeature.client).discover();
  assert.equal(
    defaultCatalog.tools.some(
      (definition) => definition.kind === "action" && definition.actionName === "inputSecret",
    ),
    false,
  );
  assert.throws(
    () =>
      new DeviceRailToolAdapter(noFeature.client, {
        includeProtectedActions: true,
      }),
    InvalidActionSpaceError,
  );

  const enabled = new Set([PROTECTED_FEATURE]);
  const withFeature = new FakeToolClient([capabilities, capabilities], enabled);
  const filtered = await new DeviceRailToolAdapter(withFeature.client).discover();
  assert.equal(
    filtered.tools.some(
      (definition) => definition.kind === "action" && definition.actionName === "inputSecret",
    ),
    false,
  );

  const opted = await new DeviceRailToolAdapter(withFeature.client, {
    includeProtectedActions: true,
  }).discover();
  const protectedDefinition = opted.tools.find(
    (definition) => definition.kind === "action" && definition.actionName === "inputSecret",
  );
  assert.ok(protectedDefinition?.kind === "action");
  assert.equal(protectedDefinition.protection, "protected");
  const standardDefinition = opted.tools.find(
    (definition) => definition.kind === "action" && definition.actionName === "tap",
  );
  assert.ok(standardDefinition?.kind === "action");
  assert.equal(Object.hasOwn(standardDefinition, "protection"), false);

  enabled.clear();
  assert.throws(
    () =>
      opted.beginInvoke({
        arguments: { secret: "sentinel-must-not-be-admitted" },
        name: actionToolName("inputSecret"),
      }),
    InvalidActionSpaceError,
  );
  assert.equal(withFeature.beginCalls.length, 0);
});

test("legacy custom clients without enabledFeatures remain compatible and fail closed", async () => {
  const fake = new FakeToolClient([
    [
      action("tap"),
      action("inputSecret", undefined, "Type a protected secret", "protected"),
    ],
  ]);
  const legacyClient = {
    beginCall: fake.client.beginCall,
    call: fake.client.call,
  };
  const catalog = await new DeviceRailToolAdapter(legacyClient).discover();
  assert.deepEqual(
    catalog.tools
      .filter((definition) => definition.kind === "action")
      .map((definition) => definition.actionName),
    ["tap"],
  );
  assert.throws(
    () =>
      new DeviceRailToolAdapter(legacyClient, {
        includeProtectedActions: true,
      }),
    InvalidActionSpaceError,
  );
});

test("unknown protection classifications fail closed before filtering", async () => {
  const fake = new FakeToolClient([
    [
      {
        ...action("inputSecret"),
        protection: "private-but-unknown",
      },
    ],
  ]);
  await assert.rejects(
    new DeviceRailToolAdapter(fake.client).discover(),
    (error: unknown) =>
      error instanceof InvalidActionSpaceError && /protection classification/u.test(error.message),
  );
});

test("explicit screenshot omission permits empty evidence while unmarked omission does not", async () => {
  const feature = new Set([PROTECTED_FEATURE]);
  const fake = new FakeToolClient(
    [
      [action("inputSecret", undefined, "Type a protected secret", "protected")],
      [action("inputSecret", undefined, "Type a protected secret", "protected")],
    ],
    feature,
  );
  const adapter = new DeviceRailToolAdapter(fake.client, { includeProtectedActions: true });

  const acceptedCatalog = await adapter.discover();
  const accepted = acceptedCatalog.beginInvoke({
    arguments: { secret: "protected-sentinel" },
    name: actionToolName("inputSecret"),
  });
  assert.equal(accepted.kind, "action");
  fake.beginCalls[0]?.resolve(omittedActionResult(accepted.actionCallId, "protectedAction"));
  const acceptedResult = await accepted.result;
  assert.equal(acceptedResult.kind, "action");
  assert.deepEqual(acceptedResult.action.evidence, []);

  const rejectedCatalog = await adapter.discover();
  const rejected = rejectedCatalog.beginInvoke({
    arguments: { secret: "second-protected-sentinel" },
    name: actionToolName("inputSecret"),
  });
  assert.equal(rejected.kind, "action");
  fake.beginCalls[1]?.resolve({
    ...omittedActionResult(rejected.actionCallId, "protectedAction"),
    after: observation(2),
  });
  await assert.rejects(rejected.result, InvalidToolResultError);
});

test("protected results require both display observations and reject all evidence", async () => {
  const feature = new Set([PROTECTED_FEATURE]);
  const fake = new FakeToolClient(
    [
      [action("inputSecret", undefined, "Type a protected secret", "protected")],
      [action("inputSecret", undefined, "Type a protected secret", "protected")],
    ],
    feature,
  );
  const adapter = new DeviceRailToolAdapter(fake.client, { includeProtectedActions: true });

  const missingBeforeCatalog = await adapter.discover();
  const missingBefore = missingBeforeCatalog.beginInvoke({
    arguments: { secret: "protected-sentinel" },
    name: actionToolName("inputSecret"),
  });
  assert.equal(missingBefore.kind, "action");
  fake.beginCalls[0]?.resolve({
    ...omittedActionResult(missingBefore.actionCallId, "protectedAction"),
    before: null,
  });
  await assert.rejects(missingBefore.result, InvalidToolResultError);

  const evidenceCatalog = await adapter.discover();
  const evidence = evidenceCatalog.beginInvoke({
    arguments: { secret: "second-protected-sentinel" },
    name: actionToolName("inputSecret"),
  });
  assert.equal(evidence.kind, "action");
  fake.beginCalls[1]?.resolve({
    ...omittedActionResult(evidence.actionCallId, "protectedAction"),
    evidence: [observation(3).screenshot],
  });
  await assert.rejects(evidence.result, InvalidToolResultError);
});

test("standard actions accept policy-marked screenshot omission", async () => {
  const fake = new FakeToolClient([[action("tap")]]);
  const catalog = await new DeviceRailToolAdapter(fake.client).discover();
  const handle = catalog.beginInvoke({ arguments: {}, name: actionToolName("tap") });
  assert.equal(handle.kind, "action");
  fake.beginCalls[0]?.resolve(omittedActionResult(handle.actionCallId, "policy"));
  const result = await handle.result;
  assert.equal(result.kind, "action");
  assert.deepEqual(result.action.evidence, []);
});
