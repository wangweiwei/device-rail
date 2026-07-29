import {
  lstat,
  mkdir,
  readFile,
  readdir,
  realpath,
  rename,
  unlink,
  writeFile,
} from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { compile } from "json-schema-to-typescript";

const scriptPath = fileURLToPath(import.meta.url);
const packageRoot = path.resolve(path.dirname(scriptPath), "..");
const repositoryRoot = path.resolve(packageRoot, "../..");
const schemaDirectory = path.join(repositoryRoot, "protocol/schema/v1");
const schemaManifestPath = path.join(schemaDirectory, "manifest.json");
const fixtureDirectory = path.join(repositoryRoot, "crates/protocol/fixtures");
const fixtureManifestPath = path.join(fixtureDirectory, "manifest.json");
const generatedDirectory = path.join(packageRoot, "src/generated/v1");
const generatedFixturePath = path.join(packageRoot, "test/fixtures.generated.ts");
const draft202012 = "https://json-schema.org/draft/2020-12/schema";
const banner = `/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run \`pnpm protocol:types:generate\` from the repository root.
 */`;

function compareText(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

function normalizeSource(source) {
  return `${source.replaceAll("\r\n", "\n").trimEnd()}\n`;
}

function assertSafeRelativeFile(value, suffix, label) {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    path.isAbsolute(value) ||
    value.includes("/") ||
    value.includes("\\") ||
    value === "." ||
    value === ".." ||
    !value.endsWith(suffix)
  ) {
    throw new Error(`${label} is not a safe ${suffix} file name: ${String(value)}`);
  }
}

async function readCheckedFile(filePath, allowedRoot) {
  const metadata = await lstat(filePath);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    throw new Error(`protocol input must be a regular file, not a link: ${filePath}`);
  }
  const [resolvedFile, resolvedRoot] = await Promise.all([
    realpath(filePath),
    realpath(allowedRoot),
  ]);
  const relative = path.relative(resolvedRoot, resolvedFile);
  if (relative.startsWith("..") || path.isAbsolute(relative)) {
    throw new Error(`protocol input escapes its trusted root: ${filePath}`);
  }
  return readFile(resolvedFile, "utf8");
}

async function readCheckedJson(filePath, allowedRoot) {
  const source = await readCheckedFile(filePath, allowedRoot);
  try {
    return JSON.parse(source);
  } catch (error) {
    throw new Error(`invalid JSON in ${filePath}: ${error.message}`, { cause: error });
  }
}

async function assertSafeOutputParents(targetDirectory) {
  const relative = path.relative(packageRoot, targetDirectory);
  if (relative.startsWith("..") || path.isAbsolute(relative)) {
    throw new Error(`generated output escapes the package root: ${targetDirectory}`);
  }
  let current = packageRoot;
  for (const segment of relative.split(path.sep).filter(Boolean)) {
    current = path.join(current, segment);
    try {
      const metadata = await lstat(current);
      if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
        throw new Error(`generated output parent must be a real directory: ${current}`);
      }
    } catch (error) {
      if (error?.code === "ENOENT") {
        return;
      }
      throw error;
    }
  }
}

function visitJson(value, visitor, location = "$") {
  visitor(value, location);
  if (Array.isArray(value)) {
    value.forEach((item, index) => visitJson(item, visitor, `${location}[${index}]`));
  } else if (value !== null && typeof value === "object") {
    for (const [key, child] of Object.entries(value)) {
      visitJson(child, visitor, `${location}.${key}`);
    }
  }
}

function validateSchemaSafety(schema, document) {
  visitJson(schema, (value, location) => {
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
      return;
    }
    if (Object.hasOwn(value, "$ref")) {
      const reference = value.$ref;
      if (typeof reference !== "string" || !reference.startsWith("#/")) {
        throw new Error(`${document.file} ${location} has a non-local $ref: ${reference}`);
      }
    }
    if (value.format === "uint64") {
      const isInteger =
        value.type === "integer" ||
        (Array.isArray(value.type) &&
          value.type.includes("integer") &&
          value.type.every((type) => type === "integer" || type === "null"));
      if (
        !isInteger ||
        !Number.isSafeInteger(value.minimum) ||
        value.minimum < 0 ||
        !Number.isSafeInteger(value.maximum) ||
        value.maximum > Number.MAX_SAFE_INTEGER
      ) {
        throw new Error(
          `${document.file} ${location} has uint64 without a JavaScript-safe maximum`,
        );
      }
    }
  });
}

