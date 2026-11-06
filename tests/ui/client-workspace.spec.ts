import { test, expect, type Page } from '@playwright/test';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

async function fixture(page: Page) {
  await page.addInitScript(() => {
    const at = Math.floor(Date.now() / 1000);
    const unknown = () => ({ status: 'unknown', at: null, message: '' });
    const source = (id: string, name: string, protocol: string) => ({ id, name, protocol, baseUrl: 'https://api.example.test/v1', model: protocol === 'responses' ? 'fixture-response-model' : 'fixture-message-model', network: 'systemProxy', enabled: true, hasKey: true, revision: 1, verification: { basic: unknown(), test: unknown(), stream: unknown(), tools: unknown() } });
    const state = {
      sources: [source('codex-api', 'Codex 测试来源', 'responses'), source('claude-api', 'Claude 测试来源', 'messages')],
      statuses: ['codex', 'claudeCode'].map(client => ({ client, mode: 'off', sourceId: null, sourceName: null, sourceRevision: null, phase: null, error: null, activeRequests: 0, desktopAcceptance: 'pending' })),
      calls: [] as { command: string; args: Record<string, any> }[], callbacks: {} as Record<number, (event: unknown) => void>,
      listeners: {} as Record<string, number>, nextCallback: 0, holdStart: false, failStart: false, removeError: false, statusError: false,
      releaseStart: null as (() => void) | null, partialUsage: false,
    };
    Object.assign(window, { apiFixture: state, __TAURI_INTERNALS__: {
      transformCallback: (callback: (event: unknown) => void) => { const id = ++state.nextCallback; state.callbacks[id] = callback; return id; },
      invoke: async (command: string, args: Record<string, any> = {}) => {
        state.calls.push({ command, args });
        if (command === 'plugin:event|listen') { state.listeners[String(args.event)] = Number(args.handler); return 1; }
        if (command === 'plugin:event|unlisten') return;
        switch (command) {
          case 'main_visible': return true;
          case 'uninstall_requested': return false;
          case 'accounts': return [{ id: 'subscription-account', email: 'subscription@example.test', available: true, state: 'ready', cooldownUntil: 0, planLabel: 'Plus', quota: null, quotaError: null }];
          case 'settings': return { port: 18765, concurrency: 4, fixedAccount: null, selectionPolicy: 'auto' };
          case 'service_status': return { running: false, activeRequests: 0, draining: false, baseUrl: 'http://127.0.0.1:18765/v1' };
          case 'connection_status': return { mode: 'off', configured: false, recoveryPending: false, operation: null, phase: null };
          case 'scheduling_status': return { policy: 'auto', currentAccountId: 'subscription-account', preferredAccountId: null };
          case 'login_status': return { phase: 'idle', message: '' };
          case 'image_compatibility': return { configurationReady: false };
          case 'logs': return [];
          case 'usage': return [];
          case 'usage_dashboard': return { tokens: {}, measured: false, days: [], hours: [], today: '', timezone: 'UTC+08:00', updatedAt: at * 1000, scanning: false, historyPartial: false, activityPartial: false, error: null };
          case 'reset_credits': return { available_count: 0, credits: [] };
          case 'conversation_usage': return { conversations: [] };
          case 'request_history': return { items: [], nextCursor: null };
          case 'update_status': return { phase: 'idle' };
          case 'api_sources': return state.sources;
          case 'api_client_status': if (state.statusError) throw new Error('模拟状态读取失败'); return state.statuses;
          case 'save_api_source': {
            const input = args.input;
            const existing = state.sources.find(value => value.id === input.id);
            const next = { id: input.id || 'new-api', name: input.name, protocol: input.protocol, baseUrl: input.baseUrl, model: input.model, network: input.network, enabled: input.enabled, hasKey: !!input.apiKey || !!existing?.hasKey, revision: (existing?.revision || 0) + 1, verification: { basic: unknown(), test: unknown(), stream: unknown(), tools: unknown() } };
            state.sources = [...state.sources.filter(value => value.id !== next.id), next]; return next;
          }
          case 'remove_api_source': if (state.removeError) throw new Error('模拟移除失败，配置仍保留'); state.sources = state.sources.filter(value => value.id !== args.id); return;
          case 'check_api_source': {
            const result = { status: 'passed', at, message: args.actual ? '少量请求通过，其他能力仍待验证' : '基础连接检查完成' };
            state.sources = state.sources.map(value => value.id === args.id ? { ...value, verification: { ...value.verification, [args.actual ? 'test' : 'basic']: result } } : value);
            return result;
          }
          case 'start_api_connection': {
            const selected = state.sources.find(value => value.id === args.sourceId)!;
            state.statuses = state.statuses.map(value => value.client === args.client ? { ...value, mode: 'starting', sourceId: selected.id, sourceName: selected.name, sourceRevision: selected.revision, phase: 'draining', activeRequests: 2 } : value);
            if (state.holdStart) await new Promise<void>(resolve => { state.releaseStart = resolve; });
            state.statuses = state.statuses.map(value => value.client === args.client ? { ...value, mode: state.failStart ? 'recoveryRequired' : 'running', phase: state.failStart ? 'failed' : null, error: state.failStart ? '模拟配置冲突，恢复记录保留' : null, activeRequests: 0 } : value);
            if (state.failStart) throw new Error('模拟配置冲突，恢复记录保留'); return;
          }
          case 'stop_api_connection': state.statuses = state.statuses.map(value => value.client === args.client ? { ...value, mode: 'off', sourceId: null, sourceName: null, sourceRevision: null, error: null, phase: null, activeRequests: 0 } : value); return;
          case 'api_request_logs': return [{ id: 'api-log-' + args.client, at, client: args.client, sourceId: args.client === 'codex' ? 'codex-api' : 'claude-api', sourceRevision: 1, operation: args.client === 'codex' ? 'responses' : 'messages', model: 'fixture-' + args.client, responseModel: state.partialUsage ? 'fixture-returned-model' : null, status: 'success', upstreamStatus: 200, elapsedMs: 1200, requestBytes: 24, responseBytes: 72, usage: state.partialUsage ? { inputTokens: 0, outputTokens: 12, totalTokens: 12 } : null, usagePartial: state.partialUsage, toolCallObserved: false }];
          case 'open_claude_login': return;
          default: return null;
        }
      },
    } });
  });
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'subscription@example.test', exact: true })).toBeVisible();
}

