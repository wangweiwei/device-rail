import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

const packageRoot = resolve(import.meta.dirname, "..");
const manifest = JSON.parse(await readFile(resolve(packageRoot, "package.json"), "utf8"));
if (typeof manifest.name !== "string" || manifest.name.length === 0) {
  throw new Error("package manifest must contain a non-empty name");
}

const module = await import(manifest.name);
if (
  module === null ||
  typeof module !== "object" ||
  typeof module.DeviceRailToolAdapter !== "function" ||
  typeof module.actionToolName !== "function" ||
  typeof module.OBSERVATION_TOOL_NAME !== "string"
) {
  throw new Error(`${manifest.name} did not expose its required runtime API`);
}
