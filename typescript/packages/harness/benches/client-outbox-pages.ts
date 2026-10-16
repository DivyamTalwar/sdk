import { estimateShallowMemoryUsageOf } from "bun:jsc";

const baseline = process.env.HARNESS_OUTBOX_PAGE_BEFORE;
if (!baseline) throw new Error("HARNESS_OUTBOX_PAGE_BEFORE must name an explicit baseline module");
const mode = process.env.HARNESS_OUTBOX_PAGE_MODE ?? "timing";
const original = Object.getOwnPropertyDescriptor;
let reads = 0, descriptors = 0, estimatedDescriptorBytes = 0;
if (mode === "work") {
  // Install the non-retaining observer before modules capture their intrinsics.
  // Timing runs import with the actual, unwrapped intrinsics.
  Object.getOwnPropertyDescriptor = (value, key) => {
    const descriptor = original(value, key);
    if (key === "operationId") reads++;
    if (descriptor) { descriptors++; estimatedDescriptorBytes += estimateShallowMemoryUsageOf(descriptor); }
    return descriptor;
  };
}
const { outboxFixture } = await import("../test/client-outbox-page-fixture.js");
const { IndexedDbClientStore } = await import("../src/client-store.js");
const { IndexedDbClientStore: Before } = await import(baseline) as typeof import("../src/client-store.js");
const count = Number(process.env.HARNESS_OUTBOX_PAGE_COUNT ?? "1024");
if (![0, 64, 129, 1024].includes(count)) throw new Error("unsupported outbox page benchmark count");
const stores = [await outboxFixture(count, undefined, Before), await outboxFixture(count)];
const median = (values: number[]) => values.sort((left, right) => left - right)[values.length >> 1]!;
let checksum = 0;

async function run(index: number): Promise<{ wall: number; cpu: number }> {
  const start = performance.now(), cpuStart = process.cpuUsage();
  const commands = await stores[index]!.load();
  const cpu = process.cpuUsage(cpuStart), wall = (performance.now() - start) * 1_000_000;
  if (commands.length !== count) throw new Error("outbox count changed");
  for (let row = 0; row < commands.length; row++) {
    const expected = String(row).padStart(12, "0");
    if (!commands[row]!.operationId.endsWith(expected)) throw new Error("outbox ordering changed");
  }
  checksum += commands.length;
  return { wall, cpu: (cpu.user + cpu.system) * 1000 };
}

if (mode === "timing") {
  await run(0); await run(1);
  const rows: { before: Awaited<ReturnType<typeof run>>; after: Awaited<ReturnType<typeof run>> }[] = [];
  const controls: typeof rows = [];
  for (let pair = 0; pair < 8; pair++) {
    if (pair % 2 === 0) rows.push({ before: await run(0), after: await run(1) });
    else { const after = await run(1); rows.push({ before: await run(0), after }); }
    controls.push({ before: await run(0), after: await run(0) });
  }
  for (const metric of ["wall", "cpu"] as const) {
    const gains = rows.map(row => 100 * (1 - row.after[metric] / row.before[metric]));
    const noise = controls.map(row => 100 * (1 - row.after[metric] / row.before[metric]));
    console.log(JSON.stringify({ count, metric, beforeNs: median(rows.map(row => row.before[metric])),
      afterNs: median(rows.map(row => row.after[metric])), pairedPercent: median(gains),
      wins: gains.filter(value => value > 0).length, range: [Math.min(...gains), Math.max(...gains)],
      controlPercent: median(noise), controlRange: [Math.min(...noise), Math.max(...noise)] }));
  }
} else {
  for (let index = 0; index < 2; index++) {
    reads = 0; descriptors = 0; estimatedDescriptorBytes = 0;
    await run(index);
    console.log(JSON.stringify({ count, legacy: index === 0, commandCounterVisits: reads,
      descriptors, estimatedDescriptorBytes }));
  }
  Object.getOwnPropertyDescriptor = original;
}
console.log(JSON.stringify({ count, mode, checksum }));
