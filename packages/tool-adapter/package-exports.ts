import {
  actionToolName,
  DeviceRailToolAdapter,
  OBSERVATION_TOOL_NAME,
  type DeviceRailToolCatalog,
  type ToolInvocationResult,
} from "@devicerail/tool-adapter";

const adapterConstructor: typeof DeviceRailToolAdapter = DeviceRailToolAdapter;
const observationName: string = OBSERVATION_TOOL_NAME;
const portableName: string = actionToolName("tap");
declare const catalog: DeviceRailToolCatalog;
const invocation: Promise<ToolInvocationResult> = catalog.invoke({ name: portableName });

void adapterConstructor;
void invocation;
void observationName;