function validateFixtureNumbers(fixture, fixtureId) {
  visitJson(fixture, (value, location) => {
    if (typeof value === "number" && Number.isInteger(value) && !Number.isSafeInteger(value)) {
      throw new Error(`${fixtureId} ${location} is not a JavaScript-safe integer`);
    }
  });
}

const annotationOnlyKeys = new Set([
  "$comment",
  "$id",
  "$schema",
  "default",
  "deprecated",
  "description",
  "examples",
  "readOnly",
  "title",
  "writeOnly",
]);

function normalizeAnnotationOnlySchemas(schema) {
  if (typeof schema === "boolean" || schema === null || typeof schema !== "object") {
    return schema;
  }
  if (Array.isArray(schema)) {
    return schema.map(normalizeAnnotationOnlySchemas);
  }

  const normalized = { ...schema };
  for (const key of ["$defs", "definitions", "properties", "patternProperties", "dependentSchemas"]) {
    if (normalized[key] !== null && typeof normalized[key] === "object") {
      normalized[key] = Object.fromEntries(
        Object.entries(normalized[key]).map(([name, child]) => [
          name,
          normalizeAnnotationOnlySchemas(child),
        ]),
      );
    }
  }
  for (const key of ["allOf", "anyOf", "oneOf", "prefixItems"]) {
    if (Array.isArray(normalized[key])) {
      normalized[key] = normalized[key].map(normalizeAnnotationOnlySchemas);
    }
  }
  for (const key of [
    "additionalProperties",
    "contains",
    "else",
    "if",
    "items",
    "not",
    "propertyNames",
    "then",
    "unevaluatedItems",
    "unevaluatedProperties",
  ]) {
    if (Object.hasOwn(normalized, key)) {
      normalized[key] = normalizeAnnotationOnlySchemas(normalized[key]);
    }
  }

  return Object.keys(normalized).every((key) => annotationOnlyKeys.has(key)) ? true : normalized;
}

function exactEmptyObjectNames(schema, rootName) {
  const names = [];
  const isExactEmptyObject = (value) =>
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    value.type === "object" &&
    value.additionalProperties === false &&
    (!Object.hasOwn(value, "properties") || Object.keys(value.properties).length === 0) &&
    (!Object.hasOwn(value, "patternProperties") ||
      Object.keys(value.patternProperties).length === 0);

  if (isExactEmptyObject(schema)) {
    names.push(rootName);
  }
  for (const definitions of [schema.$defs, schema.definitions]) {
    if (definitions !== null && typeof definitions === "object") {
      for (const [name, definition] of Object.entries(definitions)) {
        if (isExactEmptyObject(definition)) {
          names.push(name);
        }
      }
    }
  }
  return names;
}

function enforceExactEmptyObjects(source, schema, rootName) {
  let result = source;
  for (const name of exactEmptyObjectNames(schema, rootName)) {
    const escapedName = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const emptyInterface = new RegExp(`export interface ${escapedName}\\s*\\{\\s*\\}`);
    if (!emptyInterface.test(result)) {
      throw new Error(`generator did not expose exact empty object ${name} in ${rootName}`);
    }
    result = result.replace(emptyInterface, `export type ${name} = Record<string, never>;`);
  }
  return result;
}

function validateSchemaManifest(manifest) {
  if (
    manifest === null ||
    typeof manifest !== "object" ||
    !Array.isArray(manifest.documents) ||
    manifest.documents.length === 0
  ) {
    throw new Error("schema manifest must contain a non-empty documents array");
  }
  if (manifest.draft !== draft202012) {
    throw new Error(`unsupported schema draft: ${String(manifest.draft)}`);
  }
  if (manifest.schemaSetVersion !== 1) {
    throw new Error(`unsupported schema set version: ${String(manifest.schemaSetVersion)}`);
  }
  if (
    manifest.protocolVersion?.major !== 1 ||
    !Number.isSafeInteger(manifest.protocolVersion?.minor)
  ) {
    throw new Error("schema manifest must declare a valid protocol v1 version");
  }

  const names = new Set();
  const files = new Set();
  const ids = new Set();
  let previousFile = "";
  for (const document of manifest.documents) {
    assertSafeRelativeFile(document.file, ".schema.json", "schema document file");
    if (typeof document.name !== "string" || !/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(document.name)) {
      throw new Error(`schema document has an invalid TypeScript name: ${String(document.name)}`);
    }
    if (typeof document.id !== "string" || !document.id.startsWith("urn:devicerail:schema:")) {
      throw new Error(`schema document has an invalid id: ${String(document.id)}`);
    }
    if (document.file <= previousFile) {
      throw new Error("schema manifest documents must be strictly sorted by file");
    }
    previousFile = document.file;
    if (names.has(document.name) || files.has(document.file) || ids.has(document.id)) {
      throw new Error(`schema manifest contains a duplicate document: ${document.file}`);
    }
    names.add(document.name);
    files.add(document.file);
    ids.add(document.id);
  }
}

