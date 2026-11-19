import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import { DEFAULT_LANGUAGE, isSupported, LANGUAGES, type LanguageCode } from "./languages";

// One resource file per language; picked up by folder name.
const files = import.meta.glob<{ default: Record<string, unknown> }>("../locales/*/translation.json", { eager: true });
const resources: Record<string, { translation: Record<string, unknown> }> = {};
for (const [path, module] of Object.entries(files)) {
  const code = path.split("/").at(-2)!;
  resources[code] = { translation: module.default };
}

const STORAGE_KEY = "tak-manager.language";

function stored(): LanguageCode | null {
  try {
    const value = window.localStorage.getItem(STORAGE_KEY);
    return isSupported(value) ? value : null;
  } catch {
    return null;
  }
}

function fromBrowser(): LanguageCode | null {
  for (const tag of navigator.languages ?? [navigator.language]) {
    const code = tag.toLowerCase().split("-")[0];
    if (isSupported(code)) return code;
  }
  return null;
}

/**
 * Language choice: the user's saved preference, else the browser's language when the UI
 * has it, else English. Once signed in, the preference saved on the account wins (see
 * `adoptAccountLanguage`).
 */
export function initialLanguage(): LanguageCode {
  return stored() ?? fromBrowser() ?? DEFAULT_LANGUAGE;
}

/** Switches the UI and remembers the choice on this device. */
export function setLanguage(code: LanguageCode) {
  try {
    window.localStorage.setItem(STORAGE_KEY, code);
  } catch {
    // Private browsing: the choice lasts for the session only.
  }
  void i18n.changeLanguage(code);
}

/** Applies the language saved on the account that just signed in. */
export function adoptAccountLanguage(code: string) {
  if (isSupported(code) && code !== i18n.language) setLanguage(code);
}

export function currentLanguage(): LanguageCode {
  return isSupported(i18n.language) ? i18n.language : DEFAULT_LANGUAGE;
}

void i18n.use(initReactI18next).init({
  resources,
  lng: initialLanguage(),
  fallbackLng: DEFAULT_LANGUAGE,
  supportedLngs: LANGUAGES.map((l) => l.code),
  interpolation: { escapeValue: false }, // React escapes
  returnNull: false,
});

i18n.on("languageChanged", (lng) => {
  document.documentElement.lang = lng;
});
document.documentElement.lang = i18n.language;

export default i18n;
