import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, ApiError } from "./api";
import type { Me } from "./types";

/** The signed-in account, or `null` when nobody is signed in. */
export function useMe() {
  return useQuery<Me | null>({
    queryKey: ["me"],
    queryFn: async () => {
      try {
        return await api.me();
      } catch (e) {
        if (e instanceof ApiError && e.status === 401) return null;
        throw e;
      }
    },
    staleTime: 30_000,
  });
}

/** Turns an API error code into a sentence in the current language. */
export function useErrorText() {
  const { t, i18n } = useTranslation();
  return useCallback(
    (error: unknown): string => {
      const code = error instanceof ApiError ? error.code : "unknown";
      const key = `errors.${code}`;
      return i18n.exists(key) ? t(key) : t("errors.unknown");
    },
    [t, i18n],
  );
}

/** Shows an API error as a toast; a lost session sends the user back to the sign-in page. */
export function useNotifyError() {
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  return useCallback(
    (error: unknown) => {
      if (error instanceof ApiError && error.status === 401) {
        queryClient.setQueryData(["me"], null);
      }
      toast.error(errorText(error));
    },
    [errorText, queryClient],
  );
}
