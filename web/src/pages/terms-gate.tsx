import { useQueryClient } from "@tanstack/react-query";
import { CheckIcon, LogOutIcon } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Brand } from "@/components/app-shell";
import { LanguageSwitcher } from "@/components/language-switcher";
import { SiteFooter } from "@/components/site-footer";
import { TermsText } from "@/components/terms";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { api } from "@/lib/api";
import { useNotifyError } from "@/lib/hooks";
import { TERMS_VERSION } from "@/lib/terms";

/**
 * Shown instead of the platform to a signed-in account that has not accepted the terms of use
 * in force: one signed in before they existed, or before they changed. Nothing else is
 * reachable until it accepts them or signs out.
 */
export function TermsGate() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const notifyError = useNotifyError();
  const [pending, setPending] = useState(false);

  async function accept() {
    setPending(true);
    try {
      queryClient.setQueryData(["me"], await api.acceptTerms(TERMS_VERSION));
    } catch (e) {
      notifyError(e);
    } finally {
      setPending(false);
    }
  }

  async function signOut() {
    try {
      await api.logout();
      queryClient.setQueryData(["me"], null);
      queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== "me" });
    } catch (e) {
      notifyError(e);
    }
  }

  return (
    <div className="flex min-h-svh flex-col">
      <header className="mx-auto flex h-14 w-full max-w-5xl items-center justify-between px-4">
        <Brand />
        <LanguageSwitcher />
      </header>
      <main className="flex flex-1 flex-col items-center justify-center px-4 pb-8">
        <Card className="w-full max-w-2xl">
          <CardHeader>
            <CardTitle className="text-xl">{t("termsOfUse.title")}</CardTitle>
            <CardDescription>{t("termsOfUse.gateIntro")}</CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <TermsText className="max-h-[55svh] overflow-y-auto rounded-lg border bg-muted/30 p-4" />
            <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
              <Button type="button" variant="outline" onClick={() => void signOut()}>
                <LogOutIcon />
                {t("nav.signOut")}
              </Button>
              <Button type="button" disabled={pending} onClick={() => void accept()}>
                <CheckIcon />
                {pending ? t("common.working") : t("termsOfUse.gateAccept")}
              </Button>
            </div>
          </CardContent>
        </Card>
      </main>
      <SiteFooter />
    </div>
  );
}
