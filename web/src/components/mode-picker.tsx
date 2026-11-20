import { KeyRoundIcon, UsersIcon } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { TeamMode } from "@/lib/types";
import { cn } from "@/lib/utils";

const MODES: { mode: TeamMode; icon: typeof UsersIcon }[] = [
  { mode: "shared", icon: UsersIcon },
  { mode: "individual", icon: KeyRoundIcon },
];

/** The two ways a team connects to TAK Server, with what each one implies. */
export function ModePicker({ value, onChange, disabled }: { value: TeamMode; onChange: (mode: TeamMode) => void; disabled?: boolean }) {
  const { t } = useTranslation();
  return (
    <div role="radiogroup" aria-label={t("mode.label")} className="grid gap-2 sm:grid-cols-2">
      {MODES.map(({ mode, icon: Icon }) => (
        <button
          key={mode}
          type="button"
          role="radio"
          aria-checked={value === mode}
          disabled={disabled}
          onClick={() => onChange(mode)}
          className={cn(
            "flex flex-col gap-1.5 rounded-lg border bg-card p-3 text-left text-sm outline-none transition-colors hover:border-primary/50 focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-60",
            value === mode && "border-primary ring-1 ring-primary",
          )}
        >
          <span className="flex items-center gap-2 font-medium">
            <Icon className="size-4 text-primary" />
            {t(`mode.${mode}.name`)}
          </span>
          <span className="text-xs text-muted-foreground">{t(`mode.${mode}.description`)}</span>
        </button>
      ))}
    </div>
  );
}
