"""Deterministic stdio daemon used only by the Python client's E2E tests."""

from __future__ import annotations

import argparse
import json
import sys
import time
import uuid
from pathlib import Path
from typing import Any


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
FIXTURE_ROOT = REPOSITORY_ROOT / "crates" / "protocol" / "fixtures"


def fixture(path: str) -> dict[str, Any]:
    return json.loads((FIXTURE_ROOT / path).read_text(encoding="utf-8"))


def send(value: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def response_from(path: str, request_id: str | int) -> dict[str, Any]:
    value = fixture(path)
    value["id"] = request_id
    return value


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--malformed",
        choices=(
            "duplicate",
            "media-before-version",
            "semantic-hello",
            "unknown-field",
            "remote-hello-once",
            "stderr-eof",
        ),
    )
    parser.add_argument("--hello-delay-ms", type=int, default=0)
    parser.add_argument("--observe-delay-ms", type=int, default=0)
    args = parser.parse_args()
    pending_connects: list[str | int] = []
    pending_observes: set[str | int] = set()
    hello_result: dict[str, Any] | None = None
    hello_attempts = 0
    for line in sys.stdin:
        request = json.loads(line)
        request_id = request["id"]
        method = request["method"]
        if method == "system.hello":
            hello_attempts += 1
            if args.hello_delay_ms > 0:
                time.sleep(args.hello_delay_ms / 1_000)
            if args.malformed == "stderr-eof":
                sys.stderr.write("PYTHON-CLIENT-SECRET-STDERR-SENTINEL\n")
                sys.stderr.flush()
                return 1
            if args.malformed == "remote-hello-once" and hello_attempts == 1:
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "error": {
                            "code": -32000,
                            "message": "hello rejected",
                            "data": {
                                "code": "hello_rejected",
                                "message": "retry with the same offer",
                                "retryable": True,
                            },
                        },
                    }
                )
                continue
            if args.malformed == "duplicate":
                sys.stdout.write(
                    '{"jsonrpc":"2.0","id":'
                    + json.dumps(request_id)
                    + ',"result":{},"result":{}}\n'
                )
                sys.stdout.flush()
                continue
            offered = request["params"].get("features", {})
            enabled = [
                *offered.get("required", []),
                *offered.get("optional", []),
            ]
            hello_result = {
                "connectionId": str(uuid.UUID(int=0)),
                "protocol": {"selected": {"major": 1, "minor": 5}},
                "server": {"name": "python-mock-daemon", "version": "0.1.0"},
                "transport": {"kind": "stdio", "framing": "ndjson"},
                "features": {"enabled": list(dict.fromkeys(enabled))},
            }
            if args.malformed == "semantic-hello":
                hello_result["protocol"]["selected"]["minor"] = 0
            elif args.malformed == "media-before-version":
                hello_result["protocol"]["selected"]["minor"] = 3
                hello_result["features"]["enabled"] = ["media.stream.v1"]
            response = {"jsonrpc": "2.0", "id": request_id, "result": hello_result}
            if args.malformed == "unknown-field":
                response["extra"] = True
            send(response)
        elif method == "system.describe":
            response = response_from("rpc/system-describe-v1.response.json", request_id)
            response["result"]["connection"] = hello_result
            response["result"]["client"] = {
                "name": "devicerail-python",
                "version": "0.1.0",
            }
            send(response)
        elif method == "device.connect":
            if "timeoutMs" in request:
                sys.stderr.write(f"timeoutMs={request['timeoutMs']}\n")
                sys.stderr.flush()
            pending_connects.append(request_id)
            if len(pending_connects) == 2:
                for pending_id in reversed(pending_connects):
                    send(response_from("rpc/device-connect-v1.response.json", pending_id))
                pending_connects.clear()
        elif method == "device.capabilities":
            send(response_from("rpc/device-capabilities-v1.response.json", request_id))
        elif method == "session.export":
            send(response_from("rpc/session-export-v1.response.json", request_id))
        elif method == "device.observe":
            if args.observe_delay_ms > 0:
                time.sleep(args.observe_delay_ms / 1_000)
                send(response_from("rpc/device-observe-v1.response.json", request_id))
            else:
                pending_observes.add(request_id)
        elif method == "request.cancel":
            target = request["params"]["requestId"]
            status = "requested" if target in pending_observes else "notFound"
            send(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {"requestId": target, "status": status},
                }
            )
            if target in pending_observes:
                pending_observes.remove(target)
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": target,
                        "error": {
                            "code": -32000,
                            "message": "request cancelled",
                            "data": {
                                "code": "request_cancelled",
                                "message": "request cancelled by client",
                                "retryable": False,
                            },
                        },
                    }
                )
        else:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {
                        "code": -32601,
                        "message": "method not found",
                        "data": {
                            "code": "method_not_found",
                            "message": method,
                            "retryable": False,
                        },
                    },
                }
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
