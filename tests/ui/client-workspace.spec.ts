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
          case 'claude_pool_status': return {accounts:['a','b'].map((id,index)=>({id:'claude-'+id,email:index ? 'second-claude@example.test' : 'claude@example.test',planLabel:index ? 'Free' : 'Pro',state:'ready',cooldownUntil:0,available:index===0,quota:{rateLimit:{primary_window:{used_percent:index ? 100 : 18,limit_window_seconds:18000}},updatedAt:at},quotaError:null,tokens:{inputTokens:0,outputTokens:0,totalTokens:0},measuredRequests:0,requests:0})),preferred:(state as any).claudePreferred ?? null,client:state.statuses[1],login:{phase:'idle',message:''},logs:[]};
          case 'claude_pool_account': if(args.action==='select') (state as any).claudePreferred=args.id; return;
          case 'claude_pool_login': return;
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
const workspacePicker = (page: Page) => page.getByRole('combobox', { name: '工作区', exact: true });
async function workspace(page: Page, name: 'Codex' | 'Claude Code' | 'API') {
  await workspacePicker(page).click();
  await page.getByRole('listbox', { name: '工作区', exact: true }).getByRole('option', { name, exact: true }).click();
  await expect(workspacePicker(page)).toHaveAttribute('aria-expanded', 'false');
}
async function api(page: Page, client: 'codex' | 'claudeCode' = 'claudeCode') {
  await workspace(page, 'API');
  await page.getByRole('combobox', { name: '目标客户端', exact: true }).selectOption(client);
}
const codex = (page: Page) => workspace(page, 'Codex');
const official = (page: Page) => workspace(page, 'Claude Code');
const sourceCard = (page: Page, name = 'Claude 测试来源') => page.getByRole('article', { name: '来源 ' + name, exact: true });

test('the unified workspace dropdown has three English choices and supports keyboard and outside dismissal', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await fixture(page);
  const picker = workspacePicker(page);
  const list = page.getByRole('listbox', { name: '工作区', exact: true });
  await expect(picker).toHaveText('Codex');
  await expect(picker).toHaveCSS('font-weight', '600');
  await expect(picker).toHaveCSS('font-family', /^Poppins,/);
  await expect(picker).toHaveCSS('border-radius', '100px');
  expect(await page.evaluate(async () => {
    await document.fonts.ready;
    return [...document.fonts].some(font => font.family === 'Poppins' && font.weight === '600' && font.status === 'loaded');
  })).toBe(true);
  await expect(picker.locator('.cw-switch-arrow g')).toHaveCount(3);
  const restingWidth = await picker.evaluate(element => element.getBoundingClientRect().width);
  await picker.hover();
  await expect(picker.locator('.cw-switch-arrow')).toHaveCSS('margin-right', '16px');
  expect(await picker.evaluate(element => element.getBoundingClientRect().width)).toBeCloseTo(restingWidth + 16, 0);
  for (const [index, delay] of ['0.6s', '0.4s', '0.2s'].entries()) {
    await expect(picker.locator('.cw-switch-arrow g').nth(index)).toHaveCSS('animation-name', 'cw-switch-color');
    await expect(picker.locator('.cw-switch-arrow g').nth(index)).toHaveCSS('animation-delay', delay);
  }
  await expect(page.locator('.cw-client-picker, .cw-source-tabs')).toHaveCount(0);
  await expect(page.locator('main > header')).not.toContainText('账号控制台');
  await picker.focus();
  await page.keyboard.press('ArrowDown');
  await expect(list.getByRole('option')).toHaveCount(3);
  for (const name of ['Codex', 'Claude Code', 'API']) {
    await expect(list.getByRole('option', { name, exact: true })).toBeVisible();
  }
  await page.keyboard.press('End');
  await page.keyboard.press('Enter');
  await expect(picker).toHaveText('API');
  await expect(picker).toHaveAttribute('aria-expanded', 'false');
  await expect(page.getByRole('combobox', { name: '目标客户端', exact: true })).toHaveValue('codex');

  await page.keyboard.press('ArrowUp');
  await page.keyboard.press('Home');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('ArrowUp');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  await expect(picker).toHaveText('Claude Code');
  await expect(page.getByRole('heading', {name:'claude@example.test',exact:true})).toBeVisible();
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Escape');
  await expect(list).not.toBeVisible();
  await expect(picker).toHaveText('Claude Code');
  await expect(picker).toBeFocused();
  await picker.click();
  await page.keyboard.press('Tab');
  await expect(list).not.toBeVisible();
  await picker.click();
  await page.getByRole('button', { name: '账号控制台', exact: true }).click();
  await expect(list).not.toBeVisible();
  for (const command of ['start_api_connection', 'stop_api_connection', 'start_pool', 'close_pool', 'open_claude_login']) {
    expect(await calls(page, command)).toHaveLength(0);
  }
});

