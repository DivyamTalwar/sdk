import init, { WasmStream } from "../generated/wasm/acyclic_stream_wasm.js";
import { create, toBinary, fromBinary } from "@bufbuild/protobuf";
import { AppendRequestSchema, AppendResponseSchema, FollowRequestSchema, ReadResponseSchema, RecordBatchSchema } from "../generated/proto/stream/v1/stream_pb.js";

export async function test(endpoint: string) {
  await init();
  let credentials = 0;
  const stream = WasmStream.connect(endpoint, () => { credentials++; return "browser-test"; });
  const append = async (value: number) => {
    const result = await stream.dispatch("append", toBinary(AppendRequestSchema, create(AppendRequestSchema, { path: "browser/events", records: [new Uint8Array([value])] })));
    if (fromBinary(AppendResponseSchema, result).outcome.case !== "committed") throw new Error("append did not commit");
  };
  const sequence = (bytes: Uint8Array | null) => {
    if (bytes === null) throw new Error("follow ended before its delivery");
    const result = fromBinary(ReadResponseSchema, bytes);
    const records = fromBinary(RecordBatchSchema, result.data).records;
    if (records.length !== 1) throw new Error("expected one pushed record");
    return records[0].sequence;
  };
  await append(1);
  const follow = await stream.open_follow(toBinary(FollowRequestSchema, create(FollowRequestSchema, { path: "browser/events", from: 0n })));
  const sibling = await stream.open_follow(toBinary(FollowRequestSchema, create(FollowRequestSchema, { path: "browser/events", from: 1n })));
  if (sequence(await follow.next()) !== 0n) throw new Error("snapshot replay lost its cursor");
  const first = await stream.nextConnection(0n);
  if (await (await fetch(`${endpoint}/fixture/connections`)).text() !== "1") throw new Error("follows did not share one physical session");
  await fetch(`${endpoint}/fixture/recover`, { method: "POST" });
  const second = await stream.nextConnection(first);
  if (second <= first) throw new Error("reconnect without a record did not push its generation");
  await append(2);
  const [a, b] = await Promise.all([follow.next(), sibling.next()]);
  if (sequence(a) !== 1n || sequence(b) !== 1n) throw new Error("reconnect skipped or duplicated a consumed cursor");
  if (credentials < 4) throw new Error("credential resolver was cached across calls");
  follow.close(); sibling.close(); stream.free();
  return { follows: 2, reconnects: 1, records: 3 };
}
