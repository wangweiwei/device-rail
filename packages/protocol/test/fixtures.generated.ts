/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

import type {
  ActionCall,
  ActionDefinition,
  ActionResult,
  ClearElementArguments,
  ClearElementResult,
  DeviceCapabilitiesRequest,
  DeviceCapabilitiesResponse,
  DeviceConnectRequest,
  DeviceConnectResponse,
  DeviceDisconnectRequest,
  DeviceDisconnectResponse,
  DeviceExecuteRequest,
  DeviceExecuteResponse,
  DeviceInfo,
  DeviceObserveRequest,
  DeviceObserveResponse,
  DeviceSelectRequest,
  DeviceSelectResponse,
  DevicesListRequest,
  DevicesListResponse,
  ErrorInfo,
  EventsClearRequest,
  EventsClearResponse,
  EventsListRequest,
  EventsListResponse,
  EventsStreamEventNotification,
  EventsStreamOpenRequest,
  EventsStreamOpenResponse,
  EventsStreamTerminalNotification,
  EventsSubscribeRequest,
  EventsSubscribeResponse,
  FindElementArguments,
  FindElementResult,
  ManualRecording,
  MediaStreamCaptureRequest,
  MediaStreamCaptureResponse,
  MediaStreamEndRequest,
  MediaStreamEndResponse,
  MediaStreamStartRequest,
  MediaStreamStartResponse,
  Observation,
  RequestCancelRequest,
  RequestCancelResponse,
  RpcResponse,
  SessionCurrentRequest,
  SessionCurrentResponse,
  SessionEndRequest,
  SessionEndResponse,
  SessionExportRequest,
  SessionExportResponse,
  SessionStartRequest,
  SessionStartResponse,
  SessionsListRequest,
  SessionsListResponse,
  SetElementValueArguments,
  SetElementValueResult,
  SystemDescribeRequest,
  SystemDescribeResponse,
  SystemHelloRequest,
  SystemHelloResponse,
  TapElementArguments,
  TapElementResult,
  TestEvent,
  UiSnapshot,
  UiSnapshotGetRequest,
  UiSnapshotGetResponse,
  VerdictRecordRequest,
  VerdictRecordResponse,
  WaitForElementArguments,
  WaitForElementResult
} from "../src/generated/v1/index.js";

