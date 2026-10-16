/** The shared Rust client owns queued requests, recovery and live file views. */
export {
  WasmFileClient as HarnessClient,
  type FileClientLimits,
  type FileClientDurability,
} from "../generated/wasm/acyclic_harness_wasm.js";
export * from "./client-query.js";
