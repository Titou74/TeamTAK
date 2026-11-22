import { useQuery, useQueryClient } from "@tanstack/react-query";
import { DownloadIcon, EyeIcon, EyeOffIcon, RefreshCwIcon, Trash2Icon } from "lucide-react";
import { QRCodeSVG } from "qrcode.react";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ConfirmDialog } from "@/components/confirm-dialog";
import { CopyButton } from "@/components/copy-button";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { api } from "@/lib/api";
import { saveBlob } from "@/lib/download";
import { formatDate, formatRelative } from "@/lib/format";
import { useErrorText, useNotifyError } from "@/lib/hooks";
import type { AccessDetail } from "@/lib/types";

function Field({ label, value, secret }: { label: string; value: string; secret?: boolean }) {
  const { t } = useTranslation();
  const [shown, setShown] = useState(!secret);
  return (
    <div className="grid grid-cols-[8rem_1fr_auto] items-center gap-2 border-b py-2 last:border-b-0">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="truncate font-mono text-[0.8rem]" data-testid={`field-${label}`}>
        {shown ? value : "•".repeat(12)}
      </dd>
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

function QrPanel({ value, title, help }: { value: string; title: string; help: string }) {
  return (
    <div className="flex flex-col items-center gap-3 rounded-lg border bg-card p-4 text-center">
      <p className="font-medium">{title}</p>
      <div className="rounded-md bg-white p-2">
        <QRCodeSVG value={value} size={196} level="M" title={title} />
      </div>
      <p className="text-xs text-muted-foreground">{help}</p>
    </div>
  );
}

/** The port ATAK connects on after enrolling from its QR code, which carries none. */
const ATAK_DEFAULT_PORT = 8089;

interface PortPickerProps {
  ports: number[];
  main: number;
  value: number;
  onChange: (port: number) => void;
  help: string;
}

/** Which of TAK Server's client ports the QR code or the zips of a tab use. */
function PortPicker({ ports, main, value, onChange, help }: PortPickerProps) {
  const { t } = useTranslation();
  const labelId = useId();
  return (
    <div className="space-y-2 rounded-lg border bg-card p-3">
      <div className="flex flex-wrap items-center gap-2">
        <span id={labelId} className="text-sm font-medium">
          {t("access.port.label")}
        </span>
        <div role="radiogroup" aria-labelledby={labelId} className="flex flex-wrap gap-1.5">
          {ports.map((port) => (
            <Button
              key={port}
              type="button"
              role="radio"
              aria-checked={value === port}
              size="sm"
              variant={value === port ? "default" : "outline"}
              className="tabular-nums"
              onClick={() => onChange(port)}
            >
              {port === main ? t("access.port.main", { port }) : port}
            </Button>
          ))}
        </div>
      </div>
      <p className="text-xs text-muted-foreground">{help}</p>
    </div>
  );
}


interface AccessBodyProps {
  access: AccessDetail;
  /** Reloads the team, and the access itself unless it is gone. */
  onChanged: (accessGone?: boolean) => Promise<unknown>;
  onClose: () => void;
}

function AccessBody({ access, onChanged, onClose }: AccessBodyProps) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [downloading, setDownloading] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<"regenerate" | "remove" | null>(null);
  // The port of the iTAK QR code and of the zips; the manual setup lists them all.
  const [port, setPort] = useState(access.server.port);
  const main = access.server.port;
  const alternatives = access.server.ports.filter((p) => p !== main);
  const picker = (help: string) =>
    alternatives.length > 0 && <PortPicker ports={access.server.ports} main={main} value={port} onChange={setPort} help={help} />;

  async function download(form: "enrollment" | "certificate") {
    setDownloading(form);
    try {
      const { name, blob } = await api.accessPackage(access.id, form, port);
      saveBlob(name, blob);
      await onChanged();
    } catch (e) {
      notifyError(e);
    } finally {
      setDownloading(null);
    }
  }

  async function regenerate() {
    try {
      await api.regenerateAccess(access.id);
      await onChanged();
      toast.success(t("access.regenerate.done"));
    } catch (e) {
      notifyError(e);
    }
  }

  async function remove() {
    try {
      const result = await api.deleteAccess(access.id);
      await onChanged(true);
      toast.success(result.synced ? t("access.remove.done") : t("access.remove.pending"));
      onClose();
    } catch (e) {
      notifyError(e);
    }
  }

  const expiry = access.certificates.map((c) => c.expires_at).filter((d): d is string => !!d).sort()[0];
  return (
    <>
      {access.kind === "shared" && (
        <Alert>
          <AlertTitle>{t("access.sharedNote.title")}</AlertTitle>
          <AlertDescription>{t("access.sharedNote.text")}</AlertDescription>
        </Alert>
      )}
      <Tabs defaultValue="qr">
        <TabsList className="w-full">
          <TabsTrigger value="qr">{t("access.tabs.qr")}</TabsTrigger>
          <TabsTrigger value="zip">{t("access.tabs.zip")}</TabsTrigger>
          <TabsTrigger value="manual">{t("access.tabs.manual")}</TabsTrigger>
        </TabsList>

        <TabsContent value="qr" className="space-y-3 pt-2">
          {picker(t("access.port.qrHelp", { port: main }))}
          <div className="grid gap-3 sm:grid-cols-2">
            <QrPanel
              value={access.qr.atak}
              title={t("access.qr.atakTitle")}
              help={port === ATAK_DEFAULT_PORT ? t("access.qr.atakHelp") : t("access.qr.atakOtherPort", { port, atakPort: ATAK_DEFAULT_PORT })}
            />
            <QrPanel value={access.qr.itak[String(port)]} title={t("access.qr.itakTitle")} help={t("access.qr.itakHelp")} />
          </div>
          <p className="text-xs text-muted-foreground">{t("access.qr.warning")}</p>
        </TabsContent>

        <TabsContent value="zip" className="space-y-3 pt-2">
          {picker(t("access.port.zipHelp", { port: main }))}
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="flex flex-col gap-2 rounded-lg border bg-card p-4">
              <p className="font-medium">{t("access.zip.certificateTitle")}</p>
              <p className="flex-1 text-xs text-muted-foreground">{t("access.zip.certificateHelp")}</p>
              <Button onClick={() => void download("certificate")} disabled={downloading !== null || access.team.status !== "active"}>
                <DownloadIcon />
                {downloading === "certificate" ? t("common.working") : t("access.zip.download")}
              </Button>
            </div>
            <div className="flex flex-col gap-2 rounded-lg border bg-card p-4">
              <p className="font-medium">{t("access.zip.enrollmentTitle")}</p>
              <p className="flex-1 text-xs text-muted-foreground">{t("access.zip.enrollmentHelp")}</p>
              <Button variant="outline" onClick={() => void download("enrollment")} disabled={downloading !== null}>
                <DownloadIcon />
                {downloading === "enrollment" ? t("common.working") : t("access.zip.download")}
              </Button>
            </div>
          </div>
          <p className="text-xs text-muted-foreground">{t("access.zip.note")}</p>
        </TabsContent>

        <TabsContent value="manual" className="pt-2">
          <dl className="rounded-lg border bg-card px-3 text-sm">
            <Field label={t("access.manual.host")} value={access.server.host} />
            <Field label={t("access.manual.port")} value={String(main)} />
            {alternatives.length > 0 && <Field label={t("access.manual.alternativePorts")} value={alternatives.join(", ")} />}
            <Field label={t("access.manual.enrollPort")} value={String(access.server.enroll_port)} />
            <Field label={t("access.manual.username")} value={access.username} />
            <Field label={t("access.manual.password")} value={access.password} secret />
          </dl>
          <p className="pt-2 text-xs text-muted-foreground">
            {t("access.manual.help")}
            {alternatives.length > 0 && <> {t("access.manual.alternativeHelp", { port: main })}</>}
          </p>
        </TabsContent>
      </Tabs>

      <div className="flex flex-wrap items-center justify-between gap-2 border-t pt-3 text-xs text-muted-foreground">
        <span>
          {access.certificates.length === 0
            ? t("access.certificates.none")
            : t("access.certificates.summary", {
                count: access.certificates.length,
                date: formatDate(expiry),
                relative: formatRelative(expiry),
              })}
        </span>
        {access.can_manage && (
          <span className="flex gap-2">
            <Button variant="outline" size="sm" onClick={() => setConfirm("regenerate")} disabled={access.team.status !== "active"}>
              <RefreshCwIcon />
              {t("access.regenerate.action")}
            </Button>
            <Button variant="destructive" size="sm" onClick={() => setConfirm("remove")}>
              <Trash2Icon />
              {t("access.remove.action")}
            </Button>
          </span>
        )}
      </div>

      <ConfirmDialog
        open={confirm === "regenerate"}
        onOpenChange={(open) => !open && setConfirm(null)}
        title={t("access.regenerate.title")}
        description={<p>{access.kind === "shared" ? t("access.regenerate.sharedText") : t("access.regenerate.text")}</p>}
        confirmLabel={t("access.regenerate.confirm")}
        destructive
        onConfirm={regenerate}
      />
      <ConfirmDialog
        open={confirm === "remove"}
        onOpenChange={(open) => !open && setConfirm(null)}
        title={t("access.remove.title")}
        description={<p>{t("access.remove.text")}</p>}
        confirmLabel={t("access.remove.confirm")}
        destructive
        onConfirm={remove}
      />
    </>
  );
}

/** Everything needed to connect a TAK client with one access: QR codes, zips, credentials. */
export function AccessDialog({ accessId, title, teamId, onClose }: { accessId: string | null; title: string; teamId: string; onClose: () => void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const query = useQuery({
    queryKey: ["access", accessId],
    queryFn: () => api.access(accessId!),
    enabled: accessId !== null,
    gcTime: 0, // holds a password: not kept once the dialog is closed
  });

  async function refresh(accessGone = false) {
    await queryClient.invalidateQueries({ queryKey: ["team", teamId] });
    if (!accessGone) await queryClient.invalidateQueries({ queryKey: ["access", accessId] });
  }

  return (
    <Dialog open={accessId !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-2xl" closeLabel={t("common.close")}>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{t("access.description")}</DialogDescription>
        </DialogHeader>
        {query.isPending && <p className="py-8 text-center text-sm text-muted-foreground">{t("common.loading")}</p>}
        {query.isError && (
          <Alert variant="destructive">
            <AlertDescription>{errorText(query.error)}</AlertDescription>
          </Alert>
        )}
        {query.data && <AccessBody access={query.data} onChanged={refresh} onClose={onClose} />}
      </DialogContent>
    </Dialog>
  );
}
