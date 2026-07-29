import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const workspace = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const tag = process.argv[2];
const stableTag = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

if (!tag || !stableTag.test(tag)) {
  throw new Error("release tag must be a stable semantic version such as v0.1.0");
}

const version = tag.slice(1);
const publicNpmPackages = [
  "packages/protocol/package.json",
  "packages/client/package.json",
  "packages/tool-adapter/package.json",
  "packages/recorder/package.json",
  "packages/live-visualizer/package.json",
  "packages/yaml-adapter/package.json",
];

async function json(path) {
  return JSON.parse(await readFile(resolve(workspace, path), "utf8"));
}

function tomlField(document, section, field, label) {
  let active = false;
  for (const line of document.split(/\r?\n/u)) {
    const header = line.match(/^\[([A-Za-z0-9_.-]+)\]\s*$/u);
    if (header) {
      active = header[1] === section;
      continue;
    }
    if (!active) {
      continue;
    }
    const match = line.match(new RegExp(`^${field}\\s*=\\s*"([^"]+)"\\s*$`, "u"));
    if (match) {
      return match[1];
    }
  }
  throw new Error(`${label} is missing ${field} in [${section}]`);
}

const rootManifest = await json("package.json");
if (rootManifest.version !== version) {
  throw new Error(`package.json version ${rootManifest.version} does not match ${tag}`);
}
if (rootManifest.private !== true) {
  throw new Error("the workspace root must remain private");
}

for (const path of publicNpmPackages) {
  const manifest = await json(path);
  if (manifest.version !== version) {
    throw new Error(`${path} version ${manifest.version} does not match ${tag}`);
  }
  if (manifest.private === true) {
    throw new Error(`${path} is unexpectedly private`);
  }
  if (manifest.publishConfig?.access !== "public") {
    throw new Error(`${path} must publish with public access`);
  }
  if (manifest.publishConfig?.registry !== "https://registry.npmjs.org/") {
    throw new Error(`${path} must publish only to npmjs.org`);
  }
}

for (const path of [
  "packages/playwright-driver/package.json",
  "apps/live-visualizer/package.json",
]) {
  const manifest = await json(path);
  if (manifest.private !== true) {
    throw new Error(`${path} must remain private`);
  }
}

const pyproject = await readFile(
  resolve(workspace, "packages/python-client/pyproject.toml"),
  "utf8",
);
const pythonVersion = tomlField(
  pyproject,
  "project",
  "version",
  "packages/python-client/pyproject.toml",
);
if (pythonVersion !== version) {
  throw new Error(`Python package version ${pythonVersion} does not match ${tag}`);
}

const cargo = await readFile(resolve(workspace, "Cargo.toml"), "utf8");
const cargoVersion = tomlField(cargo, "workspace.package", "version", "Cargo.toml");
if (cargoVersion !== version) {
  throw new Error(`Cargo workspace version ${cargoVersion} does not match ${tag}`);
}

const rustClient = await readFile(
  resolve(workspace, "crates/client/Cargo.toml"),
  "utf8",
);
const protocolDependency = rustClient.match(
  /^devicerail-protocol\s*=\s*\{[^}\n]*\bversion\s*=\s*"([^"]+)"[^}\n]*\}\s*$/mu,
);
if (!protocolDependency) {
  throw new Error(
    "crates/client/Cargo.toml must pin devicerail-protocol with an explicit registry version",
  );
}
if (protocolDependency[1] !== version) {
  throw new Error(
    `Rust client protocol dependency ${protocolDependency[1]} does not match ${tag}`,
  );
}

console.log(`release ${tag} matches every public package and workspace version`);
