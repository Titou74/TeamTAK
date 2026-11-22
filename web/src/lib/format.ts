// Dates and numbers are formatted by the browser for the current UI language, never by hand.
import { currentLanguage } from "@/i18n";

export function formatDate(iso: string | null | undefined): string {
  if (!iso) return "";
  return new Intl.DateTimeFormat(currentLanguage(), { dateStyle: "medium" }).format(new Date(iso));
}

/** A calendar day ("2026-11-22"), the same day whatever the browser's time zone. */
export function formatDay(day: string): string {
  return new Intl.DateTimeFormat(currentLanguage(), { dateStyle: "long", timeZone: "UTC" }).format(new Date(`${day}T00:00:00Z`));
}

export function formatDateTime(iso: string | null | undefined): string {
  if (!iso) return "";
  return new Intl.DateTimeFormat(currentLanguage(), { dateStyle: "medium", timeStyle: "short" }).format(new Date(iso));
}

/** "Nov 20, 4:55 PM": a moment of the last days, short enough for the axis of a chart. */
export function formatShortDateTime(iso: string): string {
  return new Intl.DateTimeFormat(currentLanguage(), { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }).format(new Date(iso));
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

/** "12 minutes", "3 hours": the time between two instants, in its largest whole unit. */
export function formatDuration(fromIso: string, toIso: string): string {
  const seconds = Math.max(0, (new Date(toIso).getTime() - new Date(fromIso).getTime()) / 1000);
  const units: [string, number][] = [
    ["day", 86400],
    ["hour", 3600],
    ["minute", 60],
    ["second", 1],
  ];
  const [unit, size] = units.find(([, size]) => seconds >= size) ?? units[units.length - 1];
  return new Intl.NumberFormat(currentLanguage(), { style: "unit", unit, unitDisplay: "long" }).format(Math.round(seconds / size));
}

/** "2.3 GB", "640 MB": a size, with the unit written the way the current language does. */
export function formatBytes(bytes: number): string {
  const units = ["byte", "kilobyte", "megabyte", "gigabyte", "terabyte"] as const;
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return new Intl.NumberFormat(currentLanguage(), {
    style: "unit",
    unit: units[unit],
    unitDisplay: "short",
    maximumFractionDigits: unit === 0 || value >= 100 ? 0 : 1,
  }).format(value);
}

/** "100 MB", a size in megabytes as a setting gives it (decimal, unlike `formatBytes`). */
export function formatMegabytes(megabytes: number): string {
  return new Intl.NumberFormat(currentLanguage(), { style: "unit", unit: "megabyte", unitDisplay: "short" }).format(megabytes);
}

/** "93 %" or "93%", as the current language writes a share given from 0 to 100. */
export function formatPercent(value: number, fractionDigits = 0): string {
  return new Intl.NumberFormat(currentLanguage(), { style: "percent", maximumFractionDigits: fractionDigits }).format(value / 100);
}

/** A time of day alone, for the axis of a chart. */
export function formatTime(iso: string): string {
  return new Intl.DateTimeFormat(currentLanguage(), { timeStyle: "short" }).format(new Date(iso));
}

/** A list written the way the current language joins it: "a, b and c". */
export function formatList(items: string[]): string {
  return new Intl.ListFormat(currentLanguage(), { style: "long", type: "conjunction" }).format(items);
}
