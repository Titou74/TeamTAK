import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { api } from "@/lib/api";
import { formatDateTime, formatNumber, formatPercent, formatTime } from "@/lib/format";
import type { Sample } from "@/lib/monitoring-types";

import { QueryState } from "./shared";

interface Point {
  at: number;
  value: number;
}

const WIDTH = 320;
const HEIGHT = 72;

/** One series as a line over time. `max` fixes the scale (100 for a share); without it the scale follows the data. */
function LineChart({ title, points, max, format, from, to }: { title: string; points: Point[]; max?: number; format: (value: number) => string; from: number; to: number }) {
  const { t } = useTranslation();
  const top = max ?? Math.max(1, ...points.map((p) => p.value));
  const span = Math.max(1, to - from);
  const x = (at: number) => ((at - from) / span) * WIDTH;
  const y = (value: number) => HEIGHT - (Math.min(value, top) / top) * (HEIGHT - 4) - 2;
  const path = points.map((p, i) => `${i === 0 ? "M" : "L"}${x(p.at).toFixed(1)},${y(p.value).toFixed(1)}`).join(" ");
  // From a day up, a time of day alone no longer says which day.
  const long = to - from >= 86_400_000;
  const last = points.at(-1);
  const peak = points.reduce<Point | null>((best, p) => (best === null || p.value > best.value ? p : best), null);
  return (
    <figure className="rounded-lg border bg-card p-3">
      <figcaption className="mb-1 flex items-baseline justify-between gap-2 text-sm">
        <span className="font-medium">{title}</span>
        <span className="tabular-nums text-muted-foreground">{last ? format(last.value) : t("monitoring.charts.noData")}</span>
      </figcaption>
      <svg viewBox={`0 0 ${WIDTH} ${HEIGHT}`} className="h-18 w-full" preserveAspectRatio="none" role="img" aria-label={title}>
        <line x1="0" x2={WIDTH} y1={HEIGHT - 1} y2={HEIGHT - 1} className="stroke-border" strokeWidth="1" />
        <line x1="0" x2={WIDTH} y1={y(top)} y2={y(top)} className="stroke-border" strokeWidth="1" strokeDasharray="2 4" />
        {points.length > 1 && <path d={path} fill="none" className="stroke-primary" strokeWidth="1.5" vectorEffect="non-scaling-stroke" />}
        {points.length === 1 && <circle cx={x(points[0].at)} cy={y(points[0].value)} r="2" className="fill-primary" />}
      </svg>
      <div className="mt-1 flex justify-between gap-2 text-xs text-muted-foreground">
        <span>{long ? formatDateTime(new Date(from).toISOString()) : formatTime(new Date(from).toISOString())}</span>
        {peak && <span>{t("monitoring.charts.peak", { value: format(peak.value), when: formatDateTime(new Date(peak.at).toISOString()) })}</span>}
        <span>{long ? formatDateTime(new Date(to).toISOString()) : formatTime(new Date(to).toISOString())}</span>
      </div>
    </figure>
  );
}

function series(samples: Sample[], pick: (sample: Sample) => number | null): Point[] {
  const points: Point[] = [];
  for (const sample of samples) {
    const value = pick(sample);
    if (value !== null) points.push({ at: new Date(sample.sampled_at).getTime(), value });
  }
  return points;
}

/** CPU, memory, disk and connected devices over the last day or week. */
export function HistoryCharts() {
  const { t } = useTranslation();
  const [hours, setHours] = useState(24);
  const query = useQuery({ queryKey: ["admin", "history", hours], queryFn: () => api.monitoringHistory(hours), refetchInterval: 5 * 60_000 });

  // The period ends when the samples were fetched: the same data always draws the same chart.
  const now = query.dataUpdatedAt;
  const from = now - hours * 3_600_000;
  const samples = query.data?.samples ?? [];
  const percent = (value: number) => formatPercent(value);
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex flex-wrap items-center justify-between gap-2">
          {t("monitoring.charts.title")}
          <span className="flex gap-1">
            <Button variant={hours === 24 ? "secondary" : "ghost"} size="sm" onClick={() => setHours(24)}>
              {t("monitoring.charts.day")}
            </Button>
            <Button variant={hours === 168 ? "secondary" : "ghost"} size="sm" onClick={() => setHours(168)}>
              {t("monitoring.charts.week")}
            </Button>
          </span>
        </CardTitle>
        <CardDescription>{t("monitoring.charts.description", { days: query.data?.retention_days ?? 14 })}</CardDescription>
      </CardHeader>
      <CardContent>
        {!query.data ? (
          <QueryState pending={query.isPending} error={query.error} />
        ) : samples.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("monitoring.charts.empty")}</p>
        ) : (
          <div className="grid gap-3 sm:grid-cols-2">
            <LineChart title={t("monitoring.charts.cpu")} points={series(samples, (s) => s.cpu_percent)} max={100} format={percent} from={from} to={now} />
            <LineChart title={t("monitoring.charts.memory")} points={series(samples, (s) => s.memory_percent)} max={100} format={percent} from={from} to={now} />
            <LineChart title={t("monitoring.charts.disk")} points={series(samples, (s) => s.disk_percent)} max={100} format={percent} from={from} to={now} />
            <LineChart title={t("monitoring.charts.devices")} points={series(samples, (s) => s.devices)} format={formatNumber} from={from} to={now} />
          </div>
        )}
      </CardContent>
    </Card>
  );
}
