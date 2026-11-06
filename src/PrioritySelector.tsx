import { Fragment, useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import './priority-selector.css';

export type SelectionPolicy = 'auto' | 'subscription_desc' | 'subscription_asc' | 'quota_desc' | 'quota_asc';
export type SchedulingStatus = { policy: SelectionPolicy; preferredAccountId: string | null; currentAccountId: string | null };
const options: { value: SelectionPolicy; label: string; detail: string }[] = [
  { value: 'auto', label: '自动', detail: '按剩余额度、订阅与负载选择' },
  { value: 'subscription_desc', label: '高订阅优先', detail: '优先使用官方订阅档位较高的账号' },
  { value: 'subscription_asc', label: '低订阅优先', detail: '优先使用官方订阅档位较低的账号' },
  { value: 'quota_desc', label: '高额度优先', detail: '优先使用剩余比例较高的账号' },
  { value: 'quota_asc', label: '低额度优先', detail: '优先使用剩余比例较低的账号' },
];
export function PrioritySelector({ value, disabled, onChange }: { value: SelectionPolicy; disabled: boolean; onChange: (value: SelectionPolicy) => Promise<boolean> }) {
  const [open, setOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [position, setPosition] = useState({ top: 0, left: 0, width: 280, maxHeight: 480 });
  const button = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const restoreFocus = useRef(false);
  const applying = useRef(false);
  const id = useId();
  const selected = options.find(o => o.value === value) ?? options[0];
  function close(focus = false) { restoreFocus.current = focus; setOpen(false); }
  useEffect(() => {
    if (!open && !saving && !disabled && restoreFocus.current) {
      restoreFocus.current = false; button.current?.focus();
    }
  }, [open, saving, disabled]);
  async function apply(policy: SelectionPolicy) {
    if (applying.current || disabled) return;
    applying.current = true; setSaving(true);
    try { await onChange(policy); }
    finally { applying.current = false; setSaving(false); }
  }
  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      const anchor = button.current!.getBoundingClientRect();
      const width = Math.min(300, window.innerWidth - 24);
      const height = Math.min(390, window.innerHeight - 80);
      const top = anchor.bottom + height + 10 > window.innerHeight && anchor.top > height
        ? anchor.top - height - 8 : Math.min(anchor.bottom + 8, window.innerHeight - height - 12);
      setPosition({ top: Math.max(44, top), left: Math.max(12, Math.min(anchor.right - width, window.innerWidth - width - 12)), width, maxHeight: height });
    };
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => { window.removeEventListener('resize', place); window.removeEventListener('scroll', place, true); };
  }, [open, value, disabled, saving]);
  useEffect(() => {
    if (open && !saving && !disabled) panel.current?.querySelector<HTMLInputElement>('input:checked')?.focus();
  }, [open, saving, disabled]);
  useEffect(() => {
    if (!open) return;
    panel.current?.querySelector<HTMLInputElement>('input:checked')?.focus();
    const outside = (event: PointerEvent) => {
      if (!panel.current?.contains(event.target as Node) && !button.current?.contains(event.target as Node)) close();
    };
    const escape = (event: KeyboardEvent) => { if (event.key === 'Escape') { event.preventDefault(); close(true); } };
    document.addEventListener('pointerdown', outside);
    document.addEventListener('keydown', escape);
    return () => { document.removeEventListener('pointerdown', outside); document.removeEventListener('keydown', escape); };
  }, [open]);
  return <>
    <button className={'priority-trigger ' + (open ? 'expanded' : '')} ref={button} disabled={disabled || saving} aria-expanded={open} aria-haspopup="dialog" aria-controls={open ? id : undefined} onClick={() => setOpen(!open)} onKeyDown={e => { if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); setOpen(true); } }}>
      <svg className="priority-icon" viewBox="0 0 20 20" aria-hidden="true"><path d="M3 5h14M6 10h11M9 15h8" /><circle cx="3" cy="10" r="1" /><circle cx="6" cy="15" r="1" /></svg>
      <span>优先使用 <span className="priority-separator">·</span> <strong>{selected.label}</strong></span><svg className="priority-chevron" viewBox="0 0 16 16" aria-hidden="true"><path d="m4 6 4 4 4-4" /></svg>
    </button>
    {open && createPortal(<div className="priority-popover" id={id} ref={panel} style={position} role="dialog" aria-label="优先使用策略" onBlur={event => { if (event.relatedTarget && !event.currentTarget.contains(event.relatedTarget as Node) && !button.current?.contains(event.relatedTarget as Node)) close(); }}>
      <div className="priority-options radio-container" role="radiogroup" aria-label="账号优先规则" aria-busy={saving}>
        {options.map(option => <Fragment key={option.value}>
          <input id={`${id}-${option.value}`} type="radio" name={id} value={option.value} checked={value === option.value} disabled={disabled || saving} onClick={() => { if (value === option.value) void apply(option.value); }} onChange={() => void apply(option.value)} />
          <label className="priority-option" htmlFor={`${id}-${option.value}`}><span className="priority-copy"><strong>{option.label}</strong><small>{option.detail}</small></span><span className="priority-check" aria-hidden="true">✓</span></label>
        </Fragment>)}
        <div className="glider-container" aria-hidden="true"><div className="glider" /></div>
      </div>
      <p className="priority-caption">选定后持续使用，不可用时自动分配</p>
    </div>, document.body)}
  </>;
}
