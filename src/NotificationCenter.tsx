import { useEffect, useRef, useState } from 'react';
import { Modal } from './Modal';
import { createNotificationStore, type Notification } from './notifications';
import './notifications.css';

export function useNotifications() {
  const [items, setItems] = useState<Notification[]>([]);
  const [store] = useState(() => createNotificationStore(setItems));
  return { items, store };
}
const levels = { info: '提示', success: '已完成', warning: '警告', error: '错误' };
const time = (at: number) => new Date(at).toLocaleString('zh-CN', { hour12: false });

export function NotificationCenter({ items, store }: ReturnType<typeof useNotifications>) {
  const [open, setOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const opener = useRef<HTMLButtonElement>(null);
  const unread = items.filter(item => !item.read).length;
  const selected = items.find(item => item.id === selectedId);
  useEffect(() => {
    if (!open) return;
    const closeOutside = (event: PointerEvent) => { if (!root.current?.contains(event.target as Node)) setOpen(false); };
    const escape = (event: KeyboardEvent) => { if (event.key === 'Escape') { setOpen(false); opener.current?.focus(); } };
    document.addEventListener('pointerdown', closeOutside); document.addEventListener('keydown', escape);
    return () => { document.removeEventListener('pointerdown', closeOutside); document.removeEventListener('keydown', escape); };
  }, [open]);
  return <div className="notification-center" ref={root}>
    <button ref={opener} className={'notification-trigger' + (unread ? ' has-unread' : '')} aria-label={`通知中心，${unread} 条未读`} title="通知中心" aria-expanded={open} aria-controls="notification-inbox" onClick={() => setOpen(value => !value)}>
      <span className="notification-star" aria-hidden="true">{[1, 2, 3, 4, 5].map(n => <i key={n} />)}</span>
      {unread > 0 && <span className="notification-count">{unread > 99 ? '99+' : unread}</span>}
    </button>
    <span className="sr-only" role="status" aria-live="polite">{unread ? `${unread} 条未读通知` : '暂无未读通知'}</span>
    {open && <div className="notification-inbox" id="notification-inbox" role="region" aria-label="通知列表">
      <div className="notification-heading"><div><h2>通知中心</h2><span>{unread ? `${unread} 条未读` : '全部已读'} · 最近 {items.length} 条</span></div><button disabled={!unread} onClick={() => store.markAllRead()}>全部已读</button></div>
      {items.length === 0 ? <div className="notification-empty"><span aria-hidden="true">✦</span><p>暂无通知</p><small>操作结果与请求异常会汇总在这里</small></div> : <ul className="notification-list">{items.map(item => <li key={item.id}><button className={'notification-row ' + item.level + (item.read ? ' is-read' : '')} onClick={() => { store.markRead(item.id); setOpen(false); setSelectedId(item.id); }}>
        <span className="notification-level" aria-hidden="true">{item.level === 'error' || item.level === 'warning' ? '!' : item.level === 'success' ? '✓' : 'i'}</span><span className="notification-copy"><strong>{item.title}</strong><small>{time(item.lastAt)}{item.count > 1 ? ` · 已合并 ${item.count} 次` : ''}</small></span>{!item.read && <i className="notification-unread" aria-label="未读" />}
      </button></li>)}</ul>}
      <div className="notification-footer">本次运行的通知 · 传输异常在 5 分钟内汇总，其他异常按对话合并</div>
    </div>}
    {selected && <Modal label="通知详情" busy={false} returnFocus={opener} onClose={() => setSelectedId(null)}><article className={'notification-card ' + selected.level}>
      <div className="notification-card-heading"><span className="notification-card-icon" aria-hidden="true">{selected.level === 'success' ? '✓' : selected.level === 'info' ? 'i' : '!'}</span><div><small>FUSION / {levels[selected.level]}</small><h2>{selected.title}</h2></div></div>
      <div className="notification-card-message"><p>{selected.message}</p>{selected.requestId && <dl><div><dt>请求类型</dt><dd>{selected.operation}</dd></div><div><dt>最近请求</dt><dd>{selected.requestId}</dd></div><div><dt>对话标识</dt><dd>{selected.conversation || '旧日志或请求未提供对话标识'}</dd></div></dl>}</div>
      {!!selected.requests?.length && <details className="notification-requests"><summary>查看涉及的 {selected.count} 条请求</summary><ol>{selected.requests.map(r=><li key={r.id}><time>{time(r.at)}</time><span>{r.operation} · {r.status}</span><code>{r.id}</code><small>{r.conversation ? `对话 ${r.conversation}` : '对话未知'}</small></li>)}</ol>{!!selected.omittedRequests && <p>更早的 {selected.omittedRequests} 条已超出通知明细保留量，可查看请求日志。</p>}</details>}
      <div className="notification-card-meta"><span>首次发生 · {time(selected.at)}</span>{selected.count > 1 && <><span>最后发生 · {time(selected.lastAt)}</span><span>同类异常已合并 {selected.count} 次</span></>}</div>
      <button className="notification-okay" onClick={() => setSelectedId(null)}>OKAY</button>
    </article></Modal>}
  </div>;
}
