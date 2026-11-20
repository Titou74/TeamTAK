import { useTranslation } from "react-i18next";

import { LICENCE_URL, SOURCE_CODE_URL, TEAMTAK_URL } from "@/lib/project";

const linkClass = "underline-offset-4 hover:text-foreground hover:underline";

/** Footer of every page: the attribution and the source code link the licence asks for (NOTICE). */
export function SiteFooter() {
  const { t } = useTranslation();
  return (
    <footer className="mx-auto flex w-full max-w-5xl flex-wrap items-center justify-center gap-x-2 gap-y-1 px-4 py-6 text-xs text-muted-foreground">
      <a href={TEAMTAK_URL} target="_blank" rel="noopener" className={linkClass}>
        {t("footer.poweredBy")}
      </a>
      <span aria-hidden="true">·</span>
      <a href={LICENCE_URL} target="_blank" rel="noopener" className={linkClass}>
        {t("footer.licence")}
      </a>
      <span aria-hidden="true">·</span>
      <a href={SOURCE_CODE_URL} target="_blank" rel="noopener" className={linkClass}>
        {t("footer.source")}
      </a>
    </footer>
  );
}
