import { createInterface } from "node:readline";
import { create, equals, fromBinary, toBinary } from "@bufbuild/protobuf";
import * as wire from "../generated/proto/stream/v1/session_pb.js";
import { SessionNode, checkpoint, frame } from "./session-node.js";

const fault = process.env.STREAM_SESSION_FAULT ?? "";
if (fault) {
  const { plugin } = await import("bun");
  plugin({ name: "follow-data-negative-control", setup(build) {
    build.onLoad({ filter: /[\\/]src[\\/](follow-data|session)\.ts$/ }, async args => {
      let source = await Bun.file(args.path).text();
      const edits: Record<string, [string, string]> = {
        latestStrict: ["return after.next - 1n;", "if (after.next !== next + 1n) throw invalid(); return after.next - 1n;"],
        skipAll: ["if (after.next === next + 1n) return next;", "return next;"],
        emptyKey: [" && data.item.value.key.length > 0", ""],
        watermarks: ["data.durable > data.accepted", "false"],
        opaque: ["after.opaque.length !== 0", "false"],
        commit: ["validateRecord(data.item.value, next);", "void data.item.value;"],
        wrongMode: ["delivery === SessionDelivery.LATEST &&", "true &&"],
        progress: ["route.applied = delivery.sequence + 1n;", "route.applied += 1n;"],
        metadata: ["value = data;", "value = { ...data, durable: 0n };"],
        confirmation: ["!sameBytes(route.kind.value.precedingCommitId, body.value.precedingCommitId)", "false"],
        shortGuard: ["guard !== 32", "guard !== 31"],
      };
      const edit = edits[fault];
      if (edit === undefined) throw new Error(`unknown fault ${fault}`);
      const target = ["progress", "metadata", "confirmation"].includes(fault) ? "session.ts" : "follow-data.ts";
      if (args.path.endsWith(target)) {
        if (!source.includes(edit[0])) throw new Error(`negative control no longer matches: ${fault}`);
        source = source.replace(edit[0], edit[1]);
      }
      return { contents: source, loader: "ts" };
    });
  } });
}
const { StreamSessions } = await import("../src/session.js");

async function decode(input: { hex: string; delivery: number; from: string }) {
  let decoded: wire.SessionFrame;
  try { decoded = fromBinary(wire.SessionFrameSchema, Buffer.from(input.hex, "hex")); }
  catch { return { valid: false }; }
  if (!equals(wire.SessionFrameSchema, decoded, fromBinary(wire.SessionFrameSchema, toBinary(wire.SessionFrameSchema, decoded)))) throw new Error("frame round trip changed fields");
  if (decoded.body.case === "opened") return confirm(decoded.body.value);
  if (decoded.body.case !== "data") return { valid: true };
  const data = decoded.body.value;
  const peer = new SessionNode();
  const sessions = new StreamSessions(peer.connect);
  const cancel = new AbortController();
  const iterator = sessions.followData(create(wire.SessionFollowSchema, { path: "probe", delivery: input.delivery }), { from: BigInt(input.from), signal: cancel.signal });
  let outcome = "pending";
  const pending = iterator.next().then(result => {
    if (!result.done) {
      if (!equals(wire.SessionDataSchema, data, result.value)) throw new Error("raw follow changed data or watermarks");
      outcome = "data";
    }
  }, error => { outcome = error?.code ?? "unavailable"; });
  await checkpoint();
  peer.connections[0]!.output.push(frame(1n, 1n, { case: "data", value: data }));
  await checkpoint();
  cancel.abort();
  await pending;
  await iterator.return(undefined);
  for (const connection of peer.connections) peer.disconnect(connection.node);
  return { valid: true, outcome, after: data.after?.next.toString(), opaque: data.after?.opaque.length,
    accepted: data.accepted.toString(), durable: data.durable.toString(), item: data.item.case,
    sequence: data.item.case === "record" ? data.item.value.sequence.toString() : undefined,
    commit: data.item.case === "record" ? data.item.value.commitId.length : undefined,
    size: data.item.case === "record" ? data.item.value.value.length : undefined,
    key: data.item.case === "latestValue" ? data.item.value.key.length : undefined };
}

