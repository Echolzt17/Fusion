export const QUOTA_REFRESH_SECONDS = 600;

export function quotaIsStale(updatedAt: number, now = Date.now()): boolean {
  const age = now / 1000 - updatedAt;
  return !Number.isFinite(age) || age < 0 || age > QUOTA_REFRESH_SECONDS;
}

export function quotaUpdatedLabel(updatedAt: number, now = Date.now()): string {
  const age = now / 1000 - updatedAt;
  if (!Number.isFinite(age) || age < 0) return '更新时间待核实';
  if (age < 60) return '刚刚更新';
  if (age < 3600) return `${Math.floor(age / 60)} 分钟前更新`;
  if (age < 86400) return `${Math.floor(age / 3600)} 小时前更新`;
  return `${Math.floor(age / 86400)} 天前更新`;
}
