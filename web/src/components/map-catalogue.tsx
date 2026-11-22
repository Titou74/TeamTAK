import { useQuery, useQueryClient } from "@tanstack/react-query";
import { PlusIcon, Trash2Icon, TriangleAlertIcon, UploadIcon } from "lucide-react";
import { useRef, useState, type ChangeEvent } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ConfirmDialog } from "@/components/confirm-dialog";
import { CheckAgainButton, MapRow, MapTermsBadge, uploadFiles, UploadResults } from "@/components/maps-section";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { api } from "@/lib/api";
import { useErrorText, useNotifyError } from "@/lib/hooks";
import type { AdminMapSource, MapUploadResult } from "@/lib/types";

/** Administration: the map sources the teams and events pick their maps from. */
export function MapCatalogueTab() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const notifyError = useNotifyError();
  const input = useRef<HTMLInputElement>(null);
  const [uploading, setUploading] = useState(false);
  const [results, setResults] = useState<MapUploadResult[]>([]);
  const [removing, setRemoving] = useState<AdminMapSource | null>(null);
  const query = useQuery({ queryKey: ["admin", "map-sources"], queryFn: api.mapCatalogue });
  const reload = () => Promise.all([queryClient.invalidateQueries({ queryKey: ["admin", "map-sources"] }), queryClient.invalidateQueries({ queryKey: ["maps"] })]);

  if (query.isPending) return <p className="text-sm text-muted-foreground">{t("common.loading")}</p>;
  if (query.isError) {
    return (
      <Alert variant="destructive">
        <AlertDescription>{errorText(query.error)}</AlertDescription>
      </Alert>
    );
  }
  const { sources, suggestions, limits } = query.data;
  const missing = suggestions.filter((s) => !s.present);

  async function upload(event: ChangeEvent<HTMLInputElement>) {
    const files = [...(event.target.files ?? [])];
    event.target.value = "";
    if (files.length === 0) return;
    setUploading(true);
    try {
      const done = await uploadFiles(files, limits.upload_bytes, api.uploadMapSource);
      setResults(done.results);
      const stored = done.results.filter((r) => r.outcome === "added" || r.outcome === "updated").length;
      if (stored > 0) toast.success(t("maps.catalogue.uploaded", { count: stored }));
      if (!done.synced) toast.warning(t("maps.choose.pendingSync"));
    } catch (e) {
      notifyError(e);
    } finally {
      setUploading(false);
      await reload();
    }
  }

  async function addSuggestion(key: string) {
    try {
      const answer = await api.addMapSuggestions([key]);
      setResults(answer.results);
      await reload();
    } catch (e) {
      notifyError(e);
    }
  }

  async function remove(source: AdminMapSource) {
    try {
      const answer = await api.removeMapSource(source.id);
      await reload();
      if (answer.synced) toast.success(t("maps.catalogue.removed", { name: source.name }));
      else toast.warning(t("maps.choose.pendingSync"));
    } catch (e) {
      notifyError(e);
    }
  }

  return (
    <div className="space-y-4">
      <Card>
        <CardHeader>
          <CardTitle>{t("maps.catalogue.title", { count: sources.length })}</CardTitle>
          <CardDescription>{t("maps.catalogue.description")}</CardDescription>
          <CardAction>
            <input
              ref={input}
              type="file"
              multiple
              accept=".xml,.zip,text/xml,application/xml,application/zip"
              className="hidden"
              onChange={(e) => void upload(e)}
            />
            <Button variant="outline" onClick={() => input.current?.click()} disabled={uploading}>
              <UploadIcon />
              {uploading ? t("maps.addSource.checking") : t("maps.catalogue.upload")}
            </Button>
          </CardAction>
        </CardHeader>
        <CardContent className="space-y-4">
          <p className="text-xs text-muted-foreground">{t("maps.catalogue.uploadHelp", { max: limits.files })}</p>
          {results.length > 0 && <UploadResults results={results} />}
          {sources.some((s) => s.terms === "restricted") && (
            <Alert>
              <TriangleAlertIcon />
              <AlertTitle>{t("maps.catalogue.restrictedTitle")}</AlertTitle>
              <AlertDescription>{t("maps.catalogue.restrictedText")}</AlertDescription>
            </Alert>
          )}
          {sources.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t("maps.catalogue.none")}</p>
          ) : (
            <ul className="divide-y">
              {sources.map((source) => (
                <MapRow key={source.id} source={source} showOwner>
                  <span className="text-xs text-muted-foreground">
                    {t("maps.catalogue.uses", { teams: source.teams, events: source.events })}
                  </span>
                  <CheckAgainButton source={source} onChecked={reload} />
                  <Button variant="ghost" size="icon-sm" aria-label={t("maps.catalogue.remove", { name: source.name })} onClick={() => setRemoving(source)}>
                    <Trash2Icon />
                  </Button>
                </MapRow>
              ))}
            </ul>
          )}
        </CardContent>
      </Card>

      {missing.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle>{t("maps.suggestions.title")}</CardTitle>
            <CardDescription>{t("maps.suggestions.description")}</CardDescription>
          </CardHeader>
          <CardContent>
            <ul className="divide-y">
              {missing.map((suggestion) => (
                <li key={suggestion.key} className="flex flex-wrap items-center gap-3 py-2.5">
                  <div className="min-w-0 flex-1 space-y-0.5">
                    <p className="text-sm font-medium">{suggestion.name}</p>
                    <p className="text-xs text-muted-foreground">{t(`maps.suggestion.${suggestion.key}`)}</p>
                  </div>
                  <MapTermsBadge terms={suggestion.terms} />
                  <Button variant="outline" size="sm" onClick={() => void addSuggestion(suggestion.key)}>
                    <PlusIcon />
                    {t("maps.suggestions.add")}
                  </Button>
                </li>
              ))}
            </ul>
          </CardContent>
        </Card>
      )}

      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t("maps.catalogue.removeTitle", { name: removing?.name ?? "" })}
        description={<p>{t("maps.catalogue.removeText", { teams: removing?.teams ?? 0, events: removing?.events ?? 0 })}</p>}
        confirmLabel={t("maps.catalogue.removeConfirm")}
        destructive
        onConfirm={() => (removing ? remove(removing) : Promise.resolve())}
      />
    </div>
  );
}
