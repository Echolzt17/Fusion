import { useId, useState, type ReactNode } from 'react';

// LilaRest/soft-baboon-75 and ashif_6672/nice-warthog-84 (MIT).
// Source, adaptation notes and license: THIRD_PARTY_NOTICES.md.
export function RecordCategories({ system, onChange }: { system: boolean; onChange: (system: boolean) => void }) {
  const id = useId();
  return <div className="record-categories" role="radiogroup" aria-label="请求记录分类">
    <div className="category-glass" aria-hidden="true"><div /></div>
    <div className="category-selector">{['全部对话', '系统检查'].map((name, i) => <label className="category-choice" key={name}>
      <span className="category-orbit"><input type="radio" name={id} checked={system === !!i} onChange={() => onChange(!!i)} aria-label={`${i + 1} ${name}`} /><span className="category-ball" /></span>
      <span className="category-copy"><b>{i + 1}</b><span>{name}</span></span>
    </label>)}</div>
  </div>;
}

function Folder({ open }: { open: boolean }) {
  return <svg className="tree-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-8l-2-3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />{open && <path d="M2 10h20" />}</svg>;
}
export function RecordBranch({ label, count, selected, onSelect, children, initialOpen = false }: { label: string; count?: number; selected: boolean; onSelect: () => void; children?: ReactNode; initialOpen?: boolean }) {
  const [open, setOpen] = useState(initialOpen);
  const id = useId();
  return <li className="record-tree-item">
    <div className={'record-tree-row' + (selected ? ' is-selected' : '')}>
      {children ? <button className="tree-toggle" aria-label={`${open ? '收起' : '展开'}${label}`} aria-expanded={open} aria-controls={id} onClick={() => setOpen(!open)}><Folder open={open} /></button> : <span className="tree-leaf" aria-hidden="true">·</span>}
      <button className="tree-name" aria-current={selected ? 'page' : undefined} onClick={onSelect} title={label}><span>{label}</span>{count !== undefined && <small>{count}</small>}</button>
    </div>
    {children && <div id={id} className="tree-children-wrapper" data-open={open} aria-hidden={!open} inert={!open}><ul className="tree-children">{children}</ul></div>}
  </li>;
}
