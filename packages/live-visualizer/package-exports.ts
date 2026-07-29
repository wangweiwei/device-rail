import {
  DEFAULT_LIVE_TIMELINE_LIMITS,
  LIVE_TIMELINE_MAX_PAGE_SIZE,
  LiveTimeline,
  LiveTimelineError,
  type LiveTimelineState,
  type TimelinePage,
} from "@devicerail/live-visualizer";

const timelineConstructor: typeof LiveTimeline = LiveTimeline;
const errorConstructor: typeof LiveTimelineError = LiveTimelineError;
declare const state: LiveTimelineState;
declare const page: TimelinePage;

void DEFAULT_LIVE_TIMELINE_LIMITS;
void LIVE_TIMELINE_MAX_PAGE_SIZE;
void errorConstructor;
void page;
void state;
void timelineConstructor;
