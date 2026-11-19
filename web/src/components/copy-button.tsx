import { CheckIcon, CopyIcon } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";

/** Copies a value to the clipboard. `label` names what is copied, for screen readers. */
export function CopyButton({ value, label }: { value: string; label: string }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard refused (insecure origin): the value stays selectable on screen.
    }
  }

  return (
    <Button variant="outline" size="icon-sm" onClick={() => void copy()} aria-label={t("common.copyThing", { thing: label })}>
      {copied ? <CheckIcon /> : <CopyIcon />}
      <span className="sr-only" aria-live="polite">
        {copied ? t("common.copied") : ""}
      </span>
    </Button>
  );
}
