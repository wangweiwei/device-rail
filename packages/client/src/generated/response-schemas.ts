/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm --filter @devicerail/client runtime-schemas:generate` from the repository root.
 */

import type { RpcMethod } from "@devicerail/protocol";

export const RPC_RESPONSE_SCHEMAS: Readonly<Record<RpcMethod, unknown>> = {
  "device.capabilities": {
    "$defs": {
      "ActionDefinition": {
        "properties": {
          "description": {
            "type": "string"
          },
          "inputSchema": true,
          "name": {
            "type": "string"
          },
          "protection": {
            "$ref": "#/$defs/ActionProtection"
          }
        },
        "required": [
          "name",
          "description",
          "inputSchema"
        ],
        "type": "object"
      },
      "ActionProtection": {
        "enum": [
          "standard",
          "protected"
        ],
        "type": "string"
      },
      "DeviceCapabilitiesSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "items": {
              "$ref": "#/$defs/ActionDefinition"
            },
            "type": "array"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:device-capabilities-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/DeviceCapabilitiesSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "DeviceCapabilitiesResponse"
  },
  "device.connect": {
    "$defs": {
      "DeviceConnectSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/DeviceInfo"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "DeviceInfo": {
        "properties": {
          "connected": {
            "type": "boolean"
          },
          "id": {
            "type": "string"
          },
          "name": {
            "type": "string"
          },
          "osVersion": {
            "type": [
              "string",
              "null"
            ]
          },
          "platform": {
            "$ref": "#/$defs/Platform"
          }
        },
        "required": [
          "id",
          "name",
          "platform",
          "connected"
        ],
        "type": "object"
      },
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "Platform": {
        "oneOf": [
          {
            "properties": {
              "kind": {
                "const": "web",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "android",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "ios",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "harmonyOs",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "macOs",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "windows",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "linux",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "rdp",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "mock",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "other",
                "type": "string"
              },
              "value": {
                "type": "string"
              }
            },
            "required": [
              "kind",
              "value"
            ],
            "type": "object"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:device-connect-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/DeviceConnectSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "DeviceConnectResponse"
  },
  "device.disconnect": {
    "$defs": {
      "DeviceDisconnectResult": {
        "additionalProperties": false,
        "description": "Result returned by `device.disconnect`.",
        "properties": {
          "disconnected": {
            "type": "boolean"
          }
        },
        "required": [
          "disconnected"
        ],
        "type": "object"
      },
      "DeviceDisconnectSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/DeviceDisconnectResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:device-disconnect-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/DeviceDisconnectSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "DeviceDisconnectResponse"
  },
  "device.execute": {
    "$defs": {
      "ActionExecution": {
        "oneOf": [
          {
            "additionalProperties": false,
            "properties": {
              "context": {
                "$ref": "#/$defs/UiContextRef"
              },
              "mode": {
                "const": "nativeSemantic",
                "type": "string"
              }
            },
            "required": [
              "mode",
              "context"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "context": {
                "$ref": "#/$defs/UiContextRef"
              },
              "mode": {
                "const": "webSemantic",
                "type": "string"
              }
            },
            "required": [
              "mode",
              "context"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "context": {
                "$ref": "#/$defs/UiContextRef"
              },
              "fallbackReason": {
                "$ref": "#/$defs/CoordinateFallbackReason"
              },
              "mode": {
                "const": "coordinateFallback",
                "type": "string"
              }
            },
            "required": [
              "mode",
              "context",
              "fallbackReason"
            ],
            "type": "object"
          }
        ]
      },
      "ActionResult": {
        "properties": {
          "after": {
            "anyOf": [
              {
                "$ref": "#/$defs/Observation"
              },
              {
                "type": "null"
              }
            ]
          },
          "before": {
            "anyOf": [
              {
                "$ref": "#/$defs/Observation"
              },
              {
                "type": "null"
              }
            ]
          },
          "callId": {
            "format": "uuid",
            "type": "string"
          },
          "evidence": {
            "default": [],
            "items": {
              "$ref": "#/$defs/AssetRef"
            },
            "type": "array"
          },
          "execution": {
            "anyOf": [
              {
                "$ref": "#/$defs/ActionExecution"
              },
              {
                "type": "null"
              }
            ]
          },
          "finishedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "output": true,
          "startedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        },
        "required": [
          "callId",
          "startedAtMs",
          "finishedAtMs",
          "output"
        ],
        "type": "object"
      },
      "AssetRef": {
        "properties": {
          "id": {
            "type": "string"
          },
          "mediaType": {
            "type": "string"
          },
          "sha256": {
            "type": [
              "string",
              "null"
            ]
          },
          "uri": {
            "type": "string"
          }
        },
        "required": [
          "id",
          "mediaType",
          "uri"
        ],
        "type": "object"
      },
      "CoordinateFallbackReason": {
        "enum": [
          "semanticInteractionUnavailable",
          "platformLimitation"
        ],
        "type": "string"
      },
      "DeviceExecuteSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/ActionResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "Observation": {
        "properties": {
          "capturedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "deviceId": {
            "type": "string"
          },
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "metadata": {
            "additionalProperties": true,
            "default": {},
            "type": "object"
          },
          "screenshot": {
            "anyOf": [
              {
                "$ref": "#/$defs/AssetRef"
              },
              {
                "type": "null"
              }
            ]
          },
          "screenshotOmission": {
            "anyOf": [
              {
                "$ref": "#/$defs/ScreenshotOmissionReason"
              },
              {
                "type": "null"
              }
            ]
          },
          "uiSnapshot": {
            "anyOf": [
              {
                "$ref": "#/$defs/UiSnapshotRef"
              },
              {
                "type": "null"
              }
            ]
          },
          "uiSnapshotOmission": {
            "anyOf": [
              {
                "$ref": "#/$defs/UiSnapshotOmissionReason"
              },
              {
                "type": "null"
              }
            ]
          },
          "viewport": {
            "$ref": "#/$defs/Viewport"
          }
        },
        "required": [
          "id",
          "deviceId",
          "capturedAtMs",
          "viewport"
        ],
        "type": "object"
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "ScreenshotOmissionReason": {
        "enum": [
          "policy",
          "protectedAction"
        ],
        "type": "string"
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      },
      "UiContextKind": {
        "enum": [
          "native",
          "web"
        ],
        "type": "string"
      },
      "UiContextRef": {
        "additionalProperties": false,
        "description": "Full identity of one native accessibility or web-document context.\n`documentEpoch` is required for both channels and changes after reconnect,\nnavigation, or any replacement that invalidates prior node references.",
        "properties": {
          "contextId": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          },
          "contextKind": {
            "$ref": "#/$defs/UiContextKind"
          },
          "documentEpoch": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          }
        },
        "required": [
          "contextKind",
          "contextId",
          "documentEpoch"
        ],
        "type": "object"
      },
      "UiSnapshotOmissionReason": {
        "enum": [
          "driverUnsupported",
          "policy",
          "protectedAction"
        ],
        "type": "string"
      },
      "UiSnapshotRef": {
        "additionalProperties": false,
        "description": "Small Observation-side reference to a UI Tree Evidence object.",
        "properties": {
          "byteLength": {
            "format": "uint64",
            "maximum": 786432,
            "minimum": 1,
            "type": "integer"
          },
          "context": {
            "$ref": "#/$defs/UiContextRef"
          },
          "evidence": {
            "$ref": "#/$defs/AssetRef"
          },
          "formatVersion": {
            "format": "uint16",
            "maximum": 1,
            "minimum": 1,
            "type": "integer"
          },
          "nodeCount": {
            "format": "uint32",
            "maximum": 10000,
            "minimum": 1,
            "type": "integer"
          }
        },
        "required": [
          "formatVersion",
          "context",
          "nodeCount",
          "byteLength",
          "evidence"
        ],
        "type": "object"
      },
      "Viewport": {
        "properties": {
          "height": {
            "format": "uint32",
            "maximum": 4294967295,
            "minimum": 0,
            "type": "integer"
          },
          "scaleFactor": {
            "format": "double",
            "type": "number"
          },
          "width": {
            "format": "uint32",
            "maximum": 4294967295,
            "minimum": 0,
            "type": "integer"
          }
        },
        "required": [
          "width",
          "height",
          "scaleFactor"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:device-execute-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/DeviceExecuteSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "DeviceExecuteResponse"
  },
  "device.observe": {
    "$defs": {
      "AssetRef": {
        "properties": {
          "id": {
            "type": "string"
          },
          "mediaType": {
            "type": "string"
          },
          "sha256": {
            "type": [
              "string",
              "null"
            ]
          },
          "uri": {
            "type": "string"
          }
        },
        "required": [
          "id",
          "mediaType",
          "uri"
        ],
        "type": "object"
      },
      "DeviceObserveSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/Observation"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "Observation": {
        "properties": {
          "capturedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "deviceId": {
            "type": "string"
          },
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "metadata": {
            "additionalProperties": true,
            "default": {},
            "type": "object"
          },
          "screenshot": {
            "anyOf": [
              {
                "$ref": "#/$defs/AssetRef"
              },
              {
                "type": "null"
              }
            ]
          },
          "screenshotOmission": {
            "anyOf": [
              {
                "$ref": "#/$defs/ScreenshotOmissionReason"
              },
              {
                "type": "null"
              }
            ]
          },
          "uiSnapshot": {
            "anyOf": [
              {
                "$ref": "#/$defs/UiSnapshotRef"
              },
              {
                "type": "null"
              }
            ]
          },
          "uiSnapshotOmission": {
            "anyOf": [
              {
                "$ref": "#/$defs/UiSnapshotOmissionReason"
              },
              {
                "type": "null"
              }
            ]
          },
          "viewport": {
            "$ref": "#/$defs/Viewport"
          }
        },
        "required": [
          "id",
          "deviceId",
          "capturedAtMs",
          "viewport"
        ],
        "type": "object"
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "ScreenshotOmissionReason": {
        "enum": [
          "policy",
          "protectedAction"
        ],
        "type": "string"
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      },
      "UiContextKind": {
        "enum": [
          "native",
          "web"
        ],
        "type": "string"
      },
      "UiContextRef": {
        "additionalProperties": false,
        "description": "Full identity of one native accessibility or web-document context.\n`documentEpoch` is required for both channels and changes after reconnect,\nnavigation, or any replacement that invalidates prior node references.",
        "properties": {
          "contextId": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          },
          "contextKind": {
            "$ref": "#/$defs/UiContextKind"
          },
          "documentEpoch": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          }
        },
        "required": [
          "contextKind",
          "contextId",
          "documentEpoch"
        ],
        "type": "object"
      },
      "UiSnapshotOmissionReason": {
        "enum": [
          "driverUnsupported",
          "policy",
          "protectedAction"
        ],
        "type": "string"
      },
      "UiSnapshotRef": {
        "additionalProperties": false,
        "description": "Small Observation-side reference to a UI Tree Evidence object.",
        "properties": {
          "byteLength": {
            "format": "uint64",
            "maximum": 786432,
            "minimum": 1,
            "type": "integer"
          },
          "context": {
            "$ref": "#/$defs/UiContextRef"
          },
          "evidence": {
            "$ref": "#/$defs/AssetRef"
          },
          "formatVersion": {
            "format": "uint16",
            "maximum": 1,
            "minimum": 1,
            "type": "integer"
          },
          "nodeCount": {
            "format": "uint32",
            "maximum": 10000,
            "minimum": 1,
            "type": "integer"
          }
        },
        "required": [
          "formatVersion",
          "context",
          "nodeCount",
          "byteLength",
          "evidence"
        ],
        "type": "object"
      },
      "Viewport": {
        "properties": {
          "height": {
            "format": "uint32",
            "maximum": 4294967295,
            "minimum": 0,
            "type": "integer"
          },
          "scaleFactor": {
            "format": "double",
            "type": "number"
          },
          "width": {
            "format": "uint32",
            "maximum": 4294967295,
            "minimum": 0,
            "type": "integer"
          }
        },
        "required": [
          "width",
          "height",
          "scaleFactor"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:device-observe-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/DeviceObserveSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "DeviceObserveResponse"
  },
  "device.select": {
    "$defs": {
      "DeviceInfo": {
        "properties": {
          "connected": {
            "type": "boolean"
          },
          "id": {
            "type": "string"
          },
          "name": {
            "type": "string"
          },
          "osVersion": {
            "type": [
              "string",
              "null"
            ]
          },
          "platform": {
            "$ref": "#/$defs/Platform"
          }
        },
        "required": [
          "id",
          "name",
          "platform",
          "connected"
        ],
        "type": "object"
      },
      "DeviceSelectResult": {
        "additionalProperties": false,
        "description": "Result returned by `device.select`.",
        "properties": {
          "device": {
            "$ref": "#/$defs/DeviceInfo"
          }
        },
        "required": [
          "device"
        ],
        "type": "object"
      },
      "DeviceSelectSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/DeviceSelectResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "Platform": {
        "oneOf": [
          {
            "properties": {
              "kind": {
                "const": "web",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "android",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "ios",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "harmonyOs",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "macOs",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "windows",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "linux",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "rdp",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "mock",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "other",
                "type": "string"
              },
              "value": {
                "type": "string"
              }
            },
            "required": [
              "kind",
              "value"
            ],
            "type": "object"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:device-select-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/DeviceSelectSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "DeviceSelectResponse"
  },
  "devices.list": {
    "$defs": {
      "DeviceInfo": {
        "properties": {
          "connected": {
            "type": "boolean"
          },
          "id": {
            "type": "string"
          },
          "name": {
            "type": "string"
          },
          "osVersion": {
            "type": [
              "string",
              "null"
            ]
          },
          "platform": {
            "$ref": "#/$defs/Platform"
          }
        },
        "required": [
          "id",
          "name",
          "platform",
          "connected"
        ],
        "type": "object"
      },
      "DevicesListResult": {
        "additionalProperties": false,
        "description": "Result returned by `devices.list`.",
        "properties": {
          "devices": {
            "items": {
              "$ref": "#/$defs/DeviceInfo"
            },
            "type": "array"
          },
          "selectedDeviceId": {
            "type": [
              "string",
              "null"
            ]
          }
        },
        "required": [
          "devices"
        ],
        "type": "object"
      },
      "DevicesListSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/DevicesListResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "Platform": {
        "oneOf": [
          {
            "properties": {
              "kind": {
                "const": "web",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "android",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "ios",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "harmonyOs",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "macOs",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "windows",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "linux",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "rdp",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "mock",
                "type": "string"
              }
            },
            "required": [
              "kind"
            ],
            "type": "object"
          },
          {
            "properties": {
              "kind": {
                "const": "other",
                "type": "string"
              },
              "value": {
                "type": "string"
              }
            },
            "required": [
              "kind",
              "value"
            ],
            "type": "object"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:devices-list-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/DevicesListSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "DevicesListResponse"
  },
  "events.clear": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "EventsClearResult": {
        "additionalProperties": false,
        "description": "Result returned by `events.clear`.",
        "properties": {
          "deleted": {
            "type": "boolean"
          },
          "sessionId": {
            "format": "uuid",
            "type": "string"
          }
        },
        "required": [
          "deleted",
          "sessionId"
        ],
        "type": "object"
      },
      "EventsClearSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/EventsClearResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:events-clear-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/EventsClearSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "EventsClearResponse"
  },
  "events.list": {
    "$defs": {
      "ActionExecution": {
        "oneOf": [
          {
            "additionalProperties": false,
            "properties": {
              "context": {
                "$ref": "#/$defs/UiContextRef"
              },
              "mode": {
                "const": "nativeSemantic",
                "type": "string"
              }
            },
            "required": [
              "mode",
              "context"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "context": {
                "$ref": "#/$defs/UiContextRef"
              },
              "mode": {
                "const": "webSemantic",
                "type": "string"
              }
            },
            "required": [
              "mode",
              "context"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "context": {
                "$ref": "#/$defs/UiContextRef"
              },
              "fallbackReason": {
                "$ref": "#/$defs/CoordinateFallbackReason"
              },
              "mode": {
                "const": "coordinateFallback",
                "type": "string"
              }
            },
            "required": [
              "mode",
              "context",
              "fallbackReason"
            ],
            "type": "object"
          }
        ]
      },
      "ActionOutcome": {
        "description": "The terminal outcome for one action call.\n\nKeeping the four outcomes structurally distinct prevents clients from\nhaving to infer timeout or cancellation from human-readable error text.",
        "oneOf": [
          {
            "additionalProperties": false,
            "properties": {
              "outcome": {
                "const": "succeeded",
                "type": "string"
              },
              "result": {
                "$ref": "#/$defs/ActionResult"
              }
            },
            "required": [
              "outcome",
              "result"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "error": {
                "$ref": "#/$defs/ErrorInfo"
              },
              "outcome": {
                "const": "failed",
                "type": "string"
              }
            },
            "required": [
              "outcome",
              "error"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "error": {
                "$ref": "#/$defs/ErrorInfo"
              },
              "outcome": {
                "const": "cancelled",
                "type": "string"
              }
            },
            "required": [
              "outcome",
              "error"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "error": {
                "$ref": "#/$defs/ErrorInfo"
              },
              "outcome": {
                "const": "timedOut",
                "type": "string"
              },
              "timeoutMs": {
                "format": "uint64",
                "maximum": 9007199254740991,
                "minimum": 0,
                "type": "integer"
              }
            },
            "required": [
              "outcome",
              "error",
              "timeoutMs"
            ],
            "type": "object"
          }
        ]
      },
      "ActionResult": {
        "properties": {
          "after": {
            "anyOf": [
              {
                "$ref": "#/$defs/Observation"
              },
              {
                "type": "null"
              }
            ]
          },
          "before": {
            "anyOf": [
              {
                "$ref": "#/$defs/Observation"
              },
              {
                "type": "null"
              }
            ]
          },
          "callId": {
            "format": "uuid",
            "type": "string"
          },
          "evidence": {
            "default": [],
            "items": {
              "$ref": "#/$defs/AssetRef"
            },
            "type": "array"
          },
          "execution": {
            "anyOf": [
              {
                "$ref": "#/$defs/ActionExecution"
              },
              {
                "type": "null"
              }
            ]
          },
          "finishedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "output": true,
          "startedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        },
        "required": [
          "callId",
          "startedAtMs",
          "finishedAtMs",
          "output"
        ],
        "type": "object"
      },
      "AssetRef": {
        "properties": {
          "id": {
            "type": "string"
          },
          "mediaType": {
            "type": "string"
          },
          "sha256": {
            "type": [
              "string",
              "null"
            ]
          },
          "uri": {
            "type": "string"
          }
        },
        "required": [
          "id",
          "mediaType",
          "uri"
        ],
        "type": "object"
      },
      "CoordinateFallbackReason": {
        "enum": [
          "semanticInteractionUnavailable",
          "platformLimitation"
        ],
        "type": "string"
      },
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "EventSequence": {
        "description": "A one-based sequence number within one session.\n\nThe wire value is capped at JavaScript's maximum safe integer so generated\nclients can sort and resume event streams without losing precision.",
        "format": "uint64",
        "maximum": 9007199254740991,
        "minimum": 1,
        "type": "integer"
      },
      "EventsListSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "items": {
              "$ref": "#/$defs/TestEvent"
            },
            "type": "array"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "MediaFrame": {
        "additionalProperties": false,
        "properties": {
          "durationMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": [
              "integer",
              "null"
            ]
          },
          "evidence": {
            "$ref": "#/$defs/AssetRef"
          },
          "frameIndex": {
            "$ref": "#/$defs/EventSequence"
          },
          "keyFrame": {
            "type": "boolean"
          },
          "streamId": {
            "format": "uuid",
            "type": "string"
          }
        },
        "required": [
          "streamId",
          "frameIndex",
          "evidence"
        ],
        "type": "object"
      },
      "MediaStreamInfo": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "kind": {
            "$ref": "#/$defs/MediaStreamKind"
          },
          "mediaType": {
            "maxLength": 255,
            "minLength": 1,
            "type": "string"
          },
          "viewport": {
            "anyOf": [
              {
                "$ref": "#/$defs/Viewport"
              },
              {
                "type": "null"
              }
            ]
          }
        },
        "required": [
          "id",
          "kind",
          "mediaType"
        ],
        "type": "object"
      },
      "MediaStreamKind": {
        "enum": [
          "screenshot",
          "video"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "Observation": {
        "properties": {
          "capturedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "deviceId": {
            "type": "string"
          },
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "metadata": {
            "additionalProperties": true,
            "default": {},
            "type": "object"
          },
          "screenshot": {
            "anyOf": [
              {
                "$ref": "#/$defs/AssetRef"
              },
              {
                "type": "null"
              }
            ]
          },
          "screenshotOmission": {
            "anyOf": [
              {
                "$ref": "#/$defs/ScreenshotOmissionReason"
              },
              {
                "type": "null"
              }
            ]
          },
          "uiSnapshot": {
            "anyOf": [
              {
                "$ref": "#/$defs/UiSnapshotRef"
              },
              {
                "type": "null"
              }
            ]
          },
          "uiSnapshotOmission": {
            "anyOf": [
              {
                "$ref": "#/$defs/UiSnapshotOmissionReason"
              },
              {
                "type": "null"
              }
            ]
          },
          "viewport": {
            "$ref": "#/$defs/Viewport"
          }
        },
        "required": [
          "id",
          "deviceId",
          "capturedAtMs",
          "viewport"
        ],
        "type": "object"
      },
      "RecordedActionCall": {
        "description": "Durable representation of an Action invocation.\n\nStandard calls preserve the historical wire shape. Protected and unknown\ncalls retain only correlation fields and serialize `arguments` as `null`\nwith an explicit `argumentsRedacted` marker.",
        "properties": {
          "arguments": {
            "default": null
          },
          "argumentsRedacted": {
            "type": "boolean"
          },
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "name": {
            "type": "string"
          }
        },
        "required": [
          "id",
          "name"
        ],
        "type": "object"
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "ScreenshotOmissionReason": {
        "enum": [
          "policy",
          "protectedAction"
        ],
        "type": "string"
      },
      "SessionOutcome": {
        "enum": [
          "completed",
          "failed",
          "cancelled",
          "shutdown"
        ],
        "type": "string"
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      },
      "TestEvent": {
        "additionalProperties": false,
        "properties": {
          "atMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "deviceId": {
            "type": [
              "string",
              "null"
            ]
          },
          "eventId": {
            "format": "uuid",
            "type": "string"
          },
          "payload": {
            "$ref": "#/$defs/TestEventPayload"
          },
          "requestId": {
            "anyOf": [
              {
                "$ref": "#/$defs/RpcIdSchema"
              },
              {
                "type": "null"
              }
            ]
          },
          "sequence": {
            "$ref": "#/$defs/EventSequence"
          },
          "sessionId": {
            "format": "uuid",
            "type": "string"
          }
        },
        "required": [
          "eventId",
          "sessionId",
          "sequence",
          "atMs",
          "payload"
        ],
        "type": "object"
      },
      "TestEventPayload": {
        "oneOf": [
          {
            "additionalProperties": false,
            "properties": {
              "type": {
                "const": "sessionStarted",
                "type": "string"
              }
            },
            "required": [
              "type"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "outcome": {
                "$ref": "#/$defs/SessionOutcome"
              },
              "reason": {
                "type": [
                  "string",
                  "null"
                ]
              },
              "type": {
                "const": "sessionEnded",
                "type": "string"
              }
            },
            "required": [
              "type",
              "outcome"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "observation": {
                "$ref": "#/$defs/Observation"
              },
              "type": {
                "const": "observationCaptured",
                "type": "string"
              }
            },
            "required": [
              "type",
              "observation"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "call": {
                "$ref": "#/$defs/RecordedActionCall"
              },
              "type": {
                "const": "actionStarted",
                "type": "string"
              }
            },
            "required": [
              "type",
              "call"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "callId": {
                "format": "uuid",
                "type": "string"
              },
              "outcome": {
                "$ref": "#/$defs/ActionOutcome"
              },
              "type": {
                "const": "actionCompleted",
                "type": "string"
              }
            },
            "required": [
              "type",
              "callId",
              "outcome"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "stream": {
                "$ref": "#/$defs/MediaStreamInfo"
              },
              "type": {
                "const": "mediaStreamStarted",
                "type": "string"
              }
            },
            "required": [
              "type",
              "stream"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "frame": {
                "$ref": "#/$defs/MediaFrame"
              },
              "type": {
                "const": "mediaFrameCaptured",
                "type": "string"
              }
            },
            "required": [
              "type",
              "frame"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "frameCount": {
                "format": "uint64",
                "maximum": 9007199254740991,
                "minimum": 0,
                "type": "integer"
              },
              "streamId": {
                "format": "uuid",
                "type": "string"
              },
              "type": {
                "const": "mediaStreamEnded",
                "type": "string"
              }
            },
            "required": [
              "type",
              "streamId",
              "frameCount"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "type": {
                "const": "verdictRecorded",
                "type": "string"
              },
              "verdict": {
                "$ref": "#/$defs/Verdict"
              }
            },
            "required": [
              "type",
              "verdict"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "error": {
                "$ref": "#/$defs/ErrorInfo"
              },
              "type": {
                "const": "error",
                "type": "string"
              }
            },
            "required": [
              "type",
              "error"
            ],
            "type": "object"
          }
        ]
      },
      "UiContextKind": {
        "enum": [
          "native",
          "web"
        ],
        "type": "string"
      },
      "UiContextRef": {
        "additionalProperties": false,
        "description": "Full identity of one native accessibility or web-document context.\n`documentEpoch` is required for both channels and changes after reconnect,\nnavigation, or any replacement that invalidates prior node references.",
        "properties": {
          "contextId": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          },
          "contextKind": {
            "$ref": "#/$defs/UiContextKind"
          },
          "documentEpoch": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          }
        },
        "required": [
          "contextKind",
          "contextId",
          "documentEpoch"
        ],
        "type": "object"
      },
      "UiSnapshotOmissionReason": {
        "enum": [
          "driverUnsupported",
          "policy",
          "protectedAction"
        ],
        "type": "string"
      },
      "UiSnapshotRef": {
        "additionalProperties": false,
        "description": "Small Observation-side reference to a UI Tree Evidence object.",
        "properties": {
          "byteLength": {
            "format": "uint64",
            "maximum": 786432,
            "minimum": 1,
            "type": "integer"
          },
          "context": {
            "$ref": "#/$defs/UiContextRef"
          },
          "evidence": {
            "$ref": "#/$defs/AssetRef"
          },
          "formatVersion": {
            "format": "uint16",
            "maximum": 1,
            "minimum": 1,
            "type": "integer"
          },
          "nodeCount": {
            "format": "uint32",
            "maximum": 10000,
            "minimum": 1,
            "type": "integer"
          }
        },
        "required": [
          "formatVersion",
          "context",
          "nodeCount",
          "byteLength",
          "evidence"
        ],
        "type": "object"
      },
      "Verdict": {
        "additionalProperties": false,
        "properties": {
          "evidence": {
            "default": [],
            "items": {
              "$ref": "#/$defs/AssetRef"
            },
            "maxItems": 64,
            "type": "array"
          },
          "status": {
            "$ref": "#/$defs/VerdictStatus"
          },
          "summary": {
            "maxLength": 16384,
            "minLength": 1,
            "type": "string"
          }
        },
        "required": [
          "status",
          "summary"
        ],
        "type": "object"
      },
      "VerdictStatus": {
        "enum": [
          "pass",
          "fail",
          "unknown"
        ],
        "type": "string"
      },
      "Viewport": {
        "properties": {
          "height": {
            "format": "uint32",
            "maximum": 4294967295,
            "minimum": 0,
            "type": "integer"
          },
          "scaleFactor": {
            "format": "double",
            "type": "number"
          },
          "width": {
            "format": "uint32",
            "maximum": 4294967295,
            "minimum": 0,
            "type": "integer"
          }
        },
        "required": [
          "width",
          "height",
          "scaleFactor"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:events-list-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/EventsListSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "EventsListResponse"
  },
  "events.stream.open": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "EventStreamEndpoint": {
        "description": "A short-lived bearer URL. Debug intentionally never exposes its contents.",
        "maxLength": 2048,
        "minLength": 1,
        "type": "string"
      },
      "EventStreamEpoch": {
        "description": "Identifies one daemon process lifetime. A cursor from another epoch must\nnever be accepted as a resumable position.",
        "format": "uuid",
        "type": "string"
      },
      "EventsStreamOpenResult": {
        "additionalProperties": false,
        "properties": {
          "endpoint": {
            "$ref": "#/$defs/EventStreamEndpoint"
          },
          "expiresAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "streamEpoch": {
            "$ref": "#/$defs/EventStreamEpoch"
          }
        },
        "required": [
          "endpoint",
          "streamEpoch",
          "expiresAtMs"
        ],
        "type": "object"
      },
      "EventsStreamOpenSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/EventsStreamOpenResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:events-stream-open-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/EventsStreamOpenSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "EventsStreamOpenResponse"
  },
  "events.subscribe": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "EventSequence": {
        "description": "A one-based sequence number within one session.\n\nThe wire value is capped at JavaScript's maximum safe integer so generated\nclients can sort and resume event streams without losing precision.",
        "format": "uint64",
        "maximum": 9007199254740991,
        "minimum": 1,
        "type": "integer"
      },
      "EventStreamCursor": {
        "additionalProperties": false,
        "description": "A Session-scoped, daemon-epoch-scoped application acknowledgement.",
        "properties": {
          "sequence": {
            "$ref": "#/$defs/EventSequence"
          },
          "sessionId": {
            "format": "uuid",
            "type": "string"
          },
          "streamEpoch": {
            "$ref": "#/$defs/EventStreamEpoch"
          }
        },
        "required": [
          "streamEpoch",
          "sessionId",
          "sequence"
        ],
        "type": "object"
      },
      "EventStreamEpoch": {
        "description": "Identifies one daemon process lifetime. A cursor from another epoch must\nnever be accepted as a resumable position.",
        "format": "uuid",
        "type": "string"
      },
      "EventsSubscribeResult": {
        "additionalProperties": false,
        "properties": {
          "replayThrough": {
            "$ref": "#/$defs/EventStreamCursor"
          },
          "sessionId": {
            "format": "uuid",
            "type": "string"
          },
          "sessionState": {
            "$ref": "#/$defs/SessionState"
          },
          "subscriptionId": {
            "format": "uuid",
            "type": "string"
          }
        },
        "required": [
          "subscriptionId",
          "sessionId",
          "replayThrough",
          "sessionState"
        ],
        "type": "object"
      },
      "EventsSubscribeSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/EventsSubscribeResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SessionState": {
        "enum": [
          "active",
          "ended"
        ],
        "type": "string"
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:events-subscribe-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/EventsSubscribeSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "EventsSubscribeResponse"
  },
  "media.stream.capture": {
    "$defs": {
      "AssetRef": {
        "properties": {
          "id": {
            "type": "string"
          },
          "mediaType": {
            "type": "string"
          },
          "sha256": {
            "type": [
              "string",
              "null"
            ]
          },
          "uri": {
            "type": "string"
          }
        },
        "required": [
          "id",
          "mediaType",
          "uri"
        ],
        "type": "object"
      },
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "EventSequence": {
        "description": "A one-based sequence number within one session.\n\nThe wire value is capped at JavaScript's maximum safe integer so generated\nclients can sort and resume event streams without losing precision.",
        "format": "uint64",
        "maximum": 9007199254740991,
        "minimum": 1,
        "type": "integer"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "MediaFrame": {
        "additionalProperties": false,
        "properties": {
          "durationMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": [
              "integer",
              "null"
            ]
          },
          "evidence": {
            "$ref": "#/$defs/AssetRef"
          },
          "frameIndex": {
            "$ref": "#/$defs/EventSequence"
          },
          "keyFrame": {
            "type": "boolean"
          },
          "streamId": {
            "format": "uuid",
            "type": "string"
          }
        },
        "required": [
          "streamId",
          "frameIndex",
          "evidence"
        ],
        "type": "object"
      },
      "MediaStreamCaptureResult": {
        "additionalProperties": false,
        "description": "Result returned by `media.stream.capture`.",
        "properties": {
          "frame": {
            "$ref": "#/$defs/MediaFrame"
          }
        },
        "required": [
          "frame"
        ],
        "type": "object"
      },
      "MediaStreamCaptureSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/MediaStreamCaptureResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:media-stream-capture-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/MediaStreamCaptureSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "MediaStreamCaptureResponse"
  },
  "media.stream.end": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "MediaStreamEndResult": {
        "additionalProperties": false,
        "description": "Result returned by `media.stream.end`.",
        "properties": {
          "frameCount": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "streamId": {
            "format": "uuid",
            "type": "string"
          }
        },
        "required": [
          "streamId",
          "frameCount"
        ],
        "type": "object"
      },
      "MediaStreamEndSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/MediaStreamEndResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:media-stream-end-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/MediaStreamEndSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "MediaStreamEndResponse"
  },
  "media.stream.start": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "MediaStreamInfo": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "kind": {
            "$ref": "#/$defs/MediaStreamKind"
          },
          "mediaType": {
            "maxLength": 255,
            "minLength": 1,
            "type": "string"
          },
          "viewport": {
            "anyOf": [
              {
                "$ref": "#/$defs/Viewport"
              },
              {
                "type": "null"
              }
            ]
          }
        },
        "required": [
          "id",
          "kind",
          "mediaType"
        ],
        "type": "object"
      },
      "MediaStreamKind": {
        "enum": [
          "screenshot",
          "video"
        ],
        "type": "string"
      },
      "MediaStreamStartResult": {
        "additionalProperties": false,
        "description": "Result returned by `media.stream.start`.",
        "properties": {
          "stream": {
            "$ref": "#/$defs/MediaStreamInfo"
          }
        },
        "required": [
          "stream"
        ],
        "type": "object"
      },
      "MediaStreamStartSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/MediaStreamStartResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      },
      "Viewport": {
        "properties": {
          "height": {
            "format": "uint32",
            "maximum": 4294967295,
            "minimum": 0,
            "type": "integer"
          },
          "scaleFactor": {
            "format": "double",
            "type": "number"
          },
          "width": {
            "format": "uint32",
            "maximum": 4294967295,
            "minimum": 0,
            "type": "integer"
          }
        },
        "required": [
          "width",
          "height",
          "scaleFactor"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:media-stream-start-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/MediaStreamStartSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "MediaStreamStartResponse"
  },
  "request.cancel": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RequestCancelResult": {
        "additionalProperties": false,
        "properties": {
          "requestId": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "status": {
            "$ref": "#/$defs/RequestCancelStatus"
          }
        },
        "required": [
          "requestId",
          "status"
        ],
        "type": "object"
      },
      "RequestCancelStatus": {
        "enum": [
          "requested",
          "alreadyRequested",
          "notFound"
        ],
        "type": "string"
      },
      "RequestCancelSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/RequestCancelResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:request-cancel-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/RequestCancelSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "RequestCancelResponse"
  },
  "session.current": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SessionCurrentResult": {
        "additionalProperties": false,
        "description": "Result returned by `session.current`.",
        "properties": {
          "sessionId": {
            "format": "uuid",
            "type": "string"
          }
        },
        "required": [
          "sessionId"
        ],
        "type": "object"
      },
      "SessionCurrentSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/SessionCurrentResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:session-current-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/SessionCurrentSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "SessionCurrentResponse"
  },
  "session.end": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "EventSequence": {
        "description": "A one-based sequence number within one session.\n\nThe wire value is capped at JavaScript's maximum safe integer so generated\nclients can sort and resume event streams without losing precision.",
        "format": "uint64",
        "maximum": 9007199254740991,
        "minimum": 1,
        "type": "integer"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SessionEndSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/SessionInfo"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "SessionInfo": {
        "additionalProperties": false,
        "properties": {
          "endedAtMs": {
            "default": null,
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": [
              "integer",
              "null"
            ]
          },
          "eventCount": {
            "$ref": "#/$defs/EventSequence"
          },
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "lastSequence": {
            "$ref": "#/$defs/EventSequence"
          },
          "startedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "state": {
            "$ref": "#/$defs/SessionState"
          }
        },
        "required": [
          "id",
          "state",
          "startedAtMs",
          "eventCount",
          "lastSequence"
        ],
        "type": "object"
      },
      "SessionState": {
        "enum": [
          "active",
          "ended"
        ],
        "type": "string"
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:session-end-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/SessionEndSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "SessionEndResponse"
  },
  "session.export": {
    "$defs": {
      "ActionExecution": {
        "oneOf": [
          {
            "additionalProperties": false,
            "properties": {
              "context": {
                "$ref": "#/$defs/UiContextRef"
              },
              "mode": {
                "const": "nativeSemantic",
                "type": "string"
              }
            },
            "required": [
              "mode",
              "context"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "context": {
                "$ref": "#/$defs/UiContextRef"
              },
              "mode": {
                "const": "webSemantic",
                "type": "string"
              }
            },
            "required": [
              "mode",
              "context"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "context": {
                "$ref": "#/$defs/UiContextRef"
              },
              "fallbackReason": {
                "$ref": "#/$defs/CoordinateFallbackReason"
              },
              "mode": {
                "const": "coordinateFallback",
                "type": "string"
              }
            },
            "required": [
              "mode",
              "context",
              "fallbackReason"
            ],
            "type": "object"
          }
        ]
      },
      "ActionOutcome": {
        "description": "The terminal outcome for one action call.\n\nKeeping the four outcomes structurally distinct prevents clients from\nhaving to infer timeout or cancellation from human-readable error text.",
        "oneOf": [
          {
            "additionalProperties": false,
            "properties": {
              "outcome": {
                "const": "succeeded",
                "type": "string"
              },
              "result": {
                "$ref": "#/$defs/ActionResult"
              }
            },
            "required": [
              "outcome",
              "result"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "error": {
                "$ref": "#/$defs/ErrorInfo"
              },
              "outcome": {
                "const": "failed",
                "type": "string"
              }
            },
            "required": [
              "outcome",
              "error"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "error": {
                "$ref": "#/$defs/ErrorInfo"
              },
              "outcome": {
                "const": "cancelled",
                "type": "string"
              }
            },
            "required": [
              "outcome",
              "error"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "error": {
                "$ref": "#/$defs/ErrorInfo"
              },
              "outcome": {
                "const": "timedOut",
                "type": "string"
              },
              "timeoutMs": {
                "format": "uint64",
                "maximum": 9007199254740991,
                "minimum": 0,
                "type": "integer"
              }
            },
            "required": [
              "outcome",
              "error",
              "timeoutMs"
            ],
            "type": "object"
          }
        ]
      },
      "ActionResult": {
        "properties": {
          "after": {
            "anyOf": [
              {
                "$ref": "#/$defs/Observation"
              },
              {
                "type": "null"
              }
            ]
          },
          "before": {
            "anyOf": [
              {
                "$ref": "#/$defs/Observation"
              },
              {
                "type": "null"
              }
            ]
          },
          "callId": {
            "format": "uuid",
            "type": "string"
          },
          "evidence": {
            "default": [],
            "items": {
              "$ref": "#/$defs/AssetRef"
            },
            "type": "array"
          },
          "execution": {
            "anyOf": [
              {
                "$ref": "#/$defs/ActionExecution"
              },
              {
                "type": "null"
              }
            ]
          },
          "finishedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "output": true,
          "startedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        },
        "required": [
          "callId",
          "startedAtMs",
          "finishedAtMs",
          "output"
        ],
        "type": "object"
      },
      "AssetRef": {
        "properties": {
          "id": {
            "type": "string"
          },
          "mediaType": {
            "type": "string"
          },
          "sha256": {
            "type": [
              "string",
              "null"
            ]
          },
          "uri": {
            "type": "string"
          }
        },
        "required": [
          "id",
          "mediaType",
          "uri"
        ],
        "type": "object"
      },
      "CoordinateFallbackReason": {
        "enum": [
          "semanticInteractionUnavailable",
          "platformLimitation"
        ],
        "type": "string"
      },
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "EventSequence": {
        "description": "A one-based sequence number within one session.\n\nThe wire value is capped at JavaScript's maximum safe integer so generated\nclients can sort and resume event streams without losing precision.",
        "format": "uint64",
        "maximum": 9007199254740991,
        "minimum": 1,
        "type": "integer"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "MediaFrame": {
        "additionalProperties": false,
        "properties": {
          "durationMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": [
              "integer",
              "null"
            ]
          },
          "evidence": {
            "$ref": "#/$defs/AssetRef"
          },
          "frameIndex": {
            "$ref": "#/$defs/EventSequence"
          },
          "keyFrame": {
            "type": "boolean"
          },
          "streamId": {
            "format": "uuid",
            "type": "string"
          }
        },
        "required": [
          "streamId",
          "frameIndex",
          "evidence"
        ],
        "type": "object"
      },
      "MediaStreamInfo": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "kind": {
            "$ref": "#/$defs/MediaStreamKind"
          },
          "mediaType": {
            "maxLength": 255,
            "minLength": 1,
            "type": "string"
          },
          "viewport": {
            "anyOf": [
              {
                "$ref": "#/$defs/Viewport"
              },
              {
                "type": "null"
              }
            ]
          }
        },
        "required": [
          "id",
          "kind",
          "mediaType"
        ],
        "type": "object"
      },
      "MediaStreamKind": {
        "enum": [
          "screenshot",
          "video"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "Observation": {
        "properties": {
          "capturedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "deviceId": {
            "type": "string"
          },
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "metadata": {
            "additionalProperties": true,
            "default": {},
            "type": "object"
          },
          "screenshot": {
            "anyOf": [
              {
                "$ref": "#/$defs/AssetRef"
              },
              {
                "type": "null"
              }
            ]
          },
          "screenshotOmission": {
            "anyOf": [
              {
                "$ref": "#/$defs/ScreenshotOmissionReason"
              },
              {
                "type": "null"
              }
            ]
          },
          "uiSnapshot": {
            "anyOf": [
              {
                "$ref": "#/$defs/UiSnapshotRef"
              },
              {
                "type": "null"
              }
            ]
          },
          "uiSnapshotOmission": {
            "anyOf": [
              {
                "$ref": "#/$defs/UiSnapshotOmissionReason"
              },
              {
                "type": "null"
              }
            ]
          },
          "viewport": {
            "$ref": "#/$defs/Viewport"
          }
        },
        "required": [
          "id",
          "deviceId",
          "capturedAtMs",
          "viewport"
        ],
        "type": "object"
      },
      "RecordedActionCall": {
        "description": "Durable representation of an Action invocation.\n\nStandard calls preserve the historical wire shape. Protected and unknown\ncalls retain only correlation fields and serialize `arguments` as `null`\nwith an explicit `argumentsRedacted` marker.",
        "properties": {
          "arguments": {
            "default": null
          },
          "argumentsRedacted": {
            "type": "boolean"
          },
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "name": {
            "type": "string"
          }
        },
        "required": [
          "id",
          "name"
        ],
        "type": "object"
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "ScreenshotOmissionReason": {
        "enum": [
          "policy",
          "protectedAction"
        ],
        "type": "string"
      },
      "SessionExportResult": {
        "additionalProperties": false,
        "description": "Result returned by `session.export`.\n\n`nextAfterSequence` is absent for a legacy complete export and for the\nfinal page. A paged response includes it only when another page remains.",
        "properties": {
          "events": {
            "items": {
              "$ref": "#/$defs/TestEvent"
            },
            "type": "array"
          },
          "nextAfterSequence": {
            "anyOf": [
              {
                "$ref": "#/$defs/EventSequence"
              },
              {
                "type": "null"
              }
            ]
          },
          "session": {
            "$ref": "#/$defs/SessionInfo"
          }
        },
        "required": [
          "session",
          "events"
        ],
        "type": "object"
      },
      "SessionExportSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/SessionExportResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "SessionInfo": {
        "additionalProperties": false,
        "properties": {
          "endedAtMs": {
            "default": null,
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": [
              "integer",
              "null"
            ]
          },
          "eventCount": {
            "$ref": "#/$defs/EventSequence"
          },
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "lastSequence": {
            "$ref": "#/$defs/EventSequence"
          },
          "startedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "state": {
            "$ref": "#/$defs/SessionState"
          }
        },
        "required": [
          "id",
          "state",
          "startedAtMs",
          "eventCount",
          "lastSequence"
        ],
        "type": "object"
      },
      "SessionOutcome": {
        "enum": [
          "completed",
          "failed",
          "cancelled",
          "shutdown"
        ],
        "type": "string"
      },
      "SessionState": {
        "enum": [
          "active",
          "ended"
        ],
        "type": "string"
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      },
      "TestEvent": {
        "additionalProperties": false,
        "properties": {
          "atMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "deviceId": {
            "type": [
              "string",
              "null"
            ]
          },
          "eventId": {
            "format": "uuid",
            "type": "string"
          },
          "payload": {
            "$ref": "#/$defs/TestEventPayload"
          },
          "requestId": {
            "anyOf": [
              {
                "$ref": "#/$defs/RpcIdSchema"
              },
              {
                "type": "null"
              }
            ]
          },
          "sequence": {
            "$ref": "#/$defs/EventSequence"
          },
          "sessionId": {
            "format": "uuid",
            "type": "string"
          }
        },
        "required": [
          "eventId",
          "sessionId",
          "sequence",
          "atMs",
          "payload"
        ],
        "type": "object"
      },
      "TestEventPayload": {
        "oneOf": [
          {
            "additionalProperties": false,
            "properties": {
              "type": {
                "const": "sessionStarted",
                "type": "string"
              }
            },
            "required": [
              "type"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "outcome": {
                "$ref": "#/$defs/SessionOutcome"
              },
              "reason": {
                "type": [
                  "string",
                  "null"
                ]
              },
              "type": {
                "const": "sessionEnded",
                "type": "string"
              }
            },
            "required": [
              "type",
              "outcome"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "observation": {
                "$ref": "#/$defs/Observation"
              },
              "type": {
                "const": "observationCaptured",
                "type": "string"
              }
            },
            "required": [
              "type",
              "observation"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "call": {
                "$ref": "#/$defs/RecordedActionCall"
              },
              "type": {
                "const": "actionStarted",
                "type": "string"
              }
            },
            "required": [
              "type",
              "call"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "callId": {
                "format": "uuid",
                "type": "string"
              },
              "outcome": {
                "$ref": "#/$defs/ActionOutcome"
              },
              "type": {
                "const": "actionCompleted",
                "type": "string"
              }
            },
            "required": [
              "type",
              "callId",
              "outcome"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "stream": {
                "$ref": "#/$defs/MediaStreamInfo"
              },
              "type": {
                "const": "mediaStreamStarted",
                "type": "string"
              }
            },
            "required": [
              "type",
              "stream"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "frame": {
                "$ref": "#/$defs/MediaFrame"
              },
              "type": {
                "const": "mediaFrameCaptured",
                "type": "string"
              }
            },
            "required": [
              "type",
              "frame"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "frameCount": {
                "format": "uint64",
                "maximum": 9007199254740991,
                "minimum": 0,
                "type": "integer"
              },
              "streamId": {
                "format": "uuid",
                "type": "string"
              },
              "type": {
                "const": "mediaStreamEnded",
                "type": "string"
              }
            },
            "required": [
              "type",
              "streamId",
              "frameCount"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "type": {
                "const": "verdictRecorded",
                "type": "string"
              },
              "verdict": {
                "$ref": "#/$defs/Verdict"
              }
            },
            "required": [
              "type",
              "verdict"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "error": {
                "$ref": "#/$defs/ErrorInfo"
              },
              "type": {
                "const": "error",
                "type": "string"
              }
            },
            "required": [
              "type",
              "error"
            ],
            "type": "object"
          }
        ]
      },
      "UiContextKind": {
        "enum": [
          "native",
          "web"
        ],
        "type": "string"
      },
      "UiContextRef": {
        "additionalProperties": false,
        "description": "Full identity of one native accessibility or web-document context.\n`documentEpoch` is required for both channels and changes after reconnect,\nnavigation, or any replacement that invalidates prior node references.",
        "properties": {
          "contextId": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          },
          "contextKind": {
            "$ref": "#/$defs/UiContextKind"
          },
          "documentEpoch": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          }
        },
        "required": [
          "contextKind",
          "contextId",
          "documentEpoch"
        ],
        "type": "object"
      },
      "UiSnapshotOmissionReason": {
        "enum": [
          "driverUnsupported",
          "policy",
          "protectedAction"
        ],
        "type": "string"
      },
      "UiSnapshotRef": {
        "additionalProperties": false,
        "description": "Small Observation-side reference to a UI Tree Evidence object.",
        "properties": {
          "byteLength": {
            "format": "uint64",
            "maximum": 786432,
            "minimum": 1,
            "type": "integer"
          },
          "context": {
            "$ref": "#/$defs/UiContextRef"
          },
          "evidence": {
            "$ref": "#/$defs/AssetRef"
          },
          "formatVersion": {
            "format": "uint16",
            "maximum": 1,
            "minimum": 1,
            "type": "integer"
          },
          "nodeCount": {
            "format": "uint32",
            "maximum": 10000,
            "minimum": 1,
            "type": "integer"
          }
        },
        "required": [
          "formatVersion",
          "context",
          "nodeCount",
          "byteLength",
          "evidence"
        ],
        "type": "object"
      },
      "Verdict": {
        "additionalProperties": false,
        "properties": {
          "evidence": {
            "default": [],
            "items": {
              "$ref": "#/$defs/AssetRef"
            },
            "maxItems": 64,
            "type": "array"
          },
          "status": {
            "$ref": "#/$defs/VerdictStatus"
          },
          "summary": {
            "maxLength": 16384,
            "minLength": 1,
            "type": "string"
          }
        },
        "required": [
          "status",
          "summary"
        ],
        "type": "object"
      },
      "VerdictStatus": {
        "enum": [
          "pass",
          "fail",
          "unknown"
        ],
        "type": "string"
      },
      "Viewport": {
        "properties": {
          "height": {
            "format": "uint32",
            "maximum": 4294967295,
            "minimum": 0,
            "type": "integer"
          },
          "scaleFactor": {
            "format": "double",
            "type": "number"
          },
          "width": {
            "format": "uint32",
            "maximum": 4294967295,
            "minimum": 0,
            "type": "integer"
          }
        },
        "required": [
          "width",
          "height",
          "scaleFactor"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:session-export-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/SessionExportSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "SessionExportResponse"
  },
  "session.start": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "EventSequence": {
        "description": "A one-based sequence number within one session.\n\nThe wire value is capped at JavaScript's maximum safe integer so generated\nclients can sort and resume event streams without losing precision.",
        "format": "uint64",
        "maximum": 9007199254740991,
        "minimum": 1,
        "type": "integer"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SessionInfo": {
        "additionalProperties": false,
        "properties": {
          "endedAtMs": {
            "default": null,
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": [
              "integer",
              "null"
            ]
          },
          "eventCount": {
            "$ref": "#/$defs/EventSequence"
          },
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "lastSequence": {
            "$ref": "#/$defs/EventSequence"
          },
          "startedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "state": {
            "$ref": "#/$defs/SessionState"
          }
        },
        "required": [
          "id",
          "state",
          "startedAtMs",
          "eventCount",
          "lastSequence"
        ],
        "type": "object"
      },
      "SessionStartSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/SessionInfo"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "SessionState": {
        "enum": [
          "active",
          "ended"
        ],
        "type": "string"
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:session-start-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/SessionStartSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "SessionStartResponse"
  },
  "sessions.list": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "EventSequence": {
        "description": "A one-based sequence number within one session.\n\nThe wire value is capped at JavaScript's maximum safe integer so generated\nclients can sort and resume event streams without losing precision.",
        "format": "uint64",
        "maximum": 9007199254740991,
        "minimum": 1,
        "type": "integer"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SessionInfo": {
        "additionalProperties": false,
        "properties": {
          "endedAtMs": {
            "default": null,
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": [
              "integer",
              "null"
            ]
          },
          "eventCount": {
            "$ref": "#/$defs/EventSequence"
          },
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "lastSequence": {
            "$ref": "#/$defs/EventSequence"
          },
          "startedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "state": {
            "$ref": "#/$defs/SessionState"
          }
        },
        "required": [
          "id",
          "state",
          "startedAtMs",
          "eventCount",
          "lastSequence"
        ],
        "type": "object"
      },
      "SessionState": {
        "enum": [
          "active",
          "ended"
        ],
        "type": "string"
      },
      "SessionsListSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "items": {
              "$ref": "#/$defs/SessionInfo"
            },
            "type": "array"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:sessions-list-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/SessionsListSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "SessionsListResponse"
  },
  "system.describe": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "FeatureSelection": {
        "additionalProperties": false,
        "properties": {
          "enabled": {
            "items": {
              "type": "string"
            },
            "type": "array",
            "uniqueItems": true
          }
        },
        "required": [
          "enabled"
        ],
        "type": "object"
      },
      "HelloResult": {
        "additionalProperties": false,
        "properties": {
          "connectionId": {
            "format": "uuid",
            "type": "string"
          },
          "features": {
            "$ref": "#/$defs/FeatureSelection"
          },
          "protocol": {
            "$ref": "#/$defs/ProtocolSelection"
          },
          "server": {
            "$ref": "#/$defs/PeerInfo"
          },
          "transport": {
            "$ref": "#/$defs/TransportInfo"
          }
        },
        "required": [
          "connectionId",
          "protocol",
          "server",
          "transport",
          "features"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "PeerInfo": {
        "additionalProperties": false,
        "properties": {
          "name": {
            "type": "string"
          },
          "version": {
            "type": "string"
          }
        },
        "required": [
          "name",
          "version"
        ],
        "type": "object"
      },
      "ProtocolSelection": {
        "additionalProperties": false,
        "properties": {
          "selected": {
            "$ref": "#/$defs/ProtocolVersion"
          }
        },
        "required": [
          "selected"
        ],
        "type": "object"
      },
      "ProtocolVersion": {
        "additionalProperties": false,
        "properties": {
          "major": {
            "format": "uint16",
            "maximum": 65535,
            "minimum": 0,
            "type": "integer"
          },
          "minor": {
            "format": "uint16",
            "maximum": 65535,
            "minimum": 0,
            "type": "integer"
          }
        },
        "required": [
          "major",
          "minor"
        ],
        "type": "object"
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemDescribeResult": {
        "additionalProperties": false,
        "description": "Result returned by `system.describe`.",
        "properties": {
          "activeSessionId": {
            "format": "uuid",
            "type": [
              "string",
              "null"
            ]
          },
          "client": {
            "$ref": "#/$defs/PeerInfo"
          },
          "connection": {
            "$ref": "#/$defs/HelloResult"
          },
          "deviceId": {
            "type": [
              "string",
              "null"
            ]
          }
        },
        "required": [
          "connection",
          "client"
        ],
        "type": "object"
      },
      "SystemDescribeSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/SystemDescribeResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      },
      "TransportInfo": {
        "additionalProperties": false,
        "properties": {
          "framing": {
            "type": "string"
          },
          "kind": {
            "type": "string"
          }
        },
        "required": [
          "kind",
          "framing"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:system-describe-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/SystemDescribeSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "SystemDescribeResponse"
  },
  "system.hello": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "FeatureSelection": {
        "additionalProperties": false,
        "properties": {
          "enabled": {
            "items": {
              "type": "string"
            },
            "type": "array",
            "uniqueItems": true
          }
        },
        "required": [
          "enabled"
        ],
        "type": "object"
      },
      "HelloResult": {
        "additionalProperties": false,
        "properties": {
          "connectionId": {
            "format": "uuid",
            "type": "string"
          },
          "features": {
            "$ref": "#/$defs/FeatureSelection"
          },
          "protocol": {
            "$ref": "#/$defs/ProtocolSelection"
          },
          "server": {
            "$ref": "#/$defs/PeerInfo"
          },
          "transport": {
            "$ref": "#/$defs/TransportInfo"
          }
        },
        "required": [
          "connectionId",
          "protocol",
          "server",
          "transport",
          "features"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "PeerInfo": {
        "additionalProperties": false,
        "properties": {
          "name": {
            "type": "string"
          },
          "version": {
            "type": "string"
          }
        },
        "required": [
          "name",
          "version"
        ],
        "type": "object"
      },
      "ProtocolSelection": {
        "additionalProperties": false,
        "properties": {
          "selected": {
            "$ref": "#/$defs/ProtocolVersion"
          }
        },
        "required": [
          "selected"
        ],
        "type": "object"
      },
      "ProtocolVersion": {
        "additionalProperties": false,
        "properties": {
          "major": {
            "format": "uint16",
            "maximum": 65535,
            "minimum": 0,
            "type": "integer"
          },
          "minor": {
            "format": "uint16",
            "maximum": 65535,
            "minimum": 0,
            "type": "integer"
          }
        },
        "required": [
          "major",
          "minor"
        ],
        "type": "object"
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      },
      "SystemHelloSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/HelloResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "TransportInfo": {
        "additionalProperties": false,
        "properties": {
          "framing": {
            "type": "string"
          },
          "kind": {
            "type": "string"
          }
        },
        "required": [
          "kind",
          "framing"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:system-hello-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/SystemHelloSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "SystemHelloResponse"
  },
  "ui.snapshot.get": {
    "$defs": {
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      },
      "UiContextKind": {
        "enum": [
          "native",
          "web"
        ],
        "type": "string"
      },
      "UiContextRef": {
        "additionalProperties": false,
        "description": "Full identity of one native accessibility or web-document context.\n`documentEpoch` is required for both channels and changes after reconnect,\nnavigation, or any replacement that invalidates prior node references.",
        "properties": {
          "contextId": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          },
          "contextKind": {
            "$ref": "#/$defs/UiContextKind"
          },
          "documentEpoch": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          }
        },
        "required": [
          "contextKind",
          "contextId",
          "documentEpoch"
        ],
        "type": "object"
      },
      "UiNode": {
        "additionalProperties": false,
        "description": "One node in the normalized preorder list. Unknown platform values remain\n`null`; Drivers must not manufacture optimistic enabled/hittable states.",
        "properties": {
          "bounds": {
            "anyOf": [
              {
                "$ref": "#/$defs/UiRect"
              },
              {
                "type": "null"
              }
            ]
          },
          "enabled": {
            "type": [
              "boolean",
              "null"
            ]
          },
          "hittable": {
            "type": [
              "boolean",
              "null"
            ]
          },
          "identifier": {
            "maxLength": 4096,
            "type": [
              "string",
              "null"
            ]
          },
          "name": {
            "maxLength": 65536,
            "type": [
              "string",
              "null"
            ]
          },
          "parentStableNodeId": {
            "maxLength": 4096,
            "minLength": 1,
            "type": [
              "string",
              "null"
            ]
          },
          "role": {
            "maxLength": 256,
            "minLength": 1,
            "type": "string"
          },
          "stableNodeId": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          },
          "text": {
            "maxLength": 65536,
            "type": [
              "string",
              "null"
            ]
          },
          "value": {
            "maxLength": 65536,
            "type": [
              "string",
              "null"
            ]
          }
        },
        "required": [
          "stableNodeId",
          "role"
        ],
        "type": "object"
      },
      "UiRect": {
        "additionalProperties": false,
        "properties": {
          "height": {
            "format": "double",
            "minimum": 0,
            "type": "number"
          },
          "width": {
            "format": "double",
            "minimum": 0,
            "type": "number"
          },
          "x": {
            "format": "double",
            "type": "number"
          },
          "y": {
            "format": "double",
            "type": "number"
          }
        },
        "required": [
          "x",
          "y",
          "width",
          "height"
        ],
        "type": "object"
      },
      "UiSnapshot": {
        "additionalProperties": false,
        "description": "Canonical UI Tree. `nodes` is preorder and every parent must precede its\ncontiguous descendants. The serialized payload is bounded to 768 KiB.",
        "properties": {
          "context": {
            "$ref": "#/$defs/UiContextRef"
          },
          "formatVersion": {
            "format": "uint16",
            "maximum": 1,
            "minimum": 1,
            "type": "integer"
          },
          "nodes": {
            "items": {
              "$ref": "#/$defs/UiNode"
            },
            "maxItems": 10000,
            "minItems": 1,
            "type": "array"
          },
          "observationId": {
            "format": "uuid",
            "type": "string"
          },
          "rootStableNodeIds": {
            "items": {
              "type": "string"
            },
            "maxItems": 10000,
            "minItems": 1,
            "type": "array"
          }
        },
        "required": [
          "formatVersion",
          "observationId",
          "context",
          "rootStableNodeIds",
          "nodes"
        ],
        "type": "object"
      },
      "UiSnapshotGetSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/UiSnapshot"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:ui-snapshot-get-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/UiSnapshotGetSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "UiSnapshotGetResponse"
  },
  "verdict.record": {
    "$defs": {
      "ActionExecution": {
        "oneOf": [
          {
            "additionalProperties": false,
            "properties": {
              "context": {
                "$ref": "#/$defs/UiContextRef"
              },
              "mode": {
                "const": "nativeSemantic",
                "type": "string"
              }
            },
            "required": [
              "mode",
              "context"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "context": {
                "$ref": "#/$defs/UiContextRef"
              },
              "mode": {
                "const": "webSemantic",
                "type": "string"
              }
            },
            "required": [
              "mode",
              "context"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "context": {
                "$ref": "#/$defs/UiContextRef"
              },
              "fallbackReason": {
                "$ref": "#/$defs/CoordinateFallbackReason"
              },
              "mode": {
                "const": "coordinateFallback",
                "type": "string"
              }
            },
            "required": [
              "mode",
              "context",
              "fallbackReason"
            ],
            "type": "object"
          }
        ]
      },
      "ActionOutcome": {
        "description": "The terminal outcome for one action call.\n\nKeeping the four outcomes structurally distinct prevents clients from\nhaving to infer timeout or cancellation from human-readable error text.",
        "oneOf": [
          {
            "additionalProperties": false,
            "properties": {
              "outcome": {
                "const": "succeeded",
                "type": "string"
              },
              "result": {
                "$ref": "#/$defs/ActionResult"
              }
            },
            "required": [
              "outcome",
              "result"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "error": {
                "$ref": "#/$defs/ErrorInfo"
              },
              "outcome": {
                "const": "failed",
                "type": "string"
              }
            },
            "required": [
              "outcome",
              "error"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "error": {
                "$ref": "#/$defs/ErrorInfo"
              },
              "outcome": {
                "const": "cancelled",
                "type": "string"
              }
            },
            "required": [
              "outcome",
              "error"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "error": {
                "$ref": "#/$defs/ErrorInfo"
              },
              "outcome": {
                "const": "timedOut",
                "type": "string"
              },
              "timeoutMs": {
                "format": "uint64",
                "maximum": 9007199254740991,
                "minimum": 0,
                "type": "integer"
              }
            },
            "required": [
              "outcome",
              "error",
              "timeoutMs"
            ],
            "type": "object"
          }
        ]
      },
      "ActionResult": {
        "properties": {
          "after": {
            "anyOf": [
              {
                "$ref": "#/$defs/Observation"
              },
              {
                "type": "null"
              }
            ]
          },
          "before": {
            "anyOf": [
              {
                "$ref": "#/$defs/Observation"
              },
              {
                "type": "null"
              }
            ]
          },
          "callId": {
            "format": "uuid",
            "type": "string"
          },
          "evidence": {
            "default": [],
            "items": {
              "$ref": "#/$defs/AssetRef"
            },
            "type": "array"
          },
          "execution": {
            "anyOf": [
              {
                "$ref": "#/$defs/ActionExecution"
              },
              {
                "type": "null"
              }
            ]
          },
          "finishedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "output": true,
          "startedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        },
        "required": [
          "callId",
          "startedAtMs",
          "finishedAtMs",
          "output"
        ],
        "type": "object"
      },
      "AssetRef": {
        "properties": {
          "id": {
            "type": "string"
          },
          "mediaType": {
            "type": "string"
          },
          "sha256": {
            "type": [
              "string",
              "null"
            ]
          },
          "uri": {
            "type": "string"
          }
        },
        "required": [
          "id",
          "mediaType",
          "uri"
        ],
        "type": "object"
      },
      "CoordinateFallbackReason": {
        "enum": [
          "semanticInteractionUnavailable",
          "platformLimitation"
        ],
        "type": "string"
      },
      "ErrorInfo": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "type": "string"
          },
          "details": true,
          "message": {
            "type": "string"
          },
          "retryable": {
            "type": "boolean"
          }
        },
        "required": [
          "code",
          "message",
          "retryable"
        ],
        "type": "object"
      },
      "EventSequence": {
        "description": "A one-based sequence number within one session.\n\nThe wire value is capped at JavaScript's maximum safe integer so generated\nclients can sort and resume event streams without losing precision.",
        "format": "uint64",
        "maximum": 9007199254740991,
        "minimum": 1,
        "type": "integer"
      },
      "JsonRpcVersion": {
        "enum": [
          "2.0"
        ],
        "type": "string"
      },
      "MediaFrame": {
        "additionalProperties": false,
        "properties": {
          "durationMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": [
              "integer",
              "null"
            ]
          },
          "evidence": {
            "$ref": "#/$defs/AssetRef"
          },
          "frameIndex": {
            "$ref": "#/$defs/EventSequence"
          },
          "keyFrame": {
            "type": "boolean"
          },
          "streamId": {
            "format": "uuid",
            "type": "string"
          }
        },
        "required": [
          "streamId",
          "frameIndex",
          "evidence"
        ],
        "type": "object"
      },
      "MediaStreamInfo": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "kind": {
            "$ref": "#/$defs/MediaStreamKind"
          },
          "mediaType": {
            "maxLength": 255,
            "minLength": 1,
            "type": "string"
          },
          "viewport": {
            "anyOf": [
              {
                "$ref": "#/$defs/Viewport"
              },
              {
                "type": "null"
              }
            ]
          }
        },
        "required": [
          "id",
          "kind",
          "mediaType"
        ],
        "type": "object"
      },
      "MediaStreamKind": {
        "enum": [
          "screenshot",
          "video"
        ],
        "type": "string"
      },
      "NullableRpcIdSchema": {
        "anyOf": [
          {
            "$ref": "#/$defs/RpcIdSchema"
          },
          {
            "type": "null"
          }
        ]
      },
      "Observation": {
        "properties": {
          "capturedAtMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "deviceId": {
            "type": "string"
          },
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "metadata": {
            "additionalProperties": true,
            "default": {},
            "type": "object"
          },
          "screenshot": {
            "anyOf": [
              {
                "$ref": "#/$defs/AssetRef"
              },
              {
                "type": "null"
              }
            ]
          },
          "screenshotOmission": {
            "anyOf": [
              {
                "$ref": "#/$defs/ScreenshotOmissionReason"
              },
              {
                "type": "null"
              }
            ]
          },
          "uiSnapshot": {
            "anyOf": [
              {
                "$ref": "#/$defs/UiSnapshotRef"
              },
              {
                "type": "null"
              }
            ]
          },
          "uiSnapshotOmission": {
            "anyOf": [
              {
                "$ref": "#/$defs/UiSnapshotOmissionReason"
              },
              {
                "type": "null"
              }
            ]
          },
          "viewport": {
            "$ref": "#/$defs/Viewport"
          }
        },
        "required": [
          "id",
          "deviceId",
          "capturedAtMs",
          "viewport"
        ],
        "type": "object"
      },
      "RecordedActionCall": {
        "description": "Durable representation of an Action invocation.\n\nStandard calls preserve the historical wire shape. Protected and unknown\ncalls retain only correlation fields and serialize `arguments` as `null`\nwith an explicit `argumentsRedacted` marker.",
        "properties": {
          "arguments": {
            "default": null
          },
          "argumentsRedacted": {
            "type": "boolean"
          },
          "id": {
            "format": "uuid",
            "type": "string"
          },
          "name": {
            "type": "string"
          }
        },
        "required": [
          "id",
          "name"
        ],
        "type": "object"
      },
      "RpcError": {
        "additionalProperties": false,
        "properties": {
          "code": {
            "format": "int32",
            "maximum": 2147483647,
            "minimum": -2147483648,
            "type": "integer"
          },
          "data": {
            "$ref": "#/$defs/ErrorInfo"
          },
          "message": {
            "type": "string"
          }
        },
        "required": [
          "code",
          "message",
          "data"
        ],
        "type": "object"
      },
      "RpcIdSchema": {
        "anyOf": [
          {
            "type": "string"
          },
          {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          }
        ]
      },
      "ScreenshotOmissionReason": {
        "enum": [
          "policy",
          "protectedAction"
        ],
        "type": "string"
      },
      "SessionOutcome": {
        "enum": [
          "completed",
          "failed",
          "cancelled",
          "shutdown"
        ],
        "type": "string"
      },
      "SystemHelloFailureSchema": {
        "additionalProperties": false,
        "properties": {
          "error": {
            "$ref": "#/$defs/RpcError"
          },
          "id": {
            "$ref": "#/$defs/NullableRpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "error"
        ],
        "type": "object"
      },
      "TestEvent": {
        "additionalProperties": false,
        "properties": {
          "atMs": {
            "format": "uint64",
            "maximum": 9007199254740991,
            "minimum": 0,
            "type": "integer"
          },
          "deviceId": {
            "type": [
              "string",
              "null"
            ]
          },
          "eventId": {
            "format": "uuid",
            "type": "string"
          },
          "payload": {
            "$ref": "#/$defs/TestEventPayload"
          },
          "requestId": {
            "anyOf": [
              {
                "$ref": "#/$defs/RpcIdSchema"
              },
              {
                "type": "null"
              }
            ]
          },
          "sequence": {
            "$ref": "#/$defs/EventSequence"
          },
          "sessionId": {
            "format": "uuid",
            "type": "string"
          }
        },
        "required": [
          "eventId",
          "sessionId",
          "sequence",
          "atMs",
          "payload"
        ],
        "type": "object"
      },
      "TestEventPayload": {
        "oneOf": [
          {
            "additionalProperties": false,
            "properties": {
              "type": {
                "const": "sessionStarted",
                "type": "string"
              }
            },
            "required": [
              "type"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "outcome": {
                "$ref": "#/$defs/SessionOutcome"
              },
              "reason": {
                "type": [
                  "string",
                  "null"
                ]
              },
              "type": {
                "const": "sessionEnded",
                "type": "string"
              }
            },
            "required": [
              "type",
              "outcome"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "observation": {
                "$ref": "#/$defs/Observation"
              },
              "type": {
                "const": "observationCaptured",
                "type": "string"
              }
            },
            "required": [
              "type",
              "observation"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "call": {
                "$ref": "#/$defs/RecordedActionCall"
              },
              "type": {
                "const": "actionStarted",
                "type": "string"
              }
            },
            "required": [
              "type",
              "call"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "callId": {
                "format": "uuid",
                "type": "string"
              },
              "outcome": {
                "$ref": "#/$defs/ActionOutcome"
              },
              "type": {
                "const": "actionCompleted",
                "type": "string"
              }
            },
            "required": [
              "type",
              "callId",
              "outcome"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "stream": {
                "$ref": "#/$defs/MediaStreamInfo"
              },
              "type": {
                "const": "mediaStreamStarted",
                "type": "string"
              }
            },
            "required": [
              "type",
              "stream"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "frame": {
                "$ref": "#/$defs/MediaFrame"
              },
              "type": {
                "const": "mediaFrameCaptured",
                "type": "string"
              }
            },
            "required": [
              "type",
              "frame"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "frameCount": {
                "format": "uint64",
                "maximum": 9007199254740991,
                "minimum": 0,
                "type": "integer"
              },
              "streamId": {
                "format": "uuid",
                "type": "string"
              },
              "type": {
                "const": "mediaStreamEnded",
                "type": "string"
              }
            },
            "required": [
              "type",
              "streamId",
              "frameCount"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "type": {
                "const": "verdictRecorded",
                "type": "string"
              },
              "verdict": {
                "$ref": "#/$defs/Verdict"
              }
            },
            "required": [
              "type",
              "verdict"
            ],
            "type": "object"
          },
          {
            "additionalProperties": false,
            "properties": {
              "error": {
                "$ref": "#/$defs/ErrorInfo"
              },
              "type": {
                "const": "error",
                "type": "string"
              }
            },
            "required": [
              "type",
              "error"
            ],
            "type": "object"
          }
        ]
      },
      "UiContextKind": {
        "enum": [
          "native",
          "web"
        ],
        "type": "string"
      },
      "UiContextRef": {
        "additionalProperties": false,
        "description": "Full identity of one native accessibility or web-document context.\n`documentEpoch` is required for both channels and changes after reconnect,\nnavigation, or any replacement that invalidates prior node references.",
        "properties": {
          "contextId": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          },
          "contextKind": {
            "$ref": "#/$defs/UiContextKind"
          },
          "documentEpoch": {
            "maxLength": 4096,
            "minLength": 1,
            "type": "string"
          }
        },
        "required": [
          "contextKind",
          "contextId",
          "documentEpoch"
        ],
        "type": "object"
      },
      "UiSnapshotOmissionReason": {
        "enum": [
          "driverUnsupported",
          "policy",
          "protectedAction"
        ],
        "type": "string"
      },
      "UiSnapshotRef": {
        "additionalProperties": false,
        "description": "Small Observation-side reference to a UI Tree Evidence object.",
        "properties": {
          "byteLength": {
            "format": "uint64",
            "maximum": 786432,
            "minimum": 1,
            "type": "integer"
          },
          "context": {
            "$ref": "#/$defs/UiContextRef"
          },
          "evidence": {
            "$ref": "#/$defs/AssetRef"
          },
          "formatVersion": {
            "format": "uint16",
            "maximum": 1,
            "minimum": 1,
            "type": "integer"
          },
          "nodeCount": {
            "format": "uint32",
            "maximum": 10000,
            "minimum": 1,
            "type": "integer"
          }
        },
        "required": [
          "formatVersion",
          "context",
          "nodeCount",
          "byteLength",
          "evidence"
        ],
        "type": "object"
      },
      "Verdict": {
        "additionalProperties": false,
        "properties": {
          "evidence": {
            "default": [],
            "items": {
              "$ref": "#/$defs/AssetRef"
            },
            "maxItems": 64,
            "type": "array"
          },
          "status": {
            "$ref": "#/$defs/VerdictStatus"
          },
          "summary": {
            "maxLength": 16384,
            "minLength": 1,
            "type": "string"
          }
        },
        "required": [
          "status",
          "summary"
        ],
        "type": "object"
      },
      "VerdictRecordResult": {
        "additionalProperties": false,
        "description": "Result returned by `verdict.record` after the durable event append.",
        "properties": {
          "event": {
            "$ref": "#/$defs/TestEvent"
          }
        },
        "required": [
          "event"
        ],
        "type": "object"
      },
      "VerdictRecordSuccessSchema": {
        "additionalProperties": false,
        "properties": {
          "id": {
            "$ref": "#/$defs/RpcIdSchema"
          },
          "jsonrpc": {
            "$ref": "#/$defs/JsonRpcVersion"
          },
          "result": {
            "$ref": "#/$defs/VerdictRecordResult"
          }
        },
        "required": [
          "jsonrpc",
          "id",
          "result"
        ],
        "type": "object"
      },
      "VerdictStatus": {
        "enum": [
          "pass",
          "fail",
          "unknown"
        ],
        "type": "string"
      },
      "Viewport": {
        "properties": {
          "height": {
            "format": "uint32",
            "maximum": 4294967295,
            "minimum": 0,
            "type": "integer"
          },
          "scaleFactor": {
            "format": "double",
            "type": "number"
          },
          "width": {
            "format": "uint32",
            "maximum": 4294967295,
            "minimum": 0,
            "type": "integer"
          }
        },
        "required": [
          "width",
          "height",
          "scaleFactor"
        ],
        "type": "object"
      }
    },
    "$id": "urn:devicerail:schema:protocol:v1:verdict-record-response",
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "anyOf": [
      {
        "$ref": "#/$defs/VerdictRecordSuccessSchema"
      },
      {
        "$ref": "#/$defs/SystemHelloFailureSchema"
      }
    ],
    "title": "VerdictRecordResponse"
  },
};
