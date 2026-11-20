import { useQueryClient } from "@tanstack/react-query";
import { MonitorIcon, MoonIcon, SunIcon } from "lucide-react";
import { useTranslation } from "react-i18next";

import { api } from "@/lib/api";
import { useNotifyError } from "@/lib/hooks";
import { applyTheme, type Theme, useTheme } from "@/lib/theme";
import { cn } from "@/lib/utils";

const OPTIONS: { theme: Theme; icon: typeof SunIcon }[] = [
  { theme: "auto", icon: MonitorIcon },
  { theme: "light", icon: SunIcon },
  { theme: "dark", icon: MoonIcon },
];

/** Auto, light or dark: applied at once and saved on the account. */
export function ThemePicker() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const notifyError = useNotifyError();
  const current = useTheme();

  async function choose(theme: Theme) {
    if (theme === current) return;
    const previous = current;
    applyTheme(theme);
    try {
      queryClient.setQueryData(["me"], await api.updateMe({ theme }));
    } catch (e) {
      applyTheme(previous);
      notifyError(e);
    }
  }

  return (
    <div role="radiogroup" aria-label={t("profile.theme")} className="inline-flex gap-0.5 rounded-lg border bg-muted p-0.5">
      {OPTIONS.map(({ theme, icon: Icon }) => (
        <button
          key={theme}
          type="button"
          role="radio"
          aria-checked={theme === current}
          onClick={() => void choose(theme)}
          className={cn(
            "flex items-center gap-1.5 rounded-md px-3 py-1 text-sm font-medium text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50",
            theme === current && "bg-background text-foreground shadow-sm dark:bg-input/40",
          )}
        >
          <Icon className="size-4" />
          {t(`profile.themes.${theme}`)}
        </button>
      ))}
    </div>
  );
}
