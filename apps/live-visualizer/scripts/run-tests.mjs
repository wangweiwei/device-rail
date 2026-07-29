import { readdir } from "node:fs/promises";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = new URL("../.test-dist/test/", import.meta.url);

async function collect(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const child = new URL(`${entry.name}${entry.isDirectory() ? "/" : ""}`, directory);
    if (entry.isDirectory()) files.push(...(await collect(child)));
    else if (entry.isFile() && entry.name.endsWith(".test.js")) files.push(fileURLToPath(child));
  }
  return files;
}

const tests = (await collect(root)).sort();
if (tests.length === 0) throw new Error("no compiled tests found");
const child = spawn(process.execPath, ["--test", ...tests], { stdio: "inherit" });
const code = await new Promise((resolve, reject) => {
  child.once("error", reject);
  child.once("close", resolve);
});
process.exitCode = typeof code === "number" ? code : 1;
