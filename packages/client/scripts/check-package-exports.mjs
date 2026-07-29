const client = await import("@devicerail/client");

for (const name of [
  "DeviceRailClient",
  "ProtocolViolationError",
  "NdjsonDecoder",
  "validateRpcResult",
]) {
  if (typeof client[name] !== "function") {
    throw new Error(`@devicerail/client did not export ${name}`);
  }
}
