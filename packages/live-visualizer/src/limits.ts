import { LiveTimelineError } from "./errors.js";
import type { LiveTimelineLimits } from "./types.js";

export const LIVE_TIMELINE_MAX_PAGE_SIZE = 50;

export const DEFAULT_LIVE_TIMELINE_LIMITS: LiveTimelineLimits = Object.freeze({
  maxEvents: 10_000,
  maxTotalBytes: 16 * 1024 * 1024,
  maxEventBytes: 64 * 1024,
  maxInputEventBytes: 1024 * 1024,
  maxTextBytes: 4 * 1024,
  maxJsonBytes: 16 * 1024,
  maxJsonDepth: 12,
  maxEvidencePerEvent: 32,
});

const HARD_MAX: LiveTimelineLimits = {
  maxEvents: 100_000,
  maxTotalBytes: 64 * 1024 * 1024,
  maxEventBytes: 1024 * 1024,
  maxInputEventBytes: 1024 * 1024,
  maxTextBytes: 64 * 1024,
  maxJsonBytes: 256 * 1024,
  maxJsonDepth: 64,
  maxEvidencePerEvent: 256,
};

function boundedInteger(name: keyof LiveTimelineLimits, value: number): number {
  if (!Number.isSafeInteger(value) || value <= 0 || value > HARD_MAX[name]) {
    throw new LiveTimelineError("invalid_limits", `${name} is outside its supported range`, {
      details: { maximum: HARD_MAX[name], value },
    });
  }
  return value;
}

export function normalizeLiveTimelineLimits(
  overrides: Partial<LiveTimelineLimits> = {},
): LiveTimelineLimits {
  const limits: LiveTimelineLimits = {
    maxEvents: boundedInteger(
      "maxEvents",
      overrides.maxEvents ?? DEFAULT_LIVE_TIMELINE_LIMITS.maxEvents,
    ),
    maxTotalBytes: boundedInteger(
      "maxTotalBytes",
      overrides.maxTotalBytes ?? DEFAULT_LIVE_TIMELINE_LIMITS.maxTotalBytes,
    ),
    maxEventBytes: boundedInteger(
      "maxEventBytes",
      overrides.maxEventBytes ?? DEFAULT_LIVE_TIMELINE_LIMITS.maxEventBytes,
    ),
    maxInputEventBytes: boundedInteger(
      "maxInputEventBytes",
      overrides.maxInputEventBytes ?? DEFAULT_LIVE_TIMELINE_LIMITS.maxInputEventBytes,
    ),
    maxTextBytes: boundedInteger(
      "maxTextBytes",
      overrides.maxTextBytes ?? DEFAULT_LIVE_TIMELINE_LIMITS.maxTextBytes,
    ),
    maxJsonBytes: boundedInteger(
      "maxJsonBytes",
      overrides.maxJsonBytes ?? DEFAULT_LIVE_TIMELINE_LIMITS.maxJsonBytes,
    ),
    maxJsonDepth: boundedInteger(
      "maxJsonDepth",
      overrides.maxJsonDepth ?? DEFAULT_LIVE_TIMELINE_LIMITS.maxJsonDepth,
    ),
    maxEvidencePerEvent: boundedInteger(
      "maxEvidencePerEvent",
      overrides.maxEvidencePerEvent ?? DEFAULT_LIVE_TIMELINE_LIMITS.maxEvidencePerEvent,
    ),
  };
  if (
    limits.maxTotalBytes < limits.maxEventBytes ||
    limits.maxEventBytes < 512 ||
    limits.maxInputEventBytes < 256 ||
    limits.maxTextBytes < 16 ||
    limits.maxJsonBytes < 64 ||
    limits.maxTextBytes > limits.maxEventBytes ||
    limits.maxJsonBytes > limits.maxEventBytes
  ) {
    throw new LiveTimelineError(
      "invalid_limits",
      "timeline byte limits are internally inconsistent",
    );
  }
  return Object.freeze(limits);
}
