import { spawnSync } from "node:child_process";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const workspace = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const packageDirectories = [
  "packages/protocol",
  "packages/client",
  "packages/tool-adapter",
  "packages/recorder",
  "packages/live-visualizer",
  "packages/yaml-adapter",
];

function option(name) {
  const index = process.argv.indexOf(name);
  if (index === -1 || !process.argv[index + 1]) {
    throw new Error(`missing required ${name} option`);
  }
  return process.argv[index + 1];
}

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

function runPnpm(args, cwd) {
  const pnpmScript = process.env.npm_execpath;
  if (pnpmScript) {
    return run(process.execPath, [pnpmScript, ...args], cwd);
  }
  return run(process.platform === "win32" ? "pnpm.cmd" : "pnpm", args, cwd);
}

const repositoryUrl = option("--repository-url");
if (!/^git\+https:\/\/github\.com\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\.git$/.test(repositoryUrl)) {
  throw new Error(
    "--repository-url must look like git+https://github.com/owner/repository.git",
  );
}
const webUrl = repositoryUrl.slice("git+".length, -".git".length);
const output = resolve(workspace, option("--output"));
await mkdir(output, { recursive: true });
const existing = (await readdir(output)).filter((name) => name.endsWith(".tgz"));
if (existing.length !== 0) {
  throw new Error(`release output already contains tarballs: ${existing.join(", ")}`);
}

const originals = new Map();
try {
  for (const directory of packageDirectories) {
    const manifestPath = resolve(workspace, directory, "package.json");
    const source = await readFile(manifestPath, "utf8");
    originals.set(manifestPath, source);
    const manifest = JSON.parse(source);
    manifest.repository = {
      type: "git",
      url: repositoryUrl,
      directory,
    };
    manifest.homepage = `${webUrl}#readme`;
    manifest.bugs = { url: `${webUrl}/issues` };
    await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
  }

  for (const directory of packageDirectories) {
    runPnpm(["pack", "--pack-destination", output], resolve(workspace, directory));
  }
} finally {
  for (const [manifestPath, source] of originals) {
    await writeFile(manifestPath, source, "utf8");
  }
}

const packed = (await readdir(output)).filter((name) => name.endsWith(".tgz")).sort();
if (packed.length !== packageDirectories.length) {
  throw new Error(`expected ${packageDirectories.length} tarballs, found ${packed.length}`);
}

for (const tarball of packed) {
  const manifestSource = run("tar", ["-xOf", resolve(output, tarball), "package/package.json"]);
  const manifest = JSON.parse(manifestSource);
  if (manifest.repository?.url !== repositoryUrl) {
    throw new Error(`${tarball} has an incorrect repository URL`);
  }
  if (
    manifest.publishConfig?.access !== "public" ||
    manifest.publishConfig?.registry !== "https://registry.npmjs.org/" ||
    manifest.private === true
  ) {
    throw new Error(`${tarball} is not configured as a public package`);
  }
  for (const dependency of Object.values(manifest.dependencies ?? {})) {
    if (typeof dependency === "string" && dependency.startsWith("workspace:")) {
      throw new Error(`${tarball} retains a workspace dependency`);
    }
  }
}

console.log(`packed ${packed.length} npm release tarballs for ${repositoryUrl}`);