test('the longest capsule expands without clipping its label or overflowing narrow light and dark windows', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await fixture(page);
  await official(page);
  await page.evaluate(() => document.fonts.ready);
  const picker = workspacePicker(page);
  for (const width of [1080, 320]) {
    await page.setViewportSize({ width, height: 700 });
    for (const theme of ['light', 'dark']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await page.mouse.move(0, 0);
      await page.getByRole('button', { name: '账号控制台', exact: true }).focus();
      await expect(picker.locator('.cw-switch-arrow')).toHaveCSS('margin-right', '0px');
      const restingWidth = await picker.evaluate(element => element.getBoundingClientRect().width);
      await picker.hover();
      const expansion = width === 320 ? 12 : 16;
      await expect(picker.locator('.cw-switch-arrow')).toHaveCSS('margin-right', `${expansion}px`);
      expect(await picker.evaluate(element => element.getBoundingClientRect().width)).toBeCloseTo(restingWidth + expansion, 0);
      expect(await picker.evaluate(element => {
        const label = element.querySelector('span')!;
        const shapes = [element, ...element.querySelectorAll('.cw-switch-arrow path')];
        return label.scrollWidth <= label.clientWidth + 1 && shapes.every(shape => {
          const rect = shape.getBoundingClientRect();
          return rect.left >= 0 && rect.right <= innerWidth;
        });
      })).toBe(true);
      const overflow = await page.evaluate(() => ({width:innerWidth,scroll:document.documentElement.scrollWidth,offenders:[...document.querySelectorAll('main *')].filter(el => el.getBoundingClientRect().right > innerWidth + 1).map(el => ({tag:el.tagName,cls:el.className,right:Math.round(el.getBoundingClientRect().right)})).slice(0,16)}));
      expect(overflow.scroll,JSON.stringify(overflow)).toBeLessThanOrEqual(overflow.width);
    }
  }
});

test('workspace and API client view changes preserve the subscription page and never switch a connection', async ({ page }) => {
  await fixture(page);
  await api(page);
  await expect(sourceCard(page)).toBeVisible();
  await sourceCard(page).getByRole('button', { name: '选择此来源', exact: true }).click();
  await expect(sourceCard(page)).toContainText('候选已选中 · 尚未启动');
  expect(await calls(page, 'start_api_connection')).toHaveLength(0);
  expect(await calls(page, 'stop_api_connection')).toHaveLength(0);
  await codex(page);
  await expect(page.getByRole('heading', { name: 'subscription@example.test', exact: true })).toBeVisible();
  await official(page);
  await expect(page.getByRole('heading', {name:'claude@example.test',exact:true})).toBeVisible();
  expect(await calls(page, 'open_claude_login')).toHaveLength(0);
  await page.getByRole('button', { name: '添加账号', exact: true }).click();
  expect(await calls(page, 'claude_pool_login')).toHaveLength(1);
  expect(await calls(page, 'login')).toHaveLength(0);
  expect(await calls(page, 'start_api_connection')).toHaveLength(0);
});