const calls = (page: Page, command: string) => page.evaluate(command => (window as any).apiFixture.calls.filter((value: any) => value.command === command), command);
const claude = (page: Page) => page.getByRole('group', { name: '客户端工作区', exact: true }).getByRole('button', { name: 'Claude Code', exact: true }).click();
const codex = (page: Page) => page.getByRole('group', { name: '客户端工作区', exact: true }).getByRole('button', { name: 'Codex', exact: true }).click();
const sourceCard = (page: Page, name = 'Claude 测试来源') => page.getByRole('article', { name: '来源 ' + name, exact: true });

test('client and source view changes preserve the subscription page and never switch a connection', async ({ page }) => {
  await fixture(page);
  await claude(page);
  await expect(sourceCard(page)).toBeVisible();
  await sourceCard(page).getByRole('button', { name: '选择此来源', exact: true }).click();
  await expect(sourceCard(page)).toContainText('候选已选中 · 尚未启动');
  expect(await calls(page, 'start_api_connection')).toHaveLength(0);
  expect(await calls(page, 'stop_api_connection')).toHaveLength(0);
  await codex(page);
  await expect(page.getByRole('heading', { name: 'subscription@example.test', exact: true })).toBeVisible();
  await claude(page);
  await page.getByRole('button', { name: '官方登录', exact: true }).click();
  await expect(page.getByText('官方客户端自登录 · 未托管', { exact: true })).toBeVisible();
  expect(await calls(page, 'open_claude_login')).toHaveLength(0);
  await page.getByRole('button', { name: '打开 Claude Code 官方登录', exact: true }).click();
  expect(await calls(page, 'open_claude_login')).toHaveLength(1);
  await expect(page.locator('.cw-official-panel').getByRole('status')).toContainText('不验证或托管本次订阅登录');
  expect(await calls(page, 'start_api_connection')).toHaveLength(0);
});

