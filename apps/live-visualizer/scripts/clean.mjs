import { rm } from "node:fs/promises";

const target = process.argv[2];
if (target !== "dist" && target !== ".test-dist") {
  throw new Error("clean target must be dist or .test-dist");
}
await rm(new URL(`../${target}`, import.meta.url), { force: true, recursive: true });