function moduleName(schemaFile) {
  return schemaFile.slice(0, -".schema.json".length);
}

function generatedIndex(documents) {
  const exports = documents.map(
    (document) =>
      `export type { ${document.name} } from ${JSON.stringify(`./${moduleName(document.file)}.js`)};`,
  );
  exports.push(`export type {
  RpcMethod,
  RpcMethodMap,
  RpcParamsFor,
  RpcRequestFor,
  RpcResponseFor,
  RpcResultFor,
  RpcSupportsTimeout,
} from "./method-map.js";`);
  return normalizeSource(`${banner}\n\n${exports.join("\n")}`);
}

function resolveLocalReference(schema, reference) {
  if (typeof reference !== "string" || !reference.startsWith("#/")) {
    return undefined;
  }
  return reference
    .slice(2)
    .split("/")
    .map((segment) => segment.replaceAll("~1", "/").replaceAll("~0", "~"))
    .reduce((value, segment) => value?.[segment], schema);
}

function requestMethod(schema) {
  let method = schema.properties?.method;
  if (method?.$ref !== undefined) {
    method = resolveLocalReference(schema, method.$ref);
  }
  if (typeof method?.const === "string") {
    return method.const;
  }
  if (
    Array.isArray(method?.enum) &&
    method.enum.length === 1 &&
    typeof method.enum[0] === "string"
  ) {
    return method.enum[0];
  }
  return undefined;
}

function generatedMethodMap(methodRequests, documentByName) {
  const pairs = methodRequests
    .map(({ document, method }) => {
      const baseName = document.name.slice(0, -"Request".length);
      const response = documentByName.get(`${baseName}Response`);
      if (response === undefined) {
        throw new Error(`${document.name} has no matching ${baseName}Response schema`);
      }
      return { method, request: document, response };
    })
    .sort((left, right) => compareText(left.method, right.method));

  const seenMethods = new Set();
  for (const pair of pairs) {
    if (seenMethods.has(pair.method)) {
      throw new Error(`duplicate method-specific request schema for ${pair.method}`);
    }
    seenMethods.add(pair.method);
  }

  const imports = pairs
    .flatMap(({ request, response }) => [request, response])
    .sort((left, right) => compareText(left.name, right.name))
    .map(
      (document) =>
        `import type { ${document.name} } from ${JSON.stringify(`./${moduleName(document.file)}.js`)};`,
    );
  const entries = pairs.map(
    ({ method, request, response }) => `  ${JSON.stringify(method)}: {
    request: ${request.name};
    response: ${response.name};
  };`,
  );

  return normalizeSource(`${banner}

${imports.join("\n")}

export interface RpcMethodMap {
${entries.join("\n")}
}

export type RpcMethod = keyof RpcMethodMap;
export type RpcRequestFor<M extends RpcMethod> = RpcMethodMap[M]["request"];
export type RpcResponseFor<M extends RpcMethod> = RpcMethodMap[M]["response"];
export type RpcParamsFor<M extends RpcMethod> = RpcRequestFor<M> extends { params: infer P }
  ? P
  : RpcRequestFor<M> extends { params?: infer P }
    ? P | undefined
    : undefined;
export type RpcResultFor<M extends RpcMethod> = RpcResponseFor<M> extends infer Response
  ? Response extends { result: infer Result }
    ? Result
    : never
  : never;
export type RpcSupportsTimeout<M extends RpcMethod> = "timeoutMs" extends keyof RpcRequestFor<M>
  ? true
  : false;`);
}

