import { type KeyboardEvent, type PointerEvent, useState } from "react";
import { useTranslation } from "react-i18next";

import { formatDate, formatDateTime, formatPercent, formatShortDateTime, formatTime } from "@/lib/format";
import { cn } from "@/lib/utils";

export interface Point {
  at: number;
  /** The average of the samples the point stands for. */
  value: number;
  /** The highest of them. */
  peak: number;
}

/**
 * What every chart of the history shares: the period, the moment shown on all of them at once, and the zoom. Points further
 * apart than `gap` are not joined: nothing was measured in between.
 */
export interface Frame {
  from: number;
  to: number;
  gap: number;
  /** The moment under the pointer or the arrow keys, on whichever chart: every chart shows its value then. */
  cursor: number | null;
  onCursor: (at: number | null) => void;
  /** A period chosen on a chart, by dragging across it or with +. */
  onZoom: (from: number, to: number) => void;
  /** Back to the period before the last zoom, with −. */
  onZoomOut: () => void;
}

const WIDTH = 320;
/** Below this, a drag is a click: no zoom. */
const SMALLEST_ZOOM = 2 * 60_000;

export function series<T extends { sampled_at: string }>(samples: T[], pick: (sample: T) => number | null, pickPeak: (sample: T) => number | null): Point[] {
  const points: Point[] = [];
  for (const sample of samples) {
    const value = pick(sample);
    if (value !== null) points.push({ at: new Date(sample.sampled_at).getTime(), value, peak: Math.max(value, pickPeak(sample) ?? value) });
  }
  return points;
}

/** The point nearest to a time. `points` is in time order and not empty. */
function nearest(points: Point[], at: number): number {
  let best = 0;
  for (let i = 1; i < points.length; i++) {
    if (Math.abs(points[i].at - at) < Math.abs(points[best].at - at)) best = i;
  }
  return best;
}

/** The point a chart shows at the cursor: none when nothing was measured near that moment. */
function pointAt(points: Point[], frame: Frame): Point | null {
  if (frame.cursor === null || points.length === 0) return null;
  const point = points[nearest(points, frame.cursor)];
  return Math.abs(point.at - frame.cursor) <= frame.gap ? point : null;
}

/** The runs of points that are joined. */
function segments(points: Point[], gap: number): Point[][] {
  const runs: Point[][] = [];
  points.forEach((p, i) => {
    if (i === 0 || p.at - points[i - 1].at > gap) runs.push([p]);
    else runs[runs.length - 1].push(p);
  });
  return runs;
}

/** An SVG path through the joined runs of points. */
function linePath(runs: Point[][], x: (at: number) => number, y: (point: Point) => number): string {
  return runs.map((run) => run.map((p, i) => `${i === 0 ? "M" : "L"}${x(p.at).toFixed(1)},${y(p).toFixed(1)}`).join(" ")).join(" ");
}

/** A moment on the axis: the time alone within a day, the date with the year from two months. */
function axisLabel(at: number, span: number): string {
  const iso = new Date(at).toISOString();
  if (span < 86_400_000) return formatTime(iso);
  if (span < 60 * 86_400_000) return formatShortDateTime(iso);
  return formatDate(iso);
}

function AxisLabels({ frame, count }: { frame: Frame; count: number }) {
  const span = frame.to - frame.from;
  const ticks = Array.from({ length: count }, (_, i) => frame.from + (span * i) / (count - 1));
  return (
    <div className="mt-1 flex justify-between gap-2 text-xs text-muted-foreground tabular-nums" aria-hidden="true">
      {ticks.map((at, i) => (
        // On a phone, the ends and the middle only.
        <span key={at} className={cn(i !== 0 && i !== count - 1 && i * 2 !== count - 1 && "hidden sm:inline")}>
          {axisLabel(at, span)}
        </span>
      ))}
    </div>
  );
}