test('API form masks the key, preserves an existing key on blank edit, and does not start on save', async ({ page }) => {
  await fixture(page); await claude(page);
  await page.getByRole('button', { name: '＋ 添加来源', exact: true }).click();
  const drawer = page.getByRole('dialog', { name: '添加 API 来源', exact: true });
  await drawer.getByLabel('来源名称', { exact: true }).fill('同事测试来源');
  await drawer.getByLabel('Base URL', { exact: true }).fill('https://relay.example.test/anthropic');
  await drawer.getByLabel('模型名称', { exact: true }).fill('public-fixture-model');
  await expect(drawer.getByLabel('API Key', { exact: true })).toHaveAttribute('type', 'password');
  await drawer.getByLabel('API Key', { exact: true }).fill('public-fake-test-key');
  await expect(drawer.getByRole('combobox', { name: '网络连接', exact: true })).toHaveValue('systemProxy');
  await drawer.getByRole('button', { name: '保存来源', exact: true }).click();
  await expect(drawer).not.toBeVisible();
  const card = sourceCard(page, '同事测试来源');
  await expect(card).toBeVisible();
  await expect(card).not.toContainText('public-fake-test-key');
  expect(await calls(page, 'start_api_connection')).toHaveLength(0);
  const saved = await calls(page, 'save_api_source');
  expect(saved[0].args.input).toMatchObject({ protocol: 'messages', network: 'systemProxy', apiKey: 'public-fake-test-key' });
  await card.getByRole('button', { name: '编辑', exact: true }).click();
  const edit = page.getByRole('dialog', { name: '编辑 API 来源', exact: true });
  await expect(edit.getByLabel('API Key', { exact: true })).toHaveValue('');
  await edit.getByLabel('来源名称', { exact: true }).fill('同事测试来源已编辑');
  await edit.getByRole('button', { name: '保存来源', exact: true }).click();
  expect((await calls(page, 'save_api_source')).at(-1).args.input.apiKey).toBeNull();
  await expect(sourceCard(page, '同事测试来源已编辑')).toContainText('已保存');
});

test('base checks and explicit billed tests remain separate from capability acceptance', async ({ page }) => {
  await fixture(page); await claude(page);
  const card = sourceCard(page);
  expect(await calls(page, 'check_api_source')).toHaveLength(0);
  await card.getByRole('button', { name: '基础检查', exact: true }).click();
  await expect(card.locator('.cw-check').filter({ hasText: '基础检查' })).toContainText('已通过');
  expect((await calls(page, 'check_api_source'))[0].args).toEqual({ id: 'claude-api', actual: false });
  await expect(card.locator('.cw-check').filter({ hasText: '实际请求' })).toContainText('待验证');
  await card.getByRole('button', { name: '少量实际测试（可能计费）', exact: true }).click();
  expect((await calls(page, 'check_api_source')).at(-1).args).toEqual({ id: 'claude-api', actual: true });
  await expect(card.locator('.cw-check').filter({ hasText: '实际请求' })).toContainText('已通过');
  await expect(card.locator('.cw-check').filter({ hasText: '流式' })).toContainText('待验证');
  await expect(card.locator('.cw-check').filter({ hasText: '工具调用' })).toContainText('待验证');
  await expect(card.locator('.cw-check').filter({ hasText: '客户端' })).toContainText('待验收');
});

