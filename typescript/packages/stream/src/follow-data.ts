import { SessionDelivery, type SessionData, type SessionFollow } from "../generated/proto/stream/v1/session_pb.js";
import { validateRecord } from "./record.js";
import { StreamError } from "./types.js";

const MAX_U64 = (1n << 64n) - 1n;

export function validateFollow(follow: SessionFollow, from: bigint): void {
  const guard = follow.precedingCommitId.length;
  if (from < 0n || from > MAX_U64 ||
      (follow.delivery !== SessionDelivery.ALL && follow.delivery !== SessionDelivery.LATEST) ||
      (guard !== 0 && (guard !== 32 || from === 0n || follow.delivery !== SessionDelivery.ALL))) {
    throw new StreamError("invalid_argument", "invalid session follow mode, cursor or preceding commit");
  }
}

/** Last applied position: ALL is consecutive; LATEST may skip conflated values. */
export function followSequence(delivery: SessionDelivery, next: bigint, data: SessionData): bigint {
  const after = data.after;
  if (after === undefined || after.opaque.length !== 0 || after.next <= next ||
      after.next > MAX_U64 || data.accepted < after.next || data.accepted > MAX_U64 ||
      data.durable < 0n || data.durable > data.accepted) throw invalid();
  if (delivery === SessionDelivery.ALL && data.item.case === "record") {
    validateRecord(data.item.value, next);
    if (after.next === next + 1n) return next;
  } else if (delivery === SessionDelivery.LATEST && data.item.case === "latestValue" && data.item.value.key.length > 0) {
    return after.next - 1n;
  }
  throw invalid();
}

function invalid(): StreamError {
  return new StreamError("invalid_response", "invalid Streams follow data");
}
