import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

const packageRoot = resolve(import.meta.dirname, "..");
const manifest = JSON.parse(await readFile(resolve(packageRoot, "package.json"), "utf8"));
const module = await import(manifest.name);
if (
  module === null ||
  typeof module !== "object" ||
  typeof module.LiveTimeline !== "function" ||
  typeof module.LiveTimelineError !== "function" ||
  typeof module.normalizeLiveTimelineLimits !== "function"
) {
  throw new Error(`${manifest.name} did not expose its required runtime API`);
}
