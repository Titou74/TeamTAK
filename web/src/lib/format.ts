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

/** "Nov 20, 2026, 2:00 – 6:00 PM": a period, written the way the current language does. */
export function formatDateTimeRange(startIso: string, endIso: string): string {
  const format = new Intl.DateTimeFormat(currentLanguage(), { dateStyle: "medium", timeStyle: "short" });
  return format.formatRange(new Date(startIso), new Date(endIso));
}

/** The browser's time zone, as the name the system gives it ("Europe/Paris"). */
export function browserTimeZone(): string {
  return new Intl.DateTimeFormat().resolvedOptions().timeZone;
}

/** Value of an `<input type="datetime-local">` for a date, in the browser's time zone. */
export function toLocalInput(date: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** The instant a `datetime-local` value stands for, as ISO 8601 in UTC; null when it is not a date. */
export function fromLocalInput(value: string): string | null {
  const date = new Date(value);
  return value && !Number.isNaN(date.getTime()) ? date.toISOString() : null;
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
