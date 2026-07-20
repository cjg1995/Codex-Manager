import { expect, test } from "@playwright/test";

const SETTINGS_SNAPSHOT = {
  updateAutoCheck: true,
  closeToTrayOnClose: false,
  closeToTraySupported: false,
  lowTransparency: false,
  lightweightModeOnCloseToTray: false,
  codexCliGuideDismissed: true,
  webAccessPasswordConfigured: false,
  locale: "zh-CN",
  localeOptions: ["zh-CN", "en"],
  serviceAddr: "localhost:48760",
  serviceListenMode: "loopback",
  serviceListenModeOptions: ["loopback", "all_interfaces"],
  routeStrategy: "ordered",
  routeStrategyOptions: ["ordered", "balanced"],
  freeAccountMaxModel: "auto",
  freeAccountMaxModelOptions: ["auto", "gpt-5"],
  modelForwardRules: "",
  accountMaxInflight: 1,
  gatewayOriginator: "codex-cli",
  gatewayOriginatorDefault: "codex-cli",
  gatewayUserAgentVersion: "1.0.0",
  gatewayUserAgentVersionDefault: "1.0.0",
  gatewayResidencyRequirement: "",
  gatewayResidencyRequirementOptions: ["", "us"],
  pluginMarketMode: "builtin",
  pluginMarketSourceUrl: "",
  upstreamProxyUrl: "",
  upstreamStreamTimeoutMs: 600000,
  upstreamTotalTimeoutMs: 0,
  sseKeepaliveIntervalMs: 15000,
  backgroundTasks: {
    usagePollingEnabled: true,
    usagePollIntervalSecs: 600,
    gatewayKeepaliveEnabled: true,
    gatewayKeepaliveIntervalSecs: 180,
    tokenRefreshPollingEnabled: true,
    tokenRefreshPollIntervalSecs: 60,
    usageRefreshWorkers: 4,
    httpWorkerFactor: 4,
    httpWorkerMin: 8,
    httpStreamWorkerFactor: 1,
    httpStreamWorkerMin: 2,
  },
  envOverrides: {},
  envOverrideCatalog: [],
  envOverrideReservedKeys: [],
  envOverrideUnsupportedKeys: [],
  theme: "tech",
  appearancePreset: "classic",
};

