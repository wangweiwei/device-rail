import { spawnSync } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const workspace = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const stableVersion = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/u;

const packageManifests = [
  "package.json",
  "packages/protocol/package.json",
  "packages/client/package.json",
  "packages/tool-adapter/package.json",
  "packages/recorder/package.json",
  "packages/live-visualizer/package.json",
  "packages/yaml-adapter/package.json",
  "packages/playwright-driver/package.json",
  "apps/live-visualizer/package.json",
];

const npmPackages = [
  "@devicerail/protocol",
  "@devicerail/client",
  "@devicerail/live-visualizer",
  "@devicerail/tool-adapter",
  "@devicerail/recorder",
  "@devicerail/yaml-adapter",
];

const rustCrates = ["devicerail-protocol", "devicerail-client"];
const pythonPackage = "devicerail-client";

function run(command, args, cwd = workspace) {
  const result = spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    env: process.env,
    shell: false,
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(
      `${command} ${args.join(" ")} failed (${String(result.status)})\n${result.stdout}${result.stderr}`,
    );
  }
  return result.stdout;
}

function ordered(version) {
  return version.split(".").map((part) => Number(part));
}

function increases(next, current) {
  const left = ordered(next);
  const right = ordered(current);
  for (let index = 0; index < 3; index += 1) {
    if (left[index] !== right[index]) {
      return left[index] > right[index];
    }
  }
  return false;
}

function replaceOnce(source, pattern, replacement, label) {
  const matches = source.match(pattern);
  if (!matches || matches.length !== 1) {
    throw new Error(
      `${label} must contain exactly one version to rewrite, found ${String(matches ? matches.length : 0)}`,
    );
  }
  return source.replace(pattern, replacement);
}

function replaceEach(source, pattern, replacement, expected, label) {
  const matches = source.match(pattern);
  if (!matches || matches.length !== expected) {
    throw new Error(
      `${label} must contain ${String(expected)} versions to rewrite, found ${String(matches ? matches.length : 0)}`,
    );
  }
  return source.replace(pattern, replacement);
}

async function rewrite(path, rewriter) {
  const source = await readFile(resolve(workspace, path), "utf8");
  const next = rewriter(source);
  if (next === source) {
    throw new Error(`${path} was already at the requested version`);
  }
  await writeFile(resolve(workspace, path), next);
}

async function currentVersion() {
  const manifest = JSON.parse(await readFile(resolve(workspace, "package.json"), "utf8"));
  if (typeof manifest.version !== "string") {
    throw new Error("package.json is missing a string version");
  }
  return manifest.version;
}

function requireCleanTree() {
  const status = run("git", ["status", "--porcelain"]);
  if (status.trim() !== "") {
    throw new Error(`the working tree must be clean, found:\n${status}`);
  }
}

function requireAbsentTag(tag) {
  const local = run("git", ["tag", "--list", tag]).trim();
  if (local !== "") {
    throw new Error(`${tag} already exists locally; registry versions are immutable`);
  }
  const remote = run("git", ["ls-remote", "--tags", "origin", `refs/tags/${tag}`]).trim();
  if (remote !== "") {
    throw new Error(`${tag} already exists on origin; registry versions are immutable`);
  }
}

async function published(version) {
  const results = [];
  for (const name of npmPackages) {
    const response = await fetch(`https://registry.npmjs.org/${name}`);
    const body = response.ok ? await response.json() : { versions: {} };
    results.push({
      registry: "npm",
      name,
      present: Object.hasOwn(body.versions ?? {}, version),
    });
  }
  const pypi = await fetch(`https://pypi.org/pypi/${pythonPackage}/json`);
  const pypiBody = pypi.ok ? await pypi.json() : { releases: {} };
  results.push({
    registry: "PyPI",
    name: pythonPackage,
    present: Object.hasOwn(pypiBody.releases ?? {}, version),
  });
  for (const name of rustCrates) {
    const response = await fetch(`https://crates.io/api/v1/crates/${name}`, {
      headers: { "User-Agent": "DeviceRail release script" },
    });
    const body = response.ok ? await response.json() : { versions: [] };
    results.push({
      registry: "crates.io",
      name,
      present: (body.versions ?? []).some((entry) => entry.num === version),
    });
  }
  return results;
}

