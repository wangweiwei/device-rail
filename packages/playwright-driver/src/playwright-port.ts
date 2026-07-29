import {
  chromium,
  firefox,
  webkit,
  type Browser,
  type BrowserType,
  type Locator,
  type Page,
} from "playwright-core";

import type { BrowserKind } from "./types.js";
import {
  RemotePortError,
  type PlaywrightPort,
  type RemoteLocator,
  type RemotePage,
  type RemoteSession,
} from "./worker.js";

/**
 * Label→value read by GEOMETRY, evaluated in the page.
 *
 * This function is a fixed constant of the driver: only `{label, direction,
 * exact}` crosses the bridge, never code — the action must not become an
 * arbitrary-script escape hatch.
 *
 * Why geometry rather than DOM adjacency: in real label/value tables the two
 * cells routinely live in different subtrees, so parent/sibling/`following::`
 * walks land on the wrong row. Row membership is a *visual* relation.
 *
 * Fail-closed: an absent or ambiguous label, or nothing in the requested
 * direction, returns undefined rather than a guess.
 */
function readValueNearLabelInPage(
  input: { readonly label: string; readonly direction: "right" | "below"; readonly exact: boolean },
): string | undefined {
  // Every binding this function needs must live INSIDE it: Playwright ships the
  // source to the page, where the module scope does not exist — a closure
  // reference here is a ReferenceError at run time, not a compile error.
  const ownText = (element: Element): string => {
    let text = "";
    element.childNodes.forEach((node) => {
      if (node.nodeType === 3) text += node.textContent ?? "";
    });
    return text.replace(/\s+/gu, " ").trim();
  };
  const shown = (element: Element): boolean => {
    const box = element.getBoundingClientRect();
    if (box.width < 1 || box.height < 1) return false;
    const style = getComputedStyle(element);
    return style.visibility !== "hidden" && style.display !== "none";
  };
  // The value must survive the Rust output gate: strip the control characters
  // it rejects, and cap by CODE POINTS — a code-unit slice can emit a lone
  // surrogate, which serde_json refuses, failing the whole bridge response.
  const clean = (text: string): string => {
    const kept: string[] = [];
    for (const character of text) {
      const code = character.codePointAt(0) ?? 0;
      if (
        code <= 8 || code === 11 || code === 12
        || (code >= 14 && code <= 31) || (code >= 127 && code <= 159)
      ) continue;
      kept.push(character);
      if (kept.length >= 4_096) break;
    }
    return kept.join("");
  };
  // normalize BOTH sides identically, or an NBSP/whitespace-bearing label can
  // never match the whitespace-collapsed page text
  const wanted = input.label.replace(/\s+/gu, " ").trim();

  const all = Array.from(document.querySelectorAll("*"));
  const labels = all.filter((element) => {
    if (!shown(element)) return false;
    const text = ownText(element);
    return input.exact ? text === wanted : text.includes(wanted);
  });
  if (labels.length !== 1) return undefined; // absent or ambiguous → no guess
  const anchor = labels[0]!;
  const anchorBox = anchor.getBoundingClientRect();
  // The band tolerance derives from the ANCHOR alone. A per-candidate tolerance
  // lets a large unrelated element (full-width paragraph, footer) inflate its
  // own acceptance band until the row/column test is vacuous and min-gap picks
  // it — returning plausible-looking WRONG data.
  const rowTolerance = anchorBox.height * 0.6;
  const columnTolerance = anchorBox.width * 0.6;

  let best: { element: Element; gap: number } | undefined;
  let tied = false;
  for (const element of all) {
    if (element === anchor || anchor.contains(element) || element.contains(anchor)) continue;
    if (!shown(element) || ownText(element) === "") continue;
    const box = element.getBoundingClientRect();
    let inBand: boolean;
    let gap: number;
    if (input.direction === "right") {
      inBand = Math.abs((box.top + box.height / 2) - (anchorBox.top + anchorBox.height / 2)) <= rowTolerance;
      gap = box.left - anchorBox.right;
    } else {
      inBand = Math.abs((box.left + box.width / 2) - (anchorBox.left + anchorBox.width / 2)) <= columnTolerance;
      gap = box.top - anchorBox.bottom;
    }
    if (!inBand || gap < -1) continue;
    if (!best || gap < best.gap - 0.5) {
      best = { element, gap };
      tied = false;
    } else if (Math.abs(gap - best.gap) <= 0.5) {
      tied = true; // two candidates at the same distance: ambiguous, no guess
    }
  }
  if (!best || tied) return undefined;
  return clean(ownText(best.element));
}

