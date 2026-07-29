import {
  compileYamlPlan,
  executeYamlPlan,
  YAML_PLAN_VERSION,
  type YamlPlan,
  type YamlPlanClient,
} from "@devicerail/yaml-adapter";

const plan: YamlPlan = compileYamlPlan("version: devicerail/v1\nsteps:\n  - id: list\n    method: devices.list\n");
declare const client: YamlPlanClient;
const execution = executeYamlPlan(client, plan);

void execution;
void YAML_PLAN_VERSION;
