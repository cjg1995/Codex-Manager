import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";

const appsRoot = path.resolve(import.meta.dirname, "..");

async function readSource(relativePath) {
  return fs.readFile(path.join(appsRoot, relativePath), "utf8");
}

function readConstFunctionBody(source, functionName) {
  const start = source.indexOf(`const ${functionName} = async () => {`);
  assert.notEqual(start, -1, `${functionName} not found`);
  const end = source.indexOf("\n  };", start);
  assert.notEqual(end, -1, `${functionName} body end not found`);
  return source.slice(start, end);
}

test("账号登录和导入会刷新 Codex profile 候选账号", async () => {
  const source = await readSource("src/components/modals/add-account-modal.tsx");
  assert.match(source, /CODEX_PROFILE_CANDIDATES_QUERY_KEY/);
  assert.match(
    source,
    /queryClient\.invalidateQueries\(\{\s*queryKey:\s*CODEX_PROFILE_CANDIDATES_QUERY_KEY\s*,?\s*\}\)/s,
  );
});

test("账号池页面变更会刷新 Codex profile 候选账号", async () => {
  const source = await readSource("src/hooks/useAccounts.ts");
  const invalidateUsageBody = readConstFunctionBody(source, "invalidateUsageData");
  assert.match(source, /CODEX_PROFILE_CANDIDATES_QUERY_KEY/);
  assert.match(
    invalidateUsageBody,
    /queryClient\.invalidateQueries\(\{\s*queryKey:\s*CODEX_PROFILE_CANDIDATES_QUERY_KEY\s*,?\s*\}\)/,
  );
});

test("平台模式页面可见时会主动刷新候选列表", async () => {
  const source = `${await readSource("src/app/platform-mode/page.tsx")}\n${await readSource("src/app/platform-mode/page-sections.tsx")}\n${await readSource("src/app/platform-mode/use-platform-mode-state.ts")}`;
  assert.match(source, /useDesktopPageActive\("\/platform-mode\/"\)/);
  assert.match(source, /refetchInterval:\s*isServiceReady && isPageActive \? 5_000 : false/);
  assert.match(source, /pickAvailableCandidateId/);
});

test("平台模式页面采用当前模式优先的切换结构", async () => {
  const source = `${await readSource("src/app/platform-mode/page.tsx")}\n${await readSource("src/app/platform-mode/page-sections.tsx")}`;
  assert.match(source, /平台模式选择/);
  assert.match(source, /state\.mode === "web-gateway"/);
  assert.match(source, /Web \/ Docker 模式/);
  assert.match(source, /\/api\/rpc 写入 codexmanager-service/);
  assert.match(source, /当前模式/);
  assert.match(source, /账号直连/);
  assert.match(source, /本地网关/);
  assert.match(source, /高级与恢复/);
  assert.match(source, /不会产生 CodexManager 请求日志/);
  assert.match(source, /请求日志、Token、费用估算和仪表盘统计可用/);
  assert.match(source, /CodexManager 管理文件/);
  assert.match(source, /备份保存在 CodexManager 数据目录/);
  assert.match(source, /清理历史备份/);
  assert.match(source, /pruneHistoryBackups/);
  assert.match(source, /href=\{buildStaticRouteUrl\(href\)\}/);
});

test("平台密钥变更会刷新 Codex profile 候选密钥", async () => {
  const source = await readSource("src/hooks/useApiKeys.ts");
  assert.match(source, /CODEX_PROFILE_CANDIDATES_QUERY_KEY/);
  assert.match(
    source,
    /queryClient\.invalidateQueries\(\{\s*queryKey:\s*CODEX_PROFILE_CANDIDATES_QUERY_KEY\s*,?\s*\}\)/s,
  );
});

test("平台密钥弹窗创建和编辑会刷新 Codex profile 候选密钥", async () => {
  const source = await readSource("src/components/modals/api-key-modal.tsx");
  assert.match(source, /CODEX_PROFILE_CANDIDATES_QUERY_KEY/);
  assert.match(
    source,
    /queryClient\.invalidateQueries\(\{\s*queryKey:\s*CODEX_PROFILE_CANDIDATES_QUERY_KEY\s*,?\s*\}\)/s,
  );
});

