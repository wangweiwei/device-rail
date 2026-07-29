/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type ActionExecution =
  | {
      context: UiContextRef;
      mode: "nativeSemantic";
    }
  | {
      context: UiContextRef;
      mode: "webSemantic";
    }
  | {
      context: UiContextRef;
      fallbackReason: CoordinateFallbackReason;
      mode: "coordinateFallback";
    };
export type UiContextKind = "native" | "web";
export type CoordinateFallbackReason = "semanticInteractionUnavailable" | "platformLimitation";

/**
 * Full identity of one native accessibility or web-document context.
 * `documentEpoch` is required for both channels and changes after reconnect,
 * navigation, or any replacement that invalidates prior node references.
 */
export interface UiContextRef {
  contextId: string;
  contextKind: UiContextKind;
  documentEpoch: string;
}
