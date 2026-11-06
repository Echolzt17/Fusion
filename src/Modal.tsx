import { useEffect, useRef, type ReactNode, type RefObject } from 'react';

export function Modal({ label, busy, onClose, children, returnFocus }: { label: string; busy: boolean; onClose: () => void; children: ReactNode; returnFocus?: RefObject<HTMLElement | null> }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const previous = document.activeElement;
    const dialog = ref.current!;
    dialog.showModal();
    return () => { dialog.close(); const target = returnFocus?.current ?? previous; if (target instanceof HTMLElement && target.isConnected) target.focus(); };
  }, []);
  return <dialog ref={ref} className="overlay" aria-label={label} onCancel={e => { e.preventDefault(); if (!busy) onClose(); }}>{children}</dialog>;
}
