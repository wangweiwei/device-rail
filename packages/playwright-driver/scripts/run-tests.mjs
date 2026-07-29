import { readdir } from "node:fs/promises";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = new URL("../.test-dist/test/", import.meta.url);
const tests = (await readdir(root))
  .filter((name) => name.endsWith(".test.js"))
  .sort()
  .map((name) => fileURLToPath(new URL(name, root)));
if (tests.length === 0) throw new Error("no compiled tests found");
const child = spawn(process.execPath, ["--test", ...tests], { stdio: "inherit" });
const code = await new Promise((resolve, reject) => {
  child.once("error", reject);
  child.once("close", resolve);
});
process.exitCode = typeof code === "number" ? code : 1;
