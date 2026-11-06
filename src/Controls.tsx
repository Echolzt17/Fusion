import type { ButtonHTMLAttributes } from 'react';
import './controls.css';

// Adapted from the seven Uiverse components listed in THIRD_PARTY_NOTICES.md.
export function ActionButton({ kind, children, className = '', ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { kind: 'add' | 'delete' | 'start' | 'next' | 'plain' | 'reset' }) {
  return <button {...props} className={`action-${kind} ${className}`}>
    {kind === 'reset' && <span className="reset-circle" aria-hidden="true"><svg className="reset-arrow" viewBox="0 0 24 24"><path className="reset-arrow-shaft" d="M2 12h14" /><path d="M8 5l8 7-8 7" /></svg></span>}
    {kind === 'start' && <><span className="gradient-layer" aria-hidden="true" /><span className="gradient-layer" aria-hidden="true" /><span className="button-light" aria-hidden="true" /></>}
    {kind === 'add' && <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10" /><path d="M8 12h8m-4-4v8" /></svg>}
    {kind === 'delete' && <svg viewBox="0 0 448 512" aria-hidden="true"><path d="M135.2 17.7L128 32H32C14.3 32 0 46.3 0 64S14.3 96 32 96H416c17.7 0 32-14.3 32-32s-14.3-32-32-32H320l-7.2-14.3C307.4 6.8 296.3 0 284.2 0H163.8c-12.1 0-23.2 6.8-28.6 17.7zM416 128H32L53.2 467c1.6 25.3 22.6 45 47.9 45H346.9c25.3 0 46.3-19.7 47.9-45L416 128z" /></svg>}
    <span className="action-label">{children}</span>
    {kind === 'next' && <svg viewBox="0 0 66 43" aria-hidden="true">{[0, 20, 40].map((x, i) => <path key={x} className={`chevron chevron-${i}`} d={`M${x + 3} 3l19 18-19 18 4 4 23-22L${x + 7} 0z`} />)}</svg>}
  </button>;
}

export function ThemeSwitch({ dark, onChange }: { dark: boolean; onChange: () => void }) {
  return <div className="theme-control"><span>{dark ? '黑夜' : '白天'}</span><label className="theme-switch"><input type="checkbox" role="switch" aria-label="黑夜模式" checked={dark} onChange={onChange} /><span className="slider" /></label></div>;
}

export function AccountTool({ tool, ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { tool: 'refresh' | 'auth' | 'pause' | 'resume' }) {
  const paths = { refresh: 'M20 7v5h-5M4 17v-5h5M6 7a7 7 0 0 1 12-1l2 3M4 15l2 3a7 7 0 0 0 12-1', auth: 'M9 11a4 4 0 1 1 4 4H9l-3 3H3v-3l5-5', pause: 'M8 5v14M16 5v14', resume: 'M7 4l13 8-13 8z' };
  return <button {...props} className="account-tool"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d={paths[tool]} /></svg></button>;
}

// spotift_5948/polite-grasshopper-45 (MIT), restored beside the launch controls.
export function PoolPrinter({ running }: { running: boolean }) {
  return <div className={`pool-printer ${running ? 'running' : 'stopped'}`} role="img" aria-label={running ? '打印机运行中' : '打印机已停止'}>
    <div className="scene" aria-hidden="true"><div className="paper-group"><div className="paper-shadow" /><div className="paper">{Array.from({ length: 8 }, (_, i) => <div key={i} className="ink" style={{ animationDelay: `${i * .16}s`, width: `${[36, 28, 19][i % 3]}px` }} />)}</div></div>
      <div className="printer"><div className="printer-top"><div className="printer-top-inner" /></div><div className="printer-body"><div className="slot" /><div className="panel"><div className="panel-line-lg" /><div className="panel-line-sm" /></div><div className="printer-buttons"><i /><i /><i /></div><div className="led-wrap"><div className="led" /><div className="led-glow" /></div><div className="vents"><i /><i /><i /></div><div className="tray"><div className="tray-inner" /></div><div className="printer-brand" /></div></div>
    </div>
  </div>;
}