test("accounts page shows status reason and keeps compact layout usable", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1800, height: 900 });
  await page.route("**/api/runtime**", async (route) => {
    await route.fulfill({
      contentType: "application/json; charset=utf-8",
      body: JSON.stringify({
        mode: "web-gateway",
        rpcBaseUrl: "/api/rpc",
        canManageService: false,
        canSelfUpdate: false,
        canCloseToTray: false,
        canOpenLocalDir: false,
        canUseBrowserFileImport: true,
        canUseBrowserDownloadExport: true,
      }),
    });
  });

  await page.route("**/api/rpc**", async (route) => {
    const payload = route.request().postDataJSON();
    const method = typeof payload?.method === "string" ? payload.method : "";
    const id = payload?.id ?? 1;

    const ok = (result: unknown) =>
      route.fulfill({
        contentType: "application/json; charset=utf-8",
        body: JSON.stringify({
          jsonrpc: "2.0",
          id,
          result,
        }),
      });

    if (method === "appSettings/get") {
      await ok(SETTINGS_SNAPSHOT);
      return;
    }
    if (method === "initialize") {
      await ok({
        version: "0.3.1",
        userAgent: "codex_cli_rs/0.1.19",
        codexHome: "/tmp/.codex",
        platformFamily: "unix",
        platformOs: "macos",
      });
      return;
    }
    if (method === "accountManager/session/current") {
      await ok({
        mode: "none",
        currentUser: null,
        role: "system_admin",
        permissions: ["system:admin"],
        distributionEnabled: false,
      });
      return;
    }
    if (method === "codexProfile/get") {
      await ok({
        codexHome: "C:\\Users\\Tester\\.codex",
        authPath: "C:\\Users\\Tester\\.codex\\auth.json",
        configPath: "C:\\Users\\Tester\\.codex\\config.toml",
        mode: "direct_account",
        selectedAccountId: "acct-refresh-reused",
        profileWritable: true,
        warnings: [],
      });
      return;
    }
    if (method === "codexProfile/listCandidates") {
      await ok({
        accounts: [
          {
            id: "acct-refresh-reused",
            label: "angiemooreja@hotmail.com",
            status: "active",
          },
        ],
        apiKeys: [],
      });
      return;
    }
    if (method === "account/list") {
      await ok({
        items: [
          {
            id: "acct-refresh-reused",
            label: "angiemooreja@hotmail.com",
            plan_type: "plus",
            status: "unavailable",
            status_reason: "refresh_token_invalid:refresh_token_reused",
            sort: 0,
          },
        ],
        total: 1,
        page: 1,
        pageSize: 20,
      });
      return;
    }
    if (method === "account/usage/list") {
      await ok([]);
      return;
    }

    await route.fulfill({
      status: 500,
      contentType: "application/json; charset=utf-8",
      body: JSON.stringify({
        jsonrpc: "2.0",
        id,
        error: {
          code: -32000,
          message: `Unhandled RPC method in test: ${method}`,
        },
      }),
    });
  });

  await page.goto("/accounts/");

  await expect(page.getByRole("heading", { name: "OpenAI 账号池" })).toBeVisible();
  const headers = page.locator('[data-slot="table-head"]');
  const accountHeader = headers.filter({ hasText: "账号信息" });
  const quotaHeader = headers.filter({ hasText: "额度详情" });
  const orderHeader = headers.filter({ hasText: "顺序" });
  const switchHeader = headers.filter({ hasText: "Codex 运行账号" });
  const statusHeader = headers.filter({ hasText: "状态" });
  const proxyHeader = headers.filter({ hasText: "账号代理" });
  const actionHeader = headers.filter({ hasText: "操作" });
  const tableContainer = page.locator('[data-slot="table-container"]').first();
  const compactStatus = page.getByTestId("account-status-compact");
  const wideStatus = page.getByTestId("account-status-wide");

  const expectTableToFit = async () => {
    const tableMetrics = await tableContainer.evaluate((element) => ({
      clientWidth: element.clientWidth,
      scrollWidth: element.scrollWidth,
    }));
    expect(tableMetrics.scrollWidth).toBeLessThanOrEqual(
      tableMetrics.clientWidth + 2,
    );

    const switchBounds = await switchHeader.boundingBox();
    const actionBounds = await actionHeader.boundingBox();
    expect(switchBounds).not.toBeNull();
    expect(actionBounds).not.toBeNull();
    expect(switchBounds!.x + switchBounds!.width).toBeLessThanOrEqual(
      actionBounds!.x + 1,
    );
  };

  await expect(accountHeader).toBeVisible();
  await expect(quotaHeader).toBeVisible();
  await expect(orderHeader).toBeVisible();
  await expect(switchHeader).toBeVisible();
  await expect(statusHeader).toBeVisible();
  await expect(proxyHeader).toBeVisible();
  await expect(actionHeader).toBeVisible();
  await expect(page.getByText("运行中", { exact: true })).toBeVisible();
  await expect(compactStatus).toBeHidden();
  await expect(wideStatus).toBeVisible();
  await expectTableToFit();

  const reasonText = wideStatus.getByText(
    "Refresh Token 已被重复使用，需要重新登录",
  );
  await expect(reasonText).toBeVisible();

  await reasonText.hover();
  await expect(
    page.getByText("refresh_token_invalid:refresh_token_reused"),
  ).toBeVisible();
  await page.mouse.move(0, 0);
  await page.keyboard.press("Escape");

  await page.setViewportSize({ width: 1536, height: 800 });
  await expect(accountHeader).toBeVisible();
  await expect(quotaHeader).toBeVisible();
  await expect(orderHeader).toBeVisible();
  await expect(switchHeader).toBeVisible();
  await expect(statusHeader).toBeHidden();
  await expect(proxyHeader).toBeHidden();
  await expect(actionHeader).toBeVisible();
  await expect(wideStatus).toBeHidden();
  await expect(compactStatus).toBeVisible();
  await expect(
    compactStatus.getByText("Refresh Token 已被重复使用，需要重新登录"),
  ).toBeVisible();
  await expectTableToFit();

  await page.setViewportSize({ width: 1472, height: 800 });
  await expect(orderHeader).toBeVisible();
  await expect(statusHeader).toBeHidden();
  await expect(proxyHeader).toBeHidden();
  await expect(compactStatus).toBeVisible();
  await expectTableToFit();

  await page.setViewportSize({ width: 1471, height: 800 });
  await expect(orderHeader).toBeHidden();
  await expect(statusHeader).toBeHidden();
  await expect(proxyHeader).toBeHidden();
  await expect(compactStatus).toBeVisible();
  await expectTableToFit();

  await page.setViewportSize({ width: 1100, height: 750 });
  await expect(accountHeader).toBeVisible();
  await expect(quotaHeader).toBeVisible();
  await expect(switchHeader).toBeVisible();
  await expect(actionHeader).toBeVisible();
  await expect(orderHeader).toBeHidden();
  await expect(statusHeader).toBeHidden();
  await expect(proxyHeader).toBeHidden();
  await expect(compactStatus).toBeVisible();
  await expectTableToFit();

  const moreActionsButton = page.getByRole("button", { name: "更多账号操作" });
  await expect(moreActionsButton).toHaveCount(1);
  await moreActionsButton.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("menuitem", { name: "编辑账号信息" })).toBeEnabled();
  await expect(page.getByRole("menuitem", { name: "上移一位" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "下移一位" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "账号代理" })).toBeEnabled();
});
