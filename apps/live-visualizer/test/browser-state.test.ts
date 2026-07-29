import assert from "node:assert/strict";
import test from "node:test";

import { APP_JS, documentHtml } from "../src/assets.js";
import {
  browserViewFromUrl,
  browserViewQuery,
  reduceBrowserView,
} from "../src/browser-state.js";

test("browser reducer follows only the newest page and preserves explicit URL state", () => {
  let state = browserViewFromUrl(
    new URL("http://127.0.0.1/view/?filter=actions&page=3&follow=1"),
  );
  assert.deepEqual(
    { filter: state.filter, follow: state.follow, page: state.page },
    { filter: "actions", follow: true, page: 3 },
  );
  state = reduceBrowserView(state, { revision: 4, type: "revision" });
  assert.equal(state.shouldRefresh, false, "a historical page must not be stolen by live updates");
  assert.equal(state.pendingRevision, 4);
  state = reduceBrowserView(state, { page: 5, type: "page" });
  state = reduceBrowserView(state, { revision: 3, totalPages: 5, type: "refreshed" });
  state = reduceBrowserView(state, { follow: true, type: "follow" });
  assert.equal(state.shouldRefresh, true);
  state = reduceBrowserView(state, { revision: 3, totalPages: 5, type: "refreshed" });
  assert.equal(state.revision, 3);
  assert.equal(state.pendingRevision, 4, "an SSE revision racing a fetch must not be lost");
  assert.equal(state.shouldRefresh, true);
  assert.equal(browserViewQuery(state), "filter=actions&follow=1&page=5");
});

test("browser asset has no executable content sink and caps rendered timeline entries", () => {
  for (const forbidden of ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "eval("]) {
    assert.equal(APP_JS.includes(forbidden), false, `browser script must not contain ${forbidden}`);
  }
  assert.match(APP_JS, /document\.createElement/u);
  assert.match(APP_JS, /\.textContent\s*=/u);
  assert.match(APP_JS, /slice\(0, MAX_ITEMS\)/u);
  assert.match(APP_JS, /new URLSearchParams/u);
  assert.match(APP_JS, /page === pageCount/u);
  assert.match(APP_JS, /Math\.max\(pendingRevision, stateRevision\)/u);
  assert.match(APP_JS, /queueMicrotask/u);

  const capability = "a".repeat(64);
  const html = documentHtml(capability);
  assert.match(html, /class="skip-link"/u);
  assert.match(html, /aria-live="polite"/u);
  assert.match(html, new RegExp(`src="/${capability}/app\\.js"`, "u"));
  assert.doesNotMatch(html, /<script(?![^>]*\bsrc=)[^>]*>/u);
});
