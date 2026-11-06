import { test, expect, type Page } from '@playwright/test';

async function fixture(page: Page, cached = false) {
  await page.addInitScript(({ cached }) => {
    localStorage.setItem('pool-theme', 'light');
    const price = (region: string, convertedPrice: string, local: string) => ({ region, convertedPrice, price: local, tax: '含税示例' });
    const snapshot = {
      sourceUrl: 'https://opentherank.com/zh/ai-pricing/', checkedDate: '2026-11-06',
      updatedAt: 1793923200, error: cached ? '价格更新失败' : null,
      products: [
        { id: 'chatgpt', name: 'ChatGPT', sourceUrl: 'https://opentherank.com/zh/ai-pricing/chatgpt/', description: 'ChatGPT 地区订阅价', plans: [
          { name: 'Plus', prices: [price('测试地区 A', '¥120.00 / 月', 'US$20 / 月'), price('测试地区 B', '¥150.00 / 月', 'CA$30 / 月')] },
          { name: 'Pro', prices: [price('测试地区 A', '¥1200.00 / 月', 'US$200 / 月')] },
        ] },
        { id: 'claude', name: 'Claude', sourceUrl: 'https://opentherank.com/zh/ai-pricing/claude/', description: 'Claude 地区订阅价', plans: [
          { name: 'Pro', prices: [price('测试地区 C', '¥130.00 / 月', 'US$21 / 月')] },
        ] },
      ],
    };
    const state = { snapshot, calls: [] as { command: string; args: Record<string, unknown> }[], callbacks: {} as Record<number, (event: unknown) => void> };
    Object.assign(window, { pricingFixture: state, __TAURI_INTERNALS__: {
      transformCallback: (callback: (event: unknown) => void) => { const id = Object.keys(state.callbacks).length + 1; state.callbacks[id] = callback; return id; },
      invoke: async (command: string, args: Record<string, unknown> = {}) => {
        state.calls.push({ command, args });
        switch (command) {
          case 'plugin:event|listen': return 1;
          case 'plugin:event|unlisten': return;
          case 'ai_pricing': return structuredClone(state.snapshot);
          case 'open_pricing_website': return;
          case 'network_status': return {
            settings: { enabled: false, mode: 'auto', allowedRegions: [], nodeOrder: [], fixedNodeId: null, excludedNodeIds: [], regionOverrides: {} },
            kernelRunning: false, currentNodeId: null, nodes: [], subscriptionName: null,
            subscriptionUpdatedAt: null, subscriptionKind: null, pendingUpdate: false,
            phase: 'off', error: null, lastSwitchReason: null,
          };
          case 'main_visible': return true;
          case 'uninstall_requested': return false;
          case 'accounts': case 'api_client_status': case 'logs': case 'usage': case 'handoff_status': return [];
          case 'settings': return { port: 18765, concurrency: 4, fixedAccount: null, selectionPolicy: 'auto' };
          case 'connection_status': return { mode: 'off', configured: false, recoveryPending: false, operation: null, phase: null };
          case 'service_status': return { running: false, draining: false, activeRequests: 0, baseUrl: 'http://127.0.0.1:18765/v1' };
          case 'login_status': return { phase: 'idle', message: '' };
          case 'scheduling_status': return { policy: 'auto', currentAccountId: null, preferredAccountId: null };
          case 'image_compatibility': return { client: {}, detail: '', desktopAcceptance: 'pending' };
          case 'usage_dashboard': return null;
          case 'conversation_usage': return { conversations: [] };
          case 'request_history': return { items: [], nextCursor: null };
          case 'update_status': return { currentVersion: '1.0.9', phase: 'current', startupPromptPending: false, operationActive: false, installing: false };
          default: throw new Error(`Unexpected pricing fixture IPC: ${command}`);
        }
      },
    } });
  }, { cached });
  await page.goto('/');
  await expect.poll(() => page.evaluate(() => (window as any).pricingFixture.calls.filter((call: any) => call.command === 'ai_pricing').length)).toBeGreaterThan(0);
  await page.getByRole('button', { name: '连接与设置', exact: true }).click();
}

test('one comparison card defaults to Codex and switches product and plan in place', async ({ page }) => {
  await fixture(page);
  const card = page.getByRole('region', { name: 'AI订阅比价', exact: true });
  await expect(card).toHaveCount(1);
  await expect(card.getByRole('combobox', { name: 'AI软件', exact: true })).toHaveValue('chatgpt');
  await expect(card.getByRole('combobox', { name: 'AI软件', exact: true }).locator('option:checked')).toHaveText('Codex / ChatGPT');
  await expect(card).toContainText('ChatGPT 订阅价格 · 含 Codex');
  await expect(card.getByRole('row')).toHaveCount(3);
  await expect(card).toContainText('¥120.00 / 月');
  await card.getByRole('combobox', { name: '订阅套餐', exact: true }).selectOption('Pro');
  await expect(card).toContainText('¥1200.00 / 月');
  await expect(card.getByRole('row')).toHaveCount(2);
  await card.getByRole('combobox', { name: 'AI软件', exact: true }).selectOption('claude');
  await expect(card).toContainText('¥130.00 / 月');
  await expect(card).not.toContainText('¥1200.00 / 月');
  await expect(card.getByRole('combobox', { name: '订阅套餐', exact: true })).toHaveValue('Pro');
  await expect(page.getByRole('heading', { name: 'Codex 接入', exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '启动账号池', exact: true })).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).pricingFixture.calls.filter((call: any) => call.command === 'open_pricing_website'))).toEqual([]);
});

test('a failed daily refresh keeps previous prices visible and labels the cached result', async ({ page }) => {
  await fixture(page, true);
  const card = page.getByRole('region', { name: 'AI订阅比价', exact: true });
  await expect(card).toContainText('¥120.00 / 月');
  await expect(card.locator('.pricing-footer')).toContainText('价格更新失败，显示上次价格');
  await expect(card).not.toContainText('正在获取订阅价格');
  await page.setViewportSize({ width: 620, height: 850 });
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({ path: 'test-results/pricing-cached-narrow.png', fullPage: true });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test('recharge and source links open only through their fixed IPC choices', async ({ page }) => {
  const externalRequests: string[] = [];
  page.on('request', request => { if (!request.url().startsWith('http://127.0.0.1:')) externalRequests.push(request.url()); });
  await fixture(page);
  const recharge = page.getByRole('region', { name: 'AI充值', exact: true });
  await expect(recharge).toContainText('PriceAI');
  await expect(recharge).toContainText('充值');
  await expect(recharge.getByRole('button')).toHaveCount(1);
  await recharge.getByRole('button', { name: /前往 PriceAI 充值/ }).click();
  await page.getByRole('region', { name: 'AI订阅比价', exact: true }).getByRole('button', { name: /查看来源/ }).click();
  expect(await page.evaluate(() => (window as any).pricingFixture.calls.filter((call: any) => call.command === 'open_pricing_website').map((call: any) => call.args))).toEqual([{ site: 'recharge' }, { site: 'comparison' }]);
  expect(externalRequests).toEqual([]);
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({ path: 'test-results/pricing-settings-light.png', fullPage: true });
  await page.locator('.theme-switch').click();
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({ path: 'test-results/pricing-settings-dark.png', fullPage: true });
});
