import { useQuery, useQueryClient } from "@tanstack/react-query";
import { EyeIcon, EyeOffIcon, PlusIcon, RefreshCwIcon, Trash2Icon, VideoIcon } from "lucide-react";
import { QRCodeSVG } from "qrcode.react";
import { useId, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { EventPhaseBadge } from "@/components/badges";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { CopyButton } from "@/components/copy-button";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { api } from "@/lib/api";
import { formatDate, formatRelative } from "@/lib/format";
import { useErrorText, useNotifyError } from "@/lib/hooks";
import type { StreamUrls, Team, VideoChannel, VideoLive, VideoStreamDetail, VideoStreamSummary } from "@/lib/types";

type Protocol = keyof StreamUrls;
const PROTOCOLS: Protocol[] = ["rtsp", "rtmp", "srt"];
/** The protocols' names, the same in every language. */
const PROTOCOL_NAME: Record<Protocol, string> = { rtsp: "RTSP", rtmp: "RTMP", srt: "SRT" };

/** "Live · 2 watching · for 5 minutes", "Offline", or why the stream does not work now. */
function LiveState({ stream }: { stream: VideoStreamSummary }) {
  const { t } = useTranslation();
  if (!stream.valid) return <Badge variant="outline">{t("video.state.inactive")}</Badge>;
  const live: VideoLive | null = stream.live;
  if (!live?.ready) {
    return (
      <>
        <Badge variant="secondary">{t("video.state.offline")}</Badge>
        {live && live.readers > 0 && <span>{t("video.state.waiting", { count: live.readers })}</span>}
      </>
    );
  }
  return (
    <>
      <Badge>{t("video.state.live")}</Badge>
      <span>
        {t("video.state.watching", { count: live.readers })}
        {live.since && <> · {t("video.state.since", { relative: formatRelative(live.since) })}</>}
        {live.protocol && <> · {live.protocol.toUpperCase()}</>}
      </span>
    </>
  );
}

function StreamRow({ stream, onOpen }: { stream: VideoStreamSummary; onOpen: (stream: VideoStreamSummary) => void }) {
  const { t } = useTranslation();
  return (
    <li className="flex flex-wrap items-center gap-3 py-3">
      <VideoIcon className="size-4 text-muted-foreground" />
      <div className="min-w-0 flex-1 space-y-1">
        <p className="truncate text-sm font-medium">
          {stream.label}
          {!stream.own && <span className="font-normal text-muted-foreground"> · {t("video.byTeam", { team: stream.team_name })}</span>}
        </p>
        <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
          <span className="font-mono">{stream.path}</span>
          <span>·</span>
          <LiveState stream={stream} />
        </div>
      </div>
      {stream.can_use && (
        <Button size="sm" onClick={() => onOpen(stream)}>
          {t("video.addresses")}
        </Button>
      )}
    </li>
  );
}

function ChannelBlock({ channel, onOpen }: { channel: VideoChannel; onOpen: (stream: VideoStreamSummary) => void }) {
  const { t } = useTranslation();
  return (
    <section className="space-y-1">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-sm font-medium">
          {channel.kind === "team" ? t("video.channel.team") : t("video.channel.event", { name: channel.name })}
        </h3>
        <span className="font-mono text-xs text-muted-foreground">{channel.group}</span>
        {channel.phase && <EventPhaseBadge phase={channel.phase} />}
      </div>
      {channel.kind === "event" && channel.phase === "scheduled" && (
        <p className="text-xs text-muted-foreground">{t("video.channel.scheduledHelp")}</p>
      )}
      {channel.streams.length === 0 ? (
        <p className="py-2 text-sm text-muted-foreground">{t("video.none")}</p>
      ) : (
        <ul className="divide-y">
          {channel.streams.map((stream) => (
            <StreamRow key={stream.id} stream={stream} onOpen={onOpen} />
          ))}
        </ul>
      )}
    </section>
  );
}

function AddStreamForm({ teamId, channels, onCreated }: { teamId: string; channels: VideoChannel[]; onCreated: (id: string, label: string) => Promise<void> }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [label, setLabel] = useState("");
  const [channel, setChannel] = useState("");
  const [pending, setPending] = useState(false);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setPending(true);
    try {
      const created = await api.createVideoStream(teamId, { label, ...(channel ? { event_id: channel } : {}) });
      if (!created.synced) toast.warning(t("video.add.pendingSync"));
      const name = label.trim();
      setLabel("");
      await onCreated(created.id, name);
    } catch (e) {
      notifyError(e);
    } finally {
      setPending(false);
    }
  }

  const events = channels.filter((c) => c.kind === "event" && c.event_id);
  return (
    <form onSubmit={(e) => void submit(e)} className="flex flex-wrap items-end gap-2 border-t pt-4">
      <div className="min-w-52 flex-1 space-y-1.5">
        <Label htmlFor="stream-label">{t("video.add.label")}</Label>
        <Input
          id="stream-label"
          maxLength={40}
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          placeholder={t("video.add.placeholder")}
        />
      </div>
      {events.length > 0 && (
        <div className="space-y-1.5">
          <Label htmlFor="stream-channel">{t("video.add.channel")}</Label>
          <select
            id="stream-channel"
            value={channel}
            onChange={(e) => setChannel(e.target.value)}
            className="h-8 max-w-64 rounded-lg border border-input bg-transparent px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
          >
            <option value="">{t("video.channel.team")}</option>
            {events.map((c) => (
              <option key={c.event_id} value={c.event_id ?? ""}>
                {t("video.channel.event", { name: c.name })}
              </option>
            ))}
          </select>
        </div>
      )}
      <Button type="submit" variant="outline" disabled={pending || label.trim().length === 0}>
        <PlusIcon />
        {pending ? t("common.working") : t("video.add.submit")}
      </Button>
      <p className="w-full text-xs text-muted-foreground">{t("video.add.help")}</p>
    </form>
  );
}

