export const APP_CSS = String.raw`
:root {
  color-scheme: light dark;
  --background: #0b0e14;
  --surface: #121722;
  --surface-raised: #192131;
  --border: #31405a;
  --text: #f4f7fb;
  --muted: #aab6c8;
  --accent: #75b9ff;
  --danger: #ff9c9c;
  font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}

* { box-sizing: border-box; }
html { background: var(--background); color: var(--text); }
body {
  -webkit-tap-highlight-color: rgb(117 185 255 / 25%);
  margin: 0;
  min-height: 100vh;
  overflow-x: hidden;
  padding-bottom: env(safe-area-inset-bottom);
}
button, select, input { font: inherit; touch-action: manipulation; }
button, select {
  background: var(--surface-raised);
  border: 1px solid var(--border);
  border-radius: 0.5rem;
  color: var(--text);
  min-height: 2.75rem;
  padding: 0.55rem 0.8rem;
}
button:hover, select:hover { border-color: var(--accent); }
button:focus-visible, select:focus-visible, input:focus-visible, a:focus-visible {
  outline: 3px solid var(--accent);
  outline-offset: 3px;
}
button:disabled { cursor: not-allowed; opacity: 0.55; }
.skip-link {
  background: var(--text);
  color: var(--background);
  left: 1rem;
  padding: 0.75rem 1rem;
  position: fixed;
  top: 1rem;
  transform: translateY(-200%);
  z-index: 2;
}
.skip-link:focus-visible { transform: translateY(0); }
header, main {
  margin: 0 auto;
  max-width: 72rem;
  padding-block: 1rem;
  padding-left: max(1rem, env(safe-area-inset-left));
  padding-right: max(1rem, env(safe-area-inset-right));
}
h1 { font-size: clamp(1.6rem, 4vw, 2.5rem); margin-block: 0.5rem; text-wrap: balance; }
h2, h3 { scroll-margin-top: 1rem; text-wrap: balance; }
.muted { color: var(--muted); overflow-wrap: anywhere; }
.toolbar {
  align-items: end;
  display: flex;
  flex-wrap: wrap;
  gap: 0.75rem;
  margin-block: 1rem;
}
.field { display: grid; gap: 0.35rem; }
.check { align-items: center; display: flex; gap: 0.5rem; min-height: 2.75rem; }
.check input { height: 1.2rem; width: 1.2rem; }
.status {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 0.65rem;
  min-height: 3rem;
  overflow-wrap: anywhere;
  padding: 0.8rem;
}
.new-events[hidden] { display: none; }
.timeline { display: grid; gap: 0.75rem; list-style: none; margin: 1rem 0; padding: 0; }
.event-card {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 0.7rem;
  content-visibility: auto;
  overflow-wrap: anywhere;
  padding: 1rem;
}
.event-card h3 { font-size: 1rem; margin: 0 0 0.4rem; }
.event-card pre { overflow-wrap: anywhere; white-space: pre-wrap; }
.sequence { color: var(--accent); font-variant-numeric: tabular-nums; }
.evidence { color: var(--muted); font-size: 0.9rem; margin-block: 0.4rem 0; padding-left: 1.25rem; }
.error { color: var(--danger); }
.pager { align-items: center; display: flex; flex-wrap: wrap; gap: 0.75rem; justify-content: space-between; }
#page-label { font-variant-numeric: tabular-nums; }
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { scroll-behavior: auto !important; }
}
@media (prefers-color-scheme: light) {
  :root {
    --background: #f5f7fb;
    --surface: #ffffff;
    --surface-raised: #e9eef7;
    --border: #66748a;
    --text: #111827;
    --muted: #4b5563;
    --accent: #005eb8;
    --danger: #a11212;
  }
}
`;