async function generatedFixtures(schemaManifest) {
  const fixtureManifest = await readCheckedJson(fixtureManifestPath, fixtureDirectory);
  if (!Array.isArray(fixtureManifest.fixtures)) {
    throw new Error("fixture manifest must contain a fixtures array");
  }
  if (fixtureManifest.manifestVersion !== 1) {
    throw new Error(`unsupported fixture manifest version: ${String(fixtureManifest.manifestVersion)}`);
  }
  if (
    fixtureManifest.fixturePathsRelativeTo !== "manifestDirectory" ||
    fixtureManifest.schemaPathsRelativeTo !== "repositoryRoot"
  ) {
    throw new Error("fixture manifest uses unsupported fixture or schema path roots");
  }
  if (
    fixtureManifest.protocolVersion?.major !== schemaManifest.protocolVersion.major ||
    fixtureManifest.protocolVersion?.minor !== schemaManifest.protocolVersion.minor
  ) {
    throw new Error("schema and fixture manifests declare different protocol versions");
  }

  const typeBySchemaFile = new Map(
    schemaManifest.documents.map((document) => [document.file, document.name]),
  );
  const fixtureIds = new Set();
  const usedTypes = new Set();
  const entries = [];
  for (const fixture of fixtureManifest.fixtures) {
    if (typeof fixture.id !== "string" || fixtureIds.has(fixture.id)) {
      throw new Error(`fixture manifest contains an invalid or duplicate id: ${String(fixture.id)}`);
    }
    fixtureIds.add(fixture.id);
    if (typeof fixture.path !== "string" || path.isAbsolute(fixture.path)) {
      throw new Error(`${fixture.id} has an invalid fixture path`);
    }
    const fixturePath = path.resolve(fixtureDirectory, fixture.path);
    const relativeFixturePath = path.relative(fixtureDirectory, fixturePath);
    if (relativeFixturePath.startsWith("..") || path.isAbsolute(relativeFixturePath)) {
      throw new Error(`${fixture.id} escapes the fixture directory`);
    }
    if (typeof fixture.schema !== "string") {
      throw new Error(`${fixture.id} has no schema path`);
    }
    const schemaFile = path.basename(fixture.schema);
    const resolvedSchemaPath = path.resolve(repositoryRoot, fixture.schema);
    if (resolvedSchemaPath !== path.join(schemaDirectory, schemaFile)) {
      throw new Error(`${fixture.id} does not reference the canonical v1 schema directory`);
    }
    const typeName = typeBySchemaFile.get(schemaFile);
    if (typeName === undefined) {
      throw new Error(`${fixture.id} references an unknown schema: ${fixture.schema}`);
    }
    const value = await readCheckedJson(fixturePath, fixtureDirectory);
    validateFixtureNumbers(value, fixture.id);
    usedTypes.add(typeName);
    const literal = JSON.stringify(value, null, 2)
      .split("\n")
      .map((line) => `    ${line}`)
      .join("\n");
    entries.push(`  ${JSON.stringify(fixture.id)}: (\n${literal}\n  ) satisfies ${typeName},`);
  }

  const imports = [...usedTypes].sort().join(",\n  ");
  return normalizeSource(`${banner}

import type {
  ${imports}
} from "../src/generated/v1/index.js";

export const goldenFixtures = {
${entries.join("\n")}
};`);
}

export async function buildExpectedOutputs() {
  const schemaManifest = await readCheckedJson(schemaManifestPath, schemaDirectory);
  validateSchemaManifest(schemaManifest);
  const outputs = new Map();
  const documentByName = new Map(
    schemaManifest.documents.map((document) => [document.name, document]),
  );
  const methodRequests = [];

  for (const document of schemaManifest.documents) {
    const schemaPath = path.join(schemaDirectory, document.file);
    const schema = await readCheckedJson(schemaPath, schemaDirectory);
    if (schema.$schema !== schemaManifest.draft || schema.$id !== document.id || schema.title !== document.name) {
      throw new Error(`${document.file} does not match its manifest name, id, or draft`);
    }
    validateSchemaSafety(schema, document);
    const method = requestMethod(schema);
    if (method !== undefined) {
      if (document.name.endsWith("Request")) {
        methodRequests.push({ document, method });
      } else if (!document.name.endsWith("Notification")) {
        throw new Error(
          `${document.file} contains an RPC method but is neither a Request nor Notification root`,
        );
      }
    }
    const source = await compile(normalizeAnnotationOnlySchemas(schema), document.name, {
      bannerComment: banner,
      cwd: schemaDirectory,
      enableConstEnums: false,
      unknownAny: true,
      style: {
        bracketSpacing: true,
        printWidth: 100,
        semi: true,
        singleQuote: false,
        tabWidth: 2,
        trailingComma: "all",
        useTabs: false,
      },
    });
    outputs.set(
      path.join(generatedDirectory, `${moduleName(document.file)}.ts`),
      normalizeSource(enforceExactEmptyObjects(source, schema, document.name)),
    );
  }

  outputs.set(
    path.join(generatedDirectory, "method-map.ts"),
    generatedMethodMap(methodRequests, documentByName),
  );
  outputs.set(path.join(generatedDirectory, "index.ts"), generatedIndex(schemaManifest.documents));
  outputs.set(generatedFixturePath, await generatedFixtures(schemaManifest));
  return outputs;
}