/** The video streams of a team's channel and of the events it takes part in. */
export function VideoSection({ team }: { team: Team }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const [open, setOpen] = useState<{ id: string; title: string } | null>(null);
  const query = useQuery({ queryKey: ["video", team.id], queryFn: () => api.teamVideo(team.id), refetchInterval: 15_000 });

  if (query.isPending) return null;
  if (query.isError) {
    return (
      <Alert variant="destructive">
        <AlertDescription>{errorText(query.error)}</AlertDescription>
      </Alert>
    );
  }
  const video = query.data;
  if (!video.enabled) return null;
  const reload = () => queryClient.invalidateQueries({ queryKey: ["video", team.id] });

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("video.title")}</CardTitle>
        <CardDescription>{t("video.description")}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-5">
        {!video.reachable && (
          <Alert>
            <AlertDescription>{t("video.unreachable")}</AlertDescription>
          </Alert>
        )}
        {video.channels.map((channel) => (
          <ChannelBlock key={channel.group} channel={channel} onOpen={(s) => setOpen({ id: s.id, title: s.label })} />
        ))}
        {video.permissions.create && (
          <AddStreamForm
            teamId={team.id}
            channels={video.channels}
            onCreated={async (id, title) => {
              await reload();
              setOpen({ id, title });
            }}
          />
        )}
      </CardContent>
      <VideoStreamDialog streamId={open?.id ?? null} title={open?.title ?? ""} onChanged={reload} onClose={() => setOpen(null)} />
    </Card>
  );
}

/** A URL with the key in it, shown masked until asked. */
function UrlField({ label, help, url, secret }: { label: string; help?: string; url: string; secret: string }) {
  const { t } = useTranslation();
  const [shown, setShown] = useState(false);
  return (
    <div className="grid grid-cols-[5rem_1fr_auto] items-center gap-2 border-b py-2 last:border-b-0">
      <dt className="text-muted-foreground">
        {label}
        {help && <span className="block text-[0.7rem] leading-tight">{help}</span>}
      </dt>
      <dd className="truncate font-mono text-[0.8rem]" title={shown ? url : undefined}>
        {shown ? url : url.replace(secret, "•".repeat(8))}
      </dd>
      <div className="flex gap-1">
        <Button variant="outline" size="icon-sm" onClick={() => setShown(!shown)} aria-label={shown ? t("access.manual.hide") : t("access.manual.show")}>
          {shown ? <EyeOffIcon /> : <EyeIcon />}
        </Button>
        <CopyButton value={url} label={label} />
      </div>
    </div>
  );
}

