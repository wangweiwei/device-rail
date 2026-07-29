"""Static-only assertions for the generated public call overloads."""

from typing import assert_type

from devicerail import DeviceRailClient, RequestHandle
from devicerail.protocol.v1 import (
    DeviceConnectResult,
    DeviceExecuteParams,
    DeviceExecuteResult,
    DevicesListResult,
    MediaStreamCaptureResult,
    SessionExportResult,
    SystemDescribeResult,
)


async def verify(client: DeviceRailClient, execute: DeviceExecuteParams) -> None:
    assert_type(await client.call("device.connect"), DeviceConnectResult)
    assert_type(await client.call("device.connect", []), DeviceConnectResult)
    assert_type(
        await client.call("device.execute", execute, timeout_ms=1_000),
        DeviceExecuteResult,
    )
    assert_type(await client.call("devices.list"), DevicesListResult)
    assert_type(await client.call("system.describe"), SystemDescribeResult)
    assert_type(
        await client.call(
            "media.stream.capture",
            {
                "streamId": "77777777-7777-4777-8777-777777777777",
                "frameIndex": 1,
            },
            timeout_ms=1_000,
        ),
        MediaStreamCaptureResult,
    )
    assert_type(
        await client.call(
            "session.export",
            {
                "sessionId": "33333333-3333-4333-8333-333333333333",
                "limit": 100,
            },
        ),
        SessionExportResult,
    )
    assert_type(
        await client.begin_call("device.execute", execute, timeout_ms=1_000),
        RequestHandle[DeviceExecuteResult],
    )
