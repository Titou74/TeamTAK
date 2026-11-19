import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ActivityIcon, LogOutIcon, ShieldCheckIcon, UserIcon, UsersIcon } from "lucide-react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Link, NavLink, useNavigate } from "react-router-dom";

import { LanguageSwitcher } from "@/components/language-switcher";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { api } from "@/lib/api";
import { useNotifyError } from "@/lib/hooks";
import type { Me } from "@/lib/types";
import { cn } from "@/lib/utils";

export function Brand() {
  const { t } = useTranslation();
  return (
    <Link to="/" className="flex items-center gap-2 font-semibold tracking-tight">
      <span className="grid size-7 place-items-center rounded-md bg-primary text-primary-foreground">
        <ShieldCheckIcon className="size-4" />
      </span>
      {t("app.name")}
    </Link>
  );
}

function NavItem({ to, children }: { to: string; children: ReactNode }) {
  return (
    <NavLink
      to={to}
      end={to === "/"}
      className={({ isActive }) =>
        cn(
          "flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-sm font-medium text-muted-foreground hover:text-foreground",
          isActive && "bg-muted text-foreground",
        )
      }
    >
      {children}
    </NavLink>
  );
}

/** Link to the monitoring, with the number of open alerts when there are some. */
function MonitoringNavItem() {
  const { t } = useTranslation();
  const query = useQuery({ queryKey: ["admin", "alerts-summary"], queryFn: api.alertsSummary, refetchInterval: 60_000, retry: false });
  const summary = query.data;
  const serious = summary ? summary.critical + summary.warning : 0;
  return (
    <NavItem to="/monitoring">
      <ActivityIcon className="size-4" />
      {t("nav.monitoring")}
      {summary && summary.open > 0 && (
        <span
          className={cn("rounded-full px-1.5 text-xs tabular-nums", serious > 0 ? "bg-destructive text-white" : "bg-muted-foreground/20 text-foreground")}
          aria-label={t("nav.openAlerts", { count: summary.open })}
        >
          {summary.open}
        </span>
      )}
    </NavItem>
  );
}

/** Page frame of the signed-in area: brand, navigation, language, account menu. */
export function AppShell({ me, children }: { me: Me; children: ReactNode }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const notifyError = useNotifyError();

  async function signOut() {
    try {
      await api.logout();
      navigate("/");
      // Back to the sign-in page, and nothing of the account stays in memory.
      queryClient.setQueryData(["me"], null);
      queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== "me" });
    } catch (e) {
      notifyError(e);
    }
  }

  return (
    <div className="min-h-svh">
      <header className="sticky top-0 z-40 border-b bg-background/90 backdrop-blur">
        <div className="mx-auto flex h-14 max-w-5xl items-center gap-4 px-4">
          <Brand />
          <nav className="flex flex-1 items-center gap-1" aria-label={t("nav.main")}>
            <NavItem to="/">
              <UsersIcon className="size-4" />
              {t("nav.teams")}
            </NavItem>
            {me.account.is_admin && (
              <NavItem to="/admin">
                <ShieldCheckIcon className="size-4" />
                {t("nav.admin")}
              </NavItem>
            )}
            {me.account.is_admin && <MonitoringNavItem />}
          </nav>
          <LanguageSwitcher />
          <DropdownMenu>
            <DropdownMenuTrigger render={<Button variant="outline" size="sm" aria-label={t("nav.menu")} />}>
              <UserIcon />
              <span className="hidden max-w-44 truncate sm:inline">{me.account.display_name || me.account.email}</span>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="min-w-48">
              <div className="truncate px-1.5 py-1 text-xs text-muted-foreground">{me.account.email}</div>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => navigate("/profile")}>
                <UserIcon />
                {t("nav.profile")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => void signOut()}>
                <LogOutIcon />
                {t("nav.signOut")}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </header>
      <main className="mx-auto max-w-5xl px-4 py-8">{children}</main>
    </div>
  );
}

/** Title block of a page. */
export function PageHeader({ title, description, actions }: { title: ReactNode; description?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="mb-6 flex flex-wrap items-start justify-between gap-3">
      <div className="space-y-1">
        <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
        {description && <p className="max-w-2xl text-sm text-muted-foreground">{description}</p>}
      </div>
      {actions && <div className="flex items-center gap-2">{actions}</div>}
    </div>
  );
}
