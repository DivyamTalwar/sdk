import { IDBFactory } from "fake-indexeddb";
import "fake-indexeddb/auto";
import type { ClientCommand } from "../src/client.js";
import { cloneStructuredValue } from "../src/client-data.js";
import { IndexedDbClientStore } from "../src/client-store.js";
import { NativeContracts } from "../src/native-contracts.js";

export function pageCommand(index: number): ClientCommand {
  return {
    operationId: `01010101-0101-0101-0101-${String(index).padStart(12, "0")}` as ClientCommand["operationId"],
    authority: { kind: "conversation", id: "outbox-pages" }, kind: "message.append",
    payload: { metadata: { sequence: index } }, offlineSafe: true,
  };
}

export async function outboxFixture(count: number, maximumResidentBytes = 16 * 1024 * 1024,
  Store = IndexedDbClientStore): Promise<IndexedDbClientStore> {
  const indexedDB = new IDBFactory();
  const databaseName = "outbox-page-fixture";
  const store = new Store({ indexedDB, databaseName, maximumCommands: Math.max(count, 1), maximumResidentBytes });
  // Initialize through the real reader, then bulk seed canonical persisted rows.
  await store.loadPage();
  const database = await new Promise<IDBDatabase>((resolve, reject) => {
    const opening = indexedDB.open(databaseName);
    opening.onsuccess = () => resolve(opening.result);
    opening.onerror = () => reject(opening.error);
  });
  const contracts = await NativeContracts.create();
  const records = Array.from({ length: count }, (_, index) => {
    const command = pageCommand(index);
    const bytes = contracts.encodeCanonicalJson(cloneStructuredValue(command, new Set(), false, true)).byteLength;
    return { operationId: command.operationId, command, bytes, sequence: index + 1 };
  });
  const transaction = database.transaction(["outbox", "cursors"], "readwrite");
  const done = new Promise<void>((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onabort = () => reject(transaction.error ?? new Error("fixture transaction aborted"));
    transaction.onerror = () => reject(transaction.error);
  });
  for (const record of records) transaction.objectStore("outbox").put(record);
  transaction.objectStore("cursors").put({ authority: "@harness/outbox-sequence", sequence: count, checkpointVersion: 0n });
  await done;
  database.close();
  // The real writer verifies every seeded row, encoded byte count and watermark.
  if (count !== 0) await store.put(pageCommand(0));
  return store;
}
