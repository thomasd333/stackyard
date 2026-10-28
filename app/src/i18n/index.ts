import { de } from "./de";
import { en, type Messages } from "./en";
import { es } from "./es";
import { fr } from "./fr";
import { pl } from "./pl";
import { pt } from "./pt";
import { uk } from "./uk";

// To add a language: create e.g. `it.ts` exporting `it: Messages`, register it here and in
// `LANGUAGES` / `system_language()` / `tray()` in src-tauri/src/i18n.rs.
const locales: Record<string, Messages> = { en, de, es, fr, pl, pt, uk };

/** Language names in their own language, so they read right whatever the current locale. In menu order. */
export const languageNames: Record<string, string> = {
  en: "English",
  de: "Deutsch",
  es: "Español",
  fr: "Français",
  pl: "Polski",
  pt: "Português (Brasil)",
  uk: "Українська",
};

let current: Messages = en;

export function setLocale(code: string): void {
  current = locales[code] ?? en;
  document.documentElement.lang = locales[code] ? code : "en";
}

/** Translates `key`, replacing `{name}` placeholders with `vars`. */
export function t(key: keyof Messages, vars: Record<string, string | number> = {}): string {
  return current[key].replace(/\{(\w+)\}/g, (m, name) => (name in vars ? String(vars[name]) : m));
}
