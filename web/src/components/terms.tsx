import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { formatDay } from "@/lib/format";
import { termsIn } from "@/lib/terms";
import { cn } from "@/lib/utils";

/** The terms of use in the UI's language, with the date of their version. */
export function TermsText({ className }: { className?: string }) {
  const { t, i18n } = useTranslation();
  const terms = termsIn(i18n.language);
  return (
    <article className={cn("space-y-5 text-sm leading-relaxed", className)}>
      <p className="text-xs text-muted-foreground">{t("termsOfUse.version", { date: formatDay(terms.version) })}</p>
      <p>{terms.intro}</p>
      {terms.sections.map((section, i) => (
        <section key={i} className="space-y-2">
          <h3 className="font-semibold">{`${i + 1}. ${section.title}`}</h3>
          {section.body.map((block, j) =>
            typeof block === "string" ? (
              <p key={j}>{block}</p>
            ) : (
              <ul key={j} className="list-disc space-y-1 pl-5">
                {block.items.map((item, k) => (
                  <li key={k}>{item}</li>
                ))}
              </ul>
            ),
          )}
        </section>
      ))}
    </article>
  );
}

/** A link that opens the terms of use over the page, signed in or not. */
export function TermsLink({ id, className, children }: { id?: string; className?: string; children: ReactNode }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  return (
    <>
      <button id={id} type="button" className={cn("cursor-pointer underline-offset-4 hover:underline", className)} onClick={() => setOpen(true)}>
        {children}
      </button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="max-h-[88svh] overflow-y-auto sm:max-w-2xl" closeLabel={t("common.close")}>
          <DialogHeader>
            <DialogTitle className="text-lg">{t("termsOfUse.title")}</DialogTitle>
          </DialogHeader>
          <TermsText />
        </DialogContent>
      </Dialog>
    </>
  );
}
