import { useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { PageHeader } from "@/components/app-shell";
import { LanguageSwitcher } from "@/components/language-switcher";
import { TermsLink } from "@/components/terms";
import { ThemePicker } from "@/components/theme-picker";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { api } from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { useNotifyError } from "@/lib/hooks";
import type { Me } from "@/lib/types";

/** The account's own settings: display name, language and colour theme. The language and the
 *  theme are saved as soon as they are picked; the button saves the display name. */
export function ProfilePage({ me }: { me: Me }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const notifyError = useNotifyError();
  const [displayName, setDisplayName] = useState(me.account.display_name);
  const [pending, setPending] = useState(false);

  async function save(event: FormEvent) {
    event.preventDefault();
    setPending(true);
    try {
      queryClient.setQueryData(["me"], await api.updateMe({ display_name: displayName }));
      toast.success(t("profile.saved"));
    } catch (e) {
      notifyError(e);
    } finally {
      setPending(false);
    }
  }

  return (
    <>
      <PageHeader title={t("profile.title")} description={t("profile.description")} />
      <Card className="max-w-lg">
        <CardContent>
          <form onSubmit={(e) => void save(e)} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="profile-email">{t("profile.email")}</Label>
              <Input id="profile-email" value={me.account.email} readOnly disabled />
            </div>
            <div className="space-y-2">
              <Label htmlFor="profile-name">{t("profile.displayName")}</Label>
              <Input id="profile-name" maxLength={80} value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
              <p className="text-xs text-muted-foreground">{t("profile.displayNameHelp")}</p>
            </div>
            <div className="space-y-2">
              <Label>{t("common.language")}</Label>
              <div>
                <LanguageSwitcher />
              </div>
              <p className="text-xs text-muted-foreground">{t("profile.languageHelp")}</p>
            </div>
            <div className="space-y-2">
              <Label>{t("profile.theme")}</Label>
              <div>
                <ThemePicker />
              </div>
              <p className="text-xs text-muted-foreground">{t("profile.themeHelp")}</p>
            </div>
            <Button type="submit" disabled={pending}>
              {pending ? t("common.working") : t("common.save")}
            </Button>
          </form>
        </CardContent>
      </Card>
      <p className="mt-4 max-w-lg text-sm text-muted-foreground">
        {t("termsOfUse.accepted", { date: formatDateTime(me.terms.accepted_at) })}{" "}
        <TermsLink className="font-medium text-foreground underline">{t("termsOfUse.read")}</TermsLink>
      </p>
    </>
  );
}
