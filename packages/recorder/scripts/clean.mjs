import { rm } from "node:fs/promises";
import { resolve } from "node:path";

const allowed = new Set(["dist", ".test-dist"]);
const target = process.argv[2];
if (!target || !allowed.has(target)) {
  throw new Error(`clean target must be one of: ${[...allowed].join(", ")}`);
}

await rm(resolve(import.meta.dirname, "..", target), {
  force: true,
  recursive: true,
});
