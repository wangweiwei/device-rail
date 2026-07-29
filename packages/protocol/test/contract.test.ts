import type {
  ActionCall,
  ActionDefinition,
  ActionOutcome,
  ActionResult,
  DeviceExecuteRequest,
  DeviceInfo,
  DevicesListRequest,
  DevicesListResult,
  ErrorInfo,
  EventSequence,
  Platform,
  RpcId,
  RpcMethod,
  RpcParamsFor,
  RpcResponse,
  RpcResultFor,
  RpcSupportsTimeout,
  TestEventPayload,
} from "../src/generated/v1/index.js";

const executeRequest: DeviceExecuteRequest = {
  jsonrpc: "2.0",
  id: 1,
  method: "device.execute",
  timeoutMs: 1_000,
  params: {
    id: "00000000-0000-4000-8000-000000000000",
    name: "tap",
    arguments: { x: 10, y: 20 },
    actionTimeoutMs: 500,
  },
};
void executeRequest;

const executeMethod: RpcMethod = "device.execute";
const executeParams: RpcParamsFor<"device.execute"> = executeRequest.params;
const typedListResult: RpcResultFor<"devices.list"> = {
  devices: [],
  selectedDeviceId: null,
};
void [executeMethod, executeParams, typedListResult];

// @ts-expect-error The method union is generated from method-specific schemas.
const unknownMethod: RpcMethod = "device.unknown";
void unknownMethod;

const listWithoutParams: DevicesListRequest = {
  jsonrpc: "2.0",
  id: "list-1",
  method: "devices.list",
};
const listWithObjectParams: DevicesListRequest = {
  ...listWithoutParams,
  params: {},
};
const listWithArrayParams: DevicesListRequest = {
  ...listWithoutParams,
  params: [],
};
void [listWithObjectParams, listWithArrayParams];

// @ts-expect-error The runtime rejects explicit null params.
const listWithNullParams: DevicesListRequest = { ...listWithoutParams, params: null };
void listWithNullParams;

const listWithUnknownParam: DevicesListRequest = {
  ...listWithoutParams,
  // @ts-expect-error No-param requests reject non-empty objects.
  params: { unknown: true },
};
void listWithUnknownParam;

// @ts-expect-error No-param requests reject scalar params.
const listWithScalarParam: DevicesListRequest = { ...listWithoutParams, params: 42 };
void listWithScalarParam;

// @ts-expect-error No-param request arrays must be empty.
const listWithNonemptyArray: DevicesListRequest = { ...listWithoutParams, params: [1] };
void listWithNonemptyArray;

const listedDevices: DevicesListResult = {
  devices: [],
  selectedDeviceId: null,
};
void listedDevices;

const snakeCaseResult: DevicesListResult = {
  devices: [],
  // @ts-expect-error Wire fields are camelCase.
  selected_device_id: null,
};
void snakeCaseResult;

const optionalOsVersion: DeviceInfo = {
  id: "mock-1",
  name: "Mock Device",
  platform: { kind: "mock" },
  connected: false,
};
void optionalOsVersion;

// @ts-expect-error Omitted and explicit undefined are different on the wire.
const undefinedOsVersion: DeviceInfo = {
  id: "mock-1",
  name: "Mock Device",
  platform: { kind: "mock" },
  connected: false,
  osVersion: undefined,
};
void undefinedOsVersion;

type IsAny<T> = 0 extends 1 & T ? true : false;
type IsEqual<Left, Right> =
  (<Value>() => Value extends Left ? 1 : 2) extends (<Value>() =>
    Value extends Right ? 1 : 2)
    ? true
    : false;
type IsUnknown<T> = IsAny<T> extends true
  ? false
  : unknown extends T
    ? [keyof T] extends [never]
      ? true
      : false
    : false;
type AssertTrue<T extends true> = T;
type TimeoutMethods = {
  [Method in RpcMethod]: RpcSupportsTimeout<Method> extends true ? Method : never;
}[RpcMethod];
type TimeoutMethodsAreExact = AssertTrue<
  IsEqual<
    TimeoutMethods,
    | "device.capabilities"
    | "device.connect"
    | "device.disconnect"
    | "device.execute"
    | "device.observe"
    | "media.stream.capture"
  >
>;
type AnnotationOnlyFieldsAreUnknown = [
  AssertTrue<IsUnknown<ActionCall["arguments"]>>,
  AssertTrue<IsUnknown<ActionDefinition["inputSchema"]>>,
  AssertTrue<IsUnknown<ActionResult["output"]>>,
  AssertTrue<IsUnknown<ErrorInfo["details"]>>,
  AssertTrue<IsUnknown<Extract<RpcResponse, { result: unknown }>["result"]>>,
];
declare const timeoutCapabilities: TimeoutMethodsAreExact;
void timeoutCapabilities;
declare const annotationOnlyFieldsAreUnknown: AnnotationOnlyFieldsAreUnknown;
void annotationOnlyFieldsAreUnknown;

const rpcId: RpcId = Number.MAX_SAFE_INTEGER;
const sequence: EventSequence = 1;
void [rpcId, sequence];

function actionStatus(outcome: ActionOutcome): string {
  switch (outcome.outcome) {
    case "succeeded":
      return outcome.result.callId;
    case "failed":
    case "cancelled":
      return outcome.error.code;
    case "timedOut":
      return `${outcome.error.code}:${outcome.timeoutMs}`;
    default: {
      const unreachable: never = outcome;
      return unreachable;
    }
  }
}
void actionStatus;

function platformKind(platform: Platform): string {
  switch (platform.kind) {
    case "web":
    case "android":
    case "ios":
    case "harmonyOs":
    case "macOs":
    case "windows":
    case "linux":
    case "rdp":
    case "mock":
      return platform.kind;
    case "other":
      return platform.value;
    default: {
      const unreachable: never = platform;
      return unreachable;
    }
  }
}
void platformKind;

function eventKind(payload: TestEventPayload): string {
  switch (payload.type) {
    case "sessionStarted":
      return payload.type;
    case "sessionEnded":
      return payload.outcome;
    case "observationCaptured":
      return payload.observation.id;
    case "actionStarted":
      return payload.call.id;
    case "actionCompleted":
      return payload.callId;
    case "mediaStreamStarted":
      return payload.stream.id;
    case "mediaFrameCaptured":
      return payload.frame.streamId;
    case "mediaStreamEnded":
      return payload.streamId;
    case "verdictRecorded":
      return payload.verdict.status;
    case "error":
      return payload.error.code;
    default: {
      const unreachable: never = payload;
      return unreachable;
    }
  }
}
void eventKind;

// @ts-expect-error Timed-out outcomes require timeoutMs.
const incompleteTimeout: ActionOutcome = {
  outcome: "timedOut",
  error: { code: "action_timeout", message: "timed out", retryable: true },
};
void incompleteTimeout;