async function readGeneratedOutputs() {
  const outputs = new Map();
  try {
    const entries = await readdir(generatedDirectory, { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isFile() || entry.isSymbolicLink()) {
        throw new Error(`generated output contains an unexpected entry: ${entry.name}`);
      }
      const filePath = path.join(generatedDirectory, entry.name);
      outputs.set(filePath, await readFile(filePath, "utf8"));
    }
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw error;
    }
  }
  try {
    const metadata = await lstat(generatedFixturePath);
    if (!metadata.isFile() || metadata.isSymbolicLink()) {
      throw new Error("generated fixture output must be a regular file");
    }
    outputs.set(generatedFixturePath, await readFile(generatedFixturePath, "utf8"));
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw error;
    }
  }
  return outputs;
}

export function compareOutputs(expected, actual) {
  const missing = [];
  const changed = [];
  const stale = [];
  for (const [filePath, source] of expected) {
    if (!actual.has(filePath)) {
      missing.push(filePath);
    } else if (actual.get(filePath) !== source) {
      changed.push(filePath);
    }
  }
  for (const filePath of actual.keys()) {
    if (!expected.has(filePath)) {
      stale.push(filePath);
    }
  }
  missing.sort();
  changed.sort();
  stale.sort();
  return { missing, changed, stale };
}

function relativePaths(paths) {
  return paths.map((filePath) => path.relative(repositoryRoot, filePath));
}

async function atomicWrite(filePath, source, serial) {
  await mkdir(path.dirname(filePath), { recursive: true });
  const temporary = path.join(
    path.dirname(filePath),
    `.${path.basename(filePath)}.${process.pid}.${serial}.tmp`,
  );
  await writeFile(temporary, source, { encoding: "utf8", flag: "wx" });
  try {
    await rename(temporary, filePath);
  } catch (error) {
    await unlink(temporary).catch(() => {});
    throw error;
  }
}

async function run(mode) {
  await assertSafeOutputParents(generatedDirectory);
  await assertSafeOutputParents(path.dirname(generatedFixturePath));
  const expected = await buildExpectedOutputs();
  const actual = await readGeneratedOutputs();
  const differences = compareOutputs(expected, actual);
  const hasDifferences = Object.values(differences).some((paths) => paths.length > 0);

  if (mode === "check") {
    if (hasDifferences) {
      throw new Error(
        [
          "generated TypeScript protocol types are not current",
          `missing: ${relativePaths(differences.missing).join(", ") || "none"}`,
          `changed: ${relativePaths(differences.changed).join(", ") || "none"}`,
          `stale: ${relativePaths(differences.stale).join(", ") || "none"}`,
        ].join("\n"),
      );
    }
    process.stdout.write(`generated TypeScript protocol types are current (${expected.size - 3} models)\n`);
    return;
  }

  if (differences.stale.length > 0) {
    throw new Error(
      `refusing to overwrite a generated directory with stale entries: ${relativePaths(differences.stale).join(", ")}`,
    );
  }
  let serial = 0;
  for (const filePath of [...differences.missing, ...differences.changed].sort()) {
    await atomicWrite(filePath, expected.get(filePath), serial);
    serial += 1;
  }
  process.stdout.write(`wrote ${expected.size - 3} TypeScript protocol models and fixture contracts\n`);
}

async function main() {
  const args = process.argv.slice(2);
  if (args.length === 0) {
    await run("write");
  } else if (args.length === 1 && args[0] === "--check") {
    await run("check");
  } else {
    throw new Error("usage: node scripts/generate.mjs [--check]");
  }
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error) => {
    process.stderr.write(`${error.stack ?? error}\n`);
    process.exitCode = 1;
  });
}
