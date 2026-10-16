/** One Rust file fold and cursor follow; the platform provider owns all I/O. */
export {
  WasmClientQuery as ClientQuery,
  WasmQueryViews as QueryViews,
  WasmTaskVolume as TaskVolume,
  WasmStream,
  type Channel,
  type Cursor,
  type FileKey,
  type Generation,
  type Query,
  type QueryDelivery,
  type QueryFileDelta,
  type QueryFileInfo,
  type QueryViewLimits,
} from "../generated/wasm/acyclic_harness_wasm.js";
export { ensureHarnessWasm as initializeClientQueries } from "./wasm-runtime.js";