test('API form masks the key, preserves an existing key on blank edit, and does not start on save', async ({ page }) => {
  await fixture(page); await api(page);
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
  await fixture(page); await api(page);
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
  await fixture(page); await api(page);
  await page.evaluate(() => { (window as any).apiFixture.holdStart = true; (window as any).apiFixture.failStart = true; });
  await sourceCard(page).getByRole('button', { name: '选择此来源', exact: true }).click();
  await page.getByRole('button', { name: '启动选中来源', exact: true }).click();
  await expect(page.locator('.cw-running-title')).toContainText('正在启动 API');
  await expect(page.locator('.cw-progress')).toContainText('等待在途请求完成');
  await codex(page);
  await expect(page.getByRole('heading', { name: 'subscription@example.test', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '添加账号', exact: true })).toBeEnabled();
  await page.evaluate(() => (window as any).apiFixture.releaseStart?.());
  await api(page);
  await expect(page.locator('.cw-danger')).toContainText('模拟配置冲突');
  await sourceCard(page).getByRole('button', { name: '选择此来源', exact: true }).click();
  await expect(page.getByRole('button', { name: '切换到选中来源', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: '重试恢复并关闭', exact: true }).click();
  await expect(page.locator('.cw-running-title')).toContainText('API 已关闭');
  expect((await calls(page, 'stop_api_connection')).at(-1).args.client).toBe('claudeCode');
  expect(await calls(page, 'close_pool')).toHaveLength(0);
});

test('a selected source starts only on an explicit action and cannot be removed while in use', async ({ page }) => {
  await fixture(page); await api(page);
  const card = sourceCard(page);
  await card.getByRole('button', { name: '选择此来源', exact: true }).click();
  await page.getByRole('button', { name: '启动选中来源', exact: true }).click();
  await expect(page.locator('.cw-running-title')).toContainText('API 运行中');
  expect((await calls(page, 'start_api_connection')).at(-1).args).toEqual({ client: 'claudeCode', sourceId: 'claude-api' });
  await expect(card.getByRole('button', { name: '禁用', exact: true })).toBeDisabled();
  await expect(card.getByRole('button', { name: '移除', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: '当前来源已运行', exact: true })).toBeDisabled();
  await official(page);
  expect(await calls(page, 'stop_api_connection')).toHaveLength(0);
  await api(page);
  await expect(page.locator('.cw-running-title')).toContainText('API 运行中');
});

test('disabled sources do not start and a failed removal stays visible in its dialog', async ({ page }) => {
  await fixture(page); await api(page);
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
  await fixture(page); await api(page);
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
  await page.getByRole('combobox', { name: '目标客户端', exact: true }).selectOption('codex');
  await expect(page.locator('.cw-log')).toContainText('api-log-codex');
  await expect(page.locator('.cw-log')).not.toContainText('api-log-claudeCode');
});

test('source drawer fits narrow light and dark workspaces and respects reduced motion', async ({ page }) => {
  await page.setViewportSize({ width: 540, height: 640 });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await fixture(page); await api(page);
  const screenshots = mkdtempSync(join(tmpdir(), 'fusion-api-ui-'));
  for (const theme of ['light', 'dark']) {
    await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await workspacePicker(page).hover();
    await expect(workspacePicker(page)).toHaveCSS('animation-name', 'none');
    await expect(workspacePicker(page).locator('.cw-switch-arrow-one')).toHaveCSS('animation-name', 'none');
    await page.screenshot({ path: join(screenshots, `workspace-${theme}.png`), fullPage: true });
    await workspacePicker(page).click();
    const menu = page.getByRole('listbox', { name: '工作区', exact: true });
    await expect(menu).toBeInViewport();
    await expect(page.locator('.record-select-popover')).toHaveCSS('animation-name', 'none');
    await page.screenshot({ path: join(screenshots, `workspace-menu-${theme}.png`) });
    await page.keyboard.press('Escape');
    await expect(menu).not.toBeVisible();
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

 test('Claude subscription cards have their own selection and quota controls', async ({ page }) => {
  await fixture(page); await official(page);
  await expect(page.locator('.claude-pool-workspace .account')).toHaveCount(2);
  const second=page.locator('.claude-pool-workspace .account').filter({hasText:'second-claude@example.test'});
  await expect(second).toContainText('Free');
  await expect(second.locator('.quota-number').first()).toContainText('0');
  const first=page.locator('.claude-pool-workspace .account').filter({has:page.getByRole('heading',{name:'claude@example.test',exact:true})});
  await first.getByRole('button',{name:'使用此账号',exact:true}).click();
  await expect(first).toHaveClass(/active-account/);
  expect((await calls(page,'claude_pool_account'))[0].args).toEqual({action:'select',id:'claude-a'});
  expect(await calls(page,'select_account')).toHaveLength(0);
  const output=mkdtempSync(join(tmpdir(),'fusion-claude-pool-ui-'));
  await page.setViewportSize({width:1800,height:1100});
  await page.evaluate(()=>window.scrollTo(0,0));
  await expect(page.getByRole('button',{name:'添加账号',exact:true})).toBeEnabled();
  await page.screenshot({path:join(output,'claude-pool-light.png'),fullPage:true,animations:'disabled'});
  await page.evaluate(()=>document.documentElement.dataset.theme='dark');
  await page.screenshot({path:join(output,'claude-pool-dark.png'),fullPage:true,animations:'disabled'});
  console.log('Claude pool UI screenshots: '+output);
});
