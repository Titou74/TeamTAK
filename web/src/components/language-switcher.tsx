import { useQueryClient } from "@tanstack/react-query";
import { CheckIcon, LanguagesIcon } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { setLanguage } from "@/i18n";
import { LANGUAGES, type LanguageCode } from "@/i18n/languages";
import { api } from "@/lib/api";
import type { Me } from "@/lib/types";

/** Switches the UI language; when signed in, the choice is saved on the account too, so
 *  e-mails follow it. */
export function LanguageSwitcher() {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();

  async function choose(code: LanguageCode) {
    setLanguage(code);
    const me = queryClient.getQueryData<Me | null>(["me"]);
    if (me && me.account.language !== code) {
      try {
        queryClient.setQueryData(["me"], await api.updateMe({ language: code }));
      } catch {
        // The UI already switched; the account keeps its previous e-mail language.
      }
    }
  }

  const current = LANGUAGES.find((l) => l.code === i18n.language) ?? LANGUAGES[0];
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="sm" aria-label={t("common.language")} />}>
        <LanguagesIcon />
        <span lang={current.code}>{current.label}</span>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-36">
        {LANGUAGES.map((language) => (
          <DropdownMenuItem key={language.code} onClick={() => void choose(language.code)}>
            <span lang={language.code} className="flex-1">
              {language.label}
            </span>
            {language.code === current.code && <CheckIcon />}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
