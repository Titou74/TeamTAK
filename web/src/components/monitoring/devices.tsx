import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";

import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { api } from "@/lib/api";
import { formatDateTime, formatRelative } from "@/lib/format";
import type { Device, Devices } from "@/lib/monitoring-types";

import { QueryState } from "./shared";

/** Who an access is for, in words: the shared identity, a member, or a guest. */
function AccessLabel({ device }: { device: Device }) {
  const { t } = useTranslation();
  const access = device.access;
  if (!access) return <span className="text-muted-foreground">{t("devices.unknownAccess")}</span>;
  const who = access.kind === "shared" ? t("devices.sharedIdentity") : access.kind === "external" ? access.label : (access.owner ?? "");
  return (
    <>
      <span className="font-medium">{who}</span>
      <div className="font-mono text-xs text-muted-foreground" translate="no">
        {device.username}
      </div>
    </>
  );
}

/** The list itself. `admin` adds the team and the address the device connects from. */
export function DevicesTable({ devices, admin }: { devices: Device[]; admin: boolean }) {
  const { t } = useTranslation();
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>{t("devices.columns.device")}</TableHead>
          <TableHead>{t("devices.columns.access")}</TableHead>
          {admin && <TableHead>{t("devices.columns.team")}</TableHead>}
          <TableHead>{t("devices.columns.channels")}</TableHead>
          <TableHead>{t("devices.columns.since")}</TableHead>
          {admin && <TableHead>{t("devices.columns.address")}</TableHead>}
        </TableRow>
      </TableHeader>
      <TableBody>
        {devices.map((device) => (
          <TableRow key={device.id}>
            <TableCell>
              <span className="font-medium" translate="no">
                {device.callsign ?? t("devices.noCallsign")}
              </span>
              {device.silent && (
                <Badge variant="outline" className="ml-2">
                  {t("devices.silent")}
                </Badge>
              )}
              <div className="text-xs text-muted-foreground">
                {device.client ?? t("devices.unknownClient")}
                {device.last_report_at && <> · {t("devices.lastReport", { when: formatRelative(device.last_report_at) })}</>}
              </div>
            </TableCell>
            <TableCell>
              <AccessLabel device={device} />
            </TableCell>
            {admin && (
              <TableCell>
                {device.access ? (
                  <Link to={`/teams/${device.access.team_id}`} className="hover:underline">
                    {device.access.team_name}
                  </Link>
                ) : (
                  ""
                )}
              </TableCell>
            )}
            <TableCell className="font-mono text-xs whitespace-normal" translate="no">
              {device.groups.join(", ")}
            </TableCell>
            <TableCell className="text-muted-foreground" title={formatDateTime(device.connected_since)}>
              {device.since_is_lower_bound
                ? t("devices.sinceAtLeast", { when: formatRelative(device.connected_since) })
                : formatRelative(device.connected_since)}
            </TableCell>
            {admin && (
              <TableCell className="font-mono text-xs" translate="no">
                {device.ip_address ?? ""}
              </TableCell>
            )}
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

function AsOf({ data }: { data: Devices }) {
  const { t } = useTranslation();
  return <>{data.as_of ? t("devices.asOf", { when: formatRelative(data.as_of) }) : t("devices.notYet")}</>;
}

/** Administration: every device connected to TAK Server. */
export function DevicesTab() {
  const { t } = useTranslation();
  const query = useQuery({ queryKey: ["admin", "devices"], queryFn: api.adminDevices, refetchInterval: 15_000 });
  if (!query.data) return <QueryState pending={query.isPending} error={query.error} />;
  const { devices, unidentified = [] } = query.data;
  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>{t("devices.title", { count: devices.length })}</CardTitle>
          <CardDescription>
            {t("devices.adminDescription")} <AsOf data={query.data} />
          </CardDescription>
        </CardHeader>
        <CardContent>
          {devices.length === 0 ? <p className="text-sm text-muted-foreground">{t("devices.none")}</p> : <DevicesTable devices={devices} admin />}
        </CardContent>
      </Card>
      {unidentified.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle>{t("devices.unidentifiedTitle", { count: unidentified.length })}</CardTitle>
            <CardDescription>{t("devices.unidentifiedHelp")}</CardDescription>
          </CardHeader>
          <CardContent>
            <DevicesTable devices={unidentified} admin />
          </CardContent>
        </Card>
      )}
    </div>
  );
}

/** A team's page: the devices connected with the team's accesses, for its leaders. */
export function TeamDevicesSection({ teamId, shared }: { teamId: string; shared: boolean }) {
  const { t } = useTranslation();
  const query = useQuery({ queryKey: ["team", teamId, "devices"], queryFn: () => api.teamDevices(teamId), refetchInterval: 15_000 });
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("devices.title", { count: query.data?.devices.length ?? 0 })}</CardTitle>
        <CardDescription>
          {shared ? t("devices.teamDescriptionShared") : t("devices.teamDescription")} {query.data && <AsOf data={query.data} />}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {!query.data ? (
          <QueryState pending={query.isPending} error={query.error} />
        ) : query.data.devices.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("devices.noneInTeam")}</p>
        ) : (
          <DevicesTable devices={query.data.devices} admin={false} />
        )}
      </CardContent>
    </Card>
  );
}
