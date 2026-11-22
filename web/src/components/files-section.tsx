import { useQuery, useQueryClient } from "@tanstack/react-query";
import { DownloadIcon, FileArchiveIcon, FileIcon, FolderOpenIcon, LockIcon, Trash2Icon, UploadIcon } from "lucide-react";
import { useRef, useState, type ChangeEvent } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ConfirmDialog } from "@/components/confirm-dialog";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { api } from "@/lib/api";
import { saveBlob } from "@/lib/download";
import { formatBytes, formatDateTime, formatMegabytes } from "@/lib/format";
import { useErrorText, useNotifyError } from "@/lib/hooks";
import type { FileSender, Team, TeamDataPackage, TeamFiles, TeamMission, TeamMissionFile } from "@/lib/types";

/** "Sent by Ann via TeamTAK", "Sent by alpha-shared", or the device that sent it. */
function SenderText({ sender }: { sender: FileSender }) {
  const { t } = useTranslation();
  if (sender.kind === "teamtak") return <>{t("files.sender.teamtak", { name: sender.name ?? "?" })}</>;
  return <>{t("files.sender.tak", { name: sender.name ?? "?" })}</>;
}

/** Picks a file and hands it over; one button, a hidden input. */
function UploadButton({ label, busyLabel, onFile }: { label: string; busyLabel: string; onFile: (file: File) => Promise<void> }) {
  const input = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  async function picked(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    setBusy(true);
    try {
      await onFile(file);
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <input ref={input} type="file" className="hidden" onChange={(e) => void picked(e)} />
      <Button variant="outline" size="sm" onClick={() => input.current?.click()} disabled={busy}>
        <UploadIcon />
        {busy ? busyLabel : label}
      </Button>
    </>
  );
}

type Removal = { kind: "package"; file: TeamDataPackage } | { kind: "mission"; mission: string; file: TeamMissionFile };

function PackageRow({ file, onDownload, onRemove }: { file: TeamDataPackage; onDownload: () => void; onRemove: () => void }) {
  const { t } = useTranslation();
  return (
    <li className="flex flex-wrap items-center gap-3 py-2.5">
      <FileArchiveIcon className="size-4 shrink-0 text-muted-foreground" />
      <div className="min-w-0 flex-1 space-y-0.5">
        <p className="truncate text-sm font-medium">{file.name}</p>
        <p className="text-xs text-muted-foreground">
          {formatBytes(file.size)} · {formatDateTime(file.submitted_at)} · <SenderText sender={file.sender} />
        </p>
      </div>
      {file.channels.map((channel) => (
        <Badge key={channel} variant="outline" className="font-mono">
          {channel}
        </Badge>
      ))}
      <Button variant="ghost" size="icon-sm" aria-label={t("files.download", { name: file.name })} title={t("files.download", { name: file.name })} onClick={onDownload}>
        <DownloadIcon />
      </Button>
      {file.can_delete && (
        <Button variant="ghost" size="icon-sm" aria-label={t("files.delete.action", { name: file.name })} onClick={onRemove}>
          <Trash2Icon />
        </Button>
      )}
    </li>
  );
}

function MissionBlock({
  teamId,
  mission,
  maxBytes,
  maxMb,
  onRemove,
  reload,
}: {
  teamId: string;
  mission: TeamMission;
  maxBytes: number;
  maxMb: number;
  onRemove: (removal: Removal) => void;
  reload: () => Promise<unknown>;
}) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();

  async function add(file: File) {
    if (file.size > maxBytes) {
      toast.error(t("files.tooLarge", { size: formatMegabytes(maxMb) }));
      return;
    }
    try {
      await api.uploadMissionFile(teamId, mission.name, file);
      await reload();
      toast.success(t("files.missions.added", { name: file.name, mission: mission.name }));
    } catch (e) {
      notifyError(e);
    }
  }

  async function download(file: TeamMissionFile) {
    try {
      const { name, blob } = await api.missionFileContent(teamId, mission.name, file.hash);
      saveBlob(name, blob);
    } catch (e) {
      notifyError(e);
    }
  }

  return (
    <section className="space-y-1 rounded-lg border p-3">
      <div className="flex flex-wrap items-center gap-2">
        {mission.locked ? <LockIcon className="size-4 text-muted-foreground" /> : <FolderOpenIcon className="size-4 text-muted-foreground" />}
        <h3 className="text-sm font-medium">{mission.name}</h3>
        {mission.channels.map((channel) => (
          <Badge key={channel} variant="outline" className="font-mono">
            {channel}
          </Badge>
        ))}
        {mission.locked && <Badge variant="secondary">{t("files.missions.locked")}</Badge>}
        {!mission.locked && !mission.writable && <Badge variant="secondary">{t("files.missions.readOnly")}</Badge>}
        {!mission.locked && mission.writable && (
          <span className="ml-auto">
            <UploadButton label={t("files.missions.add")} busyLabel={t("files.sending")} onFile={add} />
          </span>
        )}
      </div>
      {mission.description && <p className="text-xs text-muted-foreground">{mission.description}</p>}
      {mission.locked ? (
        <p className="text-xs text-muted-foreground">{t("files.missions.lockedHelp")}</p>
      ) : !mission.files || mission.files.length === 0 ? (
        <p className="py-1 text-sm text-muted-foreground">{t("files.missions.empty")}</p>
      ) : (
        <ul className="divide-y">
          {mission.files.map((file) => (
            <li key={file.hash} className="flex flex-wrap items-center gap-3 py-2">
              <FileIcon className="size-4 shrink-0 text-muted-foreground" />
              <div className="min-w-0 flex-1 space-y-0.5">
                <p className="truncate text-sm">{file.name}</p>
                <p className="text-xs text-muted-foreground">
                  {formatBytes(file.size)} · {formatDateTime(file.added_at)} · <SenderText sender={file.sender} />
                </p>
              </div>
              <Button variant="ghost" size="icon-sm" aria-label={t("files.download", { name: file.name })} title={t("files.download", { name: file.name })} onClick={() => void download(file)}>
                <DownloadIcon />
              </Button>
              {file.can_remove && (
                <Button variant="ghost" size="icon-sm" aria-label={t("files.missions.remove.action", { name: file.name })} onClick={() => onRemove({ kind: "mission", mission: mission.name, file })}>
                  <Trash2Icon />
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** The TAK Server files of a team's channels: data packages and Data Sync missions. */
export function FilesSection({ team }: { team: Team }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const notifyError = useNotifyError();
  const [channel, setChannel] = useState("");
  const [removing, setRemoving] = useState<Removal | null>(null);
  const query = useQuery({ queryKey: ["files", team.id], queryFn: () => api.teamFiles(team.id), refetchInterval: 60_000 });

  if (query.isPending) return null;
  if (query.isError) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>{t("files.title")}</CardTitle>
        </CardHeader>
        <CardContent>
          <Alert variant="destructive">
            <AlertDescription>{errorText(query.error)}</AlertDescription>
          </Alert>
        </CardContent>
      </Card>
    );
  }
  const data: TeamFiles = query.data;
  const reload = () => queryClient.invalidateQueries({ queryKey: ["files", team.id] });
  const target = data.channels.some((c) => c.group === channel) ? channel : (data.channels[0]?.group ?? "");

  async function send(file: File) {
    if (file.size > data.max_bytes) {
      toast.error(t("files.tooLarge", { size: formatMegabytes(data.max_mb) }));
      return;
    }
    try {
      const sent = await api.uploadTeamFile(team.id, target, file);
      await reload();
      toast.success(sent.wrapped ? t("files.packages.sentWrapped", { name: sent.name }) : t("files.packages.sent", { name: sent.name }));
    } catch (e) {
      notifyError(e);
    }
  }

  async function downloadPackage(file: TeamDataPackage) {
    try {
      const { name, blob } = await api.teamFileContent(team.id, file.key);
      saveBlob(name, blob);
    } catch (e) {
      notifyError(e);
    }
  }

  async function remove(removal: Removal) {
    try {
      if (removal.kind === "package") await api.deleteTeamFile(team.id, removal.file.key);
      else await api.removeMissionFile(team.id, removal.mission, removal.file.hash);
      await reload();
      toast.success(removal.kind === "package" ? t("files.delete.done", { name: removal.file.name }) : t("files.missions.remove.done", { name: removal.file.name }));
    } catch (e) {
      notifyError(e);
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("files.title")}</CardTitle>
        <CardDescription>{t("files.description")}</CardDescription>
      </CardHeader>
      <CardContent>
        <Tabs defaultValue="packages">
          <TabsList>
            <TabsTrigger value="packages">{t("files.packages.tab", { count: data.data_packages.length })}</TabsTrigger>
            <TabsTrigger value="missions">{t("files.missions.tab", { count: data.missions.length })}</TabsTrigger>
          </TabsList>
          <TabsContent value="packages" className="space-y-3 pt-3">
            <div className="flex flex-wrap items-end gap-2">
              {data.channels.length > 1 && (
                <div className="space-y-1.5">
                  <Label htmlFor="file-channel">{t("files.packages.channel")}</Label>
                  <select
                    id="file-channel"
                    value={target}
                    onChange={(e) => setChannel(e.target.value)}
                    className="h-8 max-w-64 rounded-lg border border-input bg-transparent px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
                  >
                    {data.channels.map((c) => (
                      <option key={c.group} value={c.group}>
                        {c.kind === "team" ? t("files.packages.teamChannel") : t("files.packages.eventChannel", { name: c.name })}
                      </option>
                    ))}
                  </select>
                </div>
              )}
              <UploadButton label={t("files.packages.send")} busyLabel={t("files.sending")} onFile={send} />
            </div>
            <p className="text-xs text-muted-foreground">{t("files.packages.help", { size: formatMegabytes(data.max_mb) })}</p>
            {data.data_packages.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t("files.packages.none")}</p>
            ) : (
              <ul className="divide-y">
                {data.data_packages.map((file) => (
                  <PackageRow key={file.key} file={file} onDownload={() => void downloadPackage(file)} onRemove={() => setRemoving({ kind: "package", file })} />
                ))}
              </ul>
            )}
          </TabsContent>
          <TabsContent value="missions" className="space-y-3 pt-3">
            <p className="text-xs text-muted-foreground">{t("files.missions.help")}</p>
            {data.missions.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t("files.missions.none")}</p>
            ) : (
              data.missions.map((mission) => (
                <MissionBlock key={mission.name} teamId={team.id} mission={mission} maxBytes={data.max_bytes} maxMb={data.max_mb} onRemove={setRemoving} reload={reload} />
              ))
            )}
          </TabsContent>
        </Tabs>
      </CardContent>
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={removing?.kind === "mission" ? t("files.missions.remove.title", { name: removing.file.name }) : t("files.delete.title", { name: removing?.file.name ?? "" })}
        description={<p>{removing?.kind === "mission" ? t("files.missions.remove.text", { mission: removing.mission }) : t("files.delete.text")}</p>}
        confirmLabel={removing?.kind === "mission" ? t("files.missions.remove.confirm") : t("files.delete.confirm")}
        destructive
        onConfirm={() => (removing ? remove(removing) : Promise.resolve())}
      />
    </Card>
  );
}
