"use client";

import { CheckCircle2, Loader2, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import type { AccountCodexProfileSwitchState } from "@/hooks/useAccountCodexProfileSwitch";
import { useI18n } from "@/lib/i18n/provider";

interface AccountCodexProfileCellProps {
  accountId: string;
  state: AccountCodexProfileSwitchState;
}

export function AccountCodexProfileCell({
  accountId,
  state,
}: AccountCodexProfileCellProps) {
  const { t } = useI18n();
  const isActive = state.activeAccountId === accountId;
  const isSwitching = state.switchingAccountId === accountId;
  const isAnyAccountSwitching = Boolean(state.switchingAccountId);
  const isCandidate = state.candidateAccountIds.has(accountId);
  const isDisabled =
    !state.isServiceReady ||
    state.isLoading ||
    !state.isProfileWritable ||
    !isCandidate ||
    isAnyAccountSwitching;

  let buttonTitle = t("切换 Codex profile 到此账号");
  if (!state.isServiceReady) {
    buttonTitle = t("服务未连接，暂时无法切换 Codex 账号");
  } else if (state.isLoading) {
    buttonTitle = t("正在读取可用账号...");
  } else if (!state.isProfileWritable) {
    buttonTitle = t("Codex profile 当前不可写");
  } else if (!isCandidate) {
    buttonTitle = t("该账号当前不可用于 Codex 直连");
  } else if (isAnyAccountSwitching) {
    buttonTitle = t("正在切换 Codex 运行账号");
  }

  if (isActive) {
    return (
      <Tooltip>
        <TooltipTrigger render={<span />} className="inline-flex">
          <span className="inline-flex h-7 items-center gap-1.5 rounded-md border border-green-500/30 bg-green-500/10 px-2 text-xs font-medium text-green-700 dark:text-green-300">
            <CheckCircle2 className="h-3.5 w-3.5" />
            {t("运行中")}
          </span>
        </TooltipTrigger>
        <TooltipContent className="max-w-xs">
          <div className="space-y-1">
            <div>{t("当前服务的 Codex profile 正在使用此账号")}</div>
            {state.authPath ? (
              <div className="break-all font-mono text-[10px] opacity-80">
                {state.authPath}
              </div>
            ) : state.codexHome ? (
              <div className="break-all font-mono text-[10px] opacity-80">
                {state.codexHome}
              </div>
            ) : null}
          </div>
        </TooltipContent>
      </Tooltip>
    );
  }

  return (
    <Button
      variant="outline"
      size="sm"
      className="h-7 px-2 text-xs"
      disabled={isDisabled}
      onClick={() => state.switchAccount(accountId)}
      title={buttonTitle}
      aria-label={buttonTitle}
    >
      {isSwitching ? (
        <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
      ) : (
        <RefreshCw className="mr-1.5 h-3.5 w-3.5" />
      )}
      {t("切换")}
    </Button>
  );
}
