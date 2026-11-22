import { useQuery, useQueryClient } from "@tanstack/react-query";
import { DownloadIcon, MapIcon, PencilIcon, RefreshCwIcon, Trash2Icon, TriangleAlertIcon, UploadIcon } from "lucide-react";
import { useEffect, useRef, useState, type ChangeEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { EventPhaseBadge } from "@/components/badges";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { currentLanguage } from "@/i18n";
import { api, type Synced } from "@/lib/api";
import { fileToBase64, saveBlob } from "@/lib/download";
import { useErrorText, useNotifyError } from "@/lib/hooks";
import type { ChannelMaps, MapChannel, MapProblem, MapSource, MapTerms, MapUploadResult, Team } from "@/lib/types";

/** What the provider of a map allows, with the explanation on hover. */
export function MapTermsBadge({ terms }: { terms: MapTerms }) {
  const { t } = useTranslation();
  const variant = terms === "restricted" ? "destructive" : terms === "no_offline" || terms === "unknown" ? "secondary" : "outline";
  return (
    <Badge variant={variant} title={t(`maps.terms.${terms}.help`)}>
      {t(`maps.terms.${terms}.label`)}
    </Badge>
  );
}

/** Why a file is not a map source, or why a source does not work, in words. */
export function useProblemText() {
  const { t } = useTranslation();
  return (problem: MapProblem) => t(`maps.problem.${problem.code}`, { status: problem.status ?? "" });
}

/** "z0–19", or nothing when the file says nothing about zoom levels. */
function zoomText(source: MapSource): string {
  if (source.min_zoom === null && source.max_zoom === null) return "";
  return `z${source.min_zoom ?? 0}–${source.max_zoom ?? "?"}`;
}

/** A map source in a list; `showOwner` names the team that added it. */
export function MapRow({ source, showOwner, children }: { source: MapSource; showOwner?: boolean; children?: ReactNode }) {
  const { t } = useTranslation();
  const problemText = useProblemText();
  return (
    <li className="flex flex-wrap items-center gap-3 py-2.5">
      <MapIcon className="size-4 shrink-0 text-muted-foreground" />
      <div className="min-w-0 flex-1 space-y-0.5">
        <p className="truncate text-sm font-medium">{source.name}</p>
        <p className="flex flex-wrap items-center gap-x-1.5 text-xs text-muted-foreground">
          <span className="font-mono">{source.host}</span>
          {zoomText(source) && <span>· {zoomText(source)}</span>}
          <span>·</span>
          <span className="font-mono">{source.file_name}</span>
        </p>
        {source.problem && (
          <p className={source.broken ? "text-xs text-destructive" : "text-xs text-muted-foreground"}>
            {source.broken
              ? t("maps.broken.text", { reason: problemText(source.problem) })
              : t("maps.broken.lastCheckFailed", { reason: problemText(source.problem) })}
          </p>
        )}
      </div>
      {showOwner && source.team_name && <Badge variant="outline">{t("maps.owner", { team: source.team_name })}</Badge>}
      {source.broken && (
        <Badge variant="destructive" title={t("maps.broken.help")}>
          {t("maps.broken.label")}
        </Badge>
      )}
      <MapTermsBadge terms={source.terms} />
      {children}
    </li>
  );
}

/** Checks a source again, and says what came of it. */
export function CheckAgainButton({ source, onChecked }: { source: MapSource; onChecked: () => Promise<unknown> }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [pending, setPending] = useState(false);
  async function run() {
    setPending(true);
    try {
      const checked = await api.checkMapSource(source.id);
      await onChecked();
      if (checked.problem) toast.warning(t("maps.check.stillFailing", { name: checked.name }));
      else toast.success(t("maps.check.works", { name: checked.name }));
    } catch (e) {
      notifyError(e);
    } finally {
      setPending(false);
    }
  }
  const label = t("maps.check.action", { name: source.name });
  return (
    <Button variant="ghost" size="icon-sm" aria-label={label} title={label} disabled={pending} onClick={() => void run()}>
      <RefreshCwIcon />
    </Button>
  );
}

function OutcomeBadge({ outcome }: { outcome: MapUploadResult["outcome"] }) {
  const { t } = useTranslation();
  const variant = outcome === "rejected" ? "destructive" : outcome === "unchanged" ? "outline" : "default";
  return <Badge variant={variant}>{t(`maps.outcome.${outcome}`)}</Badge>;
}

/** What became of each file of the latest upload. */
export function UploadResults({ results }: { results: MapUploadResult[] }) {
  const problemText = useProblemText();
  return (
    <ul className="divide-y rounded-lg border px-3">
      {results.map((result, i) => (
        <li key={`${result.file}-${i}`} className="flex flex-wrap items-center gap-2 py-2 text-sm">
          <OutcomeBadge outcome={result.outcome} />
          <span className="font-mono text-xs">{result.file}</span>
          {result.name && <span>· {result.name}</span>}
          {result.problem && <span className="text-muted-foreground">· {problemText(result.problem)}</span>}
          {result.terms && result.terms !== "open" && <MapTermsBadge terms={result.terms} />}
        </li>
      ))}
    </ul>
  );
}

/** Sends picked files, one request each, and gathers what came of them. */
export async function uploadFiles(
  files: File[],
  maxBytes: number,
  send: (name: string, data: string) => Promise<Synced & { results: MapUploadResult[] }>,
): Promise<{ results: MapUploadResult[]; synced: boolean }> {
  const results: MapUploadResult[] = [];
  let synced = true;
  for (const file of files) {
    if (file.size > maxBytes) {
      results.push({ file: file.name, outcome: "rejected", problem: { code: "too_large", status: null } });
      continue;
    }
    const answer = await send(file.name, await fileToBase64(file));
    results.push(...answer.results);
    synced &&= answer.synced;
  }
  return { results, synced };
}

/** Ticks the maps of a channel among the sources it may take. */
export function MapChooserDialog({
  title,
  description,
  catalogue,
  selected,
  perChannel,
  save,
  onRemoveSource,
  onClose,
  onSaved,
}: {
  title: string;
  description: string;
  catalogue: MapSource[];
  selected: string[];
  perChannel: number;
  save: (ids: string[]) => Promise<Synced>;
  /** Removes one of the team's own sources from TeamTAK; absent where none can be removed. */
  onRemoveSource?: (source: MapSource) => void;
  onClose: () => void;
  onSaved: () => Promise<unknown>;
}) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [chosen, setChosen] = useState(() => new Set(selected));
  const [pending, setPending] = useState(false);
  const sorted = [...catalogue].sort(
    (a, b) => Number(a.team_id !== null) - Number(b.team_id !== null) || a.name.localeCompare(b.name, currentLanguage()),
  );
  const tooMany = chosen.size > perChannel;

  function toggle(id: string, on: boolean) {
    const next = new Set(chosen);
    if (on) next.add(id);
    else next.delete(id);
    setChosen(next);
  }

  async function submit() {
    setPending(true);
    try {
      const result = await save([...chosen]);
      await onSaved();
      if (result.synced) toast.success(t("maps.choose.done"));
      else toast.warning(t("maps.choose.pendingSync"));
      onClose();
    } catch (e) {
      notifyError(e);
    } finally {
      setPending(false);
    }
  }

  const restrictedChosen = sorted.some((source) => chosen.has(source.id) && source.terms === "restricted");
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-xl" closeLabel={t("common.close")}>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        {sorted.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("maps.choose.emptyCatalogue")}</p>
        ) : (
          <ul className="max-h-[50vh] divide-y overflow-y-auto">
            {sorted.map((source) => (
              <li key={source.id} className="flex items-center gap-2">
                <label className="flex min-w-0 flex-1 cursor-pointer items-center gap-3 py-2">
                  <input type="checkbox" className="size-4" checked={chosen.has(source.id)} onChange={(e) => toggle(source.id, e.target.checked)} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-sm font-medium">{source.name}</span>
                    <span className="block truncate font-mono text-xs text-muted-foreground">{source.host}</span>
                  </span>
                  {source.team_id && <Badge variant="outline">{t("maps.ownSource")}</Badge>}
                  {source.broken && <Badge variant="destructive">{t("maps.broken.label")}</Badge>}
                  <MapTermsBadge terms={source.terms} />
                </label>
                {source.team_id && onRemoveSource && (
                  <Button variant="ghost" size="icon-sm" aria-label={t("maps.removeSource.action", { name: source.name })} onClick={() => onRemoveSource(source)}>
                    <Trash2Icon />
                  </Button>
                )}
              </li>
            ))}
          </ul>
        )}
        {restrictedChosen && (
          <Alert>
            <TriangleAlertIcon />
            <AlertDescription>{t("maps.choose.restrictedWarning")}</AlertDescription>
          </Alert>
        )}
        {tooMany && <p className="text-sm text-destructive">{t("maps.choose.tooMany", { max: perChannel })}</p>}
        <p className="text-xs text-muted-foreground">{t("maps.choose.removalNote")}</p>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button onClick={() => void submit()} disabled={pending || tooMany}>
            {pending ? t("common.working") : t("common.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** The maps of an event, picked by its organisers: loaded when the dialog opens. */
export function EventMapsDialog({
  eventId,
  eventName,
  load,
  save,
  onClose,
}: {
  eventId: string;
  eventName: string;
  load: () => Promise<ChannelMaps>;
  save: (ids: string[]) => Promise<Synced>;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const notifyError = useNotifyError();
  const query = useQuery({ queryKey: ["event-maps", eventId], queryFn: load, gcTime: 0, retry: false });
  const failed = query.isError ? query.error : null;
  useEffect(() => {
    if (failed) {
      notifyError(failed);
      onClose();
    }
  }, [failed, notifyError, onClose]);
  if (query.isPending || query.isError) return null;
  return (
    <MapChooserDialog
      title={t("maps.choose.eventTitle", { name: eventName })}
      description={t("maps.choose.eventDescription")}
      catalogue={query.data.catalogue}
      selected={query.data.maps.map((m) => m.id)}
      perChannel={query.data.per_channel}
      save={save}
      onClose={onClose}
      onSaved={() => queryClient.invalidateQueries({ queryKey: ["maps"] })}
    />
  );
}

function ChannelBlock({ channel, onEdit, reload }: { channel: MapChannel; onEdit: (channel: MapChannel) => void; reload: () => Promise<unknown> }) {
  const { t } = useTranslation();
  return (
    <section className="space-y-1">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-sm font-medium">
          {channel.kind === "team" ? t("maps.channel.team") : t("maps.channel.event", { name: channel.name })}
        </h3>
        <span className="font-mono text-xs text-muted-foreground">{channel.group}</span>
        {channel.phase && <EventPhaseBadge phase={channel.phase} />}
        {channel.can_edit && (
          <Button variant="ghost" size="sm" className="ml-auto" onClick={() => onEdit(channel)}>
            <PencilIcon />
            {t("maps.choose.action")}
          </Button>
        )}
      </div>
      {channel.kind === "event" && channel.phase === "scheduled" && channel.maps.length > 0 && (
        <p className="text-xs text-muted-foreground">{t("maps.channel.scheduledHelp")}</p>
      )}
      {channel.maps.length === 0 ? (
        <p className="py-2 text-sm text-muted-foreground">{t("maps.none")}</p>
      ) : (
        <ul className="divide-y">
          {channel.maps.map((source) => (
            <MapRow key={source.id} source={source}>
              {channel.can_edit && source.problem && <CheckAgainButton source={source} onChecked={reload} />}
            </MapRow>
          ))}
        </ul>
      )}
    </section>
  );
}

/** The maps of a team's channel and of the events it takes part in. */
export function MapsSection({ team }: { team: Team }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const notifyError = useNotifyError();
  const input = useRef<HTMLInputElement>(null);
  const [editing, setEditing] = useState<MapChannel | null>(null);
  const [downloading, setDownloading] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [results, setResults] = useState<MapUploadResult[]>([]);
  const [removing, setRemoving] = useState<MapSource | null>(null);
  const query = useQuery({ queryKey: ["maps", team.id], queryFn: () => api.teamMaps(team.id) });

  if (query.isPending) return null;
  if (query.isError) {
    return (
      <Alert variant="destructive">
        <AlertDescription>{errorText(query.error)}</AlertDescription>
      </Alert>
    );
  }
  const data = query.data;
  const reload = () => queryClient.invalidateQueries({ queryKey: ["maps", team.id] });
  // What the package holds: the team's maps and those of the events running now, that work.
  const reachable = data.channels.filter((c) => c.kind === "team" || c.phase === "running").some((c) => c.maps.some((m) => !m.broken));

  async function download() {
    setDownloading(true);
    try {
      const { name, blob } = await api.teamMapsPackage(team.id);
      saveBlob(name, blob);
    } catch (e) {
      notifyError(e);
    } finally {
      setDownloading(false);
    }
  }

  async function upload(event: ChangeEvent<HTMLInputElement>) {
    const files = [...(event.target.files ?? [])];
    event.target.value = "";
    if (files.length === 0) return;
    setUploading(true);
    try {
      const done = await uploadFiles(files, data.limits.upload_bytes, (name, content) => api.uploadTeamMapSource(team.id, name, content));
      setResults(done.results);
      const stored = done.results.filter((r) => r.outcome !== "rejected").length;
      if (stored > 0) toast.success(t("maps.addSource.done", { count: stored }));
      if (!done.synced) toast.warning(t("maps.choose.pendingSync"));
    } catch (e) {
      notifyError(e);
    } finally {
      setUploading(false);
      await reload();
    }
  }

  async function removeSource(source: MapSource) {
    try {
      const answer = await api.removeTeamMapSource(team.id, source.id);
      setEditing(null);
      await reload();
      if (answer.synced) toast.success(t("maps.removeSource.done", { name: source.name }));
      else toast.warning(t("maps.choose.pendingSync"));
    } catch (e) {
      notifyError(e);
    }
  }

  function save(channel: MapChannel, ids: string[]) {
    return channel.kind === "team" || !channel.event_id ? api.setTeamMaps(team.id, ids) : api.setEventMaps(team.id, channel.event_id, ids);
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("maps.title")}</CardTitle>
        <CardDescription>{t("maps.description")}</CardDescription>
        {(reachable || data.permissions.add_sources) && (
          <CardAction className="flex flex-wrap justify-end gap-2">
            {data.permissions.add_sources && (
              <>
                <input
                  ref={input}
                  type="file"
                  multiple
                  accept=".xml,.zip,text/xml,application/xml,application/zip"
                  className="hidden"
                  onChange={(e) => void upload(e)}
                />
                <Button variant="outline" onClick={() => input.current?.click()} disabled={uploading}>
                  <UploadIcon />
                  {uploading ? t("maps.addSource.checking") : t("maps.addSource.action")}
                </Button>
              </>
            )}
            {reachable && (
              <Button variant="outline" onClick={() => void download()} disabled={downloading}>
                <DownloadIcon />
                {downloading ? t("common.working") : t("maps.download.action")}
              </Button>
            )}
          </CardAction>
        )}
      </CardHeader>
      <CardContent className="space-y-5">
        {data.permissions.add_sources && <p className="text-xs text-muted-foreground">{t("maps.addSource.help", { max: data.limits.files })}</p>}
        {results.length > 0 && <UploadResults results={results} />}
        {data.channels.map((channel) => (
          <ChannelBlock key={channel.group} channel={channel} onEdit={setEditing} reload={reload} />
        ))}
        <ul className="list-disc space-y-1 border-t pt-4 pl-5 text-xs text-muted-foreground">
          <li>{t("maps.notes.atak")}</li>
          <li>{t("maps.notes.itak")}</li>
          <li>{t("maps.notes.removal")}</li>
          <li>{t("maps.notes.checks")}</li>
          <li>{t("maps.notes.offline")}</li>
        </ul>
      </CardContent>
      {editing && (
        <MapChooserDialog
          title={editing.kind === "team" ? t("maps.choose.teamTitle") : t("maps.choose.eventTitle", { name: editing.name })}
          description={editing.kind === "team" ? t("maps.choose.teamDescription") : t("maps.choose.eventDescription")}
          catalogue={data.catalogue}
          selected={editing.maps.map((m) => m.id)}
          perChannel={data.per_channel}
          save={(ids) => save(editing, ids)}
          onRemoveSource={data.permissions.add_sources ? setRemoving : undefined}
          onClose={() => setEditing(null)}
          onSaved={reload}
        />
      )}
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t("maps.removeSource.title", { name: removing?.name ?? "" })}
        description={<p>{t("maps.removeSource.text")}</p>}
        confirmLabel={t("maps.removeSource.confirm")}
        destructive
        onConfirm={() => (removing ? removeSource(removing) : Promise.resolve())}
      />
    </Card>
  );
}