async function confirm(opened: wire.SessionOpened) {
  const peer = new SessionNode();
  const sessions = new StreamSessions(async (node, signal) => {
    const transport = await peer.connect(node, signal);
    return { ...transport, incoming: { async *[Symbol.asyncIterator]() {
      for await (const input of transport.incoming) {
        if (input.body.case === "opened") input.body.value = opened;
        yield input;
      }
    } } };
  });
  const cancel = new AbortController();
  const iterator = sessions.followData(create(wire.SessionFollowSchema, { path: "probe", delivery: wire.SessionDelivery.ALL, precedingCommitId: new Uint8Array(32).fill(7) }), { from: 1n, signal: cancel.signal });
  let outcome = "pending";
  const pending = iterator.next().then(() => {}, error => { outcome = error?.code ?? "unavailable"; });
  await checkpoint();
  const granted = peer.received.some(input => input.channelId === 1n && input.body.case === "window");
  cancel.abort(); await pending; await iterator.return(undefined);
  peer.disconnect();
  return { valid: true, confirmation: [...opened.precedingCommitId], resume: opened.resume?.next.toString(), opaque: opened.resume?.opaque.length, outcome, granted };
}

let model: Awaited<ReturnType<typeof open>> | undefined;
async function open() {
  const peer = new SessionNode();
  for (const next of [2n, 5n, 9n]) peer.publishLatest("status", next, next === 5n);
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  peer.beforeConnect = async () => { if (peer.connections.length) await gate; };
  const client = new StreamSessions(peer.connect);
  const cancel = new AbortController();
  const records = client.followData(create(wire.SessionFollowSchema, { path: "status", delivery: wire.SessionDelivery.LATEST }), { from: 0n, signal: cancel.signal });
  const seen: string[] = [];
  const attention: string[] = [];
  let error: unknown;
  let settled = true;
  function consume() {
    if (!settled) throw new Error("two next calls outstanding");
    settled = false;
    void records.next().then(result => {
      settled = true;
      if (!result.done) {
        seen.push(result.value.after!.next.toString());
        if (result.value.item.case === "latestValue" && result.value.item.value.attention) attention.push(result.value.after!.next.toString());
      }
    }, failure => { settled = true; error = failure; });
  }
  consume(); await checkpoint();
  return { peer, records, cancel, seen, attention, release, consume,
    state: () => ({ seen, attention, settled, failure: error === undefined ? null : String(error), resumes: peer.resumes.map(resume => resume.next.toString()), outstanding: peer.outstandingFrames, active: peer.active }) };
}

for await (const line of createInterface({ input: process.stdin })) {
  try {
    const input = JSON.parse(line);
    let result: unknown;
    if (input.action === "decode") result = await decode(input);
    else if (input.action === "validate") {
      try {
        new StreamSessions(new SessionNode().connect).followData(create(wire.SessionFollowSchema, { path: "probe", delivery: input.delivery, precedingCommitId: new Uint8Array(input.guard) }), { from: BigInt(input.from) });
        result = { accepted: true };
      } catch (error) { result = { accepted: false, code: error instanceof Error && "code" in error ? error.code : null }; }
    }
    else {
      model ??= await open();
      switch (input.action) {
        case "ready": break;
        case "consume": model.consume(); break;
        case "disconnect": model.peer.disconnect(); break;
        case "move": model.peer.move("status"); break;
        case "release": model.release(); break;
        case "abort": model.cancel.abort(); break;
        case "finish": model.release(); await checkpoint(); model.consume(); break;
        case "stop": model.cancel.abort(); await model.records.return(undefined); break;
        default: throw new Error(`unknown action ${input.action}`);
      }
      await checkpoint();
      result = model.state();
    }
    process.stdout.write(`${JSON.stringify(result)}\n`);
  } catch (error) { process.stdout.write(`${JSON.stringify({ error: String(error) })}\n`); }
}
