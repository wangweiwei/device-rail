import type { LiveVisualizerFilter } from "./http-host.js";

const FILTERS = new Set<LiveVisualizerFilter>([
  "all",
  "observations",
  "actions",
  "errors",
  "verdicts",
]);

export interface BrowserViewState {
  readonly filter: LiveVisualizerFilter;
  readonly follow: boolean;
  readonly page: number;
  readonly pendingRevision: number;
  readonly revision: number;
  readonly shouldRefresh: boolean;
  readonly totalPages: number;
}

export type BrowserViewAction =
  | { readonly type: "filter"; readonly filter: LiveVisualizerFilter }
  | { readonly type: "follow"; readonly follow: boolean }
  | { readonly type: "page"; readonly page: number }
  | { readonly type: "revision"; readonly revision: number }
  | { readonly type: "refreshed"; readonly revision: number; readonly totalPages: number };

function page(value: string | null): number {
  if (value === null || !/^[1-9][0-9]*$/u.test(value)) return 1;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : 1;
}

export function browserViewFromUrl(url: URL): BrowserViewState {
  const rawFilter = url.searchParams.get("filter");
  const filter = FILTERS.has(rawFilter as LiveVisualizerFilter)
    ? (rawFilter as LiveVisualizerFilter)
    : "all";
  return Object.freeze({
    filter,
    follow: url.searchParams.get("follow") !== "0",
    page: page(url.searchParams.get("page")),
    pendingRevision: 0,
    revision: 0,
    shouldRefresh: true,
    totalPages: 1,
  });
}

export function reduceBrowserView(
  state: BrowserViewState,
  action: BrowserViewAction,
): BrowserViewState {
  switch (action.type) {
    case "filter":
      return Object.freeze({
        ...state,
        filter: action.filter,
        page: 1,
        shouldRefresh: true,
        totalPages: 1,
      });
    case "follow":
      return Object.freeze({
        ...state,
        follow: action.follow,
        shouldRefresh:
          action.follow &&
          state.page === state.totalPages &&
          state.pendingRevision > state.revision,
      });
    case "page":
      if (!Number.isSafeInteger(action.page) || action.page < 1) return state;
      return Object.freeze({ ...state, page: action.page, shouldRefresh: true });
    case "revision":
      if (!Number.isSafeInteger(action.revision) || action.revision <= state.revision) return state;
      return Object.freeze({
        ...state,
        pendingRevision: Math.max(state.pendingRevision, action.revision),
        shouldRefresh: state.follow && state.page === state.totalPages,
      });
    case "refreshed":
      if (
        !Number.isSafeInteger(action.revision) ||
        action.revision < state.revision ||
        !Number.isSafeInteger(action.totalPages) ||
        action.totalPages < 1
      ) {
        return state;
      }
      const pendingRevision = Math.max(state.pendingRevision, action.revision);
      return Object.freeze({
        ...state,
        pendingRevision,
        revision: action.revision,
        shouldRefresh:
          state.follow && state.page === action.totalPages && pendingRevision > action.revision,
        totalPages: action.totalPages,
      });
  }
}

export function browserViewQuery(state: BrowserViewState): string {
  const query = new URLSearchParams({
    filter: state.filter,
    follow: state.follow ? "1" : "0",
    page: String(state.page),
  });
  return query.toString();
}
