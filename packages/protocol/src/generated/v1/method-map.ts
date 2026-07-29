/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

import type { DeviceCapabilitiesRequest } from "./device-capabilities-request.js";
import type { DeviceCapabilitiesResponse } from "./device-capabilities-response.js";
import type { DeviceConnectRequest } from "./device-connect-request.js";
import type { DeviceConnectResponse } from "./device-connect-response.js";
import type { DeviceDisconnectRequest } from "./device-disconnect-request.js";
import type { DeviceDisconnectResponse } from "./device-disconnect-response.js";
import type { DeviceExecuteRequest } from "./device-execute-request.js";
import type { DeviceExecuteResponse } from "./device-execute-response.js";
import type { DeviceObserveRequest } from "./device-observe-request.js";
import type { DeviceObserveResponse } from "./device-observe-response.js";
import type { DeviceSelectRequest } from "./device-select-request.js";
import type { DeviceSelectResponse } from "./device-select-response.js";
import type { DevicesListRequest } from "./devices-list-request.js";
import type { DevicesListResponse } from "./devices-list-response.js";
import type { EventsClearRequest } from "./events-clear-request.js";
import type { EventsClearResponse } from "./events-clear-response.js";
import type { EventsListRequest } from "./events-list-request.js";
import type { EventsListResponse } from "./events-list-response.js";
import type { EventsStreamOpenRequest } from "./events-stream-open-request.js";
import type { EventsStreamOpenResponse } from "./events-stream-open-response.js";
import type { EventsSubscribeRequest } from "./events-subscribe-request.js";
import type { EventsSubscribeResponse } from "./events-subscribe-response.js";
import type { MediaStreamCaptureRequest } from "./media-stream-capture-request.js";
import type { MediaStreamCaptureResponse } from "./media-stream-capture-response.js";
import type { MediaStreamEndRequest } from "./media-stream-end-request.js";
import type { MediaStreamEndResponse } from "./media-stream-end-response.js";
import type { MediaStreamStartRequest } from "./media-stream-start-request.js";
import type { MediaStreamStartResponse } from "./media-stream-start-response.js";
import type { RequestCancelRequest } from "./request-cancel-request.js";
import type { RequestCancelResponse } from "./request-cancel-response.js";
import type { SessionCurrentRequest } from "./session-current-request.js";
import type { SessionCurrentResponse } from "./session-current-response.js";
import type { SessionEndRequest } from "./session-end-request.js";
import type { SessionEndResponse } from "./session-end-response.js";
import type { SessionExportRequest } from "./session-export-request.js";
import type { SessionExportResponse } from "./session-export-response.js";
import type { SessionStartRequest } from "./session-start-request.js";
import type { SessionStartResponse } from "./session-start-response.js";
import type { SessionsListRequest } from "./sessions-list-request.js";
import type { SessionsListResponse } from "./sessions-list-response.js";
import type { SystemDescribeRequest } from "./system-describe-request.js";
import type { SystemDescribeResponse } from "./system-describe-response.js";
import type { SystemHelloRequest } from "./system-hello-request.js";
import type { SystemHelloResponse } from "./system-hello-response.js";
import type { UiSnapshotGetRequest } from "./ui-snapshot-get-request.js";
import type { UiSnapshotGetResponse } from "./ui-snapshot-get-response.js";
import type { VerdictRecordRequest } from "./verdict-record-request.js";
import type { VerdictRecordResponse } from "./verdict-record-response.js";

export interface RpcMethodMap {
  "device.capabilities": {
    request: DeviceCapabilitiesRequest;
    response: DeviceCapabilitiesResponse;
  };
  "device.connect": {
    request: DeviceConnectRequest;
    response: DeviceConnectResponse;
  };
  "device.disconnect": {
    request: DeviceDisconnectRequest;
    response: DeviceDisconnectResponse;
  };
  "device.execute": {
    request: DeviceExecuteRequest;
    response: DeviceExecuteResponse;
  };
  "device.observe": {
    request: DeviceObserveRequest;
    response: DeviceObserveResponse;
  };
  "device.select": {
    request: DeviceSelectRequest;
    response: DeviceSelectResponse;
  };
  "devices.list": {
    request: DevicesListRequest;
    response: DevicesListResponse;
  };
  "events.clear": {
    request: EventsClearRequest;
    response: EventsClearResponse;
  };
  "events.list": {
    request: EventsListRequest;
    response: EventsListResponse;
  };
  "events.stream.open": {
    request: EventsStreamOpenRequest;
    response: EventsStreamOpenResponse;
  };
  "events.subscribe": {
    request: EventsSubscribeRequest;
    response: EventsSubscribeResponse;
  };
  "media.stream.capture": {
    request: MediaStreamCaptureRequest;
    response: MediaStreamCaptureResponse;
  };
  "media.stream.end": {
    request: MediaStreamEndRequest;
    response: MediaStreamEndResponse;
  };
  "media.stream.start": {
    request: MediaStreamStartRequest;
    response: MediaStreamStartResponse;
  };
  "request.cancel": {
    request: RequestCancelRequest;
    response: RequestCancelResponse;
  };
  "session.current": {
    request: SessionCurrentRequest;
    response: SessionCurrentResponse;
  };
  "session.end": {
    request: SessionEndRequest;
    response: SessionEndResponse;
  };
  "session.export": {
    request: SessionExportRequest;
    response: SessionExportResponse;
  };
  "session.start": {
    request: SessionStartRequest;
    response: SessionStartResponse;
  };
  "sessions.list": {
    request: SessionsListRequest;
    response: SessionsListResponse;
  };
  "system.describe": {
    request: SystemDescribeRequest;
    response: SystemDescribeResponse;
  };
  "system.hello": {
    request: SystemHelloRequest;
    response: SystemHelloResponse;
  };
  "ui.snapshot.get": {
    request: UiSnapshotGetRequest;
    response: UiSnapshotGetResponse;
  };
  "verdict.record": {
    request: VerdictRecordRequest;
    response: VerdictRecordResponse;
  };
}

export type RpcMethod = keyof RpcMethodMap;
export type RpcRequestFor<M extends RpcMethod> = RpcMethodMap[M]["request"];
export type RpcResponseFor<M extends RpcMethod> = RpcMethodMap[M]["response"];
export type RpcParamsFor<M extends RpcMethod> = RpcRequestFor<M> extends { params: infer P }
  ? P
  : RpcRequestFor<M> extends { params?: infer P }
    ? P | undefined
    : undefined;
export type RpcResultFor<M extends RpcMethod> = RpcResponseFor<M> extends infer Response
  ? Response extends { result: infer Result }
    ? Result
    : never
  : never;
export type RpcSupportsTimeout<M extends RpcMethod> = "timeoutMs" extends keyof RpcRequestFor<M>
  ? true
  : false;
