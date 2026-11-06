import { useEffect, useRef, useState, type ButtonHTMLAttributes, type CSSProperties } from 'react';
import './launch-button.css';

// marcelodolza/fat-zebra-11 (MIT). Real state replaces the demo's :focus success.
export function LaunchButton({ label, running, transitioning, failed, ...props }: ButtonHTMLAttributes<HTMLButtonElement> & {
  label: string; running: boolean; transitioning: boolean; failed: boolean;
}) {
  const previous = useRef({ running, transitioning });
  const [complete, setComplete] = useState(false);
  useEffect(() => {
    const before = previous.current;
    previous.current = { running, transitioning };
    if (!before.transitioning || transitioning || failed) return;
    setComplete(true);
    const timer = setTimeout(() => setComplete(false), 1600);
    return () => { clearTimeout(timer); setComplete(false); };
  }, [running, transitioning, failed]);
  const text = complete ? running ? '已启动' : '已关闭' : label;
  return <button {...props} className={`fusion-launch ${transitioning ? 'is-working' : ''} ${complete ? 'is-complete' : ''}`}
    aria-label={label} aria-busy={transitioning}>
    <span className="launch-outline" aria-hidden="true" />
    <span className="launch-state" key={text} aria-hidden="true">
      <span className="launch-icon"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        {complete ? <path d="m5 12 4 4L19 6" /> : running ? <path d="M12 3v9M6.3 5.8a8 8 0 1 0 11.4 0" /> : <path d="m3 11 18-8-8 18-2-8-8-2Zm8 2L21 3" />}
      </svg></span>
      <span className="launch-letters">{Array.from(text).map((letter, i) => <span key={i} style={{ '--i': i } as CSSProperties}>{letter}</span>)}</span>
    </span>
  </button>;
}