/** Hovering shows a moment on every chart, dragging zooms, the arrow keys move from point to point, + and − zoom. */
function useChartPointer(points: Point[], frame: Frame) {
  const [drag, setDrag] = useState<{ start: number; end: number } | null>(null);
  const span = Math.max(1, frame.to - frame.from);

  function timeAt(event: PointerEvent<HTMLDivElement>): number {
    const box = event.currentTarget.getBoundingClientRect();
    return frame.from + Math.min(1, Math.max(0, (event.clientX - box.left) / box.width)) * span;
  }

  const handlers = {
    onPointerDown(event: PointerEvent<HTMLDivElement>) {
      if (event.button !== 0 || points.length === 0) return;
      event.currentTarget.setPointerCapture(event.pointerId);
      const at = timeAt(event);
      setDrag({ start: at, end: at });
    },
    onPointerMove(event: PointerEvent<HTMLDivElement>) {
      const at = timeAt(event);
      if (drag) setDrag({ ...drag, end: at });
      frame.onCursor(at);
    },
    onPointerUp() {
      if (!drag) return;
      setDrag(null);
      const from = Math.min(drag.start, drag.end);
      const to = Math.max(drag.start, drag.end);
      if (to - from >= Math.max(SMALLEST_ZOOM, span / 100)) frame.onZoom(from, to);
    },
    onPointerCancel() {
      setDrag(null);
    },
    onPointerLeave() {
      if (!drag) frame.onCursor(null);
    },
    onFocus() {
      frame.onCursor(points.at(-1)?.at ?? null);
    },
    onBlur() {
      frame.onCursor(null);
    },
    onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
      if (points.length === 0) return;
      const current = frame.cursor === null ? points.length - 1 : nearest(points, frame.cursor);
      if (event.key === "+" || event.key === "=") {
        event.preventDefault();
        const center = Math.min(frame.to - span / 4, Math.max(frame.from + span / 4, points[current].at));
        if (span / 2 >= SMALLEST_ZOOM) frame.onZoom(center - span / 4, center + span / 4);
        return;
      }
      if (event.key === "-") {
        event.preventDefault();
        frame.onZoomOut();
        return;
      }
      const next = { ArrowLeft: current - 1, ArrowRight: current + 1, Home: 0, End: points.length - 1 }[event.key];
      if (next === undefined) return;
      event.preventDefault();
      frame.onCursor(points[Math.max(0, Math.min(points.length - 1, next))].at);
    },
  };
  return { drag, handlers, span };
}

/** The area being dragged across, to zoom on. */
function Selection({ drag, frame, span }: { drag: { start: number; end: number } | null; frame: Frame; span: number }) {
  if (!drag) return null;
  const left = ((Math.min(drag.start, drag.end) - frame.from) / span) * 100;
  const width = (Math.abs(drag.end - drag.start) / span) * 100;
  return <span className="pointer-events-none absolute inset-y-0 border-x border-primary/40 bg-primary/10" style={{ left: `${left}%`, width: `${width}%` }} />;
}

/**
 * One measure as a line over time, with a light band up to the highest sample of each point. `max` fixes the scale (100 for a
 * share, or the scale shared by several charts); without it the scale follows the data. `wide` is the full-width chart of the
 * rows view: taller, with more moments on its axis.
 */
