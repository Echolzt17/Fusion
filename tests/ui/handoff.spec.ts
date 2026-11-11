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
 }

test('the account console has no manual handoff chooser or automatic history migration', async ({ page }) => {
  await fixture(page);
  await expect(page.getByRole('heading', { name: '我的账号' })).toBeVisible();
  await expect(page.getByRole('button', { name: '选择聊天续接' })).toHaveCount(0);
  await expect(page.locator('.handoff-section')).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).handoffFixture.calls.filter((c: any) => c.command.startsWith('handoff_')))).toHaveLength(0);
});
