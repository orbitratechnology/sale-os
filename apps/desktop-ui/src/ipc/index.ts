/**
 * Typed IPC entry point. `bindings.ts` is generated from the Rust command registry
 * (`cargo xtask gen-bindings`); import commands from here, not from @tauri-apps/api.
 */
export { commands } from "./bindings";
export type {
  AppInfo,
  DatabaseStatus,
  DemoCart,
  DemoProduct,
  DemoSaleResult,
  ErrorCode,
  IpcError,
  MoneyAddResult,
} from "./bindings";
