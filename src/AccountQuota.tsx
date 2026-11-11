import type { WindowQuota } from './PoolTelemetry';
const date = (n: number) => new Date(n * 1000).toLocaleString('zh-CN', { hour12: false });
export function Quota({ data, fallback, stale }: { data?: WindowQuota; fallback: string; stale: boolean }) {
  const known = typeof data?.used_percent === 'number' && Number.isFinite(data.used_percent);
  const remaining = known ? Math.round(Math.max(0, Math.min(100, 100 - data!.used_percent!))) : null;
  const seconds = data?.limit_window_seconds;
  const title = seconds ? (seconds >= 86400 ? `${Math.round(seconds / 86400)} 天周期` : `${Math.round(seconds / 3600)} 小时周期`) : fallback;
  const old = stale || !!data?.reset_at && data.reset_at <= Date.now() / 1000;
  return <div className={'quota-window ' + (remaining !== null && remaining < 20 ? 'low' : '')}>
    <div className="quota-title"><span>{title}</span><span>{old ? '上次查询' : '剩余额度'}</span></div>
    <div className="quota-number">{remaining === null ? '—' : remaining}<small>{remaining === null ? '待查询' : '%'}</small></div>
    <div className="meter" role="progressbar" aria-label={`${title}剩余额度`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={remaining ?? undefined}><i style={{ width: `${remaining ?? 0}%` }} /></div>
    <div className="quota-reset"><span>重置</span>{data?.reset_at ? <time dateTime={new Date(data.reset_at * 1000).toISOString()} title={date(data.reset_at)}>{new Date(data.reset_at * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false })}</time> : <span>时间未提供</span>}</div>
  </div>;
}
