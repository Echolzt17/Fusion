import { test, expect, type Page } from '@playwright/test';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

async function fixture(page: Page, options: { empty?: boolean; many?: boolean; enabled?: boolean } = {}) {
  await page.addInitScript(options => {
    const now = Math.floor(Date.now() / 1000);
    const settings = { enabled: options.enabled ?? false, mode: 'auto', allowedRegions: ['JP', 'US'], nodeOrder: [], fixedNodeId: null, excludedNodeIds: [], regionOverrides: {} };
    const node = (id: string, name: string, region: string, health = 'unknown') => ({ id, name, region, inferredRegion: region, health, delayMs: health === 'healthy' ? 120 : null, lastCheckedAt: health === 'healthy' ? now : null, failureCount: 0, cooldownUntil: null, excluded: false, businessSuccesses: 0, businessFailures: 0 });
    const nodes = options.empty ? [] : options.many ? Array.from({ length: 1000 }, (_, index) => node('node-' + index, `日本 测试节点 ${index + 1}`, 'JP', index % 2 ? 'unknown' : 'healthy')) : [node('jp-primary', '日本 主线路', 'JP', 'healthy'), node('jp-backup', '日本 备用线路', 'JP'), node('us-backup', '美国 备用线路', 'US', 'healthy'), node('unknown', '待归类线路', 'UNKNOWN')];
    const state = {
      status: { settings, kernelRunning: options.enabled ?? false, currentNodeId: options.enabled ? 'jp-primary' : null, nodes, subscriptionName: options.empty ? null : '公开测试订阅', subscriptionUpdatedAt: options.empty ? null : now, subscriptionKind: options.empty ? null : 'url', pendingUpdate: false, phase: options.enabled ? 'running' : 'idle', error: null, lastSwitchReason: null },
      calls: [] as { command: string; args: Record<string, any> }[], callbacks: {} as Record<number, (event: unknown) => void>, statusError: false, importError: false,
    };
    Object.assign(window, { networkFixture: state, __TAURI_INTERNALS__: {
      transformCallback: (callback: (event: unknown) => void) => { const id = Object.keys(state.callbacks).length + 1; state.callbacks[id] = callback; return id; },
      invoke: async (command: string, args: Record<string, any> = {}) => {
        state.calls.push({ command, args });
        switch (command) {
          case 'plugin:event|listen': return 1;
          case 'plugin:event|unlisten': return;
          case 'main_visible': return true;
          case 'uninstall_requested': return false;
          case 'accounts': case 'logs': case 'usage': case 'api_sources': case 'api_client_status': return [];
          case 'settings': return { port: 18765, concurrency: 4, fixedAccount: null, selectionPolicy: 'auto' };
          case 'service_status': return { running: false, draining: false, activeRequests: 0, baseUrl: 'http://127.0.0.1:18765/v1' };
          case 'connection_status': return { mode: 'off', configured: false, recoveryPending: false, error: null, operation: null };
          case 'login_status': return { phase: 'idle', message: '' };
          case 'scheduling_status': return { policy: 'auto', currentAccountId: null, preferredAccountId: null };
          case 'image_compatibility': return { client: { desktop: null, engine: null }, configurationReady: false, routesReady: false };
          case 'usage_dashboard': return { tokens: {}, measured: false, days: [], hours: [], today: '', timezone: 'UTC+08:00', updatedAt: now * 1000, scanning: false, historyPartial: false, activityPartial: false, error: null };
          case 'conversation_usage': return { conversations: [] };
          case 'request_history': return { items: [], nextCursor: null };
          case 'update_status': return { phase: 'idle', startupPromptPending: false };
          case 'ai_pricing': return { products: [], error: null, updatedAt: null };
          case 'network_status': if (state.statusError) throw new Error('公开测试状态读取失败'); return structuredClone(state.status);
          case 'network_import': {
            if (state.importError) throw new Error('公开测试订阅导入失败 ' + (args.input.url || ''));
            state.status.nodes = [node('imported', '日本 导入节点', 'JP')];
            state.status.subscriptionName = args.input.name || '已导入订阅';
            state.status.subscriptionKind = args.input.url ? 'url' : 'file';
            state.status.subscriptionUpdatedAt = now;
            return structuredClone(state.status);
          }
          case 'network_save_settings': state.status.settings = structuredClone(args.settings); return structuredClone(state.status);
          case 'network_select': state.status.currentNodeId = args.id; state.status.settings.mode = args.fixed ? 'fixed' : 'auto'; state.status.settings.fixedNodeId = args.fixed ? args.id : null; return structuredClone(state.status);
          case 'network_set_enabled': state.status.settings.enabled = args.enabled; state.status.kernelRunning = args.enabled; state.status.phase = args.enabled ? 'running' : 'idle'; return structuredClone(state.status);
          case 'network_remove': state.status = { ...state.status, settings: { ...state.status.settings, enabled: false }, kernelRunning: false, currentNodeId: null, nodes: [], subscriptionName: null, subscriptionKind: null, subscriptionUpdatedAt: null }; return structuredClone(state.status);
          case 'network_check': case 'network_refresh': case 'network_retry': return structuredClone(state.status);
          default: return null;
        }
      },
    } });
  }, options);
  await page.goto('/');
  await page.getByRole('button', { name: '连接与设置', exact: true }).click();
  await expect(page.getByRole('group', { name: '代理与节点', exact: true })).toBeVisible();
  await expect(page.getByRole('heading', { name: '当前连接', exact: true })).toBeVisible();
}

