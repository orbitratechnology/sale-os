import { useCallback, useEffect, useState } from "react";
import {
  commands,
  type BootstrapState,
  type IpcError,
  type Result,
  type Session,
} from "./ipc";
import { SetupWizard } from "./features/setup/SetupWizard";
import { SignIn } from "./features/auth/SignIn";
import { Checkout } from "./features/checkout/Checkout";
import { ProductEditor } from "./features/catalog/ProductEditor";
import { ProductList } from "./features/catalog/ProductList";
import { ReturnScreen } from "./features/returns/ReturnScreen";
import { SalesScreen } from "./features/sales/SalesScreen";
import { DrawerScreen } from "./features/cash/DrawerScreen";
import { StockScreen } from "./features/stock/StockScreen";
import { ReportsScreen } from "./features/reports/ReportsScreen";
import { AuditScreen } from "./features/audit/AuditScreen";
import { ErrorPanel } from "./features/shell/ErrorPanel";
import { SettingsScreen } from "./features/settings/SettingsScreen";
import { ScanPreferences } from "./features/scan/preferences";
import type {
  ScanDetectorConfig,
  ScanSuffix,
} from "./features/scan/scanDetector";

/**
 * The app shell: first run → sign-in → checkout. Rust holds the session; this component only
 * mirrors it, and goes back to sign-in whenever a command reports `SESSION_LOCKED`.
 */