export function LineChart({
  title,
  titleIsName = false,
  points,
  max,
  format,
  frame,
  wide = false,
}: {
  title: string;
  /** The title is a name (a service), never translated. */
  titleIsName?: boolean;
  points: Point[];
  max?: number;
  format: (value: number) => string;
  frame: Frame;
  wide?: boolean;
}) {
  const { t } = useTranslation();
  const { drag, handlers, span } = useChartPointer(points, frame);
  const height = wide ? 96 : 72;
  const top = max ?? Math.max(1, ...points.map((p) => p.peak));
  const x = (at: number) => ((at - frame.from) / span) * WIDTH;
  const y = (value: number) => height - (Math.min(value, top) / top) * (height - 4) - 2;
  const runs = segments(points, frame.gap);
  const line = linePath(runs, x, (p) => y(p.value));
  // From the average up to the highest sample, where they differ.
  const band = runs
    .filter((run) => run.length > 1 && run.some((p) => p.peak > p.value))
    .map((run) => {
      const up = run.map((p, i) => `${i === 0 ? "M" : "L"}${x(p.at).toFixed(1)},${y(p.peak).toFixed(1)}`).join(" ");
      const down = [...run].reverse().map((p) => `L${x(p.at).toFixed(1)},${y(p.value).toFixed(1)}`).join(" ");
      return `${up} ${down} Z`;
    })
    .join(" ");
  const last = points.at(-1);
  const highest = points.reduce<Point | null>((best, p) => (best === null || p.peak > best.peak ? p : best), null);
  const shown = pointAt(points, frame);
  const dot = shown ?? (points.length === 1 ? points[0] : null);

  return (
    <figure className="rounded-lg border bg-card p-3">
      <figcaption className="mb-1 flex items-baseline justify-between gap-2 text-sm">
        <span className="truncate font-medium" translate={titleIsName ? "no" : undefined}>
          {title}
        </span>
        <span className={cn("tabular-nums", shown ? "font-medium text-foreground" : "text-muted-foreground")} aria-live="polite">
          {shown ? format(shown.value) : frame.cursor !== null ? "—" : last ? format(last.value) : t("monitoring.charts.noData")}
        </span>
      </figcaption>
      <div
        className="relative cursor-crosshair touch-none rounded-sm outline-none select-none focus-visible:ring-2 focus-visible:ring-ring"
        tabIndex={points.length > 0 ? 0 : undefined}
        role="img"
        aria-label={t("monitoring.charts.chartLabel", { title })}
        {...handlers}
      >
        <svg viewBox={`0 0 ${WIDTH} ${height}`} className={cn("block w-full", wide ? "h-24" : "h-18")} preserveAspectRatio="none" aria-hidden="true">
          <line x1="0" x2={WIDTH} y1={height - 1} y2={height - 1} className="stroke-border" strokeWidth="1" vectorEffect="non-scaling-stroke" />
          <line x1="0" x2={WIDTH} y1={y(top)} y2={y(top)} className="stroke-border" strokeWidth="1" strokeDasharray="2 4" vectorEffect="non-scaling-stroke" />
          {band && <path d={band} className="fill-primary/15" />}
          {points.length > 0 && <path d={line} fill="none" className="stroke-primary" strokeWidth="2" strokeLinejoin="round" vectorEffect="non-scaling-stroke" />}
          {frame.cursor !== null && (
            <line
              x1={x(shown?.at ?? frame.cursor)}
              x2={x(shown?.at ?? frame.cursor)}
              y1="0"
              y2={height}
              className="stroke-muted-foreground"
              strokeWidth="1"
              vectorEffect="non-scaling-stroke"
            />
          )}
        </svg>
        {/* Dots are drawn over the chart, so they stay round whatever its width. */}
        {dot && (
          <span
            className="pointer-events-none absolute size-2 -translate-x-1/2 -translate-y-1/2 rounded-full bg-primary ring-2 ring-card"
            style={{ left: `${(x(dot.at) / WIDTH) * 100}%`, top: `${(y(dot.value) / height) * 100}%` }}
          />
        )}
        <Selection drag={drag} frame={frame} span={span} />
      </div>
      <AxisLabels frame={frame} count={wide ? 5 : 2} />
      {/* The moment under the pointer, else the highest point: one line either way, so nothing moves. */}
      <p className="min-h-4 truncate text-xs text-muted-foreground">
        {shown ? (
          <span className="text-foreground">
            {formatDateTime(new Date(shown.at).toISOString())}
            {shown.peak > shown.value && ` · ${t("monitoring.charts.highestThen", { value: format(shown.peak) })}`}
          </span>
        ) : (
          highest && t("monitoring.charts.peak", { value: format(highest.peak), when: formatDateTime(new Date(highest.at).toISOString()) })
        )}
      </p>
    </figure>
  );
}

export interface ComparedSeries {
  key: string;
  /** A service's name, before the measure; never translated. */
  name?: string;
  label: string;
  points: Point[];
  format: (value: number) => string;
  /** Slot of the categorical palette (`--series-N`): it follows the measure, whatever else is shown. */
  slot: number;
}

/**
 * Several measures over the same period on one chart, to see what moves together. Each curve is drawn against its own highest
 * average over the period, so measures of different units share one axis, from 0 to 100 % of their own highest; the legend
 * gives the real values at the cursor and hides or shows each curve.
 */
