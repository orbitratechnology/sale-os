import { useCallback, useEffect, useRef, useState } from "react";
import { commands, type ApprovalScope, type CartView, type DiscountInput, type IpcError, type Result } from "../../ipc";

export type Notice =
  | { kind: "error"; error: IpcError }
  | { kind: "notFound"; code: string }
  | { kind: "info"; text: string };

/** A command that needs a manager's approval: what to approve, and how to retry with it. */
export interface PendingApproval {
  scope: ApprovalScope;
  retry: (approvalTokenId: string) => Promise<boolean>;
}

/** A cart command, called again with the approval token when one is needed. */
type CartCall = (approvalTokenId: string | null) => Promise<Result<CartView>>;

/** How long an added or changed line is highlighted (E09-T03: non-animated, ≤ 150 ms). */
export const HIGHLIGHT_MS = 150;

/**
 * The cart as Rust last reported it, plus the selection and transient notices.
 *
 * Responses can arrive out of order (commands run concurrently), so a view is applied only
 * if it is newer: a higher revision of the same cart, or a later request after the cart
 * was replaced (void).
 */
export function useCart(onSignedOut: () => void) {
  const [cart, setCart] = useState<CartView | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [highlight, setHighlight] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [approval, setApproval] = useState<PendingApproval | null>(null);
  const applied = useRef({ cartId: "", revision: -1, seq: 0 });
  const issued = useRef(0);
  const flashTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const apply = useCallback((view: CartView, seq: number, touched?: string) => {
    const cur = applied.current;
    const newer = view.cartId === cur.cartId ? view.revision >= cur.revision : seq > cur.seq;
    if (!newer) return;
    applied.current = { cartId: view.cartId, revision: view.revision, seq };
    setCart(view);
    setSelected((sel) => {
      if (touched) return touched;
      if (sel && view.lines.some((l) => l.lineId === sel)) return sel;
      return view.lines.at(-1)?.lineId ?? null;
    });
    if (touched) {
      setHighlight(touched);
      clearTimeout(flashTimer.current);
      flashTimer.current = setTimeout(() => setHighlight(null), HIGHLIGHT_MS);
    }
  }, []);

  // Set once `refresh` exists; `failed` needs it and `refresh` needs `failed`.
  const refreshRef = useRef<() => Promise<boolean>>(() => Promise.resolve(false));

  const failed = useCallback(
    (error: IpcError) => {
      if (error.code === "SESSION_LOCKED") onSignedOut();
      else setNotice({ kind: "error", error });
      // A stale revision means our view is behind: fetch the current cart.
      if (error.code === "CART_REVISION_MISMATCH") void refreshRef.current();
    },
    [onSignedOut],
  );

  /**
   * Runs a command that returns a cart view and applies the result. `APPROVAL_REQUIRED`
   * opens the approval dialog, which retries the same command with the token (E05-T06).
   */
  // `run` hands itself to the approval dialog as the retry; through a ref, since a callback
  // cannot refer to itself while it is being created.
  const runRef = useRef<(call: CartCall, touched?: string, token?: string | null) => Promise<boolean>>(() =>
    Promise.resolve(false),
  );
  const run = useCallback(
    async (call: CartCall, touched?: string, token: string | null = null): Promise<boolean> => {
      const seq = ++issued.current;
      const res = await call(token);
      if (res.status === "ok") {
        apply(res.data, seq, touched);
        setNotice(null);
        return true;
      }
      const scope = res.error.details?.scope;
      if (res.error.code === "APPROVAL_REQUIRED" && scope) {
        setApproval({
          scope: { permission: scope.permission, entityId: scope.entityId, maxPpm: scope.maxPpm ?? null },
          retry: (t) => runRef.current(call, touched, t),
        });
        return false;
      }
      failed(res.error);
      return false;
    },
    [apply, failed],
  );

  useEffect(() => {
    runRef.current = run;
  }, [run]);

  const refresh = useCallback(() => run(() => commands.cartCurrent()), [run]);

  useEffect(() => {
    refreshRef.current = refresh;
    void refresh();
    return () => clearTimeout(flashTimer.current);
  }, [refresh]);

  const scan = useCallback(
    async (raw: string, scanEventId: string, source: "scanner" | "manual") => {
      const seq = ++issued.current;
      const res = await commands.scanSubmit(scanEventId, raw, source);
      if (res.status === "error") return failed(res.error);
      const out = res.data;
      if (out.outcome === "notFound") {
        setNotice({ kind: "notFound", code: out.code });
      } else if (out.outcome === "duplicate") {
        apply(out.cart, seq);
      } else {
        setNotice(null);
        apply(out.cart, seq, out.lineId);
      }
    },
    [apply, failed],
  );

  const line = (id: string | null) => cart?.lines.find((l) => l.lineId === id) ?? null;

  return {
    cart,
    selected,
    highlight,
    notice,
    setSelected,
    setNotice,
    refresh,
    scan,
    line,
    approval,
    setApproval,
    addVariant: (variantId: string) => run(() => commands.cartAddVariant(variantId, null, "search")),
    setQuantity: (lineId: string, qtyMilli: string) => run(() => commands.cartSetQuantity(lineId, qtyMilli), lineId),
    removeLine: (lineId: string) => run(() => commands.cartRemoveLine(lineId)),
    setLineDiscount: (lineId: string, d: DiscountInput | null) =>
      run((t) => commands.cartSetLineDiscount(lineId, d, t), lineId),
    setOrderDiscount: (d: DiscountInput | null) => run((t) => commands.cartSetOrderDiscount(d, t)),
    overridePrice: (lineId: string, minor: string, reason: string) =>
      run((t) => commands.cartOverridePrice(lineId, minor, reason, t), lineId),
    clear: () => run(() => commands.cartClear(cart?.revision ?? 0)),
    void: (reason: string) => run((t) => commands.cartVoid(cart?.revision ?? 0, reason, t)),
  };
}

export type CartApi = ReturnType<typeof useCart>;
