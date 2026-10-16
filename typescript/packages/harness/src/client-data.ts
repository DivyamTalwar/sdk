import { type ClientCommand } from "./client.js";
import { isSafeAuthorityId } from "./rust-policy.js";
import { NativeContracts } from "./native-contracts.js";

// Private clones must never pass through subsequently installed descriptor hooks.
const { getOwnPropertyDescriptor, getOwnPropertyDescriptors, getPrototypeOf, hasOwn, defineProperty,
  keys: objectKeys, entries: objectEntries } = Object;
const objectPrototype = Object.prototype;
const { ownKeys } = Reflect;
const { isArray } = Array;
const { isView } = ArrayBuffer;
const DataSet = Set, DataArray = Array, DataBuffer = ArrayBuffer, DataBytes = Uint8Array;
const ancestorHas = Function.prototype.call.bind(Set.prototype.has);
const ancestorAdd = Function.prototype.call.bind(Set.prototype.add);
const ancestorDelete = Function.prototype.call.bind(Set.prototype.delete);
const bufferSlice = Function.prototype.call.bind(ArrayBuffer.prototype.slice);
const bytesSlice = Function.prototype.call.bind(Uint8Array.prototype.slice);

export function retainedDataBytes(value: unknown, maximum: number, allowUndefined = false): number {
  return measureDataBytes(value, maximum, allowUndefined);
}

/** Extend a measured prefix of a private, dense array whose prefix cannot change. */
export function retainedArrayTailBytes(value: readonly unknown[], maximum: number, start: number, bytes: number): number {
  return start === 0 ? retainedDataBytes(value, maximum) : measureDataBytes(value, maximum, false, start, bytes);
}

function measureDataBytes(value: unknown, maximum: number, allowUndefined: boolean, start?: number, bytes = 0): number {
  const ancestors = new DataSet<object>();
  const charge = (amount: number): void => {
    if (amount > maximum - bytes) throw new RangeError("client command byte capacity exceeded");
    bytes += amount;
  };
  const visit = (data: unknown, depth: number): void => {
    if (depth > 64) throw new RangeError("client command nesting exceeded");
    charge(32);
    if (data === undefined && allowUndefined) return;
    if (typeof data === "string") { charge(data.length * 2); return; }
    if (data === null || typeof data === "boolean" || typeof data === "number") return;
    if (typeof data === "bigint") { charge(data.toString().length * 2); return; }
    if (typeof data !== "object") throw new TypeError("command contains a non-data value");
    if (ancestorHas(ancestors, data)) throw new TypeError("command contains a cycle");
    if (data instanceof DataBuffer) { charge(data.byteLength); return; }
    if (isView(data)) { charge(data.buffer.byteLength); return; }
    if (isArray(data)) charge(data.length * 8);
    if (!isArray(data) && getPrototypeOf(data) !== objectPrototype && getPrototypeOf(data) !== null) {
      throw new TypeError("command contains a non-canonical structured value");
    }
    ancestorAdd(ancestors, data);
    try {
      for (const key in data) {
        if (!hasOwn(data, key)) continue;
        charge(32 + key.length * 2);
        const descriptor = getOwnPropertyDescriptor(data, key)!;
        if (!("value" in descriptor)) throw new TypeError("command contains an accessor");
        visit(descriptor.value, depth + 1);
      }
    } finally { ancestorDelete(ancestors, data); }
  };
  if (start === undefined) visit(value, 0);
  else {
    const array = value as readonly unknown[];
    charge((array.length - start) * 8);
    ancestorAdd(ancestors, array);
    try {
      for (let index = start; index < array.length; index++) {
        const key = String(index);
        charge(32 + key.length * 2);
        const descriptor = getOwnPropertyDescriptor(array, key)!;
        if (!("value" in descriptor)) throw new TypeError("command contains an accessor");
        visit(descriptor.value, 1);
      }
    } finally { ancestorDelete(ancestors, array); }
  }
  return bytes;
}