test("账号页快捷切换复用 Codex profile 状态、候选和应用接口", async () => {
  const hookSource = await readSource(
    "src/hooks/useAccountCodexProfileSwitch.ts",
  );
  assert.match(hookSource, /useCodexProfileModeStatus/);
  assert.match(hookSource, /CODEX_PROFILE_STATUS_QUERY_KEY/);
  assert.match(hookSource, /CODEX_PROFILE_CANDIDATES_QUERY_KEY/);
  assert.match(hookSource, /codexProfileClient\.listCandidates\(\)/);
  assert.match(
    hookSource,
    /codexProfileClient\.applyDirectAccount\(\{\s*accountId:\s*normalizedAccountId,\s*codexHome:\s*statusQuery\.status\?\.codexHome \|\| null,/s,
  );
  assert.match(
    hookSource,
    /queryClient\.invalidateQueries\(\{\s*queryKey:\s*CODEX_PROFILE_STATUS_QUERY_KEY\s*\}\)/,
  );
  assert.match(
    hookSource,
    /queryClient\.invalidateQueries\(\{\s*queryKey:\s*CODEX_PROFILE_CANDIDATES_QUERY_KEY,?\s*\}\)/s,
  );
  assert.match(hookSource, /nextStatus\.historyRepair/);
  assert.doesNotMatch(hookSource, /codex_local_account_pool|auth\.json/);
});

test("账号页仅在服务和 profile 状态有效时标记运行账号", async () => {
  const hookSource = await readSource(
    "src/hooks/useAccountCodexProfileSwitch.ts",
  );
  assert.match(
    hookSource,
    /const hasValidActiveStatus =\s*statusQuery\.isServiceReady && statusQuery\.isSuccess;/s,
  );
  assert.match(
    hookSource,
    /activeAccountId:\s*hasValidActiveStatus && status\?\.mode === "direct_account"\s*\? status\.selectedAccountId\s*:\s*null,/s,
  );
});

test("账号页快捷切换文案不假定 Codex profile 位于本机", async () => {
  const cellSource = await readSource(
    "src/components/accounts/account-codex-profile-cell.tsx",
  );
  const catalogs = await Promise.all([
    readSource("src/lib/i18n/messages/sections/en-accounts.ts"),
    readSource("src/lib/i18n/messages/sections/ko-accounts.ts"),
    readSource("src/lib/i18n/messages/sections/ru-accounts.ts"),
  ]);
  const source = [cellSource, ...catalogs].join("\n");

  assert.match(cellSource, /切换 Codex profile 到此账号/);
  assert.match(cellSource, /当前服务的 Codex profile 正在使用此账号/);
  assert.doesNotMatch(source, /切换本机 Codex 到此账号/);
  assert.doesNotMatch(source, /当前 Codex profile 正在使用此账号/);
  for (const catalog of catalogs) {
    assert.match(catalog, /"切换 Codex profile 到此账号"/);
    assert.match(catalog, /"当前服务的 Codex profile 正在使用此账号"/);
  }
});

test("账号列表逐行展示 Codex 运行账号且触发器不嵌套按钮", async () => {
  const viewSource = await readSource(
    "src/app/accounts/accounts-page-view.tsx",
  );
  const cellSource = await readSource(
    "src/components/accounts/account-codex-profile-cell.tsx",
  );
  assert.match(viewSource, /Codex 运行账号/);
  assert.match(viewSource, /<AccountCodexProfileCell/);
  assert.match(viewSource, /colSpan=\{8\}/);
  assert.match(cellSource, /state\.activeAccountId === accountId/);
  assert.match(cellSource, /state\.candidateAccountIds\.has\(accountId\)/);
  assert.match(cellSource, /TooltipTrigger render=\{<span \/>\}/);
  assert.doesNotMatch(cellSource, /<TooltipTrigger>\s*<Button/s);
});
