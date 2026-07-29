export type ToolAdapterErrorCode =
  | "invalid_action_space"
  | "invalid_tool_arguments"
  | "invalid_tool_options"
  | "invalid_tool_result"
  | "unknown_tool";

export class ToolAdapterError extends Error {
  readonly code: ToolAdapterErrorCode;

  constructor(code: ToolAdapterErrorCode, message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = new.target.name;
    this.code = code;
  }
}

export class InvalidActionSpaceError extends ToolAdapterError {
  constructor(message: string, options?: ErrorOptions) {
    super("invalid_action_space", message, options);
  }
}

export class UnknownToolError extends ToolAdapterError {
  readonly toolName: string;

  constructor(toolName: string) {
    super("unknown_tool", `unknown DeviceRail tool: ${toolName}`);
    this.toolName = toolName;
  }
}

export class InvalidToolArgumentsError extends ToolAdapterError {
  readonly toolName: string;

  constructor(toolName: string, message: string, options?: ErrorOptions) {
    super("invalid_tool_arguments", `${toolName}: ${message}`, options);
    this.toolName = toolName;
  }
}

export class InvalidToolOptionsError extends ToolAdapterError {
  constructor(message: string) {
    super("invalid_tool_options", message);
  }
}

export class InvalidToolResultError extends ToolAdapterError {
  readonly toolName: string;

  constructor(toolName: string, message: string) {
    super("invalid_tool_result", `${toolName}: ${message}`);
    this.toolName = toolName;
  }
}
