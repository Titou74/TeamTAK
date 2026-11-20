import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { type KeyboardEvent, type PointerEvent, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { api } from "@/lib/api";
import { formatBytes, formatNumber, formatPercent, formatShortDateTime, formatTime } from "@/lib/format";
import type { History, Sample, ServiceSample } from "@/lib/monitoring-types";
import { cn } from "@/lib/utils";

import { QueryState } from "./shared";

interface Point {
  at: number;
  value: number;
}

const WIDTH = 320;
const HEIGHT = 72;

/** The periods offered, in hours. */
const PERIODS = [1, 6, 24, 168] as const;

/** The point nearest to a time. `points` is in time order and not empty. */
function nearest(points: Point[], at: number): number {
  let best = 0;
  for (let i = 1; i < points.length; i++) {
    if (Math.abs(points[i].at - at) < Math.abs(points[best].at - at)) best = i;
  }
  return best;
}

/**
 * One series as a line over time. `max` fixes the scale (100 for a share, or the scale shared by several charts); without it the
 * scale follows the data. Points further apart than `gap` are not joined: nothing was measured in between. Hovering or moving
 * with the arrow keys shows the value at a moment.
 */
function LineChart({
  title,
  titleIsName = false,
  points,
  max,
  format,
  from,
  to,
  gap,
}: {
  title: string;
  /** The title is a name (a service), never translated. */
  titleIsName?: boolean;
  points: Point[];
  max?: number;
  format: (value: number) => string;
  from: number;
  to: number;
  gap: number;
}) {
  const { t } = useTranslation();
  const [active, setActive] = useState<number | null>(null);
  const top = max ?? Math.max(1, ...points.map((p) => p.value));
  const span = Math.max(1, to - from);
  const x = (at: number) => ((at - from) / span) * WIDTH;
  const y = (value: number) => HEIGHT - (Math.min(value, top) / top) * (HEIGHT - 4) - 2;
  const path = points.map((p, i) => `${i === 0 || p.at - points[i - 1].at > gap ? "M" : "L"}${x(p.at).toFixed(1)},${y(p.value).toFixed(1)}`).join(" ");
  // From a day up, a time of day alone no longer says which day.
  const long = to - from >= 86_400_000;
  const when = (at: number) => (long ? formatShortDateTime(new Date(at).toISOString()) : formatTime(new Date(at).toISOString()));
  const last = points.at(-1);
  const peak = points.reduce<Point | null>((best, p) => (best === null || p.value > best.value ? p : best), null);
  const shown = active === null ? null : points[Math.min(active, points.length - 1)];

  function onPointerMove(event: PointerEvent<HTMLDivElement>) {
    if (points.length === 0) return;
    const box = event.currentTarget.getBoundingClientRect();
    setActive(nearest(points, from + ((event.clientX - box.left) / box.width) * span));
  }
  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (points.length === 0) return;
    const current = active ?? points.length - 1;
    const next = { ArrowLeft: current - 1, ArrowRight: current + 1, Home: 0, End: points.length - 1 }[event.key];
    if (next === undefined) return;
    event.preventDefault();
    setActive(Math.max(0, Math.min(points.length - 1, next)));
  }

  return (
    <figure className="rounded-lg border bg-card p-3">
      <figcaption className="mb-1 flex items-baseline justify-between gap-2 text-sm">
        <span className="truncate font-medium" translate={titleIsName ? "no" : undefined}>
          {title}
        </span>
        <span className={cn("tabular-nums", shown ? "font-medium text-foreground" : "text-muted-foreground")} aria-live="polite">
          {shown ? format(shown.value) : last ? format(last.value) : t("monitoring.charts.noData")}
        </span>
      </figcaption>
      <div
        className="relative touch-none rounded-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
        tabIndex={points.length > 0 ? 0 : undefined}
        role="img"
        aria-label={t("monitoring.charts.chartLabel", { title })}
        onPointerMove={onPointerMove}
        onPointerLeave={() => setActive(null)}
        onFocus={() => setActive(points.length - 1)}
        onBlur={() => setActive(null)}
        onKeyDown={onKeyDown}
      >
        <svg viewBox={`0 0 ${WIDTH} ${HEIGHT}`} className="block h-18 w-full" preserveAspectRatio="none" aria-hidden="true">
          <line x1="0" x2={WIDTH} y1={HEIGHT - 1} y2={HEIGHT - 1} className="stroke-border" strokeWidth="1" vectorEffect="non-scaling-stroke" />
          <line x1="0" x2={WIDTH} y1={y(top)} y2={y(top)} className="stroke-border" strokeWidth="1" strokeDasharray="2 4" vectorEffect="non-scaling-stroke" />
          {points.length > 0 && <path d={path} fill="none" className="stroke-primary" strokeWidth="2" strokeLinejoin="round" vectorEffect="non-scaling-stroke" />}
          {shown && <line x1={x(shown.at)} x2={x(shown.at)} y1="0" y2={HEIGHT} className="stroke-muted-foreground" strokeWidth="1" vectorEffect="non-scaling-stroke" />}
        </svg>
        {/* Dots are drawn over the chart, so they stay round whatever its width. */}
        {(shown ?? (points.length === 1 ? points[0] : null)) && (
          <span
            className="pointer-events-none absolute size-2 -translate-x-1/2 -translate-y-1/2 rounded-full bg-primary ring-2 ring-card"
            style={{ left: `${(x((shown ?? points[0]).at) / WIDTH) * 100}%`, top: `${(y((shown ?? points[0]).value) / HEIGHT) * 100}%` }}
          />
        )}
      </div>
      <div className="mt-1 flex justify-between gap-2 text-xs text-muted-foreground tabular-nums">
        <span>{when(from)}</span>
        <span>{when(to)}</span>
      </div>
      {/* The moment under the pointer, else the highest point: one line either way, so nothing moves. */}
      <p className="min-h-4 truncate text-xs text-muted-foreground">
        {shown ? (
          <span className="text-foreground">{formatShortDateTime(new Date(shown.at).toISOString())}</span>
        ) : (
          peak && t("monitoring.charts.peak", { value: format(peak.value), when: formatShortDateTime(new Date(peak.at).toISOString()) })
        )}
      </p>
    </figure>
  );
}

