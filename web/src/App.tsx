import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Navigate, Route, Routes } from "react-router-dom";

import { AppShell } from "@/components/app-shell";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { adoptAccountLanguage } from "@/i18n";
import { useErrorText, useMe, usePublicConfig } from "@/lib/hooks";
import { applyTheme, isTheme } from "@/lib/theme";
import { AdminPage } from "@/pages/admin";
import { HomePage } from "@/pages/home";
import { LoginPage } from "@/pages/login";
import { MonitoringPage } from "@/pages/monitoring";
import { ProfilePage } from "@/pages/profile";
import { TeamPage } from "@/pages/team";
import { TermsGate } from "@/pages/terms-gate";

export function App() {
  const { t } = useTranslation();
  const me = useMe();
  const serverName = usePublicConfig().data?.server_name;
  const errorText = useErrorText();
  const language = me.data?.account.language;
  // Undefined while the account loads: the theme remembered on this device stays until then.
  const theme = me.data === undefined ? undefined : (me.data?.account.theme ?? "auto");

  // The preference saved on the account wins over the browser's language.
  useEffect(() => {
    if (language) adoptAccountLanguage(language);
  }, [language]);

  // The theme saved on the account; signed out, the device's setting.
  useEffect(() => {
    if (theme !== undefined) applyTheme(isTheme(theme) ? theme : "auto");
  }, [theme]);

  // The server's name in the tab, to tell apart several servers open at once.
  useEffect(() => {
    document.title = serverName ? `${t("app.name")} · ${serverName}` : t("app.name");
  }, [t, serverName]);

  if (me.isPending) {
    return <p className="p-8 text-center text-sm text-muted-foreground">{t("common.loading")}</p>;
  }
  if (me.isError) {
    return (
      <div className="mx-auto max-w-md p-8">
        <Alert variant="destructive">
          <AlertDescription>{errorText(me.error)}</AlertDescription>
        </Alert>
      </div>
    );
  }
  if (!me.data) {
    return <LoginPage />;
  }
  // Signed in before the terms of use existed, or before they changed.
  if (!me.data.terms.accepted_at) {
    return <TermsGate />;
  }
  return (
    <AppShell me={me.data}>
      <Routes>
        <Route path="/" element={<HomePage me={me.data} />} />
        <Route path="/teams/:id" element={<TeamPage me={me.data} />} />
        <Route path="/profile" element={<ProfilePage me={me.data} />} />
        <Route path="/admin" element={me.data.account.is_admin ? <AdminPage me={me.data} /> : <Navigate to="/" replace />} />
        <Route path="/monitoring" element={me.data.account.is_admin ? <MonitoringPage /> : <Navigate to="/" replace />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </AppShell>
  );
}
