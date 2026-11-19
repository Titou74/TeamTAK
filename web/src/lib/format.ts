// Dates and numbers are formatted by the browser for the current UI language, never by hand.
import { currentLanguage } from "@/i18n";

export function formatDate(iso: string | null | undefined): string {
  if (!iso) return "";
  return new Intl.DateTimeFormat(currentLanguage(), { dateStyle: "medium" }).format(new Date(iso));
}

export function formatDateTime(iso: string | null | undefined): string {
  if (!iso) return "";
  return new Intl.DateTimeFormat(currentLanguage(), { dateStyle: "medium", timeStyle: "short" }).format(new Date(iso));
}

export function formatNumber(n: number): string {
  return new Intl.NumberFormat(currentLanguage()).format(n);
}

/** "in 3 months", "2 minutes ago", in the current language. */
export function formatRelative(iso: string | null | undefined): string {
  if (!iso) return "";
  const seconds = (new Date(iso).getTime() - Date.now()) / 1000;
  const units: [Intl.RelativeTimeFormatUnit, number][] = [
    ["year", 31536000],
    ["month", 2592000],
    ["day", 86400],
    ["hour", 3600],
    ["minute", 60],
  ];
  const rtf = new Intl.RelativeTimeFormat(currentLanguage(), { numeric: "auto" });
  for (const [unit, size] of units) {
    if (Math.abs(seconds) >= size) return rtf.format(Math.round(seconds / size), unit);
  }
  return rtf.format(Math.round(seconds), "second");
}