function series<T extends { sampled_at: string }>(samples: T[], pick: (sample: T) => number | null): Point[] {
  const points: Point[] = [];
  for (const sample of samples) {
    const value = pick(sample);
    if (value !== null) points.push({ at: new Date(sample.sampled_at).getTime(), value });
  }
  return points;
}

type Metric = "cpu" | "memory" | "disk";

const METRICS: { id: Metric; pick: (sample: ServiceSample) => number | null }[] = [
  { id: "cpu", pick: (s) => s.cpu_percent },
  { id: "memory", pick: (s) => s.memory_bytes },
  { id: "disk", pick: (s) => s.disk_bytes },
];

/** A share of the machine: a decimal while it is small, since most services use a few percent at most. */
function formatShare(value: number): string {
  return formatPercent(value, value < 10 ? 1 : 0);
}

/** One chart per service for the chosen measure, on one scale, the heaviest first. */
function ServiceCharts({ history, from, to, gap }: { history: History; from: number; to: number; gap: number }) {
  const { t } = useTranslation();
  const [metric, setMetric] = useState<Metric>("cpu");
  const { pick } = METRICS.find((m) => m.id === metric) ?? METRICS[0];
  const format = metric === "cpu" ? formatShare : formatBytes;
  const charts = history.services
    .map(({ service, samples }) => {
      const points = series(samples, pick);
      return { service, points, average: points.reduce((sum, p) => sum + p.value, 0) / Math.max(1, points.length) };
    })
    .filter((chart) => chart.points.length > 0)
    .sort((a, b) => b.average - a.average || a.service.localeCompare(b.service));
  const highest = Math.max(0, ...charts.flatMap((chart) => chart.points.map((p) => p.value)));
  // A shared scale makes the services comparable at a glance; it never collapses to zero.
  const top = highest > 0 ? highest : metric === "cpu" ? 1 : 1024;

  return (
    <section className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div>
          <h3 className="font-medium">{t("monitoring.charts.byService")}</h3>
          <p className="text-sm text-muted-foreground">{t(`monitoring.charts.serviceMetricHelp.${metric}`)}</p>
        </div>
        <span className="flex gap-1" role="group" aria-label={t("monitoring.charts.measure")}>
          {METRICS.map((m) => (
            <Button key={m.id} variant={metric === m.id ? "secondary" : "ghost"} size="sm" aria-pressed={metric === m.id} onClick={() => setMetric(m.id)}>
              {t(`monitoring.charts.serviceMetric.${m.id}`)}
            </Button>
          ))}
        </span>
      </div>
      {charts.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("monitoring.charts.noServices")}</p>
      ) : (
        <>
          <p className="text-xs text-muted-foreground">{t("monitoring.charts.sameScale", { value: format(top) })}</p>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {charts.map((chart) => (
              <LineChart key={chart.service} title={chart.service} titleIsName points={chart.points} max={top} format={format} from={from} to={to} gap={gap} />
            ))}
          </div>
        </>
      )}
    </section>
  );
}

