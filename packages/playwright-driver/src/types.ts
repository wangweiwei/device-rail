// v4: adds waitForSelector / clickByText / readValueNearLabel. The bump makes a
// stale dist/helper.js fail loudly ("unsupported bridge version") instead of
// per-action invalid_request confusion.
export const PLAYWRIGHT_BRIDGE_VERSION = 4 as const;

export type BrowserKind = "chromium" | "firefox" | "webkit";

export interface PageIdentity {
  readonly contextIndex: number;
  readonly pageIndex: number;
  readonly fingerprint: string;
}

export interface PageSnapshot extends PageIdentity {
  readonly url: string;
  readonly title: string;
  readonly viewport: {
    readonly width: number;
    readonly height: number;
    readonly scaleFactor: number;
  };
  readonly screenshotBase64?: string;
}

export type RemoteAction =
  | { readonly type: "navigate"; readonly url: string }
  | { readonly type: "click"; readonly selector: string; readonly button: "left" | "middle" | "right" }
  | { readonly type: "fill" | "fillSecret"; readonly selector: string; readonly text: string }
  | { readonly type: "press"; readonly selector: string; readonly key: string }
  | { readonly type: "select"; readonly selector: string; readonly value: string }
  | { readonly type: "scroll"; readonly deltaX: number; readonly deltaY: number }
  | { readonly type: "elementExists"; readonly selector: string }
  | {
      readonly type: "textContains";
      readonly selector: string;
      readonly text: string;
      readonly caseSensitive: boolean;
    }
  | {
      readonly type: "waitFor";
      readonly state: "load" | "domcontentloaded" | "networkidle";
    }
  | {
      readonly type: "waitForSelector";
      readonly selector: string;
      readonly state: "attached" | "detached" | "visible" | "hidden";
    }
  | { readonly type: "clickByText"; readonly text: string; readonly exact: boolean }
  | {
      readonly type: "readValueNearLabel";
      readonly label: string;
      readonly direction: "right" | "below";
      readonly exact: boolean;
    };

export type RemoteActionOutput =
  | { readonly exists: boolean }
  | { readonly contains: boolean }
  | { readonly value: string };

interface BridgeRequestBase {
  readonly version: typeof PLAYWRIGHT_BRIDGE_VERSION;
  readonly browser: BrowserKind;
  readonly endpoint: string;
  readonly timeoutMs: number;
}

export type BridgeRequest =
  | (BridgeRequestBase & { readonly operation: { readonly type: "discover" } })
  | (BridgeRequestBase & {
      readonly operation:
        | { readonly type: "probe"; readonly page: PageIdentity }
        | { readonly type: "observe"; readonly page: PageIdentity }
        | { readonly type: "action"; readonly page: PageIdentity; readonly action: RemoteAction };
    });

export interface BridgeFailure {
  readonly ok: false;
  readonly error: {
    readonly code:
      | "invalid_request"
      | "connect_failed"
      | "page_not_found"
      | "page_identity_unavailable"
      | "page_identity_changed"
      | "operation_timed_out"
      | "operation_failed"
      | "response_too_large";
    readonly retryable: boolean;
  };
}

export type BridgeSuccess =
  | { readonly ok: true; readonly result: { readonly type: "pages"; readonly pages: readonly PageSnapshot[] } }
  | {
      readonly ok: true;
      readonly result: {
        readonly type: "page";
        readonly page: PageSnapshot;
        readonly output?: RemoteActionOutput;
      };
    };

export type BridgeResponse = BridgeFailure | BridgeSuccess;
