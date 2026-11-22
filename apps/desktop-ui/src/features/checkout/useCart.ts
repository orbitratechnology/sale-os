import { useCallback, useEffect, useRef, useState } from "react";
import { currentLanguage, translate, type T } from "../../i18n";
import { commands, type ApprovalScope, type CartView, type DiscountInput, type IpcError, type Result } from "../../ipc";
import type { WeighOutcomeDto_Serialize as WeighOutcome } from "../../ipc/bindings";

export type Notice =
  | { kind: "error"; error: IpcError }
  | { kind: "notFound"; code: string; hint: string | null }
  | { kind: "info"; text: string };

/** An item sold by weight to weigh (E20-T06): a new line, or `lineId` weighed again. */
export interface WeighRequest {
  variantId: string;
  name: string;
  /** `kg` or `g`. */
  unit: string;
  via: "search" | "quickSelect" | "scanner" | "manual";
  scanEventId: string | null;
  /** Weigh this existing line again (the Weigh key) instead of adding one. */
  lineId?: string;
}

/** What a weighing came to: weighed (applied to the cart), refused with a reason, or an error shown as a notice. */
export type WeighResult = { kind: "weighed"; grams: string } | { kind: "refused"; reason: string; message: string } | { kind: "error" };

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
export function useCart(onSignedOut: () => void, onWeigh?: (request: WeighRequest) => void) {
  const weighRef = useRef(onWeigh);
  useEffect(() => {
    weighRef.current = onWeigh;
  });
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
        setNotice({ kind: "notFound", code: out.code, hint: out.hint });
      } else if (out.outcome === "weigh") {
        // Sold by weight and a scale is set up: the till weighs it under this scan (E20-T06).
        setNotice(null);
        weighRef.current?.({ variantId: out.variantId, name: out.name, unit: out.unit, via: source === "manual" ? "manual" : "scanner", scanEventId });
      } else if (out.outcome === "duplicate") {
        apply(out.cart, seq);
      } else {
        // Found, but through a set-up workaround: say how to fix the scanner (E21-T03).
        setNotice(out.hint ? { kind: "info", text: scanHintText(out.hint) } : null);
        apply(out.cart, seq, out.lineId);
      }
    },
    [apply, failed],
  );

  /** A weighing (E20-T06): a weighed line is applied and selected; a refusal is returned for the dialog to show. */
  const weigh = useCallback(
    async (call: () => Promise<Result<WeighOutcome>>): Promise<WeighResult> => {
      const seq = ++issued.current;
      const res = await call();
      if (res.status === "error") {
        failed(res.error);
        return { kind: "error" };
      }
      if (res.data.outcome === "weighed") {
        apply(res.data.cart, seq, res.data.lineId);
        setNotice(null);
        return { kind: "weighed", grams: res.data.weightGrams };
      }
      return { kind: "refused", reason: res.data.reason, message: res.data.message };
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
    /** E20-T06: weigh an item sold by weight (a new line), or weigh a line again. */
    weighItem: (r: WeighRequest) =>
      weigh(() => (r.lineId ? commands.cartWeighLine(r.lineId) : commands.cartAddWeighed(r.variantId, r.via, r.scanEventId, null))),
    /** The fallback: the weight typed in, as the quantity (thousandths of the unit); a line of its own. */
    addTyped: (r: WeighRequest, qtyMilli: string) => weigh(() => commands.cartAddWeighed(r.variantId, r.via, r.scanEventId, qtyMilli)),
    /** E09-T04: a tile on the quick-select grid. */
    addQuick: (variantId: string) => run(() => commands.cartAddVariant(variantId, null, "quickSelect")),
    setQuantity: (lineId: string, qtyMilli: string) => run(() => commands.cartSetQuantity(lineId, qtyMilli), lineId),
    removeLine: (lineId: string) => run(() => commands.cartRemoveLine(lineId)),
    setLineDiscount: (lineId: string, d: DiscountInput | null) =>
      run((t) => commands.cartSetLineDiscount(lineId, d, t), lineId),
    setOrderDiscount: (d: DiscountInput | null) => run((t) => commands.cartSetOrderDiscount(d, t)),
    overridePrice: (lineId: string, minor: string, reason: string) =>
      run((t) => commands.cartOverridePrice(lineId, minor, reason, t), lineId),
    clear: () => run(() => commands.cartClear(cart?.revision ?? 0)),
    void: (reason: string) => run((t) => commands.cartVoid(cart?.revision ?? 0, reason, t)),
    park: (label: string | null) => run(() => commands.cartPark(cart?.revision ?? 0, label)),
    resume: (cartId: string) => run(() => commands.cartResume(cartId)),
    /** E09-T02: the cart the till was closed with. */
    resumeRecovered: () => run(() => commands.cartRecoveryResume()),
    discardRecovered: (revision: number) => run(() => commands.cartRecoveryDiscard(revision)),
  };
}

export type CartApi = ReturnType<typeof useCart>;

/** What a scan hint from Rust means for the cashier (E21-T01/T03). */
export function scanHintText(hint: string, t: T = (e, v) => translate(currentLanguage(), e, v)): string {
  if (hint.startsWith("keyboard_layout:")) {
    const layout = hint.slice("keyboard_layout:".length);
    const names: Record<string, string> = { azerty_fr: "French AZERTY", azerty_be: "Belgian AZERTY", qwertz_de: "German QWERTZ" };
    return t(
      "The scanner types as if the keyboard were US while Windows uses {layout}. Scans still work, but set the scanner's keyboard country to match (see its manual's setup barcodes).",
      { layout: names[layout] ?? layout },
    );
  }
  if (hint === "check_digit") return t("That barcode's check digit is wrong: probably a misread. Scan it again or type it.");
  return hint;
}
