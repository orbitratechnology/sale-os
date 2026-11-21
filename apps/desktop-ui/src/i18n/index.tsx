/**
 * The till's language (D-02, Sri Lanka): English, Sinhala or Tamil.
 *
 * English text is the key: `t("Who is signing in?")` looks the sentence up in the chosen
 * language's catalogue and falls back to the English when it has no entry, so a missing
 * translation never blanks a button. `{name}` placeholders are filled from `vars`.
 *
 * The choice belongs to this till (a cashier picks it on the sign-in screen) and is kept in
 * the WebView's storage; it is a display preference, not business data. The document's
 * `lang` follows it, so screen readers and line breaking use the right language.
 *
 * Amounts and dates keep their digits and separators: shops in Sri Lanka write prices in
 * Western digits in all three languages.
 */
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { si } from "./si";
import { ta } from "./ta";

export type Language = "en" | "si" | "ta";

export const LANGUAGES: { code: Language; name: string }[] = [
  { code: "en", name: "English" },
  { code: "si", name: "සිංහල" },
  { code: "ta", name: "தமிழ்" },
];

const CATALOGS: Record<Language, Readonly<Record<string, string>>> = { en: {}, si, ta };

const STORAGE_KEY = "sale-os:language";

let current: Language = readStored();

function readStored(): Language {
  try {
    const v = globalThis.localStorage?.getItem(STORAGE_KEY);
    return v === "si" || v === "ta" ? v : "en";
  } catch {
    return "en";
  }
}

/** `english` in `lang`, with `{name}` placeholders filled from `vars`. */
export function translate(lang: Language, english: string, vars?: Record<string, string | number>): string {
  const text = CATALOGS[lang][english] ?? english;
  if (!vars) return text;
  return text.replace(/\{(\w+)\}/g, (whole, name: string) => (name in vars ? String(vars[name]) : whole));
}

/** For code outside React (error copy): the language currently shown. */
export function currentLanguage(): Language {
  return current;
}

export type T = (english: string, vars?: Record<string, string | number>) => string;

const LanguageContext = createContext<{ lang: Language; setLang: (l: Language) => void }>({
  lang: "en",
  setLang: () => {},
});

export function LanguageProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Language>(current);
  const setLang = useCallback((l: Language) => {
    current = l;
    try {
      globalThis.localStorage?.setItem(STORAGE_KEY, l);
    } catch {
      // Private mode or blocked storage: the choice lasts until the app closes.
    }
    setLangState(l);
  }, []);
  useEffect(() => {
    current = lang;
    document.documentElement.lang = lang;
  }, [lang]);
  const value = useMemo(() => ({ lang, setLang }), [lang, setLang]);
  return <LanguageContext.Provider value={value}>{children}</LanguageContext.Provider>;
}

/** The language and a setter. */
export function useLanguage(): [Language, (l: Language) => void] {
  const { lang, setLang } = useContext(LanguageContext);
  return [lang, setLang];
}

/** `t` for the current language; components re-render when it changes. */
export function useT(): T {
  const { lang } = useContext(LanguageContext);
  return useCallback<T>((english, vars) => translate(lang, english, vars), [lang]);
}
