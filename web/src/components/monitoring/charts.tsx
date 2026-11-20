import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { api } from "@/lib/api";
import { formatBytes, formatDateTimeRange, formatNumber, formatPercent, fromLocalInput, toLocalInput } from "@/lib/format";
import type { History, HistoryRange, Sample, ServiceSample } from "@/lib/monitoring-types";
import { cn } from "@/lib/utils";

import { CombinedChart, type ComparedSeries, type Frame, LineChart, type Point, series } from "./line-chart";
import { QueryState } from "./shared";

/** The periods offered, in hours: up to six months. Further back, or between two dates, is a custom period. */
const PERIODS = [1, 6, 24, 168, 720, 2160, 4320] as const;

type View = "grid" | "rows" | "combined";
const VIEWS: View[] = ["grid", "rows", "combined"];
const VIEW_KEY = "teamtak.history.view";

/** The layout last chosen, in this browser. */
function storedView(): View {
  try {
    const value = localStorage.getItem(VIEW_KEY);
    return VIEWS.includes(value as View) ? (value as View) : "grid";
  } catch {
    return "grid";
  }
}

type Metric = "cpu" | "memory" | "disk";

const METRICS: { id: Metric; pick: (sample: ServiceSample) => number | null; peak: (sample: ServiceSample) => number | null }[] = [
  { id: "cpu", pick: (s) => s.cpu_percent, peak: (s) => s.cpu_percent_max },
  { id: "memory", pick: (s) => s.memory_bytes, peak: (s) => s.memory_bytes_max },
  { id: "disk", pick: (s) => s.disk_bytes, peak: (s) => s.disk_bytes_max },
];

/** Services a combined chart can add to the server's five measures, in the palette's last three slots. */
const SERVICE_SLOTS = [6, 7, 8];

/** A share of the machine: a decimal while it is small, since most services use a few percent at most. */
function formatShare(value: number): string {
  return formatPercent(value, value < 10 ? 1 : 0);
}

function serviceFormat(metric: Metric): (value: number) => string {
  return metric === "cpu" ? formatShare : formatBytes;
}

function serviceSeries(history: History, metric: Metric): { service: string; points: Point[]; average: number }[] {
  const { pick, peak } = METRICS.find((m) => m.id === metric) ?? METRICS[0];
  return history.services
    .map(({ service, samples }) => {
      const points = series(samples, pick, peak);
      return { service, points, average: points.reduce((sum, p) => sum + p.value, 0) / Math.max(1, points.length) };
    })
    .filter((chart) => chart.points.length > 0)
    .sort((a, b) => b.average - a.average || a.service.localeCompare(b.service));
}

/** One chart per service for the chosen measure, on one scale, the heaviest first. */
function ServiceCharts({
  history,
  frame,
  metric,
  onMetric,
  wide,
}: {
  history: History;
  frame: Frame;
  metric: Metric;
  onMetric: (metric: Metric) => void;
  wide: boolean;
}) {
  const { t } = useTranslation();
  const format = serviceFormat(metric);
  const charts = serviceSeries(history, metric);
  const highest = Math.max(0, ...charts.flatMap((chart) => chart.points.map((p) => p.peak)));
  // A shared scale makes the services comparable at a glance; it never collapses to zero.
  const top = highest > 0 ? highest : metric === "cpu" ? 1 : 1024;

  return (
    <section className="space-y-3">
      <div className="space-y-1">
        {/* The buttons stay on the title's line whatever the help below says, so they never jump when the measure changes. */}
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h3 className="font-medium">{t("monitoring.charts.byService")}</h3>
          <span className="flex gap-1" role="group" aria-label={t("monitoring.charts.measure")}>
            {METRICS.map((m) => (
              <Button key={m.id} variant={metric === m.id ? "secondary" : "ghost"} size="sm" aria-pressed={metric === m.id} onClick={() => onMetric(m.id)}>
                {t(`monitoring.charts.serviceMetric.${m.id}`)}
              </Button>
            ))}
          </span>
        </div>
        <p className="text-sm text-muted-foreground">{t(`monitoring.charts.serviceMetricHelp.${metric}`)}</p>
      </div>
      {charts.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("monitoring.charts.noServices")}</p>
      ) : (
        <>
          <p className="text-xs text-muted-foreground">{t("monitoring.charts.sameScale", { value: format(top) })}</p>
          <div className={cn("grid gap-3", !wide && "sm:grid-cols-2 lg:grid-cols-3")}>
            {charts.map((chart) => (
              <LineChart key={chart.service} title={chart.service} titleIsName points={chart.points} max={top} format={format} frame={frame} wide={wide} />
            ))}
          </div>
        </>
      )}
    </section>
  );
}

