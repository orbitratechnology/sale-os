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
import { CategoriesScreen } from "./features/catalog/CategoriesScreen";
import { ImportWizard } from "./features/catalog/ImportWizard";
import { ReturnScreen } from "./features/returns/ReturnScreen";
import type { ExchangeCredit } from "./features/returns/exchange";
import { SalesScreen } from "./features/sales/SalesScreen";
import { DrawerScreen } from "./features/cash/DrawerScreen";
import { StockScreen } from "./features/stock/StockScreen";
import { ReportsScreen } from "./features/reports/ReportsScreen";
import { AuditScreen } from "./features/audit/AuditScreen";
import { ErrorPanel } from "./features/shell/ErrorPanel";
import { UnlockScreen } from "./features/shell/UnlockScreen";
import { SettingsScreen } from "./features/settings/SettingsScreen";
import { ConflictsScreen } from "./features/sync/ConflictsScreen";
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
  /** E04-T02: the wizard just finished; the till offers "Getting started" once. */
  const [gettingStarted, setGettingStarted] = useState(false);
  /** A return taken to the till as credit for new items; recorded when the exchange is settled. */
  const [exchange, setExchange] = useState<ExchangeCredit | null>(null);

  const place: Place = !state ? "loading" : !state.setupComplete || !state.workspace ? "setup" : !state.session || state.session.mustChangePin ? "signIn" : screen.kind;
  useEffect(() => {
    // Startup timing for scripts/ui-latency.mjs: the first screen a person can use is up.
    if (place !== "loading" && performance.getEntriesByName(FIRST_SCREEN_MARK).length === 0) performance.mark(FIRST_SCREEN_MARK);
    // The window title names the screen (WCAG 2.4.2), for the taskbar and screen readers.
    document.title = `${TITLES[place]} · Sale OS`;
    // A new screen starts focus at its heading, unless it focused a field itself; the till keeps
    // focus on the page, where the scanner types (scanner-ready mode).
    if (place === "till" || place === "loading") return;
    const frame = requestAnimationFrame(() => {
      // Focus left on a control that is no longer shown (the till hides as a screen opens over
      // it) counts as no focus.
      const active = document.activeElement;
      if (active && active !== document.body && active.getClientRects().length > 0) return;
      // The visible screen's heading (the till stays in the page, hidden, with its own).
      const heading = [...document.querySelectorAll<HTMLElement>("main h1")].find((h) => h.getClientRects().length > 0);
      if (!heading) return;
      heading.tabIndex = -1;
      heading.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [place]);

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
  const [errorSound, setErrorSound] = useState(true);
  const userId = state?.session?.userId ?? null;
  const loadPrefs = useCallback(() => {
    void commands.appPreferences().then((res) => {
      if (res.status !== "ok") return;
      setScanPrefs({
        suffix: res.data.scannerSuffix as ScanSuffix,
        maxInterKeyMs: res.data.scannerMaxKeyGapMs,
      });
      setLockAfterSecs(res.data.lockTimeoutSecs);
      setErrorSound(res.data.errorSound);
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
        {error.code === "STORE_LOCKED" ? <UnlockScreen /> : <ErrorPanel error={error} />}
      </main>
    );
  }
  if (!state) return <main className="shell" aria-busy="true" />;
  if (!state.setupComplete || !state.workspace)
    return (
      <SetupWizard
        onDone={() => {
          setGettingStarted(true);
          refresh();
        }}
      />
    );
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
  const tillActive = screen.kind === "till";
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
            onExchange={(credit) => {
              setExchange(credit);
              setScreen({ kind: "till" });
            }}
          />
        );
      case "conflicts":
        return (
          <ConflictsScreen
            currencyExponent={workspace.currencyExponent}
            onBack={() => setScreen({ kind: "till" })}
          />
        );
      case "settings":
        return (
          <SettingsScreen
            initialGroup={screen.group}
            onBack={() => setScreen({ kind: "till" })}
            onChanged={loadPrefs}
            onRelinked={() => {
              setScreen({ kind: "till" });
              refresh();
            }}
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
            workspace={workspace}
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
            onReturn={(receipt) => setScreen({ kind: "returns", receipt })}
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
            onCategories={() => setScreen({ kind: "categories" })}
            onImport={() => setScreen({ kind: "import" })}
          />
        );
      case "import":
        return <ImportWizard onClose={() => setScreen({ kind: "products" })} />;
      case "categories":
        return (
          <CategoriesScreen onBack={() => setScreen({ kind: "products" })} />
        );
      case "editor":
        return (
          <ProductEditor
            workspace={workspace}
            productId={screen.productId}
            {...(screen.barcode ? { barcode: screen.barcode } : {})}
            {...(screen.approvalToken ? { approvalToken: screen.approvalToken } : {})}
            canEdit={canEdit || Boolean(screen.approvalToken)}
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
        return null;
    }
  })();
  // The till stays mounted under the other screens, hidden, so going back to it is instant
  // (its cart, totals and layout are already there; it refreshes in the background).
  const till = (
          <Checkout
            active={tillActive}
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
              // Managers see only Settings → Devices (DeviceConfigure); owners see everything.
              state.session.role !== "cashier"
                ? (group?: string) => setScreen({ kind: "settings", group })
                : null
            }
            errorSound={errorSound}
            exchange={exchange}
            onExchangeEnd={() => setExchange(null)}
            gettingStarted={gettingStarted}
            onGettingStartedSeen={() => setGettingStarted(false)}
            onConflicts={
              state.session.role === "cashier"
                ? null
                : () => setScreen({ kind: "conflicts" })
            }
            onCreateProduct={(barcode, approvalToken) =>
              setScreen({
                kind: "editor",
                productId: null,
                barcode,
                ...(approvalToken ? { approvalToken } : {}),
                returnTo: "till",
              })
            }
          />
  );
  return (
    <ScanPreferences.Provider value={scanPrefs}>
      <div className="till-host" hidden={!tillActive}>
        {till}
      </div>
      {view}
    </ScanPreferences.Provider>
  );
}

type Place = Screen["kind"] | "loading" | "setup" | "signIn";

/** Performance marks read by scripts/ui-latency.mjs (pilot measurements). */
export const FIRST_SCREEN_MARK = "sale-os:first-screen";

const TITLES: Record<Place, string> = {
  loading: "Starting",
  setup: "Set up",
  signIn: "Sign in",
  till: "Till",
  products: "Products",
  categories: "Categories",
  import: "Import products",
  returns: "Returns",
  sales: "Sales",
  drawer: "Cash drawer",
  stock: "Stock",
  reports: "Reports",
  audit: "Audit log",
  settings: "Settings",
  conflicts: "Sync conflicts",
  editor: "Product",
};

type Screen =
  | { kind: "till" }
  | { kind: "products" }
  | { kind: "categories" }
  | { kind: "import" }
  | { kind: "returns"; receipt?: string }
  | { kind: "sales" }
  | { kind: "drawer" }
  | { kind: "stock" }
  | { kind: "reports" }
  | { kind: "audit" }
  | { kind: "settings"; group?: string | undefined }
  | { kind: "conflicts" }
  | {
      kind: "editor";
      productId: string | null;
      barcode?: string;
      /** A manager approved a cashier creating this product (E07-T04). */
      approvalToken?: string;
      returnTo: "till" | "products" | "audit";
    };
