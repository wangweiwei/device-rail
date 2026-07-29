import {
  ExecutionRecorder,
  RecorderError,
  type RecorderCheckpoint,
} from "@devicerail/recorder";

const recorderConstructor: typeof ExecutionRecorder = ExecutionRecorder;
const recorderErrorConstructor: typeof RecorderError = RecorderError;
declare const checkpoint: RecorderCheckpoint;

void checkpoint;
void recorderConstructor;
void recorderErrorConstructor;