export const APP_JS = String.raw`
(() => {
  "use strict";
  const MAX_ITEMS = 50;
  const FILTERS = new Set(["all", "observations", "actions", "errors", "verdicts"]);
  const root = document.getElementById("timeline");
  const status = document.getElementById("status");
  const filter = document.getElementById("filter");
  const follow = document.getElementById("follow");
  const previous = document.getElementById("previous");
  const next = document.getElementById("next");
  const pageLabel = document.getElementById("page-label");
  const newEvents = document.getElementById("new-events");
  if (!root || !status || !filter || !follow || !previous || !next || !pageLabel || !newEvents) return;

  const capability = location.pathname.split("/").filter(Boolean)[0];
  if (!capability || !/^[0-9a-f]{64}$/.test(capability)) return;
  const base = "/" + capability;
  const query = new URLSearchParams(location.search);
  let currentFilter = FILTERS.has(query.get("filter")) ? query.get("filter") : "all";
  let page = Number.parseInt(query.get("page") || "1", 10);
  if (!Number.isSafeInteger(page) || page < 1) page = 1;
  let pageCount = 1;
  let wantLatest = !query.has("page");
  let shouldFollow = query.get("follow") !== "0";
  let revision = 0;
  let pendingRevision = 0;
  let loading = false;
  filter.value = currentFilter;
  follow.checked = shouldFollow;

  const setStatus = (message, isError = false) => {
    status.textContent = message;
    status.classList.toggle("error", isError);
  };
  const syncUrl = () => {
    const nextQuery = new URLSearchParams();
    nextQuery.set("filter", currentFilter);
    nextQuery.set("page", String(page));
    nextQuery.set("follow", shouldFollow ? "1" : "0");
    history.replaceState(null, "", base + "/?" + nextQuery.toString());
  };
  const text = (value, fallback = "") => typeof value === "string" && value ? value : fallback;
  const boundedText = (value, fallback = "") => value && typeof value === "object" ? text(value.text, fallback) : text(value, fallback);
  const number = (value) => Number.isSafeInteger(value) ? value : null;
  const appendText = (parent, tag, value, className) => {
    if (!value) return;
    const node = document.createElement(tag);
    if (className) node.className = className;
    node.textContent = value;
    parent.append(node);
  };
  const renderEvidence = (card, evidence) => {
    if (!Array.isArray(evidence) || evidence.length === 0) return;
    const list = document.createElement("ul");
    list.className = "evidence";
    const visible = evidence.slice(0, 8);
    for (const item of visible) {
      if (!item || typeof item !== "object") continue;
      const id = boundedText(item.id, "Evidence");
      const media = boundedText(item.mediaType, "unknown media");
      const digest = boundedText(item.sha256 || item.digest);
      const line = document.createElement("li");
      line.textContent = digest ? id + " · " + media + " · " + digest : id + " · " + media;
      list.append(line);
    }
    if (evidence.length > visible.length) {
      const omitted = document.createElement("li");
      omitted.textContent = "+" + new Intl.NumberFormat().format(evidence.length - visible.length) + " more references in this event";
      list.append(omitted);
    }
    if (list.childElementCount > 0) card.append(list);
  };
  const renderEntry = (entry) => {
    const card = document.createElement("li");
    card.className = "event-card";
    if (!entry || typeof entry !== "object") {
      appendText(card, "p", "Unavailable event presentation", "error");
      return card;
    }
    const sequence = number(entry.sequence);
    const presentation = entry.presentation && typeof entry.presentation === "object" ? entry.presentation : {};
    const title = boundedText(entry.title, text(presentation.type, "Event"));
    appendText(card, "h3", (sequence === null ? "" : "#" + new Intl.NumberFormat().format(sequence) + " · ") + title, "sequence");
    appendText(card, "p", text(entry.status), "muted");
    let summary = "No additional details.";
    if (presentation.type === "sessionStarted") summary = "Session started.";
    else if (presentation.type === "sessionEnded") {
      summary = "Session ended: " + text(presentation.outcome, "unknown outcome") + ".";
      const reason = boundedText(presentation.reason);
      if (reason) summary += " Reason: " + reason;
    }
    else if (presentation.type === "observationCaptured") {
      const observation = presentation.observation || {};
      summary = "Observation " + boundedText(observation.id, "captured") + ".";
      if (observation.screenshot) renderEvidence(card, [observation.screenshot]);
      if (observation.screenshotOmission) summary += " Screenshot omitted: " + text(observation.screenshotOmission) + ".";
    } else if (presentation.type === "actionStarted") {
      summary = "Action " + boundedText(presentation.name, "started") + ".";
      if (presentation.arguments && presentation.arguments.omitted === "protected") summary += " Arguments omitted by protection policy.";
      else if (presentation.arguments) appendText(card, "pre", text(presentation.arguments.json), "muted");
    } else if (presentation.type === "actionCompleted") {
      const completion = presentation.completion || {};
      summary = "Action completed: " + text(completion.outcome, "unknown outcome") + ".";
      if (completion.error) summary += " " + boundedText(completion.error.message, "An explicit action error was recorded.");
      if (number(completion.timeoutMs) !== null) summary += " Timeout: " + new Intl.NumberFormat().format(completion.timeoutMs) + " ms.";
      renderEvidence(card, completion.evidence);
      if (number(completion.evidenceOmitted) > 0) summary += " " + new Intl.NumberFormat().format(completion.evidenceOmitted) + " evidence references omitted by the viewer limit.";
      if (completion.before && completion.before.screenshot) renderEvidence(card, [completion.before.screenshot]);
      if (completion.before && completion.before.screenshotOmission) summary += " Before screenshot omitted: " + text(completion.before.screenshotOmission) + ".";
      if (completion.after && completion.after.screenshot) renderEvidence(card, [completion.after.screenshot]);
      if (completion.after && completion.after.screenshotOmission) summary += " After screenshot omitted: " + text(completion.after.screenshotOmission) + ".";
    } else if (presentation.type === "mediaStreamStarted") {
      summary = "Media stream started: " + boundedText(presentation.streamId, "unknown stream") + " · " + text(presentation.kind, "unknown kind") + ".";
    } else if (presentation.type === "mediaFrameCaptured") {
      summary = "Media frame " + new Intl.NumberFormat().format(number(presentation.frameIndex) || 0) + " captured.";
      if (presentation.evidence) renderEvidence(card, [presentation.evidence]);
    } else if (presentation.type === "mediaStreamEnded") {
      summary = "Media stream ended after " + new Intl.NumberFormat().format(number(presentation.frameCount) || 0) + " frames.";
    } else if (presentation.type === "error") {
      summary = boundedText(presentation.error && presentation.error.message, "An explicit error was recorded.");
    } else if (presentation.type === "verdictRecorded") {
      summary = text(presentation.status, "unknown verdict") + ": " + boundedText(presentation.summary, "No verdict summary.");
      renderEvidence(card, presentation.evidence);
      if (number(presentation.evidenceOmitted) > 0) summary += " " + new Intl.NumberFormat().format(presentation.evidenceOmitted) + " evidence references omitted by the viewer limit.";
    }
    appendText(card, "p", summary);
    const atMs = number(entry.atMs);
    if (atMs !== null) appendText(card, "p", "Timestamp: " + new Intl.NumberFormat().format(atMs) + " ms", "muted");
    return card;
  };
  const render = (payload) => {
    root.replaceChildren();
    const entries = payload && Array.isArray(payload.items) ? payload.items.slice(0, MAX_ITEMS) : [];
    if (entries.length === 0) {
      const empty = document.createElement("li");
      empty.className = "event-card muted";
      empty.textContent = "No events match this filter.";
      root.append(empty);
    } else {
      const fragment = document.createDocumentFragment();
      for (const entry of entries) fragment.append(renderEntry(entry));
      root.append(fragment);
    }
    pageCount = number(payload && payload.totalPages) || 1;
    const serverPage = number(payload && payload.page);
    if (serverPage !== null && serverPage > 0) page = serverPage;
    pageLabel.textContent = "Page " + new Intl.NumberFormat().format(page) + " of " + new Intl.NumberFormat().format(pageCount);
    previous.disabled = page <= 1;
    next.disabled = page >= pageCount;
    revision = number(payload && payload.revision) || revision;
    pendingRevision = Math.max(pendingRevision, revision);
    newEvents.hidden = pendingRevision <= revision;
    syncUrl();
  };
  const refresh = async () => {
    if (loading) return;
    loading = true;
    setStatus("Loading timeline…");
    let catchUp = false;
    try {
      const loadPage = async () => {
        const params = new URLSearchParams({ filter: currentFilter, page: String(page) });
        const response = await fetch(base + "/api/page?" + params.toString(), { cache: "no-store", credentials: "omit" });
        if (!response.ok) throw new Error("Timeline request failed with status " + response.status + ". Reload this viewer.");
        return await response.json();
      };
      let payload = await loadPage();
      if (wantLatest) {
        const latest = Math.max(1, number(payload && payload.totalPages) || 1);
        if (page !== latest) {
          page = latest;
          payload = await loadPage();
        }
        wantLatest = false;
      }
      render(payload);
      const stateResponse = await fetch(base + "/api/state", { cache: "no-store", credentials: "omit" });
      if (!stateResponse.ok) throw new Error("Viewer state request failed with status " + stateResponse.status + ". Reload this viewer.");
      const state = await stateResponse.json();
      const stateRevision = number(state && state.revision);
      if (stateRevision !== null) pendingRevision = Math.max(pendingRevision, stateRevision);
      const phase = text(state && state.transport && state.transport.phase, text(state && state.status, "connected"));
      let phaseMessage = "Viewer status: " + phase + ".";
      if (phase === "viewerCapacityExceeded") phaseMessage = "Viewer capacity reached. End the Session and open its Bundle in the offline viewer.";
      else if (phase === "failed") phaseMessage = "Live updates failed. Use the offline Bundle viewer for the durable record.";
      else if (phase === "sessionEnded") phaseMessage = "Session ended. The confirmed timeline is stable.";
      setStatus(phaseMessage + " Showing revision " + new Intl.NumberFormat().format(revision) + ".", phase === "failed");
      catchUp = shouldFollow && page === pageCount && pendingRevision > revision;
      newEvents.hidden = pendingRevision <= revision;
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Viewer refresh failed. Reload this viewer.", true);
    } finally {
      loading = false;
      if (catchUp) {
        wantLatest = true;
        queueMicrotask(() => { void refresh(); });
      }
    }
  };
  filter.addEventListener("change", () => {
    if (!FILTERS.has(filter.value)) return;
    currentFilter = filter.value;
    page = 1;
    wantLatest = shouldFollow;
    void refresh();
  });
  follow.addEventListener("change", () => {
    shouldFollow = follow.checked;
    syncUrl();
    if (shouldFollow && page === pageCount && pendingRevision > revision) {
      wantLatest = true;
      void refresh();
    }
  });
  previous.addEventListener("click", () => { if (page > 1) { page -= 1; void refresh(); } });
  next.addEventListener("click", () => { page += 1; void refresh(); });
  newEvents.addEventListener("click", () => { wantLatest = true; void refresh(); });
  const source = new EventSource(base + "/api/revisions");
  source.addEventListener("revision", (event) => {
    const nextRevision = Number.parseInt(event.data, 10);
    if (!Number.isSafeInteger(nextRevision) || nextRevision <= revision) return;
    pendingRevision = Math.max(pendingRevision, nextRevision);
    if (shouldFollow && page === pageCount) {
      wantLatest = true;
      void refresh();
    } else {
      newEvents.hidden = false;
      setStatus("New events are available. Select Show New Events when ready.");
    }
  });
  source.addEventListener("error", () => setStatus("Live updates disconnected. The browser will retry; you can still refresh this page.", true));
  window.addEventListener("pagehide", () => source.close(), { once: true });
  syncUrl();
  void refresh();
})();
`;

