"use client";

import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { useCodexProfileModeStatus } from "@/hooks/useCodexProfileModeStatus";
import {
  CODEX_PROFILE_CANDIDATES_QUERY_KEY,
  CODEX_PROFILE_STATUS_QUERY_KEY,
  codexProfileClient,
} from "@/lib/api/codex-profile-client";
import { getAppErrorMessage } from "@/lib/api/transport";
import { useI18n } from "@/lib/i18n/provider";
import type { CodexProfileHistoryRepairSummary } from "@/types";

interface UseAccountCodexProfileSwitchOptions {
  enabled?: boolean;
}

export interface AccountCodexProfileSwitchState {
  activeAccountId: string | null;
  authPath: string;
  candidateAccountIds: ReadonlySet<string>;
  codexHome: string;
  isLoading: boolean;
  isProfileWritable: boolean;
  isServiceReady: boolean;
  switchingAccountId: string | null;
  switchAccount: (accountId: string) => void;
}

function historyRepairChangeCount(
  summary: CodexProfileHistoryRepairSummary,
): number {
  return (
    summary.changedRolloutFileCount +
    summary.updatedSqliteRowCount +
    summary.addedSessionIndexEntryCount
  );
}

export function useAccountCodexProfileSwitch(
  options: UseAccountCodexProfileSwitchOptions = {},
): AccountCodexProfileSwitchState {
  const { t } = useI18n();
  const queryClient = useQueryClient();
  const enabled = options.enabled ?? true;
  const statusQuery = useCodexProfileModeStatus({
    enabled,
    refetchIntervalMs: enabled ? 5_000 : false,
  });
  const candidatesQuery = useQuery({
    queryKey: CODEX_PROFILE_CANDIDATES_QUERY_KEY,
    queryFn: () => codexProfileClient.listCandidates(),
    enabled: enabled && statusQuery.isServiceReady,
    retry: 1,
    staleTime: 5_000,
    refetchInterval:
      enabled && statusQuery.isServiceReady ? 5_000 : false,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
  const candidateAccountIds = useMemo(
    () => new Set((candidatesQuery.data?.accounts || []).map((item) => item.id)),
    [candidatesQuery.data?.accounts],
  );

  const refreshProfileCaches = async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: CODEX_PROFILE_STATUS_QUERY_KEY }),
      queryClient.invalidateQueries({
        queryKey: CODEX_PROFILE_CANDIDATES_QUERY_KEY,
      }),
    ]);
  };

  const showHistoryRepairToast = (
    summary: CodexProfileHistoryRepairSummary | null,
  ) => {
    if (!summary) return;
    if (summary.warnings.length > 0) {
      toast.warning(`${t("历史修复完成但有警告")}：${summary.warnings[0]}`);
      return;
    }
    if (historyRepairChangeCount(summary) > 0) {
      toast.success(t("历史会话可见性已修复"));
    }
  };

  const applyDirectAccountMutation = useMutation({
    mutationFn: (accountId: string) => {
      const normalizedAccountId = accountId.trim();
      if (!normalizedAccountId || !candidateAccountIds.has(normalizedAccountId)) {
        throw new Error(t("该账号当前不可用于 Codex 直连"));
      }
      return codexProfileClient.applyDirectAccount({
        accountId: normalizedAccountId,
        codexHome: statusQuery.status?.codexHome || null,
      });
    },
    onSuccess: async (nextStatus) => {
      queryClient.setQueryData(CODEX_PROFILE_STATUS_QUERY_KEY, nextStatus);
      await refreshProfileCaches();
      toast.success(t("已切换到账号直连"));
      showHistoryRepairToast(nextStatus.historyRepair);
    },
    onError: (error: unknown) => {
      toast.error(`${t("切换失败")}: ${getAppErrorMessage(error)}`);
    },
  });

  const status = statusQuery.status;
  const hasValidActiveStatus =
    statusQuery.isServiceReady && statusQuery.isSuccess;
  return {
    activeAccountId:
      hasValidActiveStatus && status?.mode === "direct_account"
        ? status.selectedAccountId
        : null,
    authPath: status?.authPath || "",
    candidateAccountIds,
    codexHome: status?.codexHome || "",
    isLoading:
      enabled && (statusQuery.isLoading || candidatesQuery.isLoading),
    isProfileWritable: Boolean(status?.profileWritable),
    isServiceReady: statusQuery.isServiceReady,
    switchingAccountId:
      applyDirectAccountMutation.isPending &&
      typeof applyDirectAccountMutation.variables === "string"
        ? applyDirectAccountMutation.variables
        : null,
    switchAccount: (accountId: string) => {
      if (!statusQuery.isServiceReady) {
        toast.info(t("服务未连接，暂时无法切换 Codex 账号"));
        return;
      }
      applyDirectAccountMutation.mutate(accountId);
    },
  };
}