export function CombinedChart({
  series: all,
  hidden,
  onToggle,
  frame,
}: {
  series: ComparedSeries[];
  hidden: ReadonlySet<string>;
  onToggle: (key: string) => void;
  frame: Frame;
}) {
  const { t } = useTranslation();
  const shownSeries = all.filter((s) => !hidden.has(s.key));
  // The moments of the curves shown, once each: the arrow keys go from one to the next.
  const moments = [...new Set(shownSeries.flatMap((s) => s.points.map((p) => p.at)))].sort((a, b) => a - b).map((at) => ({ at, value: 0, peak: 0 }));
  const { drag, handlers, span } = useChartPointer(moments, frame);
  const height = 220;
  const x = (at: number) => ((at - frame.from) / span) * WIDTH;
  const y = (share: number) => height - Math.min(1, share) * (height - 6) - 3;

  return (
    <figure className="rounded-lg border bg-card p-3">
      <figcaption className="mb-2 space-y-1">
        <span className="text-sm font-medium">{t("monitoring.charts.combined")}</span>
        <p className="text-xs text-muted-foreground">{t("monitoring.charts.combinedHelp")}</p>
      </figcaption>
      <div className="flex gap-2">
        <div className="flex flex-col justify-between py-0.5 text-right text-xs text-muted-foreground tabular-nums" aria-hidden="true">
          <span>{formatPercent(100)}</span>
          <span>{formatPercent(50)}</span>
          <span>{formatPercent(0)}</span>
        </div>
        <div
          className="relative min-w-0 flex-1 cursor-crosshair touch-none rounded-sm outline-none select-none focus-visible:ring-2 focus-visible:ring-ring"
          tabIndex={moments.length > 0 ? 0 : undefined}
          role="img"
          aria-label={t("monitoring.charts.chartLabel", { title: t("monitoring.charts.combined") })}
          {...handlers}
        >
          <svg viewBox={`0 0 ${WIDTH} ${height}`} className="block h-56 w-full" preserveAspectRatio="none" aria-hidden="true">
            {[0, 0.5, 1].map((share) => (
              <line
                key={share}
                x1="0"
                x2={WIDTH}
                y1={y(share)}
                y2={y(share)}
                className="stroke-border"
                strokeWidth="1"
                strokeDasharray={share === 0 ? undefined : "2 4"}
                vectorEffect="non-scaling-stroke"
              />
            ))}
            {shownSeries.map((s) => {
              const top = Math.max(...s.points.map((p) => p.value), 0) || 1;
              const d = linePath(segments(s.points, frame.gap), x, (p) => y(p.value / top));
              return <path key={s.key} d={d} fill="none" stroke={`var(--series-${s.slot})`} strokeWidth="2" strokeLinejoin="round" vectorEffect="non-scaling-stroke" />;
            })}
            {frame.cursor !== null && (
              <line x1={x(frame.cursor)} x2={x(frame.cursor)} y1="0" y2={height} className="stroke-muted-foreground" strokeWidth="1" vectorEffect="non-scaling-stroke" />
            )}
          </svg>
          {/* A dot on each curve at the cursor, with a ring of the card so crossing curves stay apart. */}
          {shownSeries.map((s) => {
            const point = pointAt(s.points, frame);
            if (!point) return null;
            const top = Math.max(...s.points.map((p) => p.value), 0) || 1;
            return (
              <span
                key={s.key}
                className="pointer-events-none absolute size-2 -translate-x-1/2 -translate-y-1/2 rounded-full ring-2 ring-card"
                style={{ left: `${(x(point.at) / WIDTH) * 100}%`, top: `${(y(point.value / top) / height) * 100}%`, background: `var(--series-${s.slot})` }}
              />
            );
          })}
          <Selection drag={drag} frame={frame} span={span} />
        </div>
      </div>
      <div className="pl-10">
        <AxisLabels frame={frame} count={5} />
      </div>
      <p className="min-h-4 pl-10 text-xs text-foreground tabular-nums">{frame.cursor !== null && formatDateTime(new Date(frame.cursor).toISOString())}</p>
      {/* The legend: the value of each measure at the cursor (else its latest), and a button to hide or show its curve. */}
      <ul className="mt-2 flex flex-wrap gap-1.5" aria-label={t("monitoring.charts.legend")}>
        {all.map((s) => {
          const off = hidden.has(s.key);
          const point = frame.cursor === null ? s.points.at(-1) : pointAt(s.points, frame);
          return (
            <li key={s.key}>
              <button
                type="button"
                aria-pressed={!off}
                onClick={() => onToggle(s.key)}
                className={cn(
                  "flex items-center gap-1.5 rounded-md border px-2 py-1 text-xs outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring",
                  off && "text-muted-foreground opacity-60",
                )}
              >
                <span className="size-2.5 shrink-0 rounded-full" style={{ background: off ? "transparent" : `var(--series-${s.slot})`, boxShadow: `inset 0 0 0 2px var(--series-${s.slot})` }} />
                <span>
                  {s.name && <span translate="no">{s.name} · </span>}
                  {s.label}
                </span>
                <span className="font-medium tabular-nums">{point ? s.format(point.value) : "—"}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </figure>
  );
}
