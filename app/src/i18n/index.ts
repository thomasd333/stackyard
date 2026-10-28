import { en, type Messages } from "./en";
import { pl } from "./pl";

// To add a language: create e.g. `de.ts` exporting `de: Messages`, register it here and in
// `LANGUAGES` / `tray()` in src-tauri/src/i18n.rs.
const locales: Record<string, Messages> = { en, pl };

/** Language names in their own language, so they read right whatever the current locale. */
export const languageNames: Record<string, string> = { en: "English", pl: "Polski" };

let current: Messages = en;

export function setLocale(code: string): void {
  current = locales[code] ?? en;
  document.documentElement.lang = locales[code] ? code : "en";
}

/** Translates `key`, replacing `{name}` placeholders with `vars`. */
export function t(key: keyof Messages, vars: Record<string, string | number> = {}): string {
  return current[key].replace(/\{(\w+)\}/g, (m, name) => (name in vars ? String(vars[name]) : m));
}