export const goldenFixtures = {
  "handshake.system-hello.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "hello-1",
      "method": "system.hello",
      "params": {
        "client": {
          "name": "fixture-client",
          "version": "0.1.0"
        },
        "protocol": {
          "ranges": [
            {
              "major": 1,
              "minMinor": 0,
              "maxMinor": 5
            },
            {
              "major": 3,
              "minMinor": 0,
              "maxMinor": 0
            }
          ]
        },
        "features": {
          "required": [],
          "optional": [
            "action.protected.v1",
            "device.routing.v1",
            "device.semanticActions.v1",
            "events.snapshot.v1",
            "events.stream.v1",
            "media.stream.v1",
            "observation.uiSnapshot.v1",
            "request.control.v1",
            "session.export.page.v1",
            "verdict.record.v1"
          ]
        }
      }
    }
  ) satisfies SystemHelloRequest,
  "handshake.system-hello.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "hello-1",
      "result": {
        "connectionId": "00000000-0000-0000-0000-000000000000",
        "protocol": {
          "selected": {
            "major": 1,
            "minor": 5
          }
        },
        "server": {
          "name": "devicerail-daemon",
          "version": "0.1.0"
        },
        "transport": {
          "kind": "stdio",
          "framing": "ndjson"
        },
        "features": {
          "enabled": [
            "action.protected.v1",
            "device.routing.v1",
            "device.semanticActions.v1",
            "events.snapshot.v1",
            "events.stream.v1",
            "media.stream.v1",
            "observation.uiSnapshot.v1",
            "request.control.v1",
            "session.export.page.v1",
            "verdict.record.v1"
          ]
        }
      }
    }
  ) satisfies SystemHelloResponse,
  "rpc.system-describe.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "system-describe-1",
      "method": "system.describe"
    }
  ) satisfies SystemDescribeRequest,
  "rpc.system-describe.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "system-describe-1",
      "result": {
        "connection": {
          "connectionId": "00000000-0000-0000-0000-000000000000",
          "protocol": {
            "selected": {
              "major": 1,
              "minor": 2
            }
          },
          "server": {
            "name": "devicerail-daemon",
            "version": "0.1.0"
          },
          "transport": {
            "kind": "stdio",
            "framing": "ndjson"
          },
          "features": {
            "enabled": [
              "device.routing.v1",
              "events.snapshot.v1",
              "request.control.v1"
            ]
          }
        },
        "client": {
          "name": "golden-client",
          "version": "0.1.0"
        },
        "deviceId": "mock-device-1",
        "activeSessionId": "33333333-3333-4333-8333-333333333333"
      }
    }
  ) satisfies SystemDescribeResponse,
  "device.device-info.v1": (
    {
      "id": "android-emulator-5554",
      "name": "Pixel 9 API 35",
      "platform": {
        "kind": "android"
      },
      "osVersion": "15",
      "connected": true
    }
  ) satisfies DeviceInfo,
  "device.observation.v1": (
    {
      "id": "11111111-1111-4111-8111-111111111111",
      "deviceId": "android-emulator-5554",
      "capturedAtMs": 1720000000123,
      "viewport": {
        "width": 1080,
        "height": 2400,
        "scaleFactor": 2.5
      },
      "screenshot": {
        "id": "asset-screenshot-001",
        "mediaType": "image/png",
        "uri": "devicerail://assets/asset-screenshot-001",
        "sha256": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
      },
      "uiSnapshot": {
        "formatVersion": 1,
        "context": {
          "contextKind": "native",
          "contextId": "NATIVE_APP",
          "documentEpoch": "model-observation-1"
        },
        "nodeCount": 3,
        "byteLength": 512,
        "evidence": {
          "id": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
          "mediaType": "application/vnd.devicerail.ui-tree+json;version=1",
          "uri": "devicerail://assets/sha256/dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
          "sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
        }
      },
      "metadata": {
        "orientation": "portrait",
        "source": "golden-fixture"
      }
    }
  ) satisfies Observation,
  "device.observation.omitted.v1": (
    {
      "id": "11111111-1111-4111-8111-111111111112",
      "deviceId": "android-emulator-5554",
      "capturedAtMs": 1720000000124,
      "viewport": {
        "width": 1080,
        "height": 2400,
        "scaleFactor": 2.75
      },
      "screenshot": null,
      "screenshotOmission": "policy",
      "metadata": {
        "orientation": "portrait"
      }
    }
  ) satisfies Observation,
  "action.definition.v1": (
    {
      "name": "tap",
      "description": "Tap a point in device viewport coordinates.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "x": {
            "type": "integer",
            "minimum": 0
          },
          "y": {
            "type": "integer",
            "minimum": 0
          }
        },
        "required": [
          "x",
          "y"
        ],
        "additionalProperties": false
      }
    }
  ) satisfies ActionDefinition,
  "action.definition.protected.v1": (
    {
      "name": "inputSecret",
      "description": "Type a protected printable-ASCII value without durable argument or screenshot capture",
      "inputSchema": {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "required": [
          "secret"
        ],
        "properties": {
          "secret": {
            "type": "string",
            "minLength": 1,
            "maxLength": 1024,
            "pattern": "^[\\u0020-\\u007e]+$",
            "not": {
              "pattern": "%s"
            }
          }
        }
      },
      "protection": "protected"
    }
  ) satisfies ActionDefinition,
  "action.call.v1": (
    {
      "id": "22222222-2222-4222-8222-222222222222",
      "name": "tap",
      "arguments": {
        "x": 540,
        "y": 1200
      }
    }
  ) satisfies ActionCall,
  "manual.recording.v1": (
    {
      "formatVersion": 1,
      "recordingId": "44444444-4444-4444-8444-444444444444",
      "sourceDeviceId": "playwright-chromium-page-1",
      "actionSpaceSha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "startedAtMs": 1700000000000,
      "endedAtMs": 1700000000250,
      "steps": [
        {
          "sequence": 1,
          "capturedAtMs": 1700000000100,
          "callId": "55555555-5555-4555-8555-555555555555",
          "name": "click",
          "arguments": {
            "kind": "captured",
            "value": {
              "selector": "#submit"
            }
          }
        },
        {
          "sequence": 2,
          "capturedAtMs": 1700000000200,
          "callId": "66666666-6666-4666-8666-666666666666",
          "name": "fillSecret",
          "arguments": {
            "kind": "protected",
            "secretRef": "login.password"
          }
        }
      ]
    }
  ) satisfies ManualRecording,
  "action.result.v1": (
    {
      "callId": "22222222-2222-4222-8222-222222222222",
      "startedAtMs": 1720000001000,
      "finishedAtMs": 1720000001500,
      "output": {
        "accepted": true
      },
      "before": null,
      "after": {
        "id": "44444444-4444-4444-8444-444444444444",
        "deviceId": "android-emulator-5554",
        "capturedAtMs": 1720000001500,
        "viewport": {
          "width": 1080,
          "height": 2400,
          "scaleFactor": 2.5
        },
        "screenshot": null,
        "metadata": {
          "phase": "after"
        }
      },
      "evidence": [
        {
          "id": "asset-action-001",
          "mediaType": "image/png",
          "uri": "devicerail://assets/asset-action-001",
          "sha256": null
        }
      ],
      "execution": {
        "mode": "nativeSemantic",
        "context": {
          "contextKind": "native",
          "contextId": "NATIVE_APP",
          "documentEpoch": "action-result-1"
        }
      }
    }
  ) satisfies ActionResult,
  "error.info.v1": (
    {
      "code": "device_unavailable",
      "message": "Device android-emulator-5554 is unavailable",
      "retryable": true,
      "details": {
        "deviceId": "android-emulator-5554",
        "reason": "offline"
      }
    }
  ) satisfies ErrorInfo,
  "rpc.device-connect.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "device-connect-1",
      "method": "device.connect"
    }
  ) satisfies DeviceConnectRequest,
  "rpc.device-connect.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "device-connect-1",
      "result": {
        "id": "mock-device-1",
        "name": "Mock Device",
        "platform": {
          "kind": "mock"
        },
        "osVersion": "1.0",
        "connected": true
      }
    }
  ) satisfies DeviceConnectResponse,
  "rpc.device-disconnect.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "device-disconnect-1",
      "method": "device.disconnect"
    }
  ) satisfies DeviceDisconnectRequest,
  "rpc.device-disconnect.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "device-disconnect-1",
      "result": {
        "disconnected": true
      }
    }
  ) satisfies DeviceDisconnectResponse,
  "rpc.device-capabilities.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "device-capabilities-1",
      "method": "device.capabilities"
    }
  ) satisfies DeviceCapabilitiesRequest,
  "rpc.device-capabilities.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "device-capabilities-1",
      "result": [
        {
          "name": "tap",
          "description": "Tap a point on the device screen",
          "inputSchema": {
            "type": "object",
            "properties": {
              "x": {
                "type": "number"
              },
              "y": {
                "type": "number"
              }
            },
            "required": [
              "x",
              "y"
            ],
            "additionalProperties": false
          }
        }
      ]
    }
  ) satisfies DeviceCapabilitiesResponse,
  "rpc.device-observe.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "device-observe-1",
      "method": "device.observe"
    }
  ) satisfies DeviceObserveRequest,
  "rpc.device-observe.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "device-observe-1",
      "result": {
        "id": "11111111-1111-4111-8111-111111111111",
        "deviceId": "mock-device-1",
        "capturedAtMs": 1720000000123,
        "viewport": {
          "width": 1080,
          "height": 2400,
          "scaleFactor": 2.5
        },
        "screenshot": null,
        "metadata": {
          "source": "device.observe"
        }
      }
    }
  ) satisfies DeviceObserveResponse,
  "rpc.device-execute.timeout.v1": (
    {
      "jsonrpc": "2.0",
      "id": "execute-timeout-1",
      "method": "device.execute",
      "timeoutMs": 30000,
      "params": {
        "id": "22222222-2222-4222-8222-222222222222",
        "name": "tap",
        "arguments": {
          "x": 540,
          "y": 1200
        },
        "actionTimeoutMs": 5000
      }
    }
  ) satisfies DeviceExecuteRequest,
  "rpc.device-execute.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "execute-timeout-1",
      "result": {
        "callId": "22222222-2222-4222-8222-222222222222",
        "startedAtMs": 1720000001000,
        "finishedAtMs": 1720000001500,
        "output": {
          "accepted": true
        },
        "before": null,
        "after": {
          "id": "44444444-4444-4444-8444-444444444444",
          "deviceId": "mock-device-1",
          "capturedAtMs": 1720000001500,
          "viewport": {
            "width": 1080,
            "height": 2400,
            "scaleFactor": 2.5
          },
          "screenshot": null,
          "metadata": {
            "phase": "after"
          }
        },
        "evidence": []
      }
    }
  ) satisfies DeviceExecuteResponse,
  "rpc.device-select.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "device-select-1",
      "method": "device.select",
      "params": {
        "deviceId": "mock-device-1"
      }
    }
  ) satisfies DeviceSelectRequest,
  "rpc.device-select.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "device-select-1",
      "result": {
        "device": {
          "id": "mock-device-1",
          "name": "Mock Device",
          "platform": {
            "kind": "mock"
          },
          "osVersion": "1.0",
          "connected": true
        }
      }
    }
  ) satisfies DeviceSelectResponse,
  "rpc.devices-list.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "devices-list-1",
      "method": "devices.list"
    }
  ) satisfies DevicesListRequest,
  "rpc.devices-list.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "devices-list-1",
      "result": {
        "devices": [
          {
            "id": "android-emulator-5554",
            "name": "Pixel 9 API 35",
            "platform": {
              "kind": "android"
            },
            "osVersion": "15",
            "connected": true
          },
          {
            "id": "mock-device-1",
            "name": "Mock Device",
            "platform": {
              "kind": "mock"
            },
            "osVersion": "1.0",
            "connected": true
          }
        ],
        "selectedDeviceId": "android-emulator-5554"
      }
    }
  ) satisfies DevicesListResponse,
  "rpc.request-cancel.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "cancel-1",
      "method": "request.cancel",
      "params": {
        "requestId": "execute-timeout-1"
      }
    }
  ) satisfies RequestCancelRequest,
  "rpc.request-cancel.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "cancel-1",
      "result": {
        "requestId": "execute-timeout-1",
        "status": "requested"
      }
    }
  ) satisfies RequestCancelResponse,
  "rpc.session-start.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "session-start-1",
      "method": "session.start"
    }
  ) satisfies SessionStartRequest,
  "rpc.session-start.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "session-start-1",
      "result": {
        "id": "33333333-3333-4333-8333-333333333333",
        "state": "active",
        "startedAtMs": 1720000000000,
        "endedAtMs": null,
        "eventCount": 1,
        "lastSequence": 1
      }
    }
  ) satisfies SessionStartResponse,
  "rpc.session-current.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "session-current-1",
      "method": "session.current"
    }
  ) satisfies SessionCurrentRequest,
  "rpc.session-current.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "session-current-1",
      "result": {
        "sessionId": "33333333-3333-4333-8333-333333333333"
      }
    }
  ) satisfies SessionCurrentResponse,
  "rpc.session-end.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "session-end-1",
      "method": "session.end",
      "params": {
        "outcome": "completed",
        "reason": "golden baseline complete"
      }
    }
  ) satisfies SessionEndRequest,
  "rpc.session-end.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "session-end-1",
      "result": {
        "id": "33333333-3333-4333-8333-333333333333",
        "state": "ended",
        "startedAtMs": 1720000000000,
        "endedAtMs": 1720000002000,
        "eventCount": 2,
        "lastSequence": 2
      }
    }
  ) satisfies SessionEndResponse,
  "rpc.sessions-list.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "sessions-list-1",
      "method": "sessions.list"
    }
  ) satisfies SessionsListRequest,
  "rpc.sessions-list.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "sessions-list-1",
      "result": [
        {
          "id": "33333333-3333-4333-8333-333333333333",
          "state": "ended",
          "startedAtMs": 1720000000000,
          "endedAtMs": 1720000002000,
          "eventCount": 2,
          "lastSequence": 2
        },
        {
          "id": "55555555-5555-4555-8555-555555555555",
          "state": "active",
          "startedAtMs": 1720000010000,
          "endedAtMs": null,
          "eventCount": 1,
          "lastSequence": 1
        }
      ]
    }
  ) satisfies SessionsListResponse,
  "rpc.session-export.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "session-export-1",
      "method": "session.export",
      "params": {
        "sessionId": "33333333-3333-4333-8333-333333333333",
        "limit": 1
      }
    }
  ) satisfies SessionExportRequest,
  "rpc.session-export.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "session-export-1",
      "result": {
        "session": {
          "id": "33333333-3333-4333-8333-333333333333",
          "state": "ended",
          "startedAtMs": 1720000000000,
          "endedAtMs": 1720000002000,
          "eventCount": 2,
          "lastSequence": 2
        },
        "events": [
          {
            "eventId": "66666666-6666-4666-8666-666666666661",
            "sessionId": "33333333-3333-4333-8333-333333333333",
            "sequence": 1,
            "requestId": "session-start-1",
            "atMs": 1720000000000,
            "payload": {
              "type": "sessionStarted"
            }
          }
        ],
        "nextAfterSequence": 1
      }
    }
  ) satisfies SessionExportResponse,
  "rpc.events-list.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "events-list-1",
      "method": "events.list",
      "params": {
        "sessionId": "33333333-3333-4333-8333-333333333333",
        "afterSequence": 1,
        "limit": 1
      }
    }
  ) satisfies EventsListRequest,
  "rpc.events-list.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "events-list-1",
      "result": [
        {
          "eventId": "66666666-6666-4666-8666-666666666662",
          "sessionId": "33333333-3333-4333-8333-333333333333",
          "sequence": 2,
          "requestId": "session-end-1",
          "atMs": 1720000002000,
          "payload": {
            "type": "sessionEnded",
            "outcome": "completed",
            "reason": "golden baseline complete"
          }
        }
      ]
    }
  ) satisfies EventsListResponse,
  "rpc.events-clear.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "events-clear-1",
      "method": "events.clear",
      "params": {
        "sessionId": "33333333-3333-4333-8333-333333333333"
      }
    }
  ) satisfies EventsClearRequest,
  "rpc.events-clear.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "events-clear-1",
      "result": {
        "deleted": true,
        "sessionId": "33333333-3333-4333-8333-333333333333"
      }
    }
  ) satisfies EventsClearResponse,
  "rpc.media-stream-capture.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "media-capture-1",
      "method": "media.stream.capture",
      "timeoutMs": 30000,
      "params": {
        "streamId": "77777777-7777-4777-8777-777777777777",
        "frameIndex": 1,
        "durationMs": 100
      }
    }
  ) satisfies MediaStreamCaptureRequest,
  "rpc.media-stream-capture.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "media-capture-1",
      "result": {
        "frame": {
          "streamId": "77777777-7777-4777-8777-777777777777",
          "frameIndex": 1,
          "keyFrame": true,
          "durationMs": 100,
          "evidence": {
            "id": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "mediaType": "image/png",
            "uri": "devicerail://assets/sha256/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
          }
        }
      }
    }
  ) satisfies MediaStreamCaptureResponse,
  "rpc.media-stream-end.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "media-end-1",
      "method": "media.stream.end",
      "params": {
        "streamId": "77777777-7777-4777-8777-777777777777"
      }
    }
  ) satisfies MediaStreamEndRequest,
  "rpc.media-stream-end.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "media-end-1",
      "result": {
        "streamId": "77777777-7777-4777-8777-777777777777",
        "frameCount": 1
      }
    }
  ) satisfies MediaStreamEndResponse,
  "rpc.media-stream-start.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "media-start-1",
      "method": "media.stream.start",
      "params": {
        "streamId": "77777777-7777-4777-8777-777777777777",
        "kind": "video"
      }
    }
  ) satisfies MediaStreamStartRequest,
  "rpc.media-stream-start.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "media-start-1",
      "result": {
        "stream": {
          "id": "77777777-7777-4777-8777-777777777777",
          "kind": "video",
          "mediaType": "image/png"
        }
      }
    }
  ) satisfies MediaStreamStartResponse,
  "rpc.failure.v1": (
    {
      "jsonrpc": "2.0",
      "id": "connect-1",
      "error": {
        "code": -32000,
        "message": "Device unavailable",
        "data": {
          "code": "device_unavailable",
          "message": "Device android-emulator-5554 is unavailable",
          "retryable": true,
          "details": {
            "deviceId": "android-emulator-5554",
            "reason": "offline"
          }
        }
      }
    }
  ) satisfies RpcResponse,
  "event.session-started.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000001",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 1,
      "atMs": 1720000000000,
      "payload": {
        "type": "sessionStarted"
      }
    }
  ) satisfies TestEvent,
  "event.observation-captured.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000002",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 2,
      "requestId": 42,
      "deviceId": "android-emulator-5554",
      "atMs": 1720000000123,
      "payload": {
        "type": "observationCaptured",
        "observation": {
          "id": "11111111-1111-4111-8111-111111111111",
          "deviceId": "android-emulator-5554",
          "capturedAtMs": 1720000000123,
          "viewport": {
            "width": 1080,
            "height": 2400,
            "scaleFactor": 2.5
          },
          "screenshot": {
            "id": "asset-screenshot-001",
            "mediaType": "image/png",
            "uri": "devicerail://assets/asset-screenshot-001",
            "sha256": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
          },
          "metadata": {
            "orientation": "portrait",
            "source": "golden-fixture"
          }
        }
      }
    }
  ) satisfies TestEvent,
  "event.action-started.succeeded.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000003",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 3,
      "requestId": "request-success",
      "deviceId": "android-emulator-5554",
      "atMs": 1720000001000,
      "payload": {
        "type": "actionStarted",
        "call": {
          "id": "22222222-2222-4222-8222-222222222222",
          "name": "tap",
          "arguments": {
            "x": 540,
            "y": 1200
          }
        }
      }
    }
  ) satisfies TestEvent,
  "event.action-completed.succeeded.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000004",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 4,
      "requestId": "request-success",
      "deviceId": "android-emulator-5554",
      "atMs": 1720000001500,
      "payload": {
        "type": "actionCompleted",
        "callId": "22222222-2222-4222-8222-222222222222",
        "outcome": {
          "outcome": "succeeded",
          "result": {
            "callId": "22222222-2222-4222-8222-222222222222",
            "startedAtMs": 1720000001000,
            "finishedAtMs": 1720000001500,
            "output": {
              "accepted": true
            },
            "before": null,
            "after": null,
            "evidence": [
              {
                "id": "asset-action-success-001",
                "mediaType": "image/png",
                "uri": "devicerail://assets/asset-action-success-001",
                "sha256": null
              }
            ]
          }
        }
      }
    }
  ) satisfies TestEvent,
  "event.action-started.failed.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000005",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 5,
      "requestId": "request-failure",
      "deviceId": "android-emulator-5554",
      "atMs": 1720000002000,
      "payload": {
        "type": "actionStarted",
        "call": {
          "id": "22222222-2222-4222-8222-222222222223",
          "name": "inputSecret",
          "arguments": null,
          "argumentsRedacted": true
        }
      }
    }
  ) satisfies TestEvent,
  "event.action-completed.failed.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000006",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 6,
      "requestId": "request-failure",
      "deviceId": "android-emulator-5554",
      "atMs": 1720000002100,
      "payload": {
        "type": "actionCompleted",
        "callId": "22222222-2222-4222-8222-222222222223",
        "outcome": {
          "outcome": "failed",
          "error": {
            "code": "invalid_arguments",
            "message": "inputSecret arguments are invalid",
            "retryable": false,
            "details": {
              "action": "inputSecret"
            }
          }
        }
      }
    }
  ) satisfies TestEvent,
  "event.action-started.cancelled.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000007",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 7,
      "requestId": "request-cancel",
      "deviceId": "android-emulator-5554",
      "atMs": 1720000002200,
      "payload": {
        "type": "actionStarted",
        "call": {
          "id": "22222222-2222-4222-8222-222222222224",
          "name": "swipe",
          "arguments": {
            "from": {
              "x": 540,
              "y": 1600
            },
            "to": {
              "x": 540,
              "y": 600
            }
          }
        }
      }
    }
  ) satisfies TestEvent,
  "event.action-completed.cancelled.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000008",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 8,
      "requestId": "request-cancel",
      "deviceId": "android-emulator-5554",
      "atMs": 1720000002300,
      "payload": {
        "type": "actionCompleted",
        "callId": "22222222-2222-4222-8222-222222222224",
        "outcome": {
          "outcome": "cancelled",
          "error": {
            "code": "action_cancelled",
            "message": "The caller cancelled the action",
            "retryable": false,
            "details": null
          }
        }
      }
    }
  ) satisfies TestEvent,
  "event.action-started.timed-out.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000009",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 9,
      "requestId": "request-timeout",
      "deviceId": "android-emulator-5554",
      "atMs": 1720000002400,
      "payload": {
        "type": "actionStarted",
        "call": {
          "id": "22222222-2222-4222-8222-222222222225",
          "name": "waitForIdle",
          "arguments": {
            "idleMs": 500
          }
        }
      }
    }
  ) satisfies TestEvent,
  "event.action-completed.timed-out.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000010",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 10,
      "requestId": "request-timeout",
      "deviceId": "android-emulator-5554",
      "atMs": 1720000032400,
      "payload": {
        "type": "actionCompleted",
        "callId": "22222222-2222-4222-8222-222222222225",
        "outcome": {
          "outcome": "timedOut",
          "error": {
            "code": "action_timeout",
            "message": "The action exceeded its 30000 ms deadline",
            "retryable": true,
            "details": {
              "callId": "22222222-2222-4222-8222-222222222225"
            }
          },
          "timeoutMs": 30000
        }
      }
    }
  ) satisfies TestEvent,
  "event.media-stream-started.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000014",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 11,
      "requestId": "media-start-1",
      "deviceId": "playwright-chromium-page-1",
      "atMs": 1720000032500,
      "payload": {
        "type": "mediaStreamStarted",
        "stream": {
          "id": "77777777-7777-4777-8777-777777777777",
          "kind": "video",
          "mediaType": "image/png"
        }
      }
    }
  ) satisfies TestEvent,
  "event.media-frame-captured.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000015",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 12,
      "requestId": "media-capture-1",
      "deviceId": "playwright-chromium-page-1",
      "atMs": 1720000032600,
      "payload": {
        "type": "mediaFrameCaptured",
        "frame": {
          "streamId": "77777777-7777-4777-8777-777777777777",
          "frameIndex": 1,
          "keyFrame": true,
          "durationMs": 100,
          "evidence": {
            "id": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "mediaType": "image/png",
            "uri": "devicerail://assets/sha256/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
          }
        }
      }
    }
  ) satisfies TestEvent,
  "event.media-stream-ended.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000016",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 13,
      "requestId": "media-end-1",
      "deviceId": "playwright-chromium-page-1",
      "atMs": 1720000032700,
      "payload": {
        "type": "mediaStreamEnded",
        "streamId": "77777777-7777-4777-8777-777777777777",
        "frameCount": 1
      }
    }
  ) satisfies TestEvent,
  "event.verdict-recorded.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000011",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 14,
      "deviceId": "android-emulator-5554",
      "atMs": 1720000032800,
      "payload": {
        "type": "verdictRecorded",
        "verdict": {
          "status": "fail",
          "summary": "The session includes failed, cancelled, and timed out actions.",
          "evidence": [
            {
              "id": "asset-verdict-001",
              "mediaType": "image/png",
              "uri": "devicerail://assets/asset-verdict-001",
              "sha256": null
            }
          ]
        }
      }
    }
  ) satisfies TestEvent,
  "event.error.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000012",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 15,
      "requestId": "request-timeout",
      "deviceId": "android-emulator-5554",
      "atMs": 1720000032900,
      "payload": {
        "type": "error",
        "error": {
          "code": "session_degraded",
          "message": "The session contains terminal action errors",
          "retryable": false,
          "details": {
            "failedActions": 3
          }
        }
      }
    }
  ) satisfies TestEvent,
  "stream.events-open.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "stream-open-1",
      "method": "events.stream.open",
      "params": {
        "sessionId": "33333333-3333-4333-8333-333333333333",
        "originPolicy": {
          "kind": "absent"
        }
      }
    }
  ) satisfies EventsStreamOpenRequest,
  "stream.events-open.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "stream-open-1",
      "result": {
        "endpoint": "ws://127.0.0.1:43123/v/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "streamEpoch": "66666666-6666-4666-8666-666666666666",
        "expiresAtMs": 1720000005000
      }
    }
  ) satisfies EventsStreamOpenResponse,
  "stream.websocket-hello.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "ws-hello-1",
      "method": "system.hello",
      "params": {
        "client": {
          "name": "fixture-stream-client",
          "version": "0.1.0"
        },
        "protocol": {
          "ranges": [
            {
              "major": 1,
              "minMinor": 3,
              "maxMinor": 3
            }
          ]
        },
        "features": {
          "required": [
            "events.stream.v1"
          ],
          "optional": []
        }
      }
    }
  ) satisfies SystemHelloRequest,
  "stream.websocket-hello.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "ws-hello-1",
      "result": {
        "connectionId": "55555555-5555-4555-8555-555555555555",
        "protocol": {
          "selected": {
            "major": 1,
            "minor": 3
          }
        },
        "server": {
          "name": "devicerail-daemon",
          "version": "0.1.0"
        },
        "transport": {
          "kind": "webSocket",
          "framing": "jsonMessage"
        },
        "features": {
          "enabled": [
            "events.stream.v1"
          ]
        }
      }
    }
  ) satisfies SystemHelloResponse,
  "stream.events-subscribe.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "subscribe-1",
      "method": "events.subscribe",
      "params": {
        "sessionId": "33333333-3333-4333-8333-333333333333",
        "afterCursor": {
          "streamEpoch": "66666666-6666-4666-8666-666666666666",
          "sessionId": "33333333-3333-4333-8333-333333333333",
          "sequence": 4
        }
      }
    }
  ) satisfies EventsSubscribeRequest,
  "stream.events-subscribe.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "subscribe-1",
      "result": {
        "subscriptionId": "77777777-7777-4777-8777-777777777777",
        "sessionId": "33333333-3333-4333-8333-333333333333",
        "replayThrough": {
          "streamEpoch": "66666666-6666-4666-8666-666666666666",
          "sessionId": "33333333-3333-4333-8333-333333333333",
          "sequence": 8
        },
        "sessionState": "active"
      }
    }
  ) satisfies EventsSubscribeResponse,
  "stream.event.notification.v1": (
    {
      "jsonrpc": "2.0",
      "method": "events.stream.event",
      "params": {
        "subscriptionId": "77777777-7777-4777-8777-777777777777",
        "cursor": {
          "streamEpoch": "66666666-6666-4666-8666-666666666666",
          "sessionId": "33333333-3333-4333-8333-333333333333",
          "sequence": 5
        },
        "event": {
          "eventId": "44444444-4444-4444-8444-000000000005",
          "sessionId": "33333333-3333-4333-8333-333333333333",
          "sequence": 5,
          "requestId": "request-failure",
          "deviceId": "android-emulator-5554",
          "atMs": 1720000002000,
          "payload": {
            "type": "actionStarted",
            "call": {
              "id": "22222222-2222-4222-8222-222222222223",
              "name": "inputSecret",
              "arguments": null,
              "argumentsRedacted": true
            }
          }
        }
      }
    }
  ) satisfies EventsStreamEventNotification,
  "stream.terminal.slow-consumer.notification.v1": (
    {
      "jsonrpc": "2.0",
      "method": "events.stream.terminal",
      "params": {
        "subscriptionId": "77777777-7777-4777-8777-777777777777",
        "sessionId": "33333333-3333-4333-8333-333333333333",
        "lastEmittedCursor": {
          "streamEpoch": "66666666-6666-4666-8666-666666666666",
          "sessionId": "33333333-3333-4333-8333-333333333333",
          "sequence": 8
        },
        "termination": {
          "reason": "slowConsumer",
          "error": {
            "code": "stream_slow_consumer",
            "message": "subscriber exceeded its bounded queue",
            "retryable": true,
            "details": null
          }
        }
      }
    }
  ) satisfies EventsStreamTerminalNotification,
  "event.session-ended.v1": (
    {
      "eventId": "44444444-4444-4444-8444-000000000013",
      "sessionId": "33333333-3333-4333-8333-333333333333",
      "sequence": 16,
      "atMs": 1720000033000,
      "payload": {
        "type": "sessionEnded",
        "outcome": "failed",
        "reason": "The golden session exercises every terminal action outcome."
      }
    }
  ) satisfies TestEvent,
  "ui.snapshot.v1": (
    {
      "formatVersion": 1,
      "observationId": "55555555-5555-4555-8555-555555555555",
      "context": {
        "contextKind": "native",
        "contextId": "NATIVE_APP",
        "documentEpoch": "native-epoch-1"
      },
      "rootStableNodeIds": [
        "root"
      ],
      "nodes": [
        {
          "stableNodeId": "root",
          "parentStableNodeId": null,
          "role": "application",
          "name": "Safari",
          "value": null,
          "identifier": null,
          "text": null,
          "bounds": {
            "x": 0,
            "y": 0,
            "width": 393,
            "height": 852
          },
          "enabled": true,
          "hittable": null
        },
        {
          "stableNodeId": "search-button",
          "parentStableNodeId": "root",
          "role": "button",
          "name": "百度一下",
          "value": null,
          "identifier": "search-button",
          "text": "百度一下",
          "bounds": {
            "x": 290,
            "y": 210,
            "width": 88,
            "height": 44
          },
          "enabled": true,
          "hittable": true
        }
      ]
    }
  ) satisfies UiSnapshot,
  "action.find-element.arguments.v1": (
    {
      "selector": {
        "context": {
          "contextKind": "native",
          "contextId": "NATIVE_APP"
        },
        "role": "button",
        "name": "百度一下",
        "value": null,
        "identifier": "search-button",
        "text": null,
        "css": null
      }
    }
  ) satisfies FindElementArguments,
  "action.find-element.result.v1": (
    {
      "element": {
        "observationId": "55555555-5555-4555-8555-555555555555",
        "context": {
          "contextKind": "native",
          "contextId": "NATIVE_APP",
          "documentEpoch": "native-epoch-1"
        },
        "stableNodeId": "search-button"
      }
    }
  ) satisfies FindElementResult,
  "action.tap-element.arguments.v1": (
    {
      "target": {
        "kind": "node",
        "node": {
          "observationId": "55555555-5555-4555-8555-555555555555",
          "context": {
            "contextKind": "native",
            "contextId": "NATIVE_APP",
            "documentEpoch": "native-epoch-1"
          },
          "stableNodeId": "search-button"
        }
      }
    }
  ) satisfies TapElementArguments,
  "action.tap-element.result.v1": (
    {
      "element": {
        "observationId": "55555555-5555-4555-8555-555555555555",
        "context": {
          "contextKind": "native",
          "contextId": "NATIVE_APP",
          "documentEpoch": "native-epoch-1"
        },
        "stableNodeId": "search-button"
      }
    }
  ) satisfies TapElementResult,
  "action.clear-element.arguments.v1": (
    {
      "target": {
        "kind": "selector",
        "selector": {
          "context": {
            "contextKind": "native",
            "contextId": "NATIVE_APP"
          },
          "role": "textField",
          "name": null,
          "value": null,
          "identifier": "search-input",
          "text": null,
          "css": null
        }
      }
    }
  ) satisfies ClearElementArguments,
  "action.clear-element.result.v1": (
    {
      "element": {
        "observationId": "55555555-5555-4555-8555-555555555555",
        "context": {
          "contextKind": "native",
          "contextId": "NATIVE_APP",
          "documentEpoch": "native-epoch-1"
        },
        "stableNodeId": "search-input"
      }
    }
  ) satisfies ClearElementResult,
  "action.set-element-value.arguments.v1": (
    {
      "target": {
        "kind": "node",
        "node": {
          "observationId": "55555555-5555-4555-8555-555555555555",
          "context": {
            "contextKind": "native",
            "contextId": "NATIVE_APP",
            "documentEpoch": "native-epoch-1"
          },
          "stableNodeId": "search-input"
        }
      },
      "value": "123"
    }
  ) satisfies SetElementValueArguments,
  "action.set-element-value.result.v1": (
    {
      "element": {
        "observationId": "55555555-5555-4555-8555-555555555555",
        "context": {
          "contextKind": "native",
          "contextId": "NATIVE_APP",
          "documentEpoch": "native-epoch-1"
        },
        "stableNodeId": "search-input"
      }
    }
  ) satisfies SetElementValueResult,
  "action.wait-for-element.arguments.v1": (
    {
      "selector": {
        "context": {
          "contextKind": "web",
          "contextId": "WEBVIEW_com.apple.mobilesafari"
        },
        "role": null,
        "name": null,
        "value": null,
        "identifier": null,
        "text": null,
        "css": "#loading"
      },
      "condition": "absent"
    }
  ) satisfies WaitForElementArguments,
  "action.wait-for-element.result.v1": (
    {
      "matched": true,
      "condition": "absent",
      "element": null
    }
  ) satisfies WaitForElementResult,
  "rpc.ui-snapshot-get.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "ui-snapshot-1",
      "method": "ui.snapshot.get",
      "params": {
        "observationId": "55555555-5555-4555-8555-555555555555"
      }
    }
  ) satisfies UiSnapshotGetRequest,
  "rpc.ui-snapshot-get.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "ui-snapshot-1",
      "result": {
        "formatVersion": 1,
        "observationId": "55555555-5555-4555-8555-555555555555",
        "context": {
          "contextKind": "native",
          "contextId": "NATIVE_APP",
          "documentEpoch": "native-epoch-1"
        },
        "rootStableNodeIds": [
          "root"
        ],
        "nodes": [
          {
            "stableNodeId": "root",
            "parentStableNodeId": null,
            "role": "application",
            "name": "Safari",
            "value": null,
            "identifier": null,
            "text": null,
            "bounds": {
              "x": 0,
              "y": 0,
              "width": 393,
              "height": 852
            },
            "enabled": true,
            "hittable": null
          },
          {
            "stableNodeId": "search-button",
            "parentStableNodeId": "root",
            "role": "button",
            "name": "百度一下",
            "value": null,
            "identifier": "search-button",
            "text": "百度一下",
            "bounds": {
              "x": 290,
              "y": 210,
              "width": 88,
              "height": 44
            },
            "enabled": true,
            "hittable": true
          }
        ]
      }
    }
  ) satisfies UiSnapshotGetResponse,
  "rpc.verdict-record.request.v1": (
    {
      "jsonrpc": "2.0",
      "id": "verdict-1",
      "method": "verdict.record",
      "params": {
        "verdict": {
          "status": "pass",
          "summary": "The expected search result is present.",
          "evidence": []
        }
      }
    }
  ) satisfies VerdictRecordRequest,
  "rpc.verdict-record.response.v1": (
    {
      "jsonrpc": "2.0",
      "id": "verdict-1",
      "result": {
        "event": {
          "eventId": "66666666-6666-4666-8666-666666666666",
          "sessionId": "33333333-3333-4333-8333-333333333333",
          "sequence": 15,
          "requestId": "verdict-1",
          "deviceId": "ios-00008140",
          "atMs": 1720000033000,
          "payload": {
            "type": "verdictRecorded",
            "verdict": {
              "status": "pass",
              "summary": "The expected search result is present.",
              "evidence": []
            }
          }
        }
      }
    }
  ) satisfies VerdictRecordResponse,
};