function PlainField({ label, value, secret }: { label: string; value: string; secret?: boolean }) {
  const { t } = useTranslation();
  const [shown, setShown] = useState(!secret);
  return (
    <div className="grid grid-cols-[8rem_1fr_auto] items-center gap-2 border-b py-2 last:border-b-0">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="truncate font-mono text-[0.8rem]">{shown ? value : "•".repeat(12)}</dd>
      <div className="flex gap-1">
        {secret && (
          <Button variant="outline" size="icon-sm" onClick={() => setShown(!shown)} aria-label={shown ? t("access.manual.hide") : t("access.manual.show")}>
            {shown ? <EyeOffIcon /> : <EyeIcon />}
          </Button>
        )}
        <CopyButton value={value} label={label} />
      </div>
    </div>
  );
}

/** One protocol's address as a QR code, for a phone that films or watches. */
function QrOfUrls({ urls, title }: { urls: StreamUrls; title: string }) {
  const { t } = useTranslation();
  const labelId = useId();
  const [protocol, setProtocol] = useState<Protocol>("rtmp");
  return (
    <div className="flex flex-col items-center gap-3 rounded-lg border bg-card p-4 text-center">
      <div className="flex flex-wrap items-center justify-center gap-2">
        <span id={labelId} className="text-sm font-medium">
          {title}
        </span>
        <div role="radiogroup" aria-labelledby={labelId} className="flex gap-1.5">
          {PROTOCOLS.map((p) => (
            <Button key={p} type="button" role="radio" aria-checked={protocol === p} size="sm" variant={protocol === p ? "default" : "outline"} onClick={() => setProtocol(p)}>
              {PROTOCOL_NAME[p]}
            </Button>
          ))}
        </div>
      </div>
      <div className="rounded-md bg-white p-2">
        <QRCodeSVG value={urls[protocol]} size={180} level="M" title={title} />
      </div>
      <p className="text-xs text-muted-foreground">{t("video.qrHelp")}</p>
    </div>
  );
}

