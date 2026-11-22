/**
 * Languages of the UI. To add one: create `src/locales/<code>/translation.json`, `email.json`
 * and `terms.json` (the terms of use) with the same keys as English, then add its entry here.
 * Nothing else.
 */
export const LANGUAGES = [
  { code: "en", label: "English" },
  { code: "fr", label: "Français" },
] as const;

/** Default and fallback language. */
export const DEFAULT_LANGUAGE = "en";

export type LanguageCode = (typeof LANGUAGES)[number]["code"];

export function isSupported(code: string | null | undefined): code is LanguageCode {
  return LANGUAGES.some((l) => l.code === code);
}
