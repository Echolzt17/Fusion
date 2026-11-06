import { test, expect, type Page } from '@playwright/test';

async function fixture(page: Page, failSend = false) {
  await page.addInitScript(({ failSend }) => {
    const records: any[] = [];
    const calls: { command: string; args: any }[] = [];
    const state = { mode: 'on', records, calls, failSend };
    Object.assign(window, { handoffFixture: state, __TAURI_INTERNALS__: {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        calls.push({ command, args });
        if (command === 'plugin:event|listen') return 1;
        if (command === 'plugin:event|unlisten') return;
        if (command === 'accounts') return [{ id: 'a', email: 'fixture@example.test', state: 'ready', quota: null, quotaError: null, available: true, planLabel: 'Plus' }];
        if (command === 'settings') return { port: 18765, concurrency: 4, fixedAccount: null, selectionPolicy: 'auto' };
        if (command === 'connection_status') return { mode: state.mode, configured: state.mode === 'on', recoveryPending: false, reloadRequired: null, error: null, operation: null };
        if (command === 'scheduling_status') return { policy: 'auto', currentAccountId: 'a', preferredAccountId: null };
        if (command === 'service_status') return { running: state.mode === 'on', draining: false, activeRequests: 0, baseUrl: 'http://127.0.0.1:18765' };
        if (command === 'login_status') return { phase: 'idle', message: '' };
        if (command === 'usage_dashboard') return null;
        if (command === 'usage' || command === 'logs') return [];
        if (command === 'uninstall_requested') return false;
        if (command === 'reset_credits') return { available_count: 0, credits: [] };
        if (command === 'update_status') return { currentVersion: '1.0.9', phase: 'current', operationActive: false, installing: false, startupPromptPending: false };
        if (command === 'image_compatibility') return { client: { desktop: null, engine: null }, verificationState: 'pending', detail: '', desktopAcceptance: 'pending' };
        if (command === 'handoff_chats') return { data: [{ id: 'source-fixture', name: args.archived ? '已归档任务' : '源任务', provider: 'personal_codex_pool', updatedAt: 1 }], nextCursor: null };
        if (command === 'handoff_status') return structuredClone(records);
        if (command === 'handoff_prepare') {
          records.unshift({ id: 'operation-fixture', sourceId: args.sourceId, target: args.target, accountId: args.accountId, targetId: null, phase: 'prepared', message: '已准备' });
          return { id: 'operation-fixture', history: { message: '可读历史：保留约束及已完成的执行结果。', turns: 3, totalTurns: 3, warnings: ['隐藏推理未转移。'], attachments: [] } };
        }
        if (command === 'handoff_create') { records[0].targetId = 'target-fixture'; records[0].phase = 'created'; records[0].message = '聊天已创建，尚未发送'; return structuredClone(records[0]); }
        if (command === 'handoff_send') {
          if (state.failSend) { records[0].phase = 'unknown'; records[0].message = '上次操作结果未确认，不会自动重发'; throw new Error('交接结果未确认'); }
          records[0].phase = 'complete'; records[0].message = '交接轮次已完成'; return structuredClone(records[0]);
        }
        if (command === 'handoff_cancel') { records[0].phase = 'cancelled'; return; }
        if (command === 'handoff_open') return;
        throw new Error('Unexpected fixture command: ' + command);
      }
    } });
  }, { failSend });
  await page.goto('/');
  await page.getByRole('button', { name: '选择聊天续接' }).click();
  await page.getByLabel('源聊天', { exact: true }).selectOption('source-fixture');
  await page.getByLabel('目标账号', { exact: true }).selectOption('a');
}

test('readable handoff requires separate prepare, create and explicit send, then opens only on request', async ({ page }) => {
  await fixture(page);
  await page.getByRole('button', { name: '准备交接', exact: true }).click();
  await expect(page.getByText('隐藏推理未转移。', { exact: true })).toBeVisible();
  await page.getByText('查看完整交接消息').click();
  await expect(page.locator('.handoff-preview pre')).toContainText('保留约束');
  expect(await page.evaluate(() => (window as any).handoffFixture.calls.some((c: any) => c.command === 'handoff_create'))).toBe(false);
  await page.getByRole('button', { name: '创建续接聊天', exact: true }).click();
  await expect(page.getByRole('button', { name: '发送交接并等待确认' })).toBeVisible();
  expect(await page.evaluate(() => (window as any).handoffFixture.calls.some((c: any) => c.command === 'handoff_send' || c.command === 'handoff_open'))).toBe(false);
  await page.getByRole('button', { name: '发送交接并等待确认' }).click();
  await expect(page.getByText('账号池 · 交接轮次已完成')).toBeVisible();
  await page.getByRole('button', { name: '打开目标聊天' }).click();
  expect(await page.evaluate(() => (window as any).handoffFixture.calls.filter((c: any) => c.command === 'handoff_send').length)).toBe(1);
});

test('uncertain send has no replay control and a mismatched route cannot prepare', async ({ page }) => {
  await fixture(page, true);
  await page.getByRole('button', { name: '准备交接', exact: true }).click();
  await page.getByRole('button', { name: '创建续接聊天', exact: true }).click();
  await page.getByRole('button', { name: '发送交接并等待确认' }).click();
  await expect(page.getByRole('alert')).toContainText('交接结果未确认');
  await expect(page.getByRole('button', { name: '发送交接并等待确认' })).toHaveCount(0);
  await page.getByLabel('目标接入', { exact: true }).selectOption('official');
  await expect(page.getByRole('button', { name: '准备交接', exact: true })).toBeDisabled();
  await expect(page.getByText('请先关闭账号池并恢复官方接入，再准备交接。')).toBeVisible();
});
