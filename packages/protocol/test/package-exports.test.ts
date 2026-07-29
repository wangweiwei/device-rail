import type { RpcRequest as RootRpcRequest } from "@devicerail/protocol";
import type { RpcRequest as VersionedRpcRequest } from "@devicerail/protocol/v1";

const rootRequest: RootRpcRequest = {
  jsonrpc: "2.0",
  id: "root-import",
  method: "system.describe",
};

const versionedRequest: VersionedRpcRequest = rootRequest;
void versionedRequest;
