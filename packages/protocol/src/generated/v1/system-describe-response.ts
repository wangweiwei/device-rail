/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type SystemDescribeResponse = SystemDescribeSuccessSchema | SystemHelloFailureSchema;
export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type NullableRpcIdSchema = RpcIdSchema | null;

export interface SystemDescribeSuccessSchema {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  result: SystemDescribeResult;
}
/**
 * Result returned by `system.describe`.
 */
export interface SystemDescribeResult {
  activeSessionId?: string | null;
  client: PeerInfo;
  connection: HelloResult;
  deviceId?: string | null;
}
export interface PeerInfo {
  name: string;
  version: string;
}
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
export interface TransportInfo {
  framing: string;
  kind: string;
}
export interface SystemHelloFailureSchema {
  error: RpcError;
  id: NullableRpcIdSchema;
  jsonrpc: JsonRpcVersion;
}
export interface RpcError {
  code: number;
  data: ErrorInfo;
  message: string;
}
export interface ErrorInfo {
  code: string;
  details?: unknown;
  message: string;
  retryable: boolean;
}
