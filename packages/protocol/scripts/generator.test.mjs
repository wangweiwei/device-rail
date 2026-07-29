import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { buildExpectedOutputs, compareOutputs } from "./generate.mjs";

test("generation is deterministic and covers every public schema and fixture", async () => {
  const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");
  const schemaManifest = JSON.parse(
    await readFile(path.join(repositoryRoot, "protocol/schema/v1/manifest.json"), "utf8"),
  );
  const fixtureManifest = JSON.parse(
    await readFile(path.join(repositoryRoot, "crates/protocol/fixtures/manifest.json"), "utf8"),
  );
  const first = await buildExpectedOutputs();
  const second = await buildExpectedOutputs();
  assert.deepEqual([...first], [...second]);
  for (const source of first.values()) {
    assert.equal(source.includes(repositoryRoot), false);
    assert.equal(source.includes("\r"), false);
  }

  const modelFiles = [...first.keys()].filter(
    (file) =>
      path.basename(path.dirname(file)) === "v1" &&
      !["index.ts", "method-map.ts"].includes(path.basename(file)),
  );
  assert.equal(modelFiles.length, schemaManifest.documents.length);
  const actionCall = [...first.entries()].find(([file]) => path.basename(file) === "action-call.ts");
  assert.match(actionCall?.[1] ?? "", /arguments\?: unknown;/);
  const fixtures = [...first.entries()].find(([file]) => file.endsWith("fixtures.generated.ts"));
  assert.ok(fixtures);
  assert.equal(fixtures[1].match(/ satisfies /g)?.length, fixtureManifest.fixtures.length);
  const methodMap = [...first.entries()].find(([file]) => path.basename(file) === "method-map.ts");
  const methodRequestCount = schemaManifest.documents.filter(
    (document) => document.name.endsWith("Request") && document.name !== "RpcRequest",
  ).length;
  assert.equal(methodMap?.[1].match(/^  "[^"]+": \{$/gm)?.length, methodRequestCount);
});

test("generated checks distinguish missing, changed, and stale output", () => {
  const expected = new Map([
    ["a.ts", "a\n"],
    ["b.ts", "b\n"],
  ]);
  const actual = new Map([
    ["b.ts", "changed\n"],
    ["stale.ts", "stale\n"],
  ]);
  assert.deepEqual(compareOutputs(expected, actual), {
    missing: ["a.ts"],
    changed: ["b.ts"],
    stale: ["stale.ts"],
  });
});
