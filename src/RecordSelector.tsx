import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import './record-selector.css';

type Option = { value: string; label: string; detail?: string };

export function RecordSelector({ label, value, options, onChange, closeOnSelect = false, align = 'end', triggerIcon }: { label: string; value: string; options: Option[]; onChange: (value: string) => void; closeOnSelect?: boolean; align?: 'start' | 'end'; triggerIcon?: ReactNode }) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const [position, setPosition] = useState<CSSProperties>({});
  const trigger = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const id = useId();
  const selected = Math.max(0, options.findIndex(option => option.value === value));
  const current = options.find(option => option.value === value);

  function show() { setActive(selected); setOpen(true); }
  function choose(index: number) {
    const option = options[index];
    if (!option) return;
    setActive(index);
    if (option.value !== value) onChange(option.value);
    if (closeOnSelect) setOpen(false);
  }
  function keyDown(event: KeyboardEvent) {
    if (event.key === 'Escape' && open) {
      event.preventDefault(); event.stopPropagation(); setOpen(false);
    } else if (event.key === 'Tab') {
      setOpen(false);
    } else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      if (!open) { show(); return; }
      setActive(index => event.key === 'Home' ? 0 : event.key === 'End' ? options.length - 1 : Math.max(0, Math.min(options.length - 1, index + (event.key === 'ArrowDown' ? 1 : -1))));
    } else if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      if (open) choose(active); else show();
    }
  }

  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      const anchor = trigger.current!.getBoundingClientRect();
      const height = Math.min(options.length * 52 + 16, 328);
      const below = window.innerHeight - anchor.bottom - 16;
      const above = anchor.top - 16;
      const upwards = below < height && above > below;
      const maxHeight = Math.max(0, Math.min(height, upwards ? above : below));
      const width = Math.min(Math.max(anchor.width, 260), window.innerWidth - 24);
      setPosition({ left: Math.max(12, Math.min(align === 'start' ? anchor.left : anchor.right - width, window.innerWidth - width - 12)), top: upwards ? anchor.top - maxHeight - 6 : anchor.bottom + 6, width, maxHeight });
    };
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => { window.removeEventListener('resize', place); window.removeEventListener('scroll', place, true); };
  }, [open, options.length, align]);

  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      if (!panel.current?.contains(event.target as Node) && !trigger.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener('pointerdown', outside);
    return () => document.removeEventListener('pointerdown', outside);
  }, [open]);

  useLayoutEffect(() => {
    if (!open) return;
    const option = list.current?.children[active] as HTMLElement | undefined;
    const viewport = panel.current;
    if (!option || !viewport) return;
    if (option.offsetTop < viewport.scrollTop) viewport.scrollTop = option.offsetTop;
    else if (option.offsetTop + option.offsetHeight > viewport.scrollTop + viewport.clientHeight) viewport.scrollTop = option.offsetTop + option.offsetHeight - viewport.clientHeight;
  }, [open, active]);

  return <div className="record-selector">
    <button ref={trigger} className="record-select-trigger" role="combobox" aria-label={label} aria-haspopup="listbox" aria-expanded={open} aria-controls={open ? id : undefined} aria-activedescendant={open ? `${id}-${active}` : undefined} onClick={() => open ? setOpen(false) : show()} onKeyDown={keyDown} onBlur={() => setOpen(false)}>
      <span>{current?.label || '已选条件暂无记录'}</span>{triggerIcon ?? <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m4 6 4 4 4-4" /></svg>}
    </button>
    {open && createPortal(<div className="record-select-popover" ref={panel} style={position} onMouseDown={event => event.preventDefault()}>
      <div ref={list} id={id} role="listbox" aria-label={label} className="record-select-options" style={{ '--record-selected': closeOnSelect ? active : selected } as CSSProperties}>
        {options.map((option, index) => <div key={option.value} id={`${id}-${index}`} role="option" aria-selected={value === option.value} className={'record-select-option' + (active === index ? ' active' : '')} onPointerMove={() => { if (closeOnSelect) setActive(index); }} onClick={() => choose(index)}>
          <span className="record-select-copy"><strong>{option.label}</strong>{option.detail && <small>{option.detail}</small>}</span><span className="record-select-check" aria-hidden="true">✓</span>
        </div>)}
        <div className="record-select-track" aria-hidden="true"><div className="record-select-glider" /></div>
      </div>
    </div>, document.body)}
  </div>;
}
