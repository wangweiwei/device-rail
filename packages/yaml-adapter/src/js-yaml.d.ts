declare module "js-yaml" {
  export interface LoadOptions {
    readonly filename?: string;
    readonly json?: boolean;
    readonly maxDepth?: number;
    readonly maxTotalMergeKeys?: number;
    readonly schema?: unknown;
  }

  export const JSON_SCHEMA: unknown;
  export function load(source: string, options?: LoadOptions): unknown;
}
