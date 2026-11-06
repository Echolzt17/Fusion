import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
const pkg = JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8'));

test('cold production page loads all entry assets and renders the current version', async ({ page }) => {
  const failures: string[] = [];
  page.on('requestfailed', request => failures.push(new URL(request.url()).pathname));
  const response = await page.goto('/', { waitUntil: 'load' });
  expect(response?.status()).toBe(200);
  await expect(page.getByText(`Fusion ${pkg.version}`, { exact: true })).toBeVisible();
  const resources = await page.evaluate(() => performance.getEntriesByType('resource').map(r => new URL(r.name).pathname));
  expect(resources.some(path => path.startsWith('/assets/') && path.endsWith('.js'))).toBe(true);
  expect(resources.some(path => path.startsWith('/assets/') && path.endsWith('.css'))).toBe(true);
  expect(resources.some(path => path.startsWith('/src/') || path.startsWith('/@vite/'))).toBe(false);
  expect(failures).toEqual([]);
});