function browserType(kind: BrowserKind): BrowserType<Browser> {
  switch (kind) {
    case "chromium":
      return chromium;
    case "firefox":
      return firefox;
    case "webkit":
      return webkit;
  }
}

function locator(value: Locator): RemoteLocator {
  return {
    click: async (options) => await value.click(options),
    count: async () => await value.count(),
    fill: async (text, options) => await value.fill(text, options),
    press: async (key, options) => await value.press(key, options),
    selectOption: async (selected, options) => await value.selectOption(selected, options),
    textContains: async (text, caseSensitive) =>
      await value.evaluate(
        (element, expected) => {
          const content = element.textContent ?? "";
          return expected.caseSensitive
            ? content.includes(expected.text)
            : content.toLowerCase().includes(expected.text.toLowerCase());
        },
        { caseSensitive, text },
      ),
    waitFor: async (options) => await value.waitFor(options),
    first: () => locator(value.first()),
  };
}

function page(value: Page): RemotePage {
  const stableGuid = (value as unknown as { readonly _guid?: unknown })._guid;
  if (typeof stableGuid !== "string" || !/^page@[0-9a-f]{32}$/u.test(stableGuid)) {
    throw new RemotePortError("page_identity_unavailable", false);
  }
  return {
    stableGuid,
    deviceScaleFactor: async () =>
      await value.evaluate(() => globalThis.devicePixelRatio),
    getByText: (text, exact) =>
      // visible-only: hidden duplicates (menus, templates, SSR remnants) must
      // neither hang a first() wait nor false-fail a uniqueness count
      locator(value.getByText(text, { exact }).filter({ visible: true })),
    goto: async (url, options) => await value.goto(url, options),
    readValueNearLabel: async (label, direction, exact, timeout) => {
      // Explicit poll loop rather than waitForFunction: that helper polls on
      // requestAnimationFrame by default, which a headless-adjacent or
      // background window throttles to a standstill. Panels also render
      // progressively — the label can mount while the cell beside it is still
      // empty — so one shot is not enough. Still no code on the wire: only
      // {label, direction, exact} crosses the bridge.
      const deadline = Date.now() + timeout;
      for (;;) {
        let found: string | undefined;
        try {
          found = await value.evaluate(readValueNearLabelInPage, { direction, exact, label });
        } catch {
          // a navigation destroys the execution context mid-poll; that is not
          // an answer — keep polling until the new document is readable
          found = undefined;
        }
        if (found !== undefined && found !== "") return found;
        if (Date.now() >= deadline) return undefined;
        try {
          await value.waitForTimeout(200);
        } catch {
          return undefined; // the page itself is gone
        }
      }
    },
    locator: (selector) => locator(value.locator(`css=${selector}`)),
    screenshot: async (options) => await value.screenshot(options),
    title: async () => await value.title(),
    url: () => value.url(),
    viewportSize: () => value.viewportSize(),
    waitForLoadState: async (state, options) => await value.waitForLoadState(state, options),
    wheel: async (deltaX, deltaY) => await value.mouse.wheel(deltaX, deltaY),
  };
}

export class SystemPlaywrightPort implements PlaywrightPort {
  #connection:
    | {
        readonly browser: Browser;
        readonly endpoint: string;
        readonly kind: BrowserKind;
      }
    | undefined;

  async connect(kind: BrowserKind, endpoint: string, timeoutMs: number): Promise<RemoteSession> {
    try {
      if (
        this.#connection
        && (this.#connection.kind !== kind || this.#connection.endpoint !== endpoint)
      ) {
        throw new RemotePortError("invalid_request", false);
      }
      if (!this.#connection || !this.#connection.browser.isConnected()) {
        this.#connection = {
          browser: await browserType(kind).connect(endpoint, { timeout: timeoutMs }),
          endpoint,
          kind,
        };
      }
      const browser = this.#connection.browser;
      let contexts = browser.contexts();
      if (contexts.length === 0) {
        const context = await browser.newContext();
        await context.newPage();
        contexts = browser.contexts();
      } else if (contexts.every((context) => context.pages().length === 0)) {
        await contexts[0]?.newPage();
      }
      return {
        contexts: contexts.map((context) => context.pages().map(page)),
      };
    } catch (error) {
      if (error instanceof RemotePortError) throw error;
      const name = error instanceof Error ? error.name : "";
      if (name === "TimeoutError") throw new RemotePortError("operation_timed_out", true);
      throw new RemotePortError("connect_failed", true);
    }
  }
}