/** Two dates to look at any period the history still holds. */
function CustomPeriod({ history, onShow }: { history: History | undefined; onShow: (from: number, to: number) => void }) {
  const { t } = useTranslation();
  const [start, setStart] = useState(() => toLocalInput(new Date(history ? history.from : Date.now() - 86_400_000)));
  const [end, setEnd] = useState(() => toLocalInput(new Date(history ? history.to : Date.now())));
  const [invalid, setInvalid] = useState(false);

  function submit(event: FormEvent) {
    event.preventDefault();
    const from = fromLocalInput(start);
    const to = fromLocalInput(end);
    if (!from || !to || Date.parse(to) - Date.parse(from) < 60_000) {
      setInvalid(true);
      return;
    }
    setInvalid(false);
    onShow(Date.parse(from), Date.parse(to));
  }

  const min = history ? toLocalInput(new Date(history.oldest)) : undefined;
  const max = toLocalInput(new Date());
  return (
    <form onSubmit={submit} className="flex flex-wrap items-end gap-3 rounded-lg border bg-muted/40 p-3">
      <div className="space-y-1">
        <Label htmlFor="history-start">{t("monitoring.charts.customStart")}</Label>
        <Input id="history-start" type="datetime-local" value={start} min={min} max={max} onChange={(e) => setStart(e.target.value)} className="w-auto" required />
      </div>
      <div className="space-y-1">
        <Label htmlFor="history-end">{t("monitoring.charts.customEnd")}</Label>
        <Input id="history-end" type="datetime-local" value={end} min={min} max={max} onChange={(e) => setEnd(e.target.value)} className="w-auto" required />
      </div>
      <Button type="submit" size="sm">
        {t("monitoring.charts.customShow")}
      </Button>
      {invalid && (
        <p className="w-full text-sm text-destructive" role="alert">
          {t("monitoring.charts.customInvalid")}
        </p>
      )}
    </form>
  );
}

/**
 * Processor, memory, swap, disk and connected devices over a period, for the machine, and the usage of each service: in a
 * grid, in full-width rows one above the other, or together on one chart. Every chart shows the same moment under the pointer,
 * and dragging across any of them zooms all of them.
 */
