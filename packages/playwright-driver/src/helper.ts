import { SystemPlaywrightPort } from "./playwright-port.js";
import { serveBridge } from "./server.js";

try {
  await serveBridge(process.stdin, process.stdout, new SystemPlaywrightPort());
  // Playwright deliberately has no public "disconnect without closing the
  // operator-owned browser" API. Exiting after stdin EOF releases only this
  // helper's transport and avoids sending Browser.close to the remote server.
  process.exit(0);
} catch {
  process.exit(1);
}
