import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import {
  appendFile,
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const execFileAsync = promisify(execFile);
const sourceScript = fileURLToPath(
  new URL("./generate-response-schemas.mjs", import.meta.url),
);

const requestDocument = {
  file: "fixture-request.schema.json",
  id: "urn:devicerail:test:fixture-request",
  name: "FixtureRequest",
};
const responseDocument = {
  file: "fixture-response.schema.json",
  id: "urn:devicerail:test:fixture-response",
  name: "FixtureResponse",
};

function requestSchema() {
  return {
    $id: requestDocument.id,
    $schema: "https://json-schema.org/draft/2020-12/schema",
    additionalProperties: false,
    properties: {
      method: { const: "fixture.call", type: "string" },
    },
    required: ["method"],
    title: requestDocument.name,
    type: "object",
  };
}

function responseSchema() {
  return {
    $id: responseDocument.id,
    $schema: "https://json-schema.org/draft/2020-12/schema",
    additionalProperties: false,
    properties: {
      id: { type: "string" },
      jsonrpc: { const: "2.0", type: "string" },
      result: { type: "boolean" },
    },
    required: ["id", "jsonrpc", "result"],
    title: responseDocument.name,
    type: "object",
  };
}

async function writeJson(filePath, value) {
  await writeFile(filePath, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

async function createFixture(context) {
  const root = await mkdtemp(path.join(os.tmpdir(), "devicerail-client-schema-generator-"));
  context.after(async () => await rm(root, { force: true, recursive: true }));
  const schemaDirectory = path.join(root, "protocol/schema/v1");
  const packageRoot = path.join(root, "packages/client");
  const scriptsDirectory = path.join(packageRoot, "scripts");
  await mkdir(schemaDirectory, { recursive: true });
  await mkdir(scriptsDirectory, { recursive: true });
  const scriptPath = path.join(scriptsDirectory, "generate-response-schemas.mjs");
  const manifestPath = path.join(schemaDirectory, "manifest.json");
  const requestPath = path.join(schemaDirectory, requestDocument.file);
  const responsePath = path.join(schemaDirectory, responseDocument.file);
  const outputPath = path.join(packageRoot, "src/generated/response-schemas.ts");
  await Promise.all([
    copyFile(sourceScript, scriptPath),
    writeJson(manifestPath, { documents: [requestDocument, responseDocument] }),
    writeJson(requestPath, requestSchema()),
    writeJson(responsePath, responseSchema()),
  ]);
  return {
    manifestPath,
    outputPath,
    requestPath,
    responsePath,
    root,
    schemaDirectory,
    scriptPath,
  };
}

async function runGenerator(fixture, ...argumentsList) {
  return await execFileAsync(process.execPath, [fixture.scriptPath, ...argumentsList], {
    cwd: fixture.root,
    encoding: "utf8",
  });
}

async function assertGeneratorFailure(fixture, argumentsList, expectedMessage) {
  await assert.rejects(
    runGenerator(fixture, ...argumentsList),
    (error) => {
      assert.equal(error?.code, 1);
      assert.match(`${String(error?.stdout)}\n${String(error?.stderr)}`, expectedMessage);
      return true;
    },
  );
}

test("generation is deterministic and --check rejects missing and stale output", async (context) => {
  const fixture = await createFixture(context);
  await assertGeneratorFailure(
    fixture,
    ["--check"],
    /generated runtime Schema registry is missing/u,
  );

  await runGenerator(fixture);
  const first = await readFile(fixture.outputPath, "utf8");
  await runGenerator(fixture);
  assert.equal(await readFile(fixture.outputPath, "utf8"), first);
  await runGenerator(fixture, "--check");

  await appendFile(fixture.outputPath, "// stale\n", "utf8");
  await assertGeneratorFailure(
    fixture,
    ["--check"],
    /generated runtime Schema registry is stale/u,
  );
});

test("manifest files must be safe basenames inside the trusted Schema directory", async (context) => {
  const fixture = await createFixture(context);
  await writeJson(fixture.manifestPath, {
    documents: [{ ...requestDocument, file: "../fixture-request.schema.json" }, responseDocument],
  });
  await assertGeneratorFailure(fixture, [], /is not a safe \.schema\.json file name/u);
});

test("Schema inputs must be regular files rather than symbolic links", async (context) => {
  const fixture = await createFixture(context);
  const outsideResponse = path.join(fixture.root, "outside-response.schema.json");
  await writeJson(outsideResponse, responseSchema());
  await rm(fixture.responsePath);
  try {
    await symlink(outsideResponse, fixture.responsePath);
  } catch (error) {
    if (["EACCES", "ENOSYS", "EPERM"].includes(error?.code)) {
      context.skip(`symbolic links are unavailable on this runner (${error.code})`);
      return;
    }
    throw error;
  }
  await assertGeneratorFailure(
    fixture,
    [],
    /protocol Schema input must be a regular file, not a link/u,
  );
});

test("Schema identity must match the manifest", async (context) => {
  const fixture = await createFixture(context);
  await writeJson(fixture.responsePath, {
    ...responseSchema(),
    $id: "urn:devicerail:test:wrong-response",
  });
  await assertGeneratorFailure(
    fixture,
    [],
    /fixture-response\.schema\.json does not match its manifest identity/u,
  );
});

test("duplicate manifest names, ids, and files are rejected", async (context) => {
  const duplicateCases = [
    {
      expected: /duplicate document names/u,
      extra: { file: "duplicate-name.schema.json", id: "urn:duplicate:name", name: requestDocument.name },
    },
    {
      expected: /duplicate document ids/u,
      extra: { file: "duplicate-id.schema.json", id: requestDocument.id, name: "DuplicateId" },
    },
    {
      expected: /duplicate document files/u,
      extra: { file: requestDocument.file, id: "urn:duplicate:file", name: "DuplicateFile" },
    },
  ];
  for (const duplicateCase of duplicateCases) {
    const fixture = await createFixture(context);
    await writeJson(fixture.manifestPath, {
      documents: [requestDocument, responseDocument, duplicateCase.extra],
    });
    await assertGeneratorFailure(fixture, [], duplicateCase.expected);
  }
});