test('connection progress is local to the client and failure requires restoration before another start', async ({ page }) => {
  await fixture(page); await claude(page);
  await page.evaluate(() => { (window as any).apiFixture.holdStart = true; (window as any).apiFixture.failStart = true; });
  await sourceCard(page).getByRole('button', { name: '选择此来源', exact: true }).click();
  await page.getByRole('button', { name: '启动选中来源', exact: true }).click();
  await expect(page.locator('.cw-running-title')).toContainText('正在启动 API');
  await expect(page.locator('.cw-progress')).toContainText('等待在途请求完成');
  await codex(page);
  await expect(page.getByRole('heading', { name: 'subscription@example.test', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '添加账号', exact: true })).toBeEnabled();
  await page.evaluate(() => (window as any).apiFixture.releaseStart?.());
  await claude(page);
  await expect(page.locator('.cw-danger')).toContainText('模拟配置冲突');
  await sourceCard(page).getByRole('button', { name: '选择此来源', exact: true }).click();
  await expect(page.getByRole('button', { name: '切换到选中来源', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: '重试恢复并关闭', exact: true }).click();
  await expect(page.locator('.cw-running-title')).toContainText('API 已关闭');
  expect((await calls(page, 'stop_api_connection')).at(-1).args.client).toBe('claudeCode');
  expect(await calls(page, 'close_pool')).toHaveLength(0);
});

test('a selected source starts only on an explicit action and cannot be removed while in use', async ({ page }) => {
  await fixture(page); await claude(page);
  const card = sourceCard(page);
  await card.getByRole('button', { name: '选择此来源', exact: true }).click();
  await page.getByRole('button', { name: '启动选中来源', exact: true }).click();
  await expect(page.locator('.cw-running-title')).toContainText('API 运行中');
  expect((await calls(page, 'start_api_connection')).at(-1).args).toEqual({ client: 'claudeCode', sourceId: 'claude-api' });
  await expect(card.getByRole('button', { name: '禁用', exact: true })).toBeDisabled();
  await expect(card.getByRole('button', { name: '移除', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: '当前来源已运行', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: '官方登录', exact: true }).click();
  expect(await calls(page, 'stop_api_connection')).toHaveLength(0);
  await page.getByRole('button', { name: 'API 与中转来源', exact: true }).click();
  await expect(page.locator('.cw-running-title')).toContainText('API 运行中');
});

test('disabled sources do not start and a failed removal stays visible in its dialog', async ({ page }) => {
  await fixture(page); await claude(page);
  const card = sourceCard(page);
  await card.getByRole('button', { name: '禁用', exact: true }).click();
  await expect(card.getByRole('button', { name: '选择此来源', exact: true })).toBeDisabled();
  await card.getByRole('button', { name: '启用', exact: true }).click();
  await expect(card.getByRole('button', { name: '选择此来源', exact: true })).toBeEnabled();
  await page.evaluate(() => { (window as any).apiFixture.removeError = true; });
  await card.getByRole('button', { name: '移除', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '移除 API 来源', exact: true });
  await dialog.getByRole('button', { name: '确认移除', exact: true }).click();
  await expect(dialog.getByRole('alert')).toContainText('模拟移除失败');
  await page.evaluate(() => { (window as any).apiFixture.removeError = false; });
  await dialog.getByRole('button', { name: '确认移除', exact: true }).click();
  await expect(dialog).not.toBeVisible();
  await expect(card).toHaveCount(0);
});

test('API records stay client-scoped and missing usage stays unknown', async ({ page }) => {
  await fixture(page); await claude(page);
  await page.getByRole('button', { name: '请求日志', exact: true }).click();
  await expect(page.locator('.cw-log')).toHaveCount(1);
  await expect(page.locator('.cw-log')).toContainText('api-log-claudeCode');
  await expect(page.locator('.cw-log-usage')).toContainText('总 Token 未采集');
  expect((await calls(page, 'api_request_logs')).every((value: any) => value.args.client === 'claudeCode')).toBe(true);
  expect(await calls(page, 'request_history')).toHaveLength(0);
  await page.evaluate(() => { (window as any).apiFixture.partialUsage = true; });
  await page.getByRole('button', { name: '刷新记录', exact: true }).click();
  await expect(page.locator('.cw-log-top')).toContainText('→ 返回 fixture-returned-model');
  await expect(page.locator('.cw-log-usage')).toContainText('总 Token 未完整采集');
  await expect(page.locator('.cw-log')).toContainText('暂不显示 Token 合计');
  await codex(page);
  await page.getByRole('button', { name: 'API 与中转来源', exact: true }).click();
  await expect(page.locator('.cw-log')).toContainText('api-log-codex');
  await expect(page.locator('.cw-log')).not.toContainText('api-log-claudeCode');
});

test('source drawer fits narrow light and dark workspaces and respects reduced motion', async ({ page }) => {
  await page.setViewportSize({ width: 540, height: 640 });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await fixture(page); await claude(page);
  const screenshots = mkdtempSync(join(tmpdir(), 'fusion-api-ui-'));
  for (const theme of ['light', 'dark']) {
    await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: join(screenshots, `workspace-${theme}.png`), fullPage: true });
    await page.getByRole('button', { name: '＋ 添加来源', exact: true }).click();
    const drawer = page.getByRole('dialog', { name: '添加 API 来源', exact: true });
    await expect(drawer.getByLabel('来源名称', { exact: true })).toBeFocused();
    await expect(drawer.locator('.cw-drawer')).toHaveCSS('animation-name', 'none');
    await expect(drawer.getByRole('button', { name: '保存来源', exact: true })).toBeInViewport();
    expect(await drawer.evaluate(element => element.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: join(screenshots, `drawer-${theme}.png`) });
    await page.keyboard.press('Escape');
    await expect(drawer).not.toBeVisible();
    await expect(page.getByRole('button', { name: '＋ 添加来源', exact: true })).toBeFocused();
  }
  console.log('API workspace UI screenshots:', screenshots);
});
