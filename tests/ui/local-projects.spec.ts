import { test, expect, type Page } from '@playwright/test';

async function fixture(page: Page, accountReady = true) {
  await page.addInitScript(({ accountReady }) => {
    localStorage.setItem('pool-theme', 'light');
    const state = { calls: [] as string[], opens: [] as unknown[], failRead: false, failRestore: false, accountReady, restored: false };
    Object.assign(window, { localProjectsFixture: state, __TAURI_INTERNALS__: {
      transformCallback: () => 1,
      invoke: async (command: string, args: unknown) => {
        state.calls.push(command);
        switch (command) {
          case 'plugin:event|listen': return 1;
          case 'plugin:event|unlisten': return;
          case 'main_visible': return true;
          case 'uninstall_requested': return false;
          case 'accounts': case 'api_client_status': case 'logs': case 'usage': return [];
          case 'settings': return { port: 18765, concurrency: 4, fixedAccount: null, selectionPolicy: 'auto' };
          case 'connection_status': return { mode: 'off', configured: false, recoveryPending: false, operation: null, phase: null };
          case 'service_status': return { running: false, draining: false, activeRequests: 0, requests: [] };
          case 'login_status': return { phase: 'idle', message: '' };
          case 'scheduling_status': return { policy: 'auto', currentAccountId: null, preferredAccountId: null };
          case 'image_compatibility': return { client: {}, detail: '', desktopAcceptance: 'pending' };
          case 'usage_dashboard': case 'ai_pricing': return null;
          case 'conversation_usage': return { conversations: [] };
          case 'request_history': return { items: [], nextCursor: null };
          case 'update_status': return { currentVersion: '1.0.12', phase: 'current', startupPromptPending: false, operationActive: false, installing: false };
          case 'network_status': return { settings: { enabled: false, mode: 'auto', allowedRegions: [], nodeOrder: [], fixedNodeId: null, excludedNodeIds: [], regionOverrides: {} }, kernelRunning: false, nodes: [], phase: 'off' };
          case 'local_projects_status':
            if (state.failRead) throw new Error('目录正在变化，请重试');
            return { accountReady: state.accountReady, notice: '恢复本机目录入口会将 Codex 切到空白聊天输入页；不会发送消息或迁移原账号聊天。请先保存草稿。', entries: [
              { id: 'local-a', name: 'VPN 本机工程', roots: ['D:\\Projects\\本机项目\\long-directory-name-for-local-work'], order: 0, present: state.restored, available: true, canRestore: true },
              { id: 'local-b', name: '暂时离线的目录', roots: ['E:\\local-project'], order: 1, present: false, available: false, canRestore: false },
            ] };
          case 'restore_local_projects':
            await new Promise(resolve => setTimeout(resolve, 200));
            if (state.failRestore) throw new Error('Codex 已关闭，未打开项目入口');
            state.restored = true;
            return { restored: 1, alreadyPresent: 0, skippedUnavailable: 1, skippedMultiRoot: 0, remaining: 0, pendingConfirmation: false, stopped: false, message: '本机目录入口已核对；原账号聊天未迁移' };
          case 'handoff_chats': return { data: [{ id: '0199b551-3c14-7431-91aa-195be1881d11', name: '本机原聊天', updatedAt: 1794355200 }], nextCursor: null };
          case 'open_local_chat': state.opens.push(args); return;
          default: throw new Error('Unexpected fixture IPC: ' + command);
        }
      },
    } });
  }, { accountReady });
  await page.goto('/');
  await page.getByRole('button', { name: '连接与设置', exact: true }).click();
  await expect(page.getByRole('region', { name: '本机项目', exact: true })).toContainText('本机记忆 2 个项目');
}

test('restoring directory entries is explicit, single-submit, and preserves honest chat boundaries', async ({ page }, testInfo) => {
  const errors: string[] = []; page.on('pageerror', error => errors.push(error.message));
  await fixture(page);
  await expect(page).toHaveTitle('Fusion');
  const card = page.getByRole('region', { name: '本机项目', exact: true });
  await expect(card).toContainText('请先保存草稿');
  await expect(card).toContainText('不恢复或迁移原账号聊天');
  await card.scrollIntoViewIfNeeded();
  await page.screenshot({ path: testInfo.outputPath('local-projects-light-desktop.png') });
  expect(await page.evaluate(() => (window as any).localProjectsFixture.calls.filter((c: string) => c === 'restore_local_projects'))).toHaveLength(0);
  const button = card.getByRole('button', { name: '恢复本机项目入口', exact: true });
  await button.focus(); await page.keyboard.press('Enter');
  await expect(card.getByRole('button', { name: '正在恢复…', exact: true })).toBeDisabled();
  await expect(card).toContainText('已确认恢复 1 个入口');
  await expect(card).toContainText('暂不可用 1 个');
  expect(await page.evaluate(() => (window as any).localProjectsFixture.calls.filter((c: string) => c === 'restore_local_projects'))).toHaveLength(1);
  await expect(button).toBeDisabled();
  expect(errors).toEqual([]);
});

test('the local history launcher opens the original native chat without creating or sending history', async ({ page }) => {
  await fixture(page);
  const card = page.getByRole('region', { name: '本机项目', exact: true });
  await card.locator('.local-chat-history summary').click();
  await card.getByRole('button', { name: '读取本机历史', exact: true }).click();
  await card.getByRole('combobox', { name: '本机聊天', exact: true }).selectOption('0199b551-3c14-7431-91aa-195be1881d11');
  await card.getByRole('button', { name: '在 Codex 打开聊天', exact: true }).click();
  await expect(card).toContainText('没有新建、复制或发送聊天');
  expect(await page.evaluate(() => (window as any).localProjectsFixture.opens)).toEqual([{ id: '0199b551-3c14-7431-91aa-195be1881d11', cursor: null, archived: false }]);
  expect(await page.evaluate(() => (window as any).localProjectsFixture.calls.filter((command: string) => ['handoff_prepare', 'handoff_create', 'handoff_send'].includes(command)))).toEqual([]);
});

test('read and restore failures remain inline without dropping known local directories', async ({ page }) => {
  await fixture(page);
  const card = page.getByRole('region', { name: '本机项目', exact: true });
  await card.locator('.local-project-details summary').click();
  await expect(card).toContainText('VPN 本机工程');
  await page.evaluate(() => { (window as any).localProjectsFixture.failRead = true; });
  await card.getByRole('button', { name: '刷新目录', exact: true }).click();
  await expect(card.getByRole('alert')).toContainText('保留上次目录列表');
  await expect(card).toContainText('VPN 本机工程');
  await page.evaluate(() => { (window as any).localProjectsFixture.failRestore = true; });
  await card.getByRole('button', { name: '恢复本机项目入口', exact: true }).click();
  await expect(card.getByRole('alert')).toContainText('Codex 已关闭，未打开项目入口');
  await expect(card).not.toContainText('已确认恢复');
});

test('signed-out and narrow dark settings preserve readable directory metadata', async ({ page }, testInfo) => {
  await fixture(page, false);
  const card = page.getByRole('region', { name: '本机项目', exact: true });
  await expect(card.getByRole('button', { name: '恢复本机项目入口', exact: true })).toBeDisabled();
  await expect(card).toContainText('请先在 Codex 完成登录');
  await card.locator('.local-project-details summary').click();
  await page.setViewportSize({ width: 620, height: 850 });
  await page.locator('.theme-switch').click();
  await card.scrollIntoViewIfNeeded();
  await expect(card.getByText('VPN 本机工程', { exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.screenshot({ path: testInfo.outputPath('local-projects-dark-narrow.png') });
});