export function App() {
  const [state, setState] = useState<BootstrapState | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  const [screen, setScreen] = useState<Screen>({ kind: "till" });

  const apply = useCallback((res: Result<BootstrapState>) => {
    if (res.status === "ok") {
      setState(res.data);
      setError(null);
    } else setError(res.error);
  }, []);

  const refresh = useCallback(
    () => void commands.appBootstrapState().then(apply),
    [apply],
  );

  useEffect(() => {
    let live = true;
    void commands.appBootstrapState().then((res) => live && apply(res));
    return () => {
      live = false;
    };
  }, [apply]);

  const signedIn = useCallback((session: Session) => {
    setState((s) => (s ? { ...s, session } : s));
  }, []);
  const signedOut = useCallback(() => {
    setState((s) => (s ? { ...s, session: null } : s));
  }, []);

  // The shop's till preferences (E04-T03): scanner settings and the idle lock.
  const [scanPrefs, setScanPrefs] = useState<Partial<ScanDetectorConfig>>({});
  const [lockAfterSecs, setLockAfterSecs] = useState(0);
  const userId = state?.session?.userId ?? null;
  const loadPrefs = useCallback(() => {
    void commands.appPreferences().then((res) => {
      if (res.status !== "ok") return;
      setScanPrefs({
        suffix: res.data.scannerSuffix as ScanSuffix,
        maxInterKeyMs: res.data.scannerMaxKeyGapMs,
      });
      setLockAfterSecs(res.data.lockTimeoutSecs);
    });
  }, []);
  useEffect(() => {
    if (userId) loadPrefs();
  }, [userId, loadPrefs]);
  useEffect(() => {
    if (!userId || lockAfterSecs <= 0) return;
    let last = Date.now();
    const seen = () => {
      last = Date.now();
    };
    const events = ["keydown", "pointerdown", "wheel"] as const;
    for (const e of events)
      window.addEventListener(e, seen, { capture: true, passive: true });
    const timer = window.setInterval(() => {
      if (Date.now() - last < lockAfterSecs * 1000) return;
      window.clearInterval(timer);
      // The cart is kept in Rust for the next sign-in.
      void commands.authLock().then(() => {
        setScreen({ kind: "till" });
        signedOut();
      });
    }, 1000);
    return () => {
      window.clearInterval(timer);
      for (const e of events)
        window.removeEventListener(e, seen, { capture: true });
    };
  }, [userId, lockAfterSecs, signedOut]);

  if (error) {
    return (
      <main className="shell">
        <ErrorPanel error={error} />
      </main>
    );
  }
  if (!state) return <main className="shell" aria-busy="true" />;
  if (!state.setupComplete || !state.workspace)
    return <SetupWizard onDone={refresh} />;
  if (!state.session || state.session.mustChangePin) {
    return (
      <SignIn
        workspace={state.workspace}
        session={state.session}
        onSignedIn={signedIn}
      />
    );
  }
  const workspace = state.workspace;
  // UI hiding only: Rust refuses edits by roles without CatalogEdit regardless.
  const canEdit = state.session.role !== "cashier";
  const view = (() => {
    switch (screen.kind) {
      case "returns":
        return (
          <ReturnScreen
            key={screen.receipt ?? ""}
            workspace={workspace}
            session={state.session}
            {...(screen.receipt ? { initialReceipt: screen.receipt } : {})}
            onBack={() => setScreen({ kind: "till" })}
          />
        );
      case "settings":
        return (
          <SettingsScreen
            onBack={() => setScreen({ kind: "till" })}
            onChanged={loadPrefs}
          />
        );
      case "reports":
        return (
          <ReportsScreen
            workspace={workspace}
            onBack={() => setScreen({ kind: "till" })}
            onAudit={
              state.session.role === "owner"
                ? () => setScreen({ kind: "audit" })
                : null
            }
          />
        );
      case "audit":
        return (
          <AuditScreen
            onBack={() => setScreen({ kind: "reports" })}
            onOpenProduct={(productId) =>
              setScreen({ kind: "editor", productId, returnTo: "audit" })
            }
            onReturn={(receipt) => setScreen({ kind: "returns", receipt })}
          />
        );
      case "stock":
        return (
          <StockScreen
            workspace={workspace}
            canPost={canEdit}
            onBack={() => setScreen({ kind: "till" })}
          />
        );
      case "drawer":
        return (
          <DrawerScreen
            workspace={workspace}
            session={state.session}
            onBack={() => setScreen({ kind: "till" })}
          />
        );
      case "sales":
        return (
          <SalesScreen
            workspace={workspace}
            onBack={() => setScreen({ kind: "till" })}
            onReturn={(receipt) => setScreen({ kind: "returns", receipt })}
          />
        );
      case "products":
        return (
          <ProductList
            workspace={workspace}
            canEdit={canEdit}
            onOpen={(productId) =>
              setScreen({ kind: "editor", productId, returnTo: "products" })
            }
            onNew={() =>
              setScreen({
                kind: "editor",
                productId: null,
                returnTo: "products",
              })
            }
            onBack={() => setScreen({ kind: "till" })}
          />
        );
      case "editor":
        return (
          <ProductEditor
            workspace={workspace}
            productId={screen.productId}
            {...(screen.barcode ? { barcode: screen.barcode } : {})}
            canEdit={canEdit}
            onSaved={(productId, created) => {
              // From the unknown-barcode prompt: put the new item in the cart and go back (E07-T04).
              if (created && screen.barcode) {
                void commands
                  .scanSubmit(crypto.randomUUID(), screen.barcode, "manual")
                  .then(() => setScreen({ kind: "till" }));
              } else if (created)
                setScreen({
                  kind: "editor",
                  productId,
                  returnTo: screen.returnTo,
                });
            }}
            onClose={() => setScreen({ kind: screen.returnTo })}
          />
        );
      default:
        return (
          <Checkout
            key={
              workspace.mode /* going live re-reads the drawer and the banner */
            }
            workspace={workspace}
            session={state.session}
            onSignedOut={signedOut}
            canEditCatalog={canEdit}
            onProducts={() => setScreen({ kind: "products" })}
            onReturns={() => setScreen({ kind: "returns" })}
            onSales={() => setScreen({ kind: "sales" })}
            onDrawer={() => setScreen({ kind: "drawer" })}
            onStock={() => setScreen({ kind: "stock" })}
            onReports={canEdit ? () => setScreen({ kind: "reports" }) : null}
            onWentLive={state.session.role === "owner" ? refresh : null}
            onSettings={
              state.session.role === "owner"
                ? () => setScreen({ kind: "settings" })
                : null
            }
            onCreateProduct={(barcode) =>
              setScreen({
                kind: "editor",
                productId: null,
                barcode,
                returnTo: "till",
              })
            }
          />
        );
    }
  })();
  return (
    <ScanPreferences.Provider value={scanPrefs}>
      {view}
    </ScanPreferences.Provider>
  );
}

type Screen =
  | { kind: "till" }
  | { kind: "products" }
  | { kind: "returns"; receipt?: string }
  | { kind: "sales" }
  | { kind: "drawer" }
  | { kind: "stock" }
  | { kind: "reports" }
  | { kind: "audit" }
  | { kind: "settings" }
  | {
      kind: "editor";
      productId: string | null;
      barcode?: string;
      returnTo: "till" | "products" | "audit";
    };
