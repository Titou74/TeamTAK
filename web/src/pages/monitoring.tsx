import { useQuery, useQueryClient } from "@tanstack/react-query";
import { RefreshCwIcon } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useSearchParams } from "react-router-dom";

import { PageHeader } from "@/components/app-shell";
import { AuditTab } from "@/components/monitoring/audit";
import { BackupsTab } from "@/components/monitoring/backups";
import { DevicesTab } from "@/components/monitoring/devices";
import { OverviewTab } from "@/components/monitoring/overview";
import { SettingsTab } from "@/components/monitoring/settings";
import { QueryState } from "@/components/monitoring/shared";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { api } from "@/lib/api";
import { formatRelative } from "@/lib/format";
import { useNotifyError } from "@/lib/hooks";

const TABS = ["overview", "devices", "backups", "audit", "settings"] as const;

/**
 * Monitoring, for administrators: the server, the services, TAK Server, certificates,
 * backups, the audit log and their settings. Everything shown is what the platform last saw
 * by itself; "Look now" makes it look again.
 */
export function MonitoringPage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const notifyError = useNotifyError();
  const [looking, setLooking] = useState(false);
  const query = useQuery({ queryKey: ["admin", "monitoring"], queryFn: () => api.monitoring(), refetchInterval: 15_000 });
  // `?tab=settings` opens a tab directly: the administration links to the settings.
  const [params] = useSearchParams();
  const tab = TABS.find((name) => name === params.get("tab")) ?? "overview";

  async function lookNow() {
    setLooking(true);
    try {
      queryClient.setQueryData(["admin", "monitoring"], await api.monitoring(true));
      await queryClient.invalidateQueries({ queryKey: ["admin", "alerts-summary"] });
    } catch (e) {
      notifyError(e);
    } finally {
      setLooking(false);
    }
  }

  const collectedAt = query.data?.snapshot?.collected_at;
  return (
    <>
      <PageHeader
        title={t("monitoring.title")}
        description={t("monitoring.description")}
        actions={
          <>
            {collectedAt && <span className="text-xs text-muted-foreground">{t("monitoring.lastLook", { when: formatRelative(collectedAt) })}</span>}
            <Button variant="outline" size="sm" disabled={looking} onClick={() => void lookNow()}>
              <RefreshCwIcon className={looking ? "animate-spin" : ""} />
              {looking ? t("common.working") : t("monitoring.lookNow")}
            </Button>
          </>
        }
      />
      <Tabs key={tab} defaultValue={tab}>
        <TabsList>
          <TabsTrigger value="overview">{t("monitoring.tabs.overview")}</TabsTrigger>
          <TabsTrigger value="devices">{t("monitoring.tabs.devices")}</TabsTrigger>
          <TabsTrigger value="backups">{t("monitoring.tabs.backups")}</TabsTrigger>
          <TabsTrigger value="audit">{t("monitoring.tabs.audit")}</TabsTrigger>
          <TabsTrigger value="settings">{t("monitoring.tabs.settings")}</TabsTrigger>
        </TabsList>
        <TabsContent value="overview" className="pt-4">
          {query.data ? <OverviewTab data={query.data} /> : <QueryState pending={query.isPending} error={query.error} />}
        </TabsContent>
        <TabsContent value="devices" className="pt-4">
          <DevicesTab />
        </TabsContent>
        <TabsContent value="backups" className="pt-4">
          {query.data ? <BackupsTab data={query.data} /> : <QueryState pending={query.isPending} error={query.error} />}
        </TabsContent>
        <TabsContent value="audit" className="pt-4">
          <AuditTab />
        </TabsContent>
        <TabsContent value="settings" className="pt-4">
          <SettingsTab />
        </TabsContent>
      </Tabs>
    </>
  );
}
