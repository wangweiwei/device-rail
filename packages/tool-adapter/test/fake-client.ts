import type {
  ActionDefinition,
  ActionResult,
  Observation,
  RequestCancelResult,
  RpcId,
} from "@devicerail/protocol";

import type { DeviceRailToolClient } from "../src/index.js";

export interface ClientCallRecord {
  readonly method: string;
  readonly options: unknown;
  readonly params: unknown;
}

export interface ClientBeginCallRecord extends ClientCallRecord {
  cancelCount: number;
  readonly requestId: RpcId;
  reject(error: unknown): void;
  resolve(result: unknown): void;
}

export class FakeToolClient {
  readonly beginCalls: ClientBeginCallRecord[] = [];
  readonly calls: ClientCallRecord[] = [];
  readonly client: DeviceRailToolClient;

  readonly #capabilityResponses: unknown[];
  #requestSequence = 0;

  constructor(
    capabilityResponses: readonly unknown[] = [],
    enabledFeatures: ReadonlySet<string> = new Set(),
  ) {
    this.#capabilityResponses = [...capabilityResponses];
    this.client = {
      beginCall: this.#beginCall.bind(this) as DeviceRailToolClient["beginCall"],
      call: this.#call.bind(this) as DeviceRailToolClient["call"],
      enabledFeatures,
    };
  }

  enqueueCapabilities(capabilities: unknown): void {
    this.#capabilityResponses.push(capabilities);
  }

  async #call(method: string, params?: unknown, options?: unknown): Promise<unknown> {
    this.calls.push({ method, options, params });
    if (method !== "device.capabilities") {
      throw new Error(`unexpected fake call method ${method}`);
    }
    if (this.#capabilityResponses.length === 0) {
      throw new Error("no fake capabilities response remains");
    }
    const response = this.#capabilityResponses.shift();
    if (response instanceof Error) {
      throw response;
    }
    return response;
  }

  #beginCall(method: string, params?: unknown, options?: unknown): unknown {
    this.#requestSequence += 1;
    const requestId = `fake-request-${this.#requestSequence}`;
    let resolveResult!: (result: unknown) => void;
    let rejectResult!: (error: unknown) => void;
    const result = new Promise<unknown>((resolve, reject) => {
      resolveResult = resolve;
      rejectResult = reject;
    });
    const record: ClientBeginCallRecord = {
      cancelCount: 0,
      method,
      options,
      params,
      reject: rejectResult,
      requestId,
      resolve: resolveResult,
    };
    this.beginCalls.push(record);
    return {
      cancel: async (): Promise<RequestCancelResult> => {
        record.cancelCount += 1;
        return {
          requestId,
          status: record.cancelCount === 1 ? "requested" : "alreadyRequested",
        };
      },
      id: requestId,
      result,
    };
  }
}

export function action(
  name: string,
  inputSchema: unknown = {
    additionalProperties: false,
    properties: {},
    type: "object",
  },
  description = `Execute ${name}`,
  protection?: "protected" | "standard",
): ActionDefinition {
  return {
    description,
    inputSchema,
    name,
    ...(protection === undefined ? {} : { protection }),
  } as ActionDefinition;
}

export function observation(sequence = 1): Observation {
  return {
    capturedAtMs: sequence,
    deviceId: "mock-1",
    id: `00000000-0000-4000-8000-${sequence.toString().padStart(12, "0")}`,
    metadata: { sequence },
    screenshot: null,
    viewport: { height: 800, scaleFactor: 1, width: 600 },
  };
}

export function actionResult(callId: string, output: unknown): ActionResult {
  return {
    after: observation(2),
    before: observation(1),
    callId,
    evidence: [
      {
        id: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        mediaType: "image/png",
        sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        uri: "devicerail://assets/sha256/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      },
    ],
    finishedAtMs: 20,
    output,
    startedAtMs: 10,
  };
}
