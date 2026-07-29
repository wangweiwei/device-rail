import { lstat, rm } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const output = path.join(packageRoot, "dist");

try {
  const metadata = await lstat(output);
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new Error(`refusing to clean a non-directory or symlink: ${output}`);
  }
  await rm(output, { recursive: true });
} catch (error) {
  if (error?.code !== "ENOENT") {
    throw error;
  }
}
