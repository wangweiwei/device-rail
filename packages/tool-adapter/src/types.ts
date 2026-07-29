import type { DeviceRailClient, RequestHandle } from "@devicerail/client";
import type {
  ActionResult,
  Observation,
  RequestCancelResult,
  RpcId,
} from "@devicerail/protocol";

export type DeviceRailToolClient = Pick<DeviceRailClient, "beginCall" | "call"> & {
  readonly enabledFeatures?: ReadonlySet<string>;
};

export type ToolInputSchema = Readonly<Record<string, unknown>>;

export interface ObservationToolDefinition {
  readonly description: string;
  readonly inputSchema: ToolInputSchema;
  readonly kind: "observation";
  readonly name: string;
}

export interface ActionToolDefinition {
  readonly actionName: string;
  readonly description: string;
  readonly inputSchema: ToolInputSchema;
  readonly kind: "action";
  readonly name: string;
  readonly protection?: "protected";
}

export type DeviceRailToolDefinition = ObservationToolDefinition | ActionToolDefinition;

export interface ToolInvocation {
  readonly arguments?: unknown;
  readonly invocationId?: string;
  readonly name: string;
}

export interface ToolInvocationOptions {
  readonly actionTimeoutMs?: number;
  readonly requestTimeoutMs?: number;
  readonly signal?: AbortSignal;
}

export interface ObservationToolResult {
  readonly invocationId?: string;
  readonly kind: "observation";
  readonly observation: Observation;
  readonly requestId: RpcId;
  readonly toolName: string;
}

export interface ActionToolResult {
  readonly action: ActionResult;
  readonly actionCallId: string;
  readonly actionName: string;
  readonly invocationId?: string;
  readonly kind: "action";
  readonly requestId: RpcId;
  readonly toolName: string;
}

export type ToolInvocationResult = ObservationToolResult | ActionToolResult;

interface BaseToolInvocationHandle<Result extends ToolInvocationResult> {
  readonly requestId: RpcId;
  readonly result: Promise<Result>;
  cancel(): Promise<RequestCancelResult>;
}

export interface ObservationToolInvocationHandle
  extends BaseToolInvocationHandle<ObservationToolResult> {
  readonly kind: "observation";
}

export interface ActionToolInvocationHandle extends BaseToolInvocationHandle<ActionToolResult> {
  readonly actionCallId: string;
  readonly actionName: string;
  readonly kind: "action";
}

export type ToolInvocationHandle =
  | ObservationToolInvocationHandle
  | ActionToolInvocationHandle;

export interface DeviceRailToolCatalog {
  readonly id: string;
  readonly revision: number;
  readonly tools: readonly DeviceRailToolDefinition[];

  beginInvoke(
    invocation: ToolInvocation,
    options?: ToolInvocationOptions,
  ): ToolInvocationHandle;

  invoke(
    invocation: ToolInvocation,
    options?: ToolInvocationOptions,
  ): Promise<ToolInvocationResult>;
}

export interface ToolDiscoveryOptions {
  readonly requestTimeoutMs?: number;
  readonly signal?: AbortSignal;
}

export interface DeviceRailToolAdapterOptions {
  readonly includeObservation?: boolean;
  readonly includeProtectedActions?: boolean;
  readonly maxActions?: number;
}

// Compile-time assertion that the client handle shape remains compatible with
// the narrower public handle surface used by this package.
type _Assert<T extends true> = T;
type _ClientHandleCompatibility = _Assert<
  RequestHandle<unknown> extends {
    readonly id: RpcId;
    readonly result: Promise<unknown>;
    cancel(): Promise<RequestCancelResult>;
  }
    ? true
    : false
>;
