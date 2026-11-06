import { test, expect, type Page } from '@playwright/test';

async function fixture(page: Page, empty = false) {
  await page.addInitScript(({ empty }) => {
    const tokens = { inputTokens: 2_800_000, outputTokens: 40_000, totalTokens: 2_840_000, cachedTokens: 1_200_000, reasoningTokens: 3000 };
    const data = { tokens, measured: !empty, today: '2026-11-04', timezone: 'UTC+08:00', updatedAt: 1793772000000, scanning: false, historyPartial: true, activityPartial: true, error: null as string | null,
      days: ['10-29','10-30','10-31','11-01','11-02','11-03','11-04'].map((day, i) => ({ date: `2026-${day}`, totalTokens: empty ? null : [null, 32000, 0, 200000, 800000, 508000, 1300000][i] })),
      hours: Array.from({ length: 24 }, (_, hour) => ({ hour, minutes: empty || hour > 14 ? null : hour === 11 ? 60 : hour === 10 ? 45 : hour === 14 ? 10.5 : 0, provisional: hour === 14 })),
    };
    const state = { data, calls: [] as string[], callbacks: {} as Record<number, (value: unknown) => void> };
    Object.assign(window, { usageFixture: state, __TAURI_INTERNALS__: {
      transformCallback: (cb: (value: unknown) => void) => { const id = Object.keys(state.callbacks).length + 1; state.callbacks[id] = cb; return id; },
      invoke: async (command: string) => {
        state.calls.push(command);
        switch (command) {
          case 'usage_dashboard': return state.data;
          case 'accounts': case 'usage': case 'logs': return [];
          case 'settings': return { port: 18765, concurrency: 4, fixedAccount: null, selectionPolicy: 'auto' };
          case 'service_status': return { running: false, draining: false, activeRequests: 0, baseUrl: '' };
          case 'connection_status': return { mode: 'off', configured: false, recoveryPending: false };
          case 'login_status': return { phase: 'idle', message: '' };
          case 'scheduling_status': return { policy: 'auto' };
          case 'image_compatibility': return { configurationReady: false };
          case 'update_status': return { phase: 'idle', startupPromptPending: false };
          case 'main_visible': return true;
          case 'uninstall_requested': return false;
          case 'conversation_usage': return { conversations: [] };
          case 'request_history': return { items: [], nextCursor: null };
          case 'error_report_status': return { phase: 'idle', history: [], historyError: null };
          default: return null;
        }
      },
    } });
  }, { empty });
  await page.goto('/');
  await expect(page.getByRole('region', { name: '本机 Codex 用量' })).toBeVisible();
}

test('official-only dashboard works with no pool accounts; click and keyboard toggle persists', async ({ page }) => {
  await fixture(page);
  const panel = page.getByRole('region', { name: '本机 Codex 用量' });
  await expect(panel).toContainText('累计 2.84M');
  const chart = page.getByRole('button', { name: /近 7 天柱状图/ });
  await expect(chart.locator('.chart-column')).toHaveCount(7);
  await chart.click();
  const line = page.getByRole('button', { name: /近 7 天折线图/ });
  await expect(line.locator('.usage-week-line')).toBeVisible();
  await page.reload();
  await expect(line).toBeVisible();
  await line.focus();
  await page.keyboard.press('Enter');
  await expect(chart).toBeVisible();
  await page.keyboard.press('Space');
  await expect(line).toBeVisible();
  await expect(panel.getByRole('button', { name: '账号池明细' })).toBeVisible();
});

test('hour chart shows 10 minute grid, exact hover values, provisional data and no future zeros', async ({ page }) => {
  await fixture(page);
  const chart = page.locator('.usage-hours');
  await expect(chart.locator('.usage-gridline')).toHaveCount(7);
  await expect(chart.locator('.usage-point')).toHaveCount(15);
  await expect(chart.locator('.usage-hour-label')).toHaveCount(24);
  const point = chart.getByRole('img', { name: '14:00—15:00：10.5 分钟（暂计）', exact: true });
  await point.hover();
  await expect(chart.locator('.usage-hour-detail')).toHaveText('14:00—15:00：10.5 分钟（暂计）');
  await point.focus();
  await expect(chart.locator('.usage-hour-detail')).toContainText('10.5 分钟');
  await expect(chart.getByRole('img', { name: '15:00—16:00：0 分钟', exact: true })).toHaveCount(0);
  await expect(page.locator('.usage-dashboard')).toContainText('UTC+08:00');
});

test('empty and failed scans never invent token counts and keep readable cached totals', async ({ page }) => {
  await fixture(page, true);
  const panel = page.locator('.usage-dashboard');
  await expect(panel).toContainText('暂无用量');
  await expect(panel).toContainText('暂无时长记录');
  await expect(panel.locator('.token-metrics strong')).toHaveText(['—', '—']);
  await expect(panel.locator('.usage-point')).toHaveCount(0);
  await page.evaluate(() => { const s = (window as any).usageFixture; s.data.measured = true; s.data.error = '部分本机用量暂不可读，已保留上次记录'; });
  await expect(panel).toContainText('累计 2.84M', { timeout: 8000 });
  await expect(panel).toContainText('已保留上次记录');
  await expect(panel).toContainText('部分历史缺少明细');
});

test('usage card remains within the existing surface at narrow and wide widths in both themes', async ({ page }) => {
  await fixture(page);
  for (const width of [840, 1080, 1920]) {
    await page.setViewportSize({ width, height: 1100 });
    for (const theme of ['light', 'dark']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      const panel = page.locator('.usage-dashboard');
      await expect(panel).toBeVisible();
      expect(await panel.evaluate(e => e.scrollWidth <= e.clientWidth + 1)).toBe(true);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      const labels = await panel.locator('.usage-hour-label.major').evaluateAll(nodes => nodes.map(node => { const r=node.getBoundingClientRect(); return {left:r.left,right:r.right}; }));
      expect(labels.every((label,i)=>i===0 || label.left>=labels[i-1].right)).toBe(true);
      await panel.screenshot({ path: `test-results/usage-${width}-${theme}.png` });
    }
  }
  await page.getByRole('button', { name: /近 7 天柱状图/ }).click();
  await page.locator('.usage-dashboard').screenshot({ path: 'test-results/usage-line-dark.png' });
});