export async function assertOutboxSafe(command: ClientCommand): Promise<ClientCommand> {
  const admitted = cloneStructuredValue(command, new DataSet()) as ClientCommand;
  if (admitted.offlineSafe !== true || admitted.kind === "interaction.resolve.approval") {
    throw new TypeError("command is not safe for the offline outbox");
  }
  const fields = objectKeys(admitted);
  if (fields.length !== 5 || fields.some(field => !["operationId", "authority", "kind", "payload", "offlineSafe"].includes(field))) {
    throw new TypeError("offline outbox command contains an unsupported field");
  }
  if (admitted.authority === null || typeof admitted.authority !== "object"
    || getPrototypeOf(admitted.authority) !== objectPrototype
    || objectKeys(admitted.authority).length !== 2
    || !objectKeys(admitted.authority).every(field => field === "kind" || field === "id")) {
    throw new TypeError("offline outbox authority contains an unsupported field");
  }
  if (!(await isSafeAuthorityId(admitted.authority.id))) {
    throw new TypeError("offline outbox authority contains an unsafe identity");
  }
  const forbidden = /(?:token|authorization|credential|secret|password|api[_-]?key|(?:^|[_-])(?:scope|proof|body|text|bytes|base64|data)(?:$|[_-]))/i;
  const visit = (value: unknown): void => {
    if (value === null || typeof value !== "object") return;
    if (value instanceof DataBuffer || isView(value)) {
      throw new TypeError("offline outbox cannot persist inline bytes or credentials");
    }
    const entries = objectEntries(value);
    for (let index = 0; index < entries.length; index++) {
      const key = entries[index]![0], item = entries[index]![1];
      if (forbidden.test(key)) throw new TypeError("offline outbox cannot persist inline bytes or credentials");
      visit(item);
    }
  };
  visit(admitted);
  const payload = admitted.payload;
  if (payload === null || typeof payload !== "object" || isArray(payload) ||
      getPrototypeOf(payload) !== objectPrototype) {
    throw new TypeError("offline outbox payload must be a ref-only record");
  }
  for (const key of objectKeys(payload)) {
    if (!["content", "attachments", "artifacts", "references", "metadata"].includes(key)) {
      throw new TypeError("offline outbox payload contains an unsupported field");
    }
  }
  const contracts = await NativeContracts.create();
  if (payload.content !== undefined) contracts.validate("file_ref", payload.content);
  if (payload.attachments !== undefined) contracts.validate("attachments", payload.attachments);
  for (const field of ["artifacts", "references"] as const) {
    const refs = payload[field];
    if (refs === undefined) continue;
    if (!isArray(refs)) throw new TypeError("offline outbox references must be a list");
    for (let index = 0; index < refs.length; index++) contracts.validate("file_ref", refs[index]!);
  }
  if (payload.metadata !== undefined) {
    if (payload.metadata === null || typeof payload.metadata !== "object" || isArray(payload.metadata) ||
        getPrototypeOf(payload.metadata) !== objectPrototype) {
      throw new TypeError("offline outbox metadata must be a record");
    }
    const entries = objectEntries(payload.metadata);
    for (let index = 0; index < entries.length; index++) {
      const key = entries[index]![0], value = entries[index]![1];
      if (forbidden.test(key) || !(value === null || ["number", "boolean", "bigint"].includes(typeof value))) {
        throw new TypeError("offline outbox metadata contains an unsafe value");
      }
    }
  }
  return admitted;
}

export function cloneStructuredValue(value: unknown, ancestors: Set<object>, allowUndefined = false, canonical = false): unknown {
  if (value === undefined && allowUndefined) return value;
  if (
    value === null || typeof value === "string" || typeof value === "boolean"
  ) return value;
  if (typeof value === "bigint") return canonical ? { $bigint: value.toString() } : value;
  if (typeof value === "number") {
    if (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value))) {
      throw new TypeError("command contains a non-finite number or inexact integer");
    }
    return value;
  }
  if (typeof value !== "object") throw new TypeError("command contains a non-data value");
  if (ancestorHas(ancestors, value)) throw new TypeError("command contains a cycle");
  if (value instanceof DataBuffer) return canonical ? { $bytes: [...new DataBytes(value)] } : bufferSlice(value, 0);
  if (value instanceof DataBytes) return canonical ? { $bytes: [...value] } : bytesSlice(value);
  if (isView(value)) {
    throw new TypeError("command contains an unsupported binary view");
  }
  if (!isArray(value) && getPrototypeOf(value) !== objectPrototype && getPrototypeOf(value) !== null) {
    throw new TypeError("command contains a non-canonical structured value");
  }
  const descriptors = getOwnPropertyDescriptors(value);
  const keys = ownKeys(descriptors);
  if (keys.some(key => typeof key !== "string")) {
    throw new TypeError("command contains symbol properties");
  }
  ancestorAdd(ancestors, value);
  try {
    if (isArray(value)) {
      const cloned = new DataArray<unknown>(value.length);
      for (const key of keys as string[]) {
        if (key === "length" || descriptors[key]?.enumerable !== true) continue;
        const index = Number(key);
        if (!Number.isSafeInteger(index) || index < 0 || index >= value.length || String(index) !== key) {
          throw new TypeError("command array contains custom properties");
        }
        const descriptor = descriptors[key];
        if (descriptor === undefined || !("value" in descriptor)) {
          throw new TypeError("command contains an accessor");
        }
        cloned[index] = cloneStructuredValue(descriptor.value, ancestors, allowUndefined, canonical);
      }
      return cloned;
    }
    const cloned: Record<string, unknown> = {};
    for (const key of keys as string[]) {
      const descriptor = descriptors[key];
      if (descriptor?.enumerable !== true) continue;
      if (!("value" in descriptor)) throw new TypeError("command contains an accessor");
      defineProperty(cloned, key, { value: cloneStructuredValue(descriptor.value, ancestors, allowUndefined, canonical),
        enumerable: true, configurable: true, writable: true });
    }
    return cloned;
  } finally {
    ancestorDelete(ancestors, value);
  }
}
