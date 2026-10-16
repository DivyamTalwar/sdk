// Original full traversal, independent of the incremental entry point.
export function legacyRetainedDataBytes(value: unknown, maximum: number, allowUndefined = false): number {
  let bytes = 0;
  const ancestors = new Set<object>();
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
    if (ancestors.has(data)) throw new TypeError("command contains a cycle");
    if (data instanceof ArrayBuffer) { charge(data.byteLength); return; }
    if (ArrayBuffer.isView(data)) { charge(data.buffer.byteLength); return; }
    if (Array.isArray(data)) charge(data.length * 8);
    if (!Array.isArray(data) && Object.getPrototypeOf(data) !== Object.prototype && Object.getPrototypeOf(data) !== null) {
      throw new TypeError("command contains a non-canonical structured value");
    }
    ancestors.add(data);
    try {
      for (const key in data) {
        if (!Object.hasOwn(data, key)) continue;
        charge(32 + key.length * 2);
        const descriptor = Object.getOwnPropertyDescriptor(data, key)!;
        if (!("value" in descriptor)) throw new TypeError("command contains an accessor");
        visit(descriptor.value, depth + 1);
      }
    } finally { ancestors.delete(data); }
  };
  visit(value, 0);
  return bytes;
}

