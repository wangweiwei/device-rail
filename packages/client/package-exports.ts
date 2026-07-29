import {
  DeviceRailClient,
  DeviceRailEventStream,
  ProtocolViolationError,
  validateRpcResult,
  type ClientRpcMethod,
  type ClientTransport,
  type EventStreamItem,
} from "@devicerail/client";

const method: ClientRpcMethod = "system.describe";
const clientConstructor: typeof DeviceRailClient = DeviceRailClient;
const streamConstructor: typeof DeviceRailEventStream = DeviceRailEventStream;
const protocolErrorConstructor: typeof ProtocolViolationError = ProtocolViolationError;
const resultValidator: typeof validateRpcResult = validateRpcResult;
declare const transport: ClientTransport;
declare const streamItem: EventStreamItem;

void method;
void clientConstructor;
void streamConstructor;
void protocolErrorConstructor;
void resultValidator;
void transport;
void streamItem;
