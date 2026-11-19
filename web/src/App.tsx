import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Navigate, Route, Routes } from "react-router-dom";

import { AppShell } from "@/components/app-shell";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { adoptAccountLanguage } from "@/i18n";
import { useErrorText, useMe } from "@/lib/hooks";
import { AdminPage } from "@/pages/admin";
import { HomePage } from "@/pages/home";
import { LoginPage } from "@/pages/login";
import { ProfilePage } from "@/pages/profile";
import { TeamPage } from "@/pages/team";

export function App() {
  const { t } = useTranslation();
  const me = useMe();
  const errorText = useErrorText();
  const language = me.data?.account.language;

  // The preference saved on the account wins over the browser's language.
  useEffect(() => {
    if (language) adoptAccountLanguage(language);
  }, [language]);

  useEffect(() => {
    document.title = t("app.name");
  }, [t]);

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
  return (
    <AppShell me={me.data}>
      <Routes>
        <Route path="/" element={<HomePage me={me.data} />} />
        <Route path="/teams/:id" element={<TeamPage me={me.data} />} />
        <Route path="/profile" element={<ProfilePage me={me.data} />} />
        <Route path="/admin" element={me.data.account.is_admin ? <AdminPage me={me.data} /> : <Navigate to="/" replace />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </AppShell>
  );
}
