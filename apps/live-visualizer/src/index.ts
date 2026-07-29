import {
  LIVE_TIMELINE_MAX_PAGE_SIZE,
  normalizeLiveTimelineLimits,
  type LiveTimelineLimits,
} from "@devicerail/live-visualizer";

import {
  LiveVisualizerController,
  type LiveVisualizerClient,
  type LiveVisualizerControllerLimits,
  type LiveVisualizerControllerPage,
  type LiveVisualizerControllerState,
} from "./controller.js";
import {
  bindLiveVisualizerHttp,
  DEFAULT_LIVE_VISUALIZER_MAX_API_BYTES,
  type LiveVisualizerHttpHost,
  type LiveVisualizerHttpLimits,
  type LiveVisualizerHttpStats,
} from "./http-host.js";

const PAGE_RESPONSE_OVERHEAD_BYTES = 4 * 1024;

export interface BindLiveVisualizerLimits extends LiveVisualizerControllerLimits {
  readonly http?: LiveVisualizerHttpLimits;
  readonly timeline?: Partial<LiveTimelineLimits>;
}

export interface BindLiveVisualizerOptions {
  readonly client: LiveVisualizerClient;
  readonly limits?: BindLiveVisualizerLimits;
  readonly sessionId: string;
  readonly signal?: AbortSignal;
}

export interface BoundLiveVisualizer {
  readonly endpoint: LiveVisualizerHttpHost["endpoint"];
  close(): Promise<void>;
  page(request: {
    readonly filter: "all" | "observations" | "actions" | "errors" | "verdicts";
    readonly page: number;
    readonly pageSize: 50;
  }): LiveVisualizerControllerPage;
  state(): LiveVisualizerControllerState;
  stats(): LiveVisualizerHttpStats;
  waitUntilStopped(): Promise<void>;
}

/**
 * Attach a private viewer to one explicit Session on an already-owned client.
 * Closing the viewer never closes the client and never changes device/Session state.
 */
export async function bindLiveVisualizer(
  options: BindLiveVisualizerOptions,
): Promise<BoundLiveVisualizer> {
  if (options.signal?.aborted) {
    throw new DOMException("live visualizer binding was aborted", "AbortError");
  }
  const limits: BindLiveVisualizerLimits = options.limits ?? {};
  const { http, ...controllerLimits } = limits;
  const timelineLimits = normalizeLiveTimelineLimits(controllerLimits.timeline);
  const maximumPageEntries = Math.min(
    LIVE_TIMELINE_MAX_PAGE_SIZE,
    timelineLimits.maxEvents,
  );
  const maximumPageEntryBytes = Math.min(
    timelineLimits.maxTotalBytes,
    timelineLimits.maxEventBytes * maximumPageEntries,
  );
  const requiredApiBytes = maximumPageEntryBytes + PAGE_RESPONSE_OVERHEAD_BYTES;
  const configuredApiBytes = http?.maxApiBytes ?? DEFAULT_LIVE_VISUALIZER_MAX_API_BYTES;
  if (configuredApiBytes < requiredApiBytes) {
    throw new RangeError(
      `http.maxApiBytes must be at least ${requiredApiBytes} for the configured timeline limits`,
    );
  }
  const controller = new LiveVisualizerController(
    options.client,
    options.sessionId,
    controllerLimits,
  );
  const host = await bindLiveVisualizerHttp({
    ...(http === undefined ? {} : { limits: http }),
    source: controller,
  });
  let closed = false;
  let closePromise: Promise<void> | undefined;
  let signalListener: (() => void) | undefined;
  const close = async (): Promise<void> => {
    if (closePromise) return await closePromise;
    closePromise = (async () => {
      if (closed) return;
      closed = true;
      if (signalListener) options.signal?.removeEventListener("abort", signalListener);
      try {
        await controller.stop();
      } finally {
        await host.close();
      }
    })();
    return await closePromise;
  };
  if (options.signal) {
    signalListener = () => {
      void close();
    };
    options.signal.addEventListener("abort", signalListener, { once: true });
    if (options.signal.aborted) {
      await close();
      throw new DOMException("live visualizer binding was aborted", "AbortError");
    }
  }
  controller.start();
  return {
    endpoint: host.endpoint,
    close,
    page: (request) => controller.page(request),
    state: () => controller.state(),
    stats: () => host.stats(),
    waitUntilStopped: async () => await controller.waitUntilStopped(),
  };
}

export type {
  LiveVisualizerClient,
  LiveVisualizerControllerLimits,
  LiveVisualizerControllerPage,
  LiveVisualizerControllerState,
  LiveVisualizerHttpLimits,
  LiveVisualizerHttpStats,
};
export {
  DEFAULT_LIVE_VISUALIZER_MAX_API_BYTES,
  LiveVisualizerEndpoint,
} from "./http-host.js";
