import { types as utilTypes } from "node:util";

const MAX_JSON_NODES = 100_000;
const MAX_JSON_DEPTH = 256;

type FailureFactory = (message: string) => Error;

interface ArrayWork {
  readonly depth: number;
  readonly kind: "array";
  readonly path: string;
  readonly source: readonly unknown[];
  readonly target: unknown[];
}

interface ObjectWork {
  readonly depth: number;
  readonly descriptors: Readonly<Record<string, PropertyDescriptor>>;
  readonly kind: "object";
  readonly path: string;
  readonly source: object;
  readonly target: Record<string, unknown>;
}

type Work = ArrayWork | ObjectWork;

function isPlainObject(value: object): boolean {
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

export function clonePureJson(value: unknown, failure: FailureFactory): unknown {
  const seen = new Set<object>();
  const pending: Work[] = [];
  let nodeCount = 0;

  const cloneNode = (source: unknown, path: string, depth: number): unknown => {
    nodeCount += 1;
    if (nodeCount > MAX_JSON_NODES) {
      throw failure(`JSON value exceeds ${MAX_JSON_NODES} nodes`);
    }
    if (depth > MAX_JSON_DEPTH) {
      throw failure(`JSON value exceeds ${MAX_JSON_DEPTH} levels`);
    }
    if (source === null || typeof source === "string" || typeof source === "boolean") {
      return source;
    }
    if (typeof source === "number") {
      if (!Number.isFinite(source) || (Number.isInteger(source) && !Number.isSafeInteger(source))) {
        throw failure(`${path} contains a non-finite or unsafe number`);
      }
      return source;
    }
    if (typeof source !== "object") {
      throw failure(`${path} is not a pure JSON value`);
    }
    if (utilTypes.isProxy(source)) {
      throw failure(`${path} must not be a Proxy`);
    }
    if (seen.has(source)) {
      throw failure(`${path} contains a repeated or cyclic object`);
    }
    seen.add(source);

    if (Array.isArray(source)) {
      for (const key of Reflect.ownKeys(source)) {
        if (key === "length") {
          continue;
        }
        if (typeof key !== "string" || !/^(0|[1-9][0-9]*)$/u.test(key)) {
          throw failure(`${path} contains a non-JSON array property`);
        }
        const index = Number(key);
        if (!Number.isSafeInteger(index) || index < 0 || index >= source.length) {
          throw failure(`${path} contains an invalid array index`);
        }
        const descriptor = Object.getOwnPropertyDescriptor(source, key);
        if (!descriptor?.enumerable || !("value" in descriptor)) {
          throw failure(`${path}[${key}] is not a plain JSON property`);
        }
      }
      for (let index = 0; index < source.length; index += 1) {
        if (!Object.hasOwn(source, index)) {
          throw failure(`${path} contains a sparse array slot`);
        }
      }
      const target = new Array<unknown>(source.length);
      pending.push({ depth, kind: "array", path, source, target });
      return target;
    }

    if (!isPlainObject(source)) {
      throw failure(`${path} must use a plain JSON object`);
    }
    const ownKeys = Reflect.ownKeys(source);
    if (ownKeys.some((key) => typeof key !== "string")) {
      throw failure(`${path} contains a symbol property`);
    }
    const descriptors = Object.getOwnPropertyDescriptors(source);
    for (const key of ownKeys as string[]) {
      const descriptor = descriptors[key];
      if (!descriptor?.enumerable || !("value" in descriptor)) {
        throw failure(`${path}.${key} is not a plain JSON property`);
      }
    }
    const target: Record<string, unknown> = {};
    pending.push({ depth, descriptors, kind: "object", path, source, target });
    return target;
  };

  const root = cloneNode(value, "$", 0);
  while (pending.length > 0) {
    const work = pending.pop();
    if (!work) {
      continue;
    }
    if (work.kind === "array") {
      for (let index = 0; index < work.source.length; index += 1) {
        work.target[index] = cloneNode(
          work.source[index],
          `${work.path}[${index}]`,
          work.depth + 1,
        );
      }
      continue;
    }
    for (const [key, descriptor] of Object.entries(work.descriptors)) {
      const child = cloneNode(descriptor.value, `${work.path}.${key}`, work.depth + 1);
      Object.defineProperty(work.target, key, {
        configurable: true,
        enumerable: true,
        value: child,
        writable: true,
      });
    }
  }
  return root;
}

export function deepFreezeJson<T>(value: T): T {
  if (value === null || typeof value !== "object") {
    return value;
  }
  const pending: object[] = [value];
  const seen = new Set<object>();
  while (pending.length > 0) {
    const current = pending.pop();
    if (!current || seen.has(current)) {
      continue;
    }
    seen.add(current);
    for (const child of Object.values(current)) {
      if (child !== null && typeof child === "object") {
        pending.push(child);
      }
    }
    Object.freeze(current);
  }
  return value;
}
