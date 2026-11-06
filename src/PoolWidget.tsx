import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { WidgetSummary, type WidgetDetails } from './WidgetSummary';
import './widget.css';

type Control = { instance: string; revision: number; expanded: boolean; ready: boolean };
type Pointer = { instance: string; sequence: number; gesture: number; kind: string; x: number; y: number; delta: number };
type Snapshot = {
  control?: Control;
  on: boolean; storageWarning?: string | null; remaining?: number | null;
  totalRemaining?: number | null; quotaAccounts?: number; totalAccounts?: number;
  tokens?: number | null; usagePartial?: boolean; currentAccountLabel?: string | null;
  conversationKey?: string | null; details?: WidgetDetails;
};
const compact = (n: number) => n >= 1e9 ? `${(n / 1e9).toFixed(1)}B` : n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${(n / 1e3).toFixed(1)}k` : String(n);

export function PoolWidget() {
  const [snapshot, setSnapshot] = useState<Snapshot>({ on: false });
  const [open, setOpen] = useState(false);
  const [hovered, setHovered] = useState(false);
  const capsule = useRef<HTMLDivElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const [scope, setScope] = useState<'current' | 'all'>('current');
  const [error, setError] = useState('');
  const desired = useRef(false);
  const changing = useRef(false);
  const disposed = useRef(false);
  const control = useRef<Control | null>(null);
  const listenersReady = useRef(false);
  const completion = useRef<Promise<unknown> | null>(null);
  const applyControl = useCallback((value: Control | undefined) => {
    if (!value || !control.current || value.instance !== control.current.instance || value.revision < control.current.revision) return;
    control.current = value;
    if (!changing.current) desired.current = value.expanded;
    if (!disposed.current) setOpen(value.expanded);
  }, []);
  const expand = useCallback(async (next: boolean) => {
    desired.current = next;
    if (changing.current || !control.current?.ready) return;
    changing.current = true;
    try {
      let requested: boolean;
      do {
        requested = desired.current;
        const value = await invoke<Control>('widget_set_expanded', { expanded: requested, instance: control.current!.instance });
        applyControl(value);
        if (desired.current === requested) { desired.current = control.current!.expanded; break; }
      } while (!disposed.current);
    } catch {
      desired.current = control.current?.expanded ?? false;
      if (!disposed.current) setError('无法调整挂件，请在托盘打开 Fusion');
      throw new Error('挂件操作未完成');
    } finally { changing.current = false; }
  }, [applyControl]);
  const action = (promise: Promise<unknown>) => {
    completion.current = promise;
    void promise.catch(() => setError('挂件操作未完成，请重试'));
  };
  useEffect(() => {
    disposed.current = false;
    const theme = () => {
      try { document.documentElement.dataset.theme = localStorage.getItem('pool-theme') || (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'); } catch { /* Local preference only. */ }
    };
    theme(); window.addEventListener('storage', theme);
    let pressed: Element | null = null, gesture = 0, sequence = 0;
    let pending = Promise.resolve();
    const event = listen<Control>('widget-expanded', e => applyControl(e.payload));
    const hover = listen<boolean>('widget-hovered', e => { if (!disposed.current) setHovered(e.payload); });
    const pointer = listen<Pointer>('widget-pointer', e => {
      const value = e.payload;
      // Serialize delivery: the native capture-cancel after up must not overtake
      // its async action. Old instances, duplicate and late actions never replay.
      pending = pending.then(async () => {
        if (disposed.current || value.instance !== control.current?.instance || value.sequence <= sequence) return;
        sequence = value.sequence;
        const { kind, x, y, delta } = value;
        if (![x, y, delta].every(Number.isFinite)) return;
        let completed = false;
        try {
          if (kind === 'cancel') { if (value.gesture === gesture) pressed = null; }
          else {
            const target = document.elementFromPoint(x, y);
            const hit = target && root.current?.contains(target) && !target.closest('[inert]') ? target : null;
            completion.current = null;
            if (kind === 'down') {
              gesture = value.gesture;
              pressed = hit?.closest('button,.widget-grip') ?? null;
              if (pressed?.classList.contains('widget-grip')) {
                pressed.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true, button: 0 }));
                completed = true;
              }
            } else if (kind === 'up') {
              const button = hit?.closest('button');
              if (value.gesture === gesture && button instanceof HTMLButtonElement && button === pressed && !button.disabled) { button.click(); completed = true; }
              pressed = null;
            } else if (kind === 'wheel' || kind === 'horizontalWheel') {
              const body = hit?.closest('.widget-summary-body');
              if (body instanceof HTMLElement) { body.scrollBy(kind === 'wheel' ? { top: -delta } : { left: delta }); completed = true; }
            }
            if (completion.current) await completion.current;
            await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
          }
        } catch { completed = false; pressed = null; }
        if (!disposed.current) await invoke('widget_input_ack', { instance: value.instance, sequence: value.sequence, completed });
      }).catch(() => { pressed = null; if (!disposed.current) setError('挂件交互连接中断，请在托盘打开 Fusion'); });
    });
    void Promise.all([event, hover, pointer]).then(async () => {
      listenersReady.current = true;
      const value = await invoke<Control>('widget_ready');
      if (disposed.current) return;
      control.current = value; desired.current = value.expanded; setOpen(value.expanded);
    }).catch(() => { if (!disposed.current) setError('挂件初始化未完成，请在托盘打开 Fusion'); });
    const blur = () => { setHovered(false); pressed = null; if (document.activeElement instanceof HTMLElement) document.activeElement.blur(); };
    const resize = () => { setHovered(false); };
    window.addEventListener('blur', blur); window.addEventListener('resize', resize);
    const key = (e: KeyboardEvent) => { if (e.key === 'Escape') action(expand(false)); };
    document.addEventListener('keydown', key);
    return () => {
      disposed.current = true; listenersReady.current = false; control.current = null; pressed = null;
      window.removeEventListener('storage', theme); document.removeEventListener('keydown', key);
      window.removeEventListener('blur', blur); window.removeEventListener('resize', resize);
      for (const listener of [event, hover, pointer]) void listener.then(unlisten => unlisten()).catch(() => {});
    };
  }, [expand, applyControl]);
  useEffect(() => {
    if (!open) {
      setHovered(false);
      const active = document.activeElement;
      if (active instanceof HTMLElement && capsule.current?.contains(active)) active.blur();
    }
  }, [open]);
  useEffect(() => {
    let cancelled = false, reading = false;
    const read = async () => {
      if (reading) return;
      reading = true;
      try {
        const data = await invoke<Snapshot>('widget_snapshot', { details: open });
        if (!cancelled) {
          if (listenersReady.current && !control.current?.ready) {
            const ready = await invoke<Control>('widget_ready');
            if (!cancelled && !disposed.current) { control.current = ready; desired.current = ready.expanded; setOpen(ready.expanded); }
          }
          applyControl(data.control);
          setSnapshot(previous => ({ ...data, details: data.on ? data.details ?? (previous.conversationKey === data.conversationKey ? previous.details : undefined) : undefined }));
          setError('');
          if (!data.on) { desired.current = false; setOpen(false); }
        }
      } catch { if (!cancelled) setError('本机统计读取失败，保留上次数据'); }
      finally { reading = false; }
    };
    void read(); const timer = setInterval(() => void read(), 3000);
    return () => { cancelled = true; clearInterval(timer); };
  }, [open, applyControl]);

  if (!snapshot.on) return null;
  const quota = snapshot.remaining == null ? '—' : `${Math.round(snapshot.remaining)}%`;
  const quotaPartial = (snapshot.quotaAccounts ?? 0) < (snapshot.totalAccounts ?? 0);
  const poolQuota = snapshot.totalRemaining == null ? '—' : `${Math.round(snapshot.totalRemaining)}%${quotaPartial ? '*' : ''}`;
  const tokens = snapshot.tokens == null ? '—' : compact(snapshot.tokens) + (snapshot.usagePartial ? '*' : '');
  const quotaTitle = `总余为 ${snapshot.quotaAccounts ?? 0}/${snapshot.totalAccounts ?? 0} 个账号的有效额度快照之和；过期、查询失败或缺失不按零计算。不同套餐的百分比不代表相同 Token 余额。`;
  const warning = error || snapshot.storageWarning;
  return <div ref={root} className="widget-root" onMouseDownCapture={e => {
    // This read-only surface must not move the DOM caret away from Codex.
    // Preventing mousedown's default still allows button clicks and scrolling.
    if (e.button === 0 && e.target instanceof Element && e.target.closest('button')) e.preventDefault();
  }}>
    <section className={'widget-panel' + (open ? ' is-open' : '')} inert={!open} aria-hidden={!open} aria-label="账号池详情面板">
      <WidgetSummary details={snapshot.details} scope={scope} onScopeChange={setScope} warning={warning}
        onOpenConsole={() => action(invoke('widget_open_console', { instance: control.current?.instance }))} />
    </section>
    <div ref={capsule} className={'widget-capsule' + (hovered ? ' is-hovered' : '') + (open ? ' is-expanded' : '') + (warning ? ' has-error' : '')} aria-label="账号池小胶囊"
      onPointerEnter={() => setHovered(true)} onPointerMove={() => setHovered(true)} onPointerLeave={() => setHovered(false)}>
      <span className="widget-grip" title="拖动挂件" onMouseDown={e => {
        if (e.button !== 0) return;
        e.preventDefault(); desired.current = false; setOpen(false);
        action(invoke('widget_drag', { instance: control.current?.instance }));
      }}><i /><i /><i /><i /><i /><i /></span>
      <button aria-label="展开或收起账号池详情" aria-expanded={open}
        onMouseDown={e => e.preventDefault()}
        title={warning || `${snapshot.currentAccountLabel || '待分配账号'} · 当前余 ${quota}。${quotaTitle}累计 ${snapshot.tokens?.toLocaleString('zh-CN') ?? '未知'} Token${snapshot.usagePartial ? '，仅含已上报用量，历史覆盖可能不完整' : ''}`}
        onClick={() => action(expand(!desired.current))}>
        <span>当前余 <b>{quota}</b></span><span className="widget-divider" />
        <span title={quotaTitle}>总余 <b>{poolQuota}</b></span><span className="widget-divider" />
        <span>Token <b>{tokens}</b></span>
        <span className="widget-arrow" aria-hidden="true"><svg viewBox="0 0 20 20"><path d="M10 15V5m-4 4 4-4 4 4" /></svg></span>
      </button>
    </div>
  </div>;
}
