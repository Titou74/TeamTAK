import { useTranslation } from "react-i18next";

import { Badge } from "@/components/ui/badge";
import type { AccessKind, TeamRole, TeamStatus } from "@/lib/types";

export function StatusBadge({ status }: { status: TeamStatus }) {
  const { t } = useTranslation();
  const variant = status === "active" ? "default" : status === "pending" ? "secondary" : status === "rejected" ? "destructive" : "outline";
  return <Badge variant={variant}>{t(`status.${status}`)}</Badge>;
}

export function RoleBadge({ role }: { role: TeamRole }) {
  const { t } = useTranslation();
  return <Badge variant={role === "leader" ? "default" : "secondary"}>{t(`role.${role}`)}</Badge>;
}

export function ModeBadge({ mode }: { mode: AccessKind }) {
  const { t } = useTranslation();
  return <Badge variant="outline">{t(`mode.${mode}.name`)}</Badge>;
}