/** Processor, memory, swap, disk and connected devices over the last hours or days, for the machine, and the usage of each service. */
export function HistoryCharts() {
  const { t } = useTranslation();
  const [hours, setHours] = useState(24);
  const query = useQuery({
    queryKey: ["admin", "history", hours],
    queryFn: () => api.monitoringHistory(hours),
    refetchInterval: 60_000,
    // Another period keeps the charts in place, dimmed, until it arrives.
    placeholderData: keepPreviousData,
  });

  const data = query.data;
  const samples = data?.samples ?? [];
  // The period ends when the samples were fetched: the same data always draws the same chart.
  const lastSample = samples.length > 0 ? new Date(samples[samples.length - 1].sampled_at).getTime() : 0;
  const to = query.isPlaceholderData ? lastSample : query.dataUpdatedAt || lastSample;
  const from = to - (data?.hours ?? hours) * 3_600_000;
  const minutes = Math.max(1, Math.round((data?.bucket_seconds ?? 60) / 60));
  // Three missing points in a row: the platform was not looking, the line is cut.
  const gap = 3 * minutes * 60_000 + 30_000;
  const swap = series(samples, (s: Sample) => s.swap_percent);
  const periodLabel = (period: number) => (period < 24 ? t("monitoring.charts.hours", { count: period }) : period === 24 ? t("monitoring.charts.day") : t("monitoring.charts.week"));
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex flex-wrap items-center justify-between gap-2">
          {t("monitoring.charts.title")}
          <span className="flex gap-1" role="group" aria-label={t("monitoring.charts.period")}>
            {PERIODS.map((period) => (
              <Button key={period} variant={hours === period ? "secondary" : "ghost"} size="sm" aria-pressed={hours === period} onClick={() => setHours(period)}>
                {periodLabel(period)}
              </Button>
            ))}
          </span>
        </CardTitle>
        <CardDescription>
          {t("monitoring.charts.description", { days: data?.retention_days ?? 14 })} {t("monitoring.charts.points", { count: minutes })}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {!data ? (
          <QueryState pending={query.isPending} error={query.error} />
        ) : samples.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("monitoring.charts.empty")}</p>
        ) : (
          <div className={cn("space-y-6 transition-opacity", query.isPlaceholderData && "opacity-60")}>
            <section className="space-y-3">
              <h3 className="font-medium">{t("monitoring.charts.machine")}</h3>
              <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
                <LineChart title={t("monitoring.charts.cpu")} points={series(samples, (s: Sample) => s.cpu_percent)} max={100} format={formatPercent} from={from} to={to} gap={gap} />
                <LineChart title={t("monitoring.charts.memory")} points={series(samples, (s: Sample) => s.memory_percent)} max={100} format={formatPercent} from={from} to={to} gap={gap} />
                {/* A server without swap has no swap chart: its card already says so. */}
                {swap.length > 0 && <LineChart title={t("monitoring.charts.swap")} points={swap} max={100} format={formatPercent} from={from} to={to} gap={gap} />}
                <LineChart title={t("monitoring.charts.disk")} points={series(samples, (s: Sample) => s.disk_percent)} max={100} format={formatPercent} from={from} to={to} gap={gap} />
                <LineChart title={t("monitoring.charts.devices")} points={series(samples, (s: Sample) => s.devices)} format={formatNumber} from={from} to={to} gap={gap} />
              </div>
            </section>
            <ServiceCharts history={data} from={from} to={to} gap={gap} />
          </div>
        )}
      </CardContent>
    </Card>
  );
}
