import { useEffect, useState } from 'react';
import { type Account } from './PoolTelemetry';

export function subscriptionRemaining(expiresAt: number, now: number): string {
  const seconds = Math.max(0, Math.ceil(expiresAt - now / 1000));
  if (!seconds) return '快照已到期，待确认';
  const days = Math.floor(seconds / 86400);
  const part = (n: number) => String(n).padStart(2, '0');
  return `剩余 ${days} 天 ${part(Math.floor(seconds / 3600) % 24)}:${part(Math.floor(seconds / 60) % 60)}:${part(seconds % 60)}`;
}

export function SubscriptionCountdown({ account }: { account: Account }) {
  const [now, setNow] = useState(Date.now);
  const free = (account.quota?.planType || account.planLabel).toLowerCase() === 'free';
  const snapshot = account.quota?.subscription;
  const expiresAt = snapshot?.expiresAt;
  const known = !free && typeof expiresAt === 'number' && Number.isFinite(expiresAt) && expiresAt >= 946684800 && expiresAt <= 253402300799;
  useEffect(() => {
    setNow(Date.now());
    if (!known) return;
    const update = () => { if (!document.hidden) setNow(Date.now()); };
    const timer = setInterval(update, 1000);
    document.addEventListener('visibilitychange', update);
    return () => { clearInterval(timer); document.removeEventListener('visibilitychange', update); };
  }, [known, expiresAt]);
  const date = (n: number) => new Date(n * 1000).toLocaleString('zh-CN', { hour12: false });
  const title = free ? '免费套餐没有会员到期倒计时' : known
    ? `授权快照到期时间：${date(expiresAt)}${snapshot?.checkedAt ? `；上游核对时间：${date(snapshot.checkedAt)}` : ''}。续费后可能延迟更新，以官方订阅页面为准。`
    : '上游授权未提供与当前账号及套餐匹配的有效到期时间；重新授权后可再次查询。';
  return <div className={`subscription-expiry${known && expiresAt - now / 1000 <= 3 * 86400 ? ' is-soon' : ''}`} title={title}>
    <span>会员到期{known && <small>授权快照</small>}</span>
    {known ? <time dateTime={new Date(expiresAt * 1000).toISOString()}>{subscriptionRemaining(expiresAt, now)}</time> : <span className="subscription-unknown">{free ? '免费套餐，无需倒计时' : '到期时间未知'}</span>}
  </div>;
}
