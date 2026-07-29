import { lstat, mkdir, readFile, realpath, rename, unlink, writeFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(import.meta.url);
const packageRoot = path.resolve(path.dirname(scriptPath), "..");
const repositoryRoot = path.resolve(packageRoot, "../..");
const schemaDirectory = path.join(repositoryRoot, "protocol/schema/v1");
const manifestPath = path.join(schemaDirectory, "manifest.json");
const outputPath = path.join(packageRoot, "src/generated/response-schemas.ts");
const argumentsList = process.argv.slice(2);
if (
  argumentsList.some((argument) => argument !== "--check") ||
  argumentsList.filter((argument) => argument === "--check").length > 1
) {
  throw new Error("usage: generate-response-schemas.mjs [--check]");
}
const check = argumentsList.includes("--check");

function compareText(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
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

function assertSafeSchemaFile(fileName, label) {
  if (
    typeof fileName !== "string" ||
    fileName.length === 0 ||
    path.isAbsolute(fileName) ||
    fileName.includes("/") ||
    fileName.includes("\\") ||
    !fileName.endsWith(".schema.json")
  ) {
    throw new Error(`${label} is not a safe .schema.json file name: ${String(fileName)}`);
  }
}

async function readJson(filePath, allowedRoot) {
  const metadata = await lstat(filePath);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    throw new Error(`protocol Schema input must be a regular file, not a link: ${filePath}`);
  }
  const [resolvedFile, resolvedRoot] = await Promise.all([
    realpath(filePath),
    realpath(allowedRoot),
  ]);
  const relative = path.relative(resolvedRoot, resolvedFile);
  if (relative.startsWith("..") || path.isAbsolute(relative)) {
    throw new Error(`protocol Schema input escapes its trusted root: ${filePath}`);
  }
  return JSON.parse(await readFile(resolvedFile, "utf8"));
}

async function expectedOutput() {
  const manifest = await readJson(manifestPath, schemaDirectory);
  if (!Array.isArray(manifest.documents)) {
    throw new Error("protocol Schema manifest has no documents array");
  }
  for (const [index, document] of manifest.documents.entries()) {
    assertSafeSchemaFile(document?.file, `manifest document ${index}.file`);
    if (typeof document?.name !== "string" || typeof document?.id !== "string") {
      throw new Error(`manifest document ${index} has an invalid name or id`);
    }
  }
  const documentsByName = new Map(manifest.documents.map((document) => [document.name, document]));
  if (documentsByName.size !== manifest.documents.length) {
    throw new Error("protocol Schema manifest contains duplicate document names");
  }
  if (new Set(manifest.documents.map((document) => document.id)).size !== manifest.documents.length) {
    throw new Error("protocol Schema manifest contains duplicate document ids");
  }
  if (new Set(manifest.documents.map((document) => document.file)).size !== manifest.documents.length) {
    throw new Error("protocol Schema manifest contains duplicate document files");
  }
  const entries = [];
  const methods = new Set();
  for (const document of manifest.documents) {
    if (!document.name.endsWith("Request")) {
      continue;
    }
    const requestSchema = await readJson(path.join(schemaDirectory, document.file), schemaDirectory);
    if (requestSchema.$id !== document.id || requestSchema.title !== document.name) {
      throw new Error(`${document.file} does not match its manifest identity`);
    }
    const method = requestMethod(requestSchema);
    if (method === undefined) {
      continue;
    }
    if (methods.has(method)) {
      throw new Error(`duplicate method-specific request Schema for ${method}`);
    }
    methods.add(method);
    const baseName = document.name.slice(0, -"Request".length);
    const responseDocument = documentsByName.get(`${baseName}Response`);
    if (responseDocument === undefined) {
      throw new Error(`${document.name} has no matching ${baseName}Response Schema`);
    }
    const responseSchema = await readJson(
      path.join(schemaDirectory, responseDocument.file),
      schemaDirectory,
    );
    if (responseSchema.$id !== responseDocument.id || responseSchema.title !== responseDocument.name) {
      throw new Error(`${responseDocument.file} does not match its manifest identity`);
    }
    entries.push({ method, schema: responseSchema });
  }
  entries.sort((left, right) => compareText(left.method, right.method));
  if (entries.length === 0) {
    throw new Error("no method-specific response Schemas were found");
  }
  const properties = entries
    .map(
      ({ method, schema }) =>
        `  ${JSON.stringify(method)}: ${JSON.stringify(schema, null, 2).replaceAll("\n", "\n  ")},`,
    )
    .join("\n");
  return `/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run \`pnpm --filter @devicerail/client runtime-schemas:generate\` from the repository root.
 */

import type { RpcMethod } from "@devicerail/protocol";

export const RPC_RESPONSE_SCHEMAS: Readonly<Record<RpcMethod, unknown>> = {
${properties}
};
`;
}

const expected = await expectedOutput();
if (check) {
  let actual;
  try {
    actual = await readFile(outputPath, "utf8");
  } catch (error) {
    if (error?.code === "ENOENT") {
      throw new Error(`generated runtime Schema registry is missing: ${outputPath}`);
    }
    throw error;
  }
  if (actual !== expected) {
    throw new Error(
      "generated runtime Schema registry is stale; run pnpm --filter @devicerail/client runtime-schemas:generate",
    );
  }
} else {
  await mkdir(path.dirname(outputPath), { recursive: true });
  const temporaryPath = `${outputPath}.${process.pid}.tmp`;
  try {
    await writeFile(temporaryPath, expected, { encoding: "utf8", flag: "wx" });
    await rename(temporaryPath, outputPath);
  } finally {
    await unlink(temporaryPath).catch((error) => {
      if (error?.code !== "ENOENT") {
        throw error;
      }
    });
  }
}