const group = (page: Page) => page.getByRole('group', { name: '代理与节点', exact: true });
const calls = (page: Page, command: string) => page.evaluate(command => (window as any).networkFixture.calls.filter((value: any) => value.command === command), command);
const expandRegion = (page: Page, name: string) => group(page).locator('.network-region-group').filter({ has: page.locator('summary').filter({ hasText: name }) }).locator('summary').click();

test('network cards belong to connection settings and pool and kernel states remain separate', async ({ page }) => {
  await fixture(page, { enabled: true });
  await expect(page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '代理与节点', exact: true })).toHaveCount(0);
  await expect(group(page).getByRole('region', { name: '专用网络状态' })).toContainText('账号池已关闭，专用网络仍供授权和额度查询使用。');
  await expect(group(page).getByLabel('内核运行中', { exact: true })).toBeVisible();
  expect(await calls(page, 'network_check')).toHaveLength(0);
  expect(await calls(page, 'network_set_enabled')).toHaveLength(0);
  await expandRegion(page, '日本');
  const primary = page.getByRole('article', { name: '节点 日本 主线路', exact: true });
  await expect(primary).toContainText('基础健康');
  await expect(primary).toContainText('近期业务：尚无记录');
  await group(page).getByRole('button', { name: '改用系统代理', exact: true }).click();
  expect((await calls(page, 'network_set_enabled'))[0].args).toEqual({ enabled: false });
  expect(await calls(page, 'close_pool')).toHaveLength(0);
});

test('subscription link is masked, cleared after import, and never enables the exit implicitly', async ({ page }) => {
  await fixture(page, { empty: true });
  await group(page).getByRole('button', { name: '导入订阅', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '导入代理订阅', exact: true });
  const input = dialog.getByLabel('HTTPS 订阅链接', { exact: true });
  await expect(input).toHaveAttribute('type', 'password');
  await input.fill('https://subscription.example.test/public-fixture?token=public-fake');
  await dialog.getByLabel('订阅名称（可选）', { exact: true }).fill('手动导入测试');
  await dialog.getByRole('button', { name: '导入链接', exact: true }).click();
  await expect(dialog).not.toBeVisible();
  await expect(group(page)).toContainText('手动导入测试');
  await expect(group(page)).not.toContainText('public-fake');
  expect((await calls(page, 'network_import'))[0].args.input).toEqual({ url: 'https://subscription.example.test/public-fixture?token=public-fake', yaml: null, name: '手动导入测试' });
  expect(await calls(page, 'network_set_enabled')).toHaveLength(0);
  await group(page).getByRole('button', { name: '替换订阅', exact: true }).click();
  await expect(input).toHaveValue('');
  await page.keyboard.press('Escape');
  await expect(dialog).not.toBeVisible();
});

test('an import failure hides the address and local YAML is passed only to the import IPC', async ({ page }) => {
  await fixture(page, { empty: true });
  await group(page).getByRole('button', { name: '导入订阅', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '导入代理订阅', exact: true });
  const input = dialog.getByLabel('HTTPS 订阅链接', { exact: true });
  await input.fill('http://subscription.example.test/public-fixture');
  await dialog.getByRole('button', { name: '导入链接', exact: true }).click();
  await expect(dialog.getByRole('alert')).toContainText('需要使用 HTTPS');
  expect(await calls(page, 'network_import')).toHaveLength(0);
  await page.evaluate(() => { (window as any).networkFixture.importError = true; });
  await input.fill('https://subscription.example.test/public-fixture?token=public-fake');
  await dialog.getByRole('button', { name: '导入链接', exact: true }).click();
  await expect(dialog.getByRole('alert')).toContainText('[已隐藏]');
  await expect(dialog.getByRole('alert')).not.toContainText('public-fake');
  await page.evaluate(() => { (window as any).networkFixture.importError = false; });
  const yaml = 'proxies:\n  - name: 日本 公开测试\n    type: http\n    server: 127.0.0.1\n    port: 1\n';
  await dialog.locator('input[type=file]').setInputFiles({ name: 'public-fixture.yaml', mimeType: 'application/yaml', buffer: Buffer.from(yaml) });
  await expect(dialog).not.toBeVisible();
  expect((await calls(page, 'network_import')).at(-1).args.input).toEqual({ yaml, url: null, name: '本地订阅' });
  await expect(group(page)).toContainText('本地 YAML · 手动重新导入');
  await expect(group(page).getByRole('button', { name: '更新订阅', exact: true })).toBeDisabled();
});

