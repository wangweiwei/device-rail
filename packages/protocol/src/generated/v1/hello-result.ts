/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export interface HelloResult {
  connectionId: string;
  features: FeatureSelection;
  protocol: ProtocolSelection;
  server: PeerInfo;
  transport: TransportInfo;
}
export interface FeatureSelection {
  enabled: string[];
}
export interface ProtocolSelection {
  selected: ProtocolVersion;
}
export interface ProtocolVersion {
  major: number;
  minor: number;
}
export interface PeerInfo {
  name: string;
  version: string;
}
export interface TransportInfo {
  framing: string;
  kind: string;
}