export function HistoryCharts() {
  const { t } = useTranslation();
  const [range, setRange] = useState<HistoryRange>({ hours: 24 });
  // The periods before each zoom, the latest last.
  const [zooms, setZooms] = useState<HistoryRange[]>([]);
  const [customOpen, setCustomOpen] = useState(false);
  const [view, setViewState] = useState<View>(storedView);
  const [metric, setMetric] = useState<Metric>("cpu");
  const [cursor, setCursor] = useState<number | null>(null);
  const [hidden, setHidden] = useState<ReadonlySet<string>>(new Set());
  // Services added to the combined chart, each with the palette slot it keeps while it is there.
  const [compared, setCompared] = useState<{ service: string; slot: number }[]>([]);
  const query = useQuery({
    queryKey: ["admin", "history", range],
    queryFn: () => api.monitoringHistory(range),
    // The last hours move on; a period between two dates does not.
    refetchInterval: "hours" in range ? 60_000 : false,
    // Another period keeps the charts in place, dimmed, until it arrives.
    placeholderData: keepPreviousData,
  });

  function setView(next: View) {
    setViewState(next);
    try {
      localStorage.setItem(VIEW_KEY, next);
    } catch {
      // Not remembered: the next visit shows the grid.
    }
  }
  function choosePeriod(hours: number) {
    setRange({ hours });
    setZooms([]);
    setCustomOpen(false);
  }
  function zoom(from: number, to: number) {
    setZooms((before) => [...before, range]);
    setRange({ from: Math.round(from), to: Math.round(to) });
  }
  function zoomOut() {
    const previous = zooms.at(-1);
    if (!previous) return;
    setZooms(zooms.slice(0, -1));
    setRange(previous);
  }
  function resetZoom() {
    const first = zooms[0];
    if (!first) return;
    setZooms([]);
    setRange(first);
  }
  function toggleService(service: string) {
    setCompared((current) => {
      if (current.some((c) => c.service === service)) return current.filter((c) => c.service !== service);
      const slot = SERVICE_SLOTS.find((s) => !current.some((c) => c.slot === s));
      return slot === undefined ? current : [...current, { service, slot }];
    });
  }
  function toggleSeries(key: string) {
    setHidden((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  const data = query.data;
  const samples = data?.samples ?? [];
  const from = data ? Date.parse(data.from) : 0;
  const to = data ? Date.parse(data.to) : 0;
  // Three missing points in a row: the platform was not looking, the line is cut.
  const gap = 3 * (data?.bucket_seconds ?? 60) * 1000 + 30_000;
  const frame: Frame = { from, to, gap, cursor, onCursor: setCursor, onZoom: zoom, onZoomOut: zoomOut };
  const bucketMinutes = Math.max(1, Math.round((data?.bucket_seconds ?? 60) / 60));
  const periodLabel = (period: number) =>
    period < 24
      ? t("monitoring.charts.hours", { count: period })
      : period === 24
        ? t("monitoring.charts.day")
        : period < 720
          ? t("monitoring.charts.week")
          : period === 720
            ? t("monitoring.charts.days", { count: 30 })
            : t("monitoring.charts.months", { count: Math.round(period / 720) });
  const customActive = customOpen || ("from" in range && zooms.length === 0);

  const machine = [
    { key: "cpu", title: t("monitoring.charts.cpu"), points: series(samples, (s: Sample) => s.cpu_percent, (s) => s.cpu_percent_max), max: 100, format: formatPercent, slot: 1 },
    { key: "memory", title: t("monitoring.charts.memory"), points: series(samples, (s: Sample) => s.memory_percent, (s) => s.memory_percent_max), max: 100, format: formatPercent, slot: 2 },
    { key: "swap", title: t("monitoring.charts.swap"), points: series(samples, (s: Sample) => s.swap_percent, (s) => s.swap_percent_max), max: 100, format: formatPercent, slot: 3 },
    { key: "disk", title: t("monitoring.charts.disk"), points: series(samples, (s: Sample) => s.disk_percent, (s) => s.disk_percent_max), max: 100, format: formatPercent, slot: 4 },
    { key: "devices", title: t("monitoring.charts.devices"), points: series(samples, (s: Sample) => s.devices, (s) => s.devices_max), max: undefined, format: formatNumber, slot: 5 },
    // A server without swap has no swap chart: its card already says so.
  ].filter((m) => m.key !== "swap" || m.points.length > 0);
  const services = data ? serviceSeries(data, metric) : [];
  const combined: ComparedSeries[] = [
    ...machine.map((m) => ({ key: m.key, label: m.title, points: m.points, format: m.format, slot: m.slot })),
    ...compared.flatMap(({ service, slot }) => {
      const found = services.find((s) => s.service === service);
      return found
        ? [{ key: `service:${service}`, name: service, label: t(`monitoring.charts.serviceMetric.${metric}`), points: found.points, format: serviceFormat(metric), slot }]
        : [];
    }),
  ];

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex flex-wrap items-center justify-between gap-2">
          {t("monitoring.charts.title")}
          <span className="flex flex-wrap gap-1" role="group" aria-label={t("monitoring.charts.period")}>
            {PERIODS.map((period) => {
              const active = "hours" in range && range.hours === period && zooms.length === 0 && !customOpen;
              return (
                <Button key={period} variant={active ? "secondary" : "ghost"} size="sm" aria-pressed={active} onClick={() => choosePeriod(period)}>
                  {periodLabel(period)}
                </Button>
              );
            })}
            <Button variant={customActive ? "secondary" : "ghost"} size="sm" aria-pressed={customActive} aria-expanded={customOpen} onClick={() => setCustomOpen(!customOpen)}>
              {t("monitoring.charts.custom")}
            </Button>
          </span>
        </CardTitle>
        <CardDescription>
          {t("monitoring.charts.description", { days: data?.retention_days ?? 14, years: Math.round((data?.hourly_retention_days ?? 3650) / 365) })}{" "}
          {data?.hourly
            ? t("monitoring.charts.pointsHours", { count: Math.round(data.bucket_seconds / 3600) })
            : t("monitoring.charts.points", { count: bucketMinutes })}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {customOpen && (
          <CustomPeriod
            history={data}
            onShow={(start, end) => {
              setRange({ from: start, to: end });
              setZooms([]);
            }}
          />
        )}
        {!data ? (
          <QueryState pending={query.isPending} error={query.error} />
        ) : (
          <>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <span className="flex gap-1" role="group" aria-label={t("monitoring.charts.view")}>
                {VIEWS.map((v) => (
                  <Button key={v} variant={view === v ? "secondary" : "ghost"} size="sm" aria-pressed={view === v} onClick={() => setView(v)}>
                    {t(`monitoring.charts.views.${v}`)}
                  </Button>
                ))}
              </span>
              <span className="flex flex-wrap items-center gap-2 text-sm">
                <span className="text-muted-foreground tabular-nums">{formatDateTimeRange(data.from, data.to)}</span>
                {zooms.length > 0 && (
                  <>
                    <Button variant="outline" size="sm" onClick={zoomOut}>
                      {t("monitoring.charts.zoomOut")}
                    </Button>
                    {zooms.length > 1 && (
                      <Button variant="ghost" size="sm" onClick={resetZoom}>
                        {t("monitoring.charts.zoomReset")}
                      </Button>
                    )}
                  </>
                )}
              </span>
            </div>
            <p className="text-xs text-muted-foreground">{t("monitoring.charts.hint")}</p>
            {samples.length === 0 && data.services.length === 0 ? (
              <p className="text-sm text-muted-foreground">{"hours" in range ? t("monitoring.charts.empty") : t("monitoring.charts.noDataPeriod")}</p>
            ) : (
            <div className={cn("space-y-6 transition-opacity", query.isPlaceholderData && "opacity-60")}>
              <section className="space-y-3">
                <h3 className="font-medium">{t("monitoring.charts.machine")}</h3>
                {view === "combined" ? (
                  <>
                    <CombinedChart series={combined} hidden={hidden} onToggle={toggleSeries} frame={frame} />
                    {services.length > 0 && (
                      <div className="flex flex-wrap items-center gap-1.5 text-sm">
                        <span className="text-muted-foreground">
                          {t("monitoring.charts.compareServices", { measure: t(`monitoring.charts.serviceMetric.${metric}`), count: SERVICE_SLOTS.length })}
                        </span>
                        {services.map(({ service }) => {
                          const on = compared.some((c) => c.service === service);
                          return (
                            <Button
                              key={service}
                              variant={on ? "secondary" : "outline"}
                              size="sm"
                              aria-pressed={on}
                              disabled={!on && compared.length >= SERVICE_SLOTS.length}
                              onClick={() => toggleService(service)}
                              translate="no"
                            >
                              {service}
                            </Button>
                          );
                        })}
                      </div>
                    )}
                  </>
                ) : (
                  <div className={cn("grid gap-3", view === "grid" && "sm:grid-cols-2 lg:grid-cols-3")}>
                    {machine.map((m) => (
                      <LineChart key={m.key} title={m.title} points={m.points} max={m.max} format={m.format} frame={frame} wide={view === "rows"} />
                    ))}
                  </div>
                )}
              </section>
              <ServiceCharts history={data} frame={frame} metric={metric} onMetric={setMetric} wide={view === "rows"} />
            </div>
            )}
          </>
        )}
      </CardContent>
    </Card>
  );
}
