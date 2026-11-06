/**
 * Typed IPC entry point. `bindings.ts` is generated from the Rust command registry
 * (`cargo xtask gen-bindings`); import commands from here, not from @tauri-apps/api.
 *
 * The generator emits a `_Serialize` shape (what Rust returns) and a `_Deserialize` shape
 * (what Rust accepts) for each DTO; the short names below are the ones screens use.
 */
export { commands } from "./bindings";
export type {
  AppInfo,
  ApprovalTokenDto,
  DatabaseStatus,
  DemoCart,
  DemoProduct,
  DemoSaleResult,
  ErrorCode,
  ErrorDetails,
  IpcError,
  MoneyAddResult,
  Role,
  SessionDto as Session,
  SignInUser,
  TaxMode,
  Unit,
  WorkspaceDto as Workspace,
} from "./bindings";
export type {
  ApprovalScope_Serialize as ApprovalScope,
  BootstrapState_Serialize as BootstrapState,
  CartLineDto_Serialize as CartLine,
  CartViewDto_Serialize as CartView,
  CatalogHitDto_Serialize as CatalogHit,
  CreateWorkspaceInput_Deserialize as CreateWorkspaceInput,
  DiscountDto_Deserialize as DiscountInput,
  DiscountDto_Serialize as Discount,
  ScanOutcomeDto_Serialize as ScanOutcome,
} from "./bindings";

/** The `{status, data | error}` result every command returns. */
export type Result<T> = { status: "ok"; data: T } | { status: "error"; error: import("./bindings").IpcError };
