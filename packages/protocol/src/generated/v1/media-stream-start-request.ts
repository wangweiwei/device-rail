/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type MediaStreamStartMethodSchema = "media.stream.start";
export type MediaStreamKind = "screenshot" | "video";

export interface MediaStreamStartRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: MediaStreamStartMethodSchema;
  params: MediaStreamStartParams;
}
/**
 * Parameters accepted by `media.stream.start`.
 *
 * The caller chooses only a lifetime-unique stream identifier and the logical
 * stream kind. The daemon assigns the media type from its selected, leased
 * device producer; viewport metadata may be absent. This control method never
 * accepts frame bytes, Evidence references, or filesystem paths.
 */
export interface MediaStreamStartParams {
  kind: MediaStreamKind;
  streamId: string;
}
