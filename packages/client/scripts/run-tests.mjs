import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { spawn } from "node:child_process";

async function testFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await testFiles(path)));
    } else if (entry.isFile() && entry.name.endsWith(".test.js")) {
      files.push(path);
    }
  }
  return files;
}

const files = (await testFiles(".test-dist/test")).sort();
if (files.length === 0) {
  throw new Error("no compiled client tests were found");
}

const child = spawn(process.execPath, ["--test", ...files], {
  stdio: "inherit",
  windowsHide: true,
});

const exitCode = await new Promise((resolve, reject) => {
  child.once("error", reject);
  child.once("exit", (code, signal) => {
    if (signal) {
      reject(new Error(`test process exited from signal ${signal}`));
      return;
    }
    resolve(code ?? 1);
  });
});

process.exitCode = exitCode;