export function documentHtml(capability: string): string {
  const prefix = `/${capability}`;
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="theme-color" content="#0b0e14" media="(prefers-color-scheme: dark)">
  <meta name="theme-color" content="#f5f7fb" media="(prefers-color-scheme: light)">
  <title>DeviceRail live visualizer</title>
  <link rel="stylesheet" href="${prefix}/app.css">
  <script src="${prefix}/app.js" defer></script>
</head>
<body>
  <a class="skip-link" href="#main">Skip to timeline</a>
  <header>
    <p class="muted" translate="no">DeviceRail</p>
    <h1>Live session timeline</h1>
    <p class="muted">A bounded, reference-only view of confirmed Session events.</p>
  </header>
  <main id="main" tabindex="-1">
    <section aria-labelledby="controls-title">
      <h2 id="controls-title">Timeline controls</h2>
      <div class="toolbar">
        <label class="field" for="filter"><span>Event filter</span>
          <select id="filter" name="event-filter" autocomplete="off">
            <option value="all">All</option><option value="observations">Observations</option>
            <option value="actions">Actions</option><option value="errors">Errors</option>
            <option value="verdicts">Verdicts</option>
          </select>
        </label>
        <label class="check"><input id="follow" name="follow-latest" type="checkbox"> Follow latest events</label>
        <button class="new-events" id="new-events" type="button" hidden>Show new events</button>
      </div>
    </section>
    <p class="status" id="status" role="status" aria-live="polite">Loading timeline…</p>
    <section aria-labelledby="timeline-title">
      <h2 id="timeline-title">Confirmed events</h2>
      <ol class="timeline" id="timeline" aria-label="Session events"></ol>
      <nav class="pager" aria-label="Timeline pages">
        <button id="previous" type="button">Previous page</button>
        <span id="page-label" aria-live="polite">Page 1</span>
        <button id="next" type="button">Next page</button>
      </nav>
    </section>
  </main>
</body>
</html>`;
}
