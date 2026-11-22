// The terms of use, one text per language: src/locales/<language>/terms.json.

import { DEFAULT_LANGUAGE } from "@/i18n/languages";

/** A paragraph, or a bulleted list. */
export type TermsBlock = string | { items: string[] };

export interface TermsDocument {
  /** The day the text last changed. The server accepts only this version (TERMS_VERSION in
   *  crates/server/src/terms.rs, which a test keeps equal to it). */
  version: string;
  intro: string;
  sections: { title: string; body: TermsBlock[] }[];
}

const files = import.meta.glob<{ default: TermsDocument }>("../locales/*/terms.json", { eager: true });
const documents: Record<string, TermsDocument> = {};
for (const [path, module] of Object.entries(files)) {
  documents[path.split("/").at(-2)!] = module.default;
}

/** The terms in `language`, or in English. */
export function termsIn(language: string): TermsDocument {
  return documents[language] ?? documents[DEFAULT_LANGUAGE];
}

/** The version in force, the same in every language: what the sign-in page accepts. */
export const TERMS_VERSION = documents[DEFAULT_LANGUAGE].version;