function report(results, version) {
  for (const result of results) {
    process.stdout.write(
      `  ${result.present ? "published" : "missing  "}  ${result.registry.padEnd(9)}  ${result.name}\n`,
    );
  }
  const missing = new Set(
    results.filter((result) => !result.present).map((result) => result.registry),
  );
  if (missing.size === 0) {
    console.log(`every public package is published at ${version}`);
    return;
  }
  const ecosystems = {
    npm: "publish_npm",
    PyPI: "publish_pypi",
    "crates.io": "publish_crates",
  };
  process.stdout.write("\nretry the missing ecosystems against the release tag:\n");
  const flags = Object.entries(ecosystems)
    .map(([registry, input]) => `-f ${input}=${missing.has(registry) ? "true" : "false"}`)
    .join(" ");
  console.log(
    `  gh workflow run publish-packages.yml --ref v${version} -f release_tag=v${version} ${flags}`,
  );
}

async function prepare(version) {
  requireCleanTree();
  requireAbsentTag(`v${version}`);
  const previous = await currentVersion();
  if (!increases(version, previous)) {
    throw new Error(`${version} must be greater than the current version ${previous}`);
  }
  const alreadyPublished = (await published(version)).filter((result) => result.present);
  if (alreadyPublished.length > 0) {
    const names = alreadyPublished.map((result) => result.name).join(", ");
    throw new Error(`${version} is already published and immutable for ${names}`);
  }

  for (const path of packageManifests) {
    await rewrite(path, (source) =>
      replaceOnce(
        source,
        /^ {2}"version": "[^"]+",$/mu,
        `  "version": "${version}",`,
        path,
      ),
    );
  }
  await rewrite("Cargo.toml", (source) =>
    replaceOnce(source, /^version = "[^"]+"$/mu, `version = "${version}"`, "Cargo.toml"),
  );
  await rewrite("crates/client/Cargo.toml", (source) =>
    replaceEach(
      source,
      /(^devicerail-protocol = \{[^}\n]*\bversion = ")[^"]+(")/gmu,
      `$1${version}$2`,
      2,
      "crates/client/Cargo.toml",
    ),
  );
  await rewrite("packages/python-client/pyproject.toml", (source) =>
    replaceOnce(
      source,
      /^version = "[^"]+"$/mu,
      `version = "${version}"`,
      "packages/python-client/pyproject.toml",
    ),
  );
  await rewrite("packages/python-client/src/devicerail/__init__.py", (source) =>
    replaceOnce(
      source,
      /^__version__ = "[^"]+"$/mu,
      `__version__ = "${version}"`,
      "packages/python-client/src/devicerail/__init__.py",
    ),
  );
  await rewrite("packages/python-client/src/devicerail/client.py", (source) =>
    replaceOnce(
      source,
      /client_version: str = "[^"]+"/u,
      `client_version: str = "${version}"`,
      "packages/python-client/src/devicerail/client.py",
    ),
  );
  await rewrite("CHANGELOG.md", (source) =>
    replaceOnce(
      source,
      /^## \[Unreleased\]$/mu,
      `## [Unreleased]\n\n## [${version}] - ${new Date().toISOString().slice(0, 10)}`,
      "CHANGELOG.md",
    ),
  );

  run("cargo", ["metadata", "--format-version", "1", "--quiet"]);
  run("node", ["scripts/check-release-version.mjs", `v${version}`]);

  const changed = run("git", ["diff", "--name-only"]).trim().split("\n");
  process.stdout.write(`${changed.map((path) => `  ${path}`).join("\n")}\n`);
  console.log(
    `prepared ${version} across ${String(changed.length)} files; describe the release in CHANGELOG.md, commit, then run: node scripts/release.mjs publish ${version}`,
  );
}

async function publish(version) {
  requireCleanTree();
  requireAbsentTag(`v${version}`);
  const current = await currentVersion();
  if (current !== version) {
    throw new Error(`the committed version is ${current}, not ${version}`);
  }
  run("node", ["scripts/check-release-version.mjs", `v${version}`]);
  const alreadyPublished = (await published(version)).filter((result) => result.present);
  if (alreadyPublished.length > 0) {
    const names = alreadyPublished.map((result) => result.name).join(", ");
    throw new Error(`${version} is already published and immutable for ${names}`);
  }
  run("git", ["tag", "--annotate", `v${version}`, "--message", `v${version}`]);
  run("git", ["push", "origin", `v${version}`]);
  console.log(
    `pushed v${version}; publish-packages.yml now needs an approval on each of the crates-io, npm, and pypi environments`,
  );
}

async function status(version) {
  const results = await published(version);
  report(results, version);
}

const command = process.argv[2];
const version = process.argv[3] ?? (await currentVersion());

if (!stableVersion.test(version)) {
  throw new Error("release version must be a stable semantic version such as 0.1.0");
}

if (command === "prepare") {
  await prepare(version);
} else if (command === "publish") {
  await publish(version);
} else if (command === "status") {
  await status(version);
} else {
  throw new Error("command must be prepare, publish, or status");
}
