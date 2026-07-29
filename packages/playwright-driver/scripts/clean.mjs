import { rm } from "node:fs/promises";

const target = process.argv[2];
if (!target || target.includes("/") || target.includes("\\") || target === "." || target === "..") {
  throw new Error("clean target must be one local directory name");
}
await rm(new URL(`../${target}/`, import.meta.url), { force: true, recursive: true });
