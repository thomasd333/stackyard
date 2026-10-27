import { en, type Messages } from "./en";

// To add a language: create e.g. `pl.ts` exporting `pl: Messages` and register it here.
const locales: Record<string, Messages> = { en };

let current: Messages = en;

export function setLocale(code: string): void {
  current = locales[code] ?? en;
}

/** Translates `key`, replacing `{name}` placeholders with `vars`. */
export function t(key: keyof Messages, vars: Record<string, string | number> = {}): string {
  return current[key].replace(/\{(\w+)\}/g, (m, name) => (name in vars ? String(vars[name]) : m));
}