function StreamBody({ stream, onChanged, onClose }: { stream: VideoStreamDetail; onChanged: (gone?: boolean) => Promise<unknown>; onClose: () => void }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [confirm, setConfirm] = useState<"regenerate" | "remove" | null>(null);
  const { publish, view, server } = stream;

  async function regenerate() {
    try {
      const result = await api.regenerateVideoStream(stream.id);
      await onChanged();
      toast.success(result.synced ? t("video.regenerate.done") : t("video.regenerate.pending"));
    } catch (e) {
      notifyError(e);
    }
  }

  async function remove() {
    try {
      const result = await api.deleteVideoStream(stream.id);
      await onChanged(true);
      toast.success(result.synced ? t("video.remove.done") : t("video.remove.pending"));
      onClose();
    } catch (e) {
      notifyError(e);
    }
  }

  return (
    <>
      {!stream.valid && (
        <Alert>
          <AlertTitle>{t("video.inactive.title")}</AlertTitle>
          <AlertDescription>{stream.channel.kind === "event" ? t("video.inactive.event") : t("video.inactive.team")}</AlertDescription>
        </Alert>
      )}
      <Tabs defaultValue="publish">
        <TabsList className="w-full">
          <TabsTrigger value="publish">{t("video.tabs.publish")}</TabsTrigger>
          <TabsTrigger value="view">{t("video.tabs.view")}</TabsTrigger>
          <TabsTrigger value="manual">{t("video.tabs.manual")}</TabsTrigger>
        </TabsList>

        <TabsContent value="publish" className="space-y-3 pt-2">
          <p className="text-sm text-muted-foreground">{t("video.publish.intro")}</p>
          <dl className="rounded-lg border bg-card px-3 text-sm">
            <UrlField label={PROTOCOL_NAME.rtmp} help={t("video.publish.rtmpHelp")} url={publish.urls.rtmp} secret={publish.key} />
            <UrlField label={PROTOCOL_NAME.srt} help={t("video.publish.srtHelp")} url={publish.urls.srt} secret={publish.key} />
            <UrlField label={PROTOCOL_NAME.rtsp} help={t("video.publish.rtspHelp")} url={publish.urls.rtsp} secret={publish.key} />
          </dl>
          <QrOfUrls urls={publish.urls} title={t("video.publish.qrTitle")} />
          <p className="text-xs text-muted-foreground">{t("video.publish.warning")}</p>
        </TabsContent>

        <TabsContent value="view" className="space-y-3 pt-2">
          <Alert>
            <AlertTitle>{t("video.view.atakTitle")}</AlertTitle>
            <AlertDescription>{t("video.view.atakText", { channel: stream.channel.group })}</AlertDescription>
          </Alert>
          <dl className="rounded-lg border bg-card px-3 text-sm">
            <UrlField label={PROTOCOL_NAME.rtsp} help={t("video.view.rtspHelp")} url={view.urls.rtsp} secret={view.key} />
            <UrlField label={PROTOCOL_NAME.srt} help={t("video.view.srtHelp")} url={view.urls.srt} secret={view.key} />
            <UrlField label={PROTOCOL_NAME.rtmp} help={t("video.view.rtmpHelp")} url={view.urls.rtmp} secret={view.key} />
          </dl>
          <p className="text-xs text-muted-foreground">{t("video.view.warning")}</p>
        </TabsContent>

        <TabsContent value="manual" className="pt-2">
          <dl className="rounded-lg border bg-card px-3 text-sm">
            <PlainField label={t("video.manual.host")} value={server.host} />
            <PlainField label={t("video.manual.ports")} value={t("video.manual.portsValue", { rtsp: server.rtsp_port, rtmp: server.rtmp_port, srt: server.srt_port })} />
            <PlainField label={t("video.manual.path")} value={stream.path} />
            <PlainField label={t("video.manual.publishUser")} value={publish.user} />
            <PlainField label={t("video.manual.publishKey")} value={publish.key} secret />
            <PlainField label={t("video.manual.viewUser")} value={view.user} />
            <PlainField label={t("video.manual.viewKey")} value={view.key} secret />
          </dl>
          <p className="pt-2 text-xs text-muted-foreground">{t("video.manual.help")}</p>
        </TabsContent>
      </Tabs>

      <div className="flex flex-wrap items-center justify-between gap-2 border-t pt-3 text-xs text-muted-foreground">
        <span>
          {stream.regenerated_at
            ? t("video.regeneratedOn", { date: formatDate(stream.regenerated_at) })
            : t("video.declaredOn", { date: formatDate(stream.created_at) })}
        </span>
        {stream.can_manage && (
          <span className="flex gap-2">
            <Button variant="outline" size="sm" onClick={() => setConfirm("regenerate")}>
              <RefreshCwIcon />
              {t("video.regenerate.action")}
            </Button>
            <Button variant="destructive" size="sm" onClick={() => setConfirm("remove")}>
              <Trash2Icon />
              {t("video.remove.action")}
            </Button>
          </span>
        )}
      </div>

      <ConfirmDialog
        open={confirm === "regenerate"}
        onOpenChange={(open) => !open && setConfirm(null)}
        title={t("video.regenerate.title")}
        description={<p>{t("video.regenerate.text")}</p>}
        confirmLabel={t("video.regenerate.confirm")}
        destructive
        onConfirm={regenerate}
      />
      <ConfirmDialog
        open={confirm === "remove"}
        onOpenChange={(open) => !open && setConfirm(null)}
        title={t("video.remove.title")}
        description={<p>{t("video.remove.text")}</p>}
        confirmLabel={t("video.remove.confirm")}
        destructive
        onConfirm={remove}
      />
    </>
  );
}

/** The addresses of one stream, to publish to it and to watch it, keys included. */
function VideoStreamDialog({ streamId, title, onChanged, onClose }: { streamId: string | null; title: string; onChanged: () => Promise<unknown>; onClose: () => void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const query = useQuery({
    queryKey: ["video-stream", streamId],
    queryFn: () => api.videoStream(streamId!),
    enabled: streamId !== null,
    gcTime: 0, // holds keys: not kept once the dialog is closed
  });

  async function refresh(gone = false) {
    await onChanged();
    if (!gone) await queryClient.invalidateQueries({ queryKey: ["video-stream", streamId] });
  }

  return (
    <Dialog open={streamId !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-2xl" closeLabel={t("common.close")}>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>
            {query.data
              ? query.data.channel.kind === "team"
                ? t("video.dialog.teamChannel", { path: query.data.path })
                : t("video.dialog.eventChannel", { path: query.data.path, event: query.data.channel.name })
              : t("video.dialog.description")}
          </DialogDescription>
        </DialogHeader>
        {query.isPending && <p className="py-8 text-center text-sm text-muted-foreground">{t("common.loading")}</p>}
        {query.isError && (
          <Alert variant="destructive">
            <AlertDescription>{errorText(query.error)}</AlertDescription>
          </Alert>
        )}
        {query.data && <StreamBody stream={query.data} onChanged={refresh} onClose={onClose} />}
      </DialogContent>
    </Dialog>
  );
}
