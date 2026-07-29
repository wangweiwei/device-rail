/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type SystemHelloMethodSchema = "system.hello";

export interface SystemHelloRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: SystemHelloMethodSchema;
  params: HelloParams;
}
export interface HelloParams {
  client: PeerInfo;
  features?: FeatureOffer;
  protocol: ProtocolOffer;
}
export interface PeerInfo {
  name: string;
  version: string;
}
export interface FeatureOffer {
  optional?: string[];
  required?: string[];
}
/**
 * An explicit set of supported ranges. Multiple entries avoid implying support
 * for major versions that a peer intentionally skipped.
 */
export interface ProtocolOffer {
  /**
   * @minItems 1
   */
  ranges: [ProtocolRange, ...ProtocolRange[]];
}
/**
 * A contiguous minor-version range within one protocol major version.
 */
export interface ProtocolRange {
  major: number;
  maxMinor: number;
  minMinor: number;
}
