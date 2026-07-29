import { spawnSync } from "node:child_process";
import {
  cp,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  realpath,
  rename,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const workspace = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const NODE_TYPES_VERSION = "26.1.1";
const JS_YAML_VERSION = "5.2.1";

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

function runPnpm(args, cwd = workspace) {
  const pnpmScript = process.env.npm_execpath;
  if (pnpmScript) {
    return run(process.execPath, [pnpmScript, ...args], cwd);
  }
  return run(process.platform === "win32" ? "pnpm.cmd" : "pnpm", args, cwd);
}

async function extract(tarball, destination) {
  const staging = `${destination}-staging`;
  await mkdir(staging, { recursive: true });
  run("tar", ["-xzf", tarball, "-C", staging]);
  await rename(join(staging, "package"), destination);
  await rm(staging, { force: true, recursive: true });
}

async function packageJson(directory) {
  return JSON.parse(await readFile(join(directory, "package.json"), "utf8"));
}

const temporary = await mkdtemp(join(tmpdir(), "devicerail-pack-"));
try {
  const tarballs = join(temporary, "tarballs");
  await mkdir(tarballs, { recursive: true });
  runPnpm(["pack", "--pack-destination", tarballs], join(workspace, "packages/protocol"));
  runPnpm(["pack", "--pack-destination", tarballs], join(workspace, "packages/client"));
  runPnpm(
    ["pack", "--pack-destination", tarballs],
    join(workspace, "packages/tool-adapter"),
  );
  runPnpm(["pack", "--pack-destination", tarballs], join(workspace, "packages/recorder"));
  runPnpm(
    ["pack", "--pack-destination", tarballs],
    join(workspace, "packages/live-visualizer"),
  );
  runPnpm(
    ["pack", "--pack-destination", tarballs],
    join(workspace, "packages/yaml-adapter"),
  );

  const packed = (await readdir(tarballs)).filter((name) => name.endsWith(".tgz"));
  const protocolTarball = packed.find((name) => name.includes("protocol"));
  const clientTarball = packed.find((name) => name.includes("client"));
  const toolAdapterTarball = packed.find((name) => name.includes("tool-adapter"));
  const recorderTarball = packed.find((name) => name.includes("recorder"));
  const liveVisualizerTarball = packed.find((name) => name.includes("live-visualizer"));
  const yamlAdapterTarball = packed.find((name) => name.includes("yaml-adapter"));
  if (
    !protocolTarball ||
    !clientTarball ||
    !toolAdapterTarball ||
    !recorderTarball ||
    !liveVisualizerTarball ||
    !yamlAdapterTarball ||
    packed.length !== 6
  ) {
    throw new Error(
      `expected protocol, client, tool-adapter, recorder, live-visualizer, and yaml-adapter tarballs, found: ${packed.join(", ")}`,
    );
  }

  const consumer = join(temporary, "consumer");
  const scope = join(consumer, "node_modules", "@devicerail");
  const protocolPackage = join(scope, "protocol");
  const clientPackage = join(scope, "client");
  const toolAdapterPackage = join(scope, "tool-adapter");
  const recorderPackage = join(scope, "recorder");
  const liveVisualizerPackage = join(scope, "live-visualizer");
  const yamlAdapterPackage = join(scope, "yaml-adapter");
  await mkdir(scope, { recursive: true });
  await extract(join(tarballs, protocolTarball), protocolPackage);
  await extract(join(tarballs, clientTarball), clientPackage);
  await extract(join(tarballs, toolAdapterTarball), toolAdapterPackage);
  await extract(join(tarballs, recorderTarball), recorderPackage);
  await extract(join(tarballs, liveVisualizerTarball), liveVisualizerPackage);
  await extract(join(tarballs, yamlAdapterTarball), yamlAdapterPackage);

  const protocolManifest = await packageJson(protocolPackage);
  const clientManifest = await packageJson(clientPackage);
  const toolAdapterManifest = await packageJson(toolAdapterPackage);
  const recorderManifest = await packageJson(recorderPackage);
  const liveVisualizerManifest = await packageJson(liveVisualizerPackage);
  const yamlAdapterManifest = await packageJson(yamlAdapterPackage);
  const expectedLicense = await readFile(join(workspace, "LICENSE"), "utf8");
  for (const [manifest, directory] of [
    [protocolManifest, protocolPackage],
    [clientManifest, clientPackage],
    [toolAdapterManifest, toolAdapterPackage],
    [recorderManifest, recorderPackage],
    [liveVisualizerManifest, liveVisualizerPackage],
    [yamlAdapterManifest, yamlAdapterPackage],
  ]) {
    if (manifest.license !== "Apache-2.0") {
      throw new Error(`${String(manifest.name)} must declare Apache-2.0`);
    }
    if (manifest.publishConfig?.registry !== "https://registry.npmjs.org/") {
      throw new Error(`${String(manifest.name)} must publish only to npmjs.org`);
    }
    const packedLicense = await readFile(join(directory, "LICENSE"), "utf8");
    if (packedLicense !== expectedLicense) {
      throw new Error(`${String(manifest.name)} must include the canonical LICENSE`);
    }
  }
  if (
    protocolManifest.private === true ||
    clientManifest.private === true ||
    toolAdapterManifest.private === true ||
    recorderManifest.private === true ||
    liveVisualizerManifest.private === true ||
    yamlAdapterManifest.private === true
  ) {
    throw new Error("packed TypeScript packages must be publishable");
  }
  const protocolDependency = clientManifest.dependencies?.["@devicerail/protocol"];
  if (
    protocolDependency !== protocolManifest.version ||
    protocolDependency.startsWith("workspace:")
  ) {
    throw new Error(`packed client has invalid protocol dependency ${String(protocolDependency)}`);
  }
  if (clientManifest.dependencies?.["@types/node"] !== NODE_TYPES_VERSION) {
    throw new Error("packed client must carry its public Node.js declarations");
  }
  if (clientManifest.dependencies?.ajv !== "8.20.0") {
    throw new Error("packed client must pin its runtime JSON Schema validator");
  }
  if (clientManifest.engines?.node !== ">=22") {
    throw new Error("packed client must declare its Node.js 22 runtime floor");
  }
  const clientDependencies = Object.keys(clientManifest.dependencies ?? {}).sort();
  const allowedClientDependencies = ["@devicerail/protocol", "@types/node", "ajv"].sort();
  if (JSON.stringify(clientDependencies) !== JSON.stringify(allowedClientDependencies)) {
    throw new Error(
      `packed client has non-allowlisted runtime dependencies: ${clientDependencies.join(", ")}`,
    );
  }
  const adapterClientDependency =
    toolAdapterManifest.dependencies?.["@devicerail/client"];
  if (
    adapterClientDependency !== clientManifest.version ||
    adapterClientDependency.startsWith("workspace:")
  ) {
    throw new Error(
      `packed tool adapter has invalid client dependency ${String(adapterClientDependency)}`,
    );
  }
  const adapterProtocolDependency =
    toolAdapterManifest.dependencies?.["@devicerail/protocol"];
  if (
    adapterProtocolDependency !== protocolManifest.version ||
    adapterProtocolDependency.startsWith("workspace:")
  ) {
    throw new Error(
      `packed tool adapter has invalid protocol dependency ${String(adapterProtocolDependency)}`,
    );
  }
  if (toolAdapterManifest.dependencies?.["@types/node"] !== NODE_TYPES_VERSION) {
    throw new Error("packed tool adapter must carry its public Node.js declarations");
  }
  if (toolAdapterManifest.engines?.node !== ">=22") {
    throw new Error("packed tool adapter must declare its Node.js 22 runtime floor");
  }
  if (toolAdapterManifest.publishConfig?.access !== "public") {
    throw new Error("packed tool adapter must be configured for public publication");
  }
  const adapterDependencies = Object.keys(toolAdapterManifest.dependencies ?? {}).sort();
  const allowedAdapterDependencies = [
    "@devicerail/client",
    "@devicerail/protocol",
    "@types/node",
  ].sort();
  if (JSON.stringify(adapterDependencies) !== JSON.stringify(allowedAdapterDependencies)) {
    throw new Error(
      `packed tool adapter has non-allowlisted runtime dependencies: ${adapterDependencies.join(", ")}`,
    );
  }
  const adapterDevDependencies = Object.keys(toolAdapterManifest.devDependencies ?? {}).sort();
  if (JSON.stringify(adapterDevDependencies) !== JSON.stringify(["typescript"])) {
    throw new Error(
      `packed tool adapter has non-allowlisted development dependencies: ${adapterDevDependencies.join(", ")}`,
    );
  }

  const recorderClientDependency = recorderManifest.dependencies?.["@devicerail/client"];
  if (
    recorderClientDependency !== clientManifest.version ||
    recorderClientDependency.startsWith("workspace:")
  ) {
    throw new Error(
      `packed recorder has invalid client dependency ${String(recorderClientDependency)}`,
    );
  }
  const recorderProtocolDependency = recorderManifest.dependencies?.["@devicerail/protocol"];
  if (
    recorderProtocolDependency !== protocolManifest.version ||
    recorderProtocolDependency.startsWith("workspace:")
  ) {
    throw new Error(
      `packed recorder has invalid protocol dependency ${String(recorderProtocolDependency)}`,
    );
  }
  if (recorderManifest.dependencies?.["@types/node"] !== NODE_TYPES_VERSION) {
    throw new Error("packed recorder must carry its public Node.js declarations");
  }
  if (recorderManifest.engines?.node !== ">=22") {
    throw new Error("packed recorder must declare its Node.js 22 runtime floor");
  }
  if (recorderManifest.publishConfig?.access !== "public") {
    throw new Error("packed recorder must be configured for public publication");
  }
  const recorderDependencies = Object.keys(recorderManifest.dependencies ?? {}).sort();
  const allowedRecorderDependencies = [
    "@devicerail/client",
    "@devicerail/protocol",
    "@types/node",
  ].sort();
  if (JSON.stringify(recorderDependencies) !== JSON.stringify(allowedRecorderDependencies)) {
    throw new Error(
      `packed recorder has non-allowlisted runtime dependencies: ${recorderDependencies.join(", ")}`,
    );
  }
  const recorderDevDependencies = Object.keys(recorderManifest.devDependencies ?? {}).sort();
  if (JSON.stringify(recorderDevDependencies) !== JSON.stringify(["typescript"])) {
    throw new Error(
      `packed recorder has non-allowlisted development dependencies: ${recorderDevDependencies.join(", ")}`,
    );
  }

  const liveVisualizerProtocolDependency =
    liveVisualizerManifest.dependencies?.["@devicerail/protocol"];
  if (
    liveVisualizerProtocolDependency !== protocolManifest.version ||
    liveVisualizerProtocolDependency.startsWith("workspace:")
  ) {
    throw new Error(
      `packed live visualizer has invalid protocol dependency ${String(liveVisualizerProtocolDependency)}`,
    );
  }
  if (liveVisualizerManifest.devDependencies?.["@types/node"] !== NODE_TYPES_VERSION) {
    throw new Error("live visualizer development must pin its Node.js declarations");
  }
  if (liveVisualizerManifest.engines?.node !== ">=22") {
    throw new Error("packed live visualizer must declare its Node.js 22 runtime floor");
  }
  if (liveVisualizerManifest.publishConfig?.access !== "public") {
    throw new Error("packed live visualizer must be configured for public publication");
  }
  const liveVisualizerDependencies = Object.keys(
    liveVisualizerManifest.dependencies ?? {},
  ).sort();
  const allowedLiveVisualizerDependencies = ["@devicerail/protocol"];
  if (
    JSON.stringify(liveVisualizerDependencies) !==
    JSON.stringify(allowedLiveVisualizerDependencies)
  ) {
    throw new Error(
      `packed live visualizer has non-allowlisted runtime dependencies: ${liveVisualizerDependencies.join(", ")}`,
    );
  }
  const liveVisualizerDevDependencies = Object.keys(
    liveVisualizerManifest.devDependencies ?? {},
  ).sort();
  if (
    JSON.stringify(liveVisualizerDevDependencies) !==
    JSON.stringify(["@types/node", "typescript"])
  ) {
    throw new Error(
      `packed live visualizer has non-allowlisted development dependencies: ${liveVisualizerDevDependencies.join(", ")}`,
    );
  }

  const yamlAdapterClientDependency =
    yamlAdapterManifest.dependencies?.["@devicerail/client"];
  if (
    yamlAdapterClientDependency !== clientManifest.version ||
    yamlAdapterClientDependency.startsWith("workspace:")
  ) {
    throw new Error(
      `packed YAML adapter has invalid client dependency ${String(yamlAdapterClientDependency)}`,
    );
  }
  if (yamlAdapterManifest.dependencies?.["js-yaml"] !== JS_YAML_VERSION) {
    throw new Error("packed YAML adapter must pin its parser runtime");
  }
  if (yamlAdapterManifest.devDependencies?.["@types/node"] !== NODE_TYPES_VERSION) {
    throw new Error("YAML adapter development must pin its Node.js declarations");
  }
  if (yamlAdapterManifest.engines?.node !== ">=22") {
    throw new Error("packed YAML adapter must declare its Node.js 22 runtime floor");
  }
  if (yamlAdapterManifest.publishConfig?.access !== "public") {
    throw new Error("packed YAML adapter must be configured for public publication");
  }
  const yamlAdapterDependencies = Object.keys(
    yamlAdapterManifest.dependencies ?? {},
  ).sort();
  const allowedYamlAdapterDependencies = ["@devicerail/client", "js-yaml"];
  if (
    JSON.stringify(yamlAdapterDependencies) !==
    JSON.stringify(allowedYamlAdapterDependencies)
  ) {
    throw new Error(
      `packed YAML adapter has non-allowlisted runtime dependencies: ${yamlAdapterDependencies.join(", ")}`,
    );
  }
  const yamlAdapterDevDependencies = Object.keys(
    yamlAdapterManifest.devDependencies ?? {},
  ).sort();
  if (
    JSON.stringify(yamlAdapterDevDependencies) !==
    JSON.stringify(["@types/node", "typescript"])
  ) {
    throw new Error(
      `packed YAML adapter has non-allowlisted development dependencies: ${yamlAdapterDevDependencies.join(", ")}`,
    );
  }

  const nodeTypes = await realpath(join(workspace, "packages/client/node_modules/@types/node"));
  const nodeRequire = createRequire(join(nodeTypes, "package.json"));
  const undiciTypes = dirname(nodeRequire.resolve("undici-types/package.json"));
  await mkdir(join(consumer, "node_modules", "@types"), { recursive: true });
  await cp(nodeTypes, join(consumer, "node_modules", "@types", "node"), {
    dereference: true,
    recursive: true,
  });
  await cp(undiciTypes, join(consumer, "node_modules", "undici-types"), {
    dereference: true,
    recursive: true,
  });
  const jsYaml = await realpath(
    join(workspace, "packages/yaml-adapter/node_modules/js-yaml"),
  );
  const jsYamlRequire = createRequire(join(jsYaml, "package.json"));
  const argparse = dirname(jsYamlRequire.resolve("argparse/package.json"));
  await cp(jsYaml, join(consumer, "node_modules", "js-yaml"), {
    dereference: true,
    recursive: true,
  });
  await cp(argparse, join(consumer, "node_modules", "argparse"), {
    dereference: true,
    recursive: true,
  });
  const ajv = await realpath(join(workspace, "packages/client/node_modules/ajv"));
  const ajvRequire = createRequire(join(ajv, "package.json"));
  await cp(ajv, join(consumer, "node_modules", "ajv"), {
    dereference: true,
    recursive: true,
  });
  for (const dependency of [
    "fast-deep-equal",
    "fast-uri",
    "json-schema-traverse",
    "require-from-string",
  ]) {
    const dependencyDirectory = dirname(ajvRequire.resolve(`${dependency}/package.json`));
    await cp(dependencyDirectory, join(consumer, "node_modules", dependency), {
      dereference: true,
      recursive: true,
    });
  }

  await writeFile(
    join(consumer, "package.json"),
    JSON.stringify({ name: "devicerail-packed-consumer", private: true, type: "module" }),
  );
  await writeFile(
    join(consumer, "tsconfig.json"),
    JSON.stringify({
      compilerOptions: {
        exactOptionalPropertyTypes: true,
        module: "NodeNext",
        moduleResolution: "NodeNext",
        noEmit: true,
        strict: true,
        target: "ES2022",
        types: ["node"],
      },
      include: ["index.ts"],
    }),
  );
  await writeFile(
    join(consumer, "index.ts"),
    [
      'import { DeviceRailClient, validateRpcResult, type ClientTransport } from "@devicerail/client";',
      'import { ExecutionRecorder, RecorderError, type RecorderCheckpoint } from "@devicerail/recorder";',
      'import { LiveTimeline, type LiveTimelineState } from "@devicerail/live-visualizer";',
      'import { DeviceRailToolAdapter, actionToolName, type DeviceRailToolCatalog } from "@devicerail/tool-adapter";',
      'import { compileYamlPlan, executeYamlPlan, type YamlPlan } from "@devicerail/yaml-adapter";',
      'import type { RpcMethod } from "@devicerail/protocol";',
      'const method: RpcMethod = "system.describe";',
      'const mediaStartMethod: RpcMethod = "media.stream.start";',
      'const mediaCaptureMethod: RpcMethod = "media.stream.capture";',
      'const mediaEndMethod: RpcMethod = "media.stream.end";',
      "const constructor: typeof DeviceRailClient = DeviceRailClient;",
      "const recorderConstructor: typeof ExecutionRecorder = ExecutionRecorder;",
      "const recorderErrorConstructor: typeof RecorderError = RecorderError;",
      "const timelineConstructor: typeof LiveTimeline = LiveTimeline;",
      "const adapterConstructor: typeof DeviceRailToolAdapter = DeviceRailToolAdapter;",
      'const toolName: string = actionToolName("tap");',
      "declare const transport: ClientTransport;",
      "declare const client: DeviceRailClient;",
      "declare const catalog: DeviceRailToolCatalog;",
      "declare const checkpoint: RecorderCheckpoint;",
      "declare const timelineState: LiveTimelineState;",
      "declare const yamlPlan: YamlPlan;",
      'const streamId = "00000000-0000-4000-8000-000000000001";',
      'const mediaStart = client.call("media.stream.start", { kind: "screenshot", streamId });',
      'const mediaCapture = client.call("media.stream.capture", { frameIndex: 1, streamId }, { timeoutMs: 1_000 });',
      'const mediaEnd = client.call("media.stream.end", { streamId });',
      'validateRpcResult("device.capabilities", []);',
      "void method; void mediaStartMethod; void mediaCaptureMethod; void mediaEndMethod; void mediaStart; void mediaCapture; void mediaEnd; void constructor; void recorderConstructor; void recorderErrorConstructor; void timelineConstructor; void adapterConstructor; void toolName; void transport; void catalog; void checkpoint; void timelineState; void yamlPlan; void compileYamlPlan; void executeYamlPlan;",
    ].join("\n"),
  );

  const tsc = await realpath(
    join(workspace, "packages/client/node_modules/typescript/lib/tsc.js"),
  );
  run(process.execPath, [tsc, "--project", "tsconfig.json"], consumer);
  run(
    process.execPath,
    [
      "--input-type=module",
      "--eval",
      'const [client, recorder, liveVisualizer, toolAdapter, yamlAdapter] = await Promise.all([import("@devicerail/client"), import("@devicerail/recorder"), import("@devicerail/live-visualizer"), import("@devicerail/tool-adapter"), import("@devicerail/yaml-adapter")]); const plan = yamlAdapter.compileYamlPlan("version: devicerail/v1\\nsteps:\\n  - id: list\\n    method: devices.list\\n"); client.validateRpcResult("device.capabilities", []); let rejected = false; try { client.validateRpcResult("system.describe", { connection: {}, client: {}, unknown: true }); } catch (error) { rejected = error instanceof client.ProtocolViolationError; } if (!rejected || typeof client.DeviceRailClient !== "function" || typeof recorder.ExecutionRecorder !== "function" || typeof recorder.RecorderError !== "function" || typeof liveVisualizer.LiveTimeline !== "function" || typeof toolAdapter.DeviceRailToolAdapter !== "function" || toolAdapter.actionToolName("tap") !== "devicerail_action_raw_tap" || plan.steps[0]?.method !== "devices.list") process.exit(1);',
    ],
    consumer,
  );
  process.stdout.write(
    "packed protocol, client, tool-adapter, recorder, live-visualizer, and yaml-adapter packages passed isolated consumer checks\n",
  );
} finally {
  await rm(temporary, { force: true, recursive: true });
}