test('region order and node edits wait for explicit save and selection preserves automatic versus fixed mode', async ({ page }) => {
  await fixture(page);
  await group(page).getByRole('button', { name: '上移美国地区', exact: true }).click();
  await expect(group(page)).toContainText('有未保存的策略修改');
  await expandRegion(page, '日本');
  const backup = page.getByRole('article', { name: '节点 日本 备用线路', exact: true });
  await backup.getByLabel('排除', { exact: true }).check();
  await group(page).getByRole('button', { name: '上移节点 日本 备用线路', exact: true }).click();
  await expandRegion(page, '未分类');
  await page.getByRole('article', { name: '节点 待归类线路', exact: true }).getByRole('button', { name: '地区', exact: true }).click();
  const edit = page.getByRole('dialog', { name: '修改节点地区', exact: true });
  await edit.getByLabel('国家／地区', { exact: true }).selectOption('SG');
  await edit.getByRole('button', { name: '确定地区', exact: true }).click();
  expect(await calls(page, 'network_save_settings')).toHaveLength(0);
  await group(page).getByRole('button', { name: '保存节点策略', exact: true }).click();
  const saved = (await calls(page, 'network_save_settings'))[0].args.settings;
  expect(saved.allowedRegions).toEqual(['US', 'JP']);
  expect(saved.excludedNodeIds).toEqual(['jp-backup']);
  expect(saved.nodeOrder.slice(0, 2)).toEqual(['jp-backup', 'jp-primary']);
  expect(saved.regionOverrides).toEqual({ unknown: 'SG' });
  const primary = page.getByRole('article', { name: '节点 日本 主线路', exact: true });
  await primary.getByRole('button', { name: '固定', exact: true }).click();
  expect((await calls(page, 'network_select')).at(-1).args).toEqual({ id: 'jp-primary', fixed: true });
  await expect(group(page).getByRole('button', { name: '手动固定', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await primary.getByRole('button', { name: '使用', exact: true }).click();
  expect((await calls(page, 'network_select')).at(-1).args).toEqual({ id: 'jp-primary', fixed: false });
  await expect(group(page).getByRole('button', { name: '自动切换', exact: true })).toHaveAttribute('aria-pressed', 'true');
  expect(await calls(page, 'network_set_enabled')).toHaveLength(0);
});

test('status read failures preserve the last snapshot and prevent stale controls from changing the exit', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => { (window as any).networkFixture.statusError = true; });
  await expect(group(page).getByRole('alert').filter({ hasText: '网络状态读取失败' })).toBeVisible({ timeout: 7000 });
  await expect(group(page)).toContainText('公开测试订阅');
  await expect(group(page).getByRole('button', { name: '使用内置代理', exact: true })).toBeDisabled();
  await page.evaluate(() => { (window as any).networkFixture.statusError = false; });
  await group(page).getByRole('button', { name: '重试读取网络状态', exact: true }).click();
  await expect(group(page).getByRole('button', { name: '使用内置代理', exact: true })).toBeEnabled();
});

test('a thousand nodes stay contained in light and dark narrow settings and removal requires an explicit action', async ({ page }) => {
  await page.setViewportSize({ width: 540, height: 700 });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await fixture(page, { many: true });
  const screenshots = mkdtempSync(join(tmpdir(), 'fusion-network-ui-'));
  await expandRegion(page, '日本');
  await expect(group(page).locator('.network-node')).toHaveCount(1000);
  for (const theme of ['light', 'dark']) {
    await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    expect(await group(page).locator('.network-region-list').evaluate(element => element.clientHeight <= 441 && element.scrollHeight > element.clientHeight)).toBe(true);
    await group(page).getByRole('heading', { name: '代理与节点', exact: true }).scrollIntoViewIfNeeded();
    await page.screenshot({ path: join(screenshots, `network-${theme}.png`), fullPage: true });
  }
  await group(page).getByRole('button', { name: '移除订阅', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '移除代理订阅', exact: true });
  expect(await calls(page, 'network_remove')).toHaveLength(0);
  await dialog.getByRole('button', { name: '确认移除订阅', exact: true }).click();
  await expect(dialog).not.toBeVisible();
  expect(await calls(page, 'network_remove')).toHaveLength(1);
  await expect(group(page)).toContainText('尚未导入订阅');
  console.log('Network card UI screenshots:', screenshots);
});
