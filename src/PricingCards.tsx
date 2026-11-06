import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import './pricing.css';

type Price = { region: string; price: string; convertedPrice: string; tax: string; note?: string | null };
type Plan = { name: string; prices: Price[] };
type Product = { id: string; name: string; sourceUrl: string; description: string; plans: Plan[] };
type Snapshot = { sourceUrl: string; sourceUpdatedAt?: string | null; checkedDate: string | null; updatedAt: number | null; error: string | null; warning?: string | null; products: Product[] };

// Mounted with the app, so the daily check also runs when the settings page is closed.
// The backend merges reads and persists the local date across app restarts.
export function usePricing() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pending = useRef(false);
  useEffect(() => {
    let disposed = false;
    const read = async () => {
      if (pending.current || document.hidden) return;
      pending.current = true;
      try {
        const value = await invoke<Snapshot>('ai_pricing');
        if (!disposed) { setSnapshot(value); setError(null); }
      } catch { if (!disposed) setError('价格暂时无法更新'); }
      finally { pending.current = false; }
    };
    void read();
    const visible = listen<boolean>('main-visible', event => { if (event.payload) void read(); });
    window.addEventListener('focus', read);
    document.addEventListener('visibilitychange', read);
    return () => {
      disposed = true;
      window.removeEventListener('focus', read);
      document.removeEventListener('visibilitychange', read);
      void visible.then(unlisten => unlisten());
    };
  }, []);
  return { snapshot, error };
}

export function PricingCards({ data }: { data: ReturnType<typeof usePricing> }) {
  const { snapshot } = data;
  const [productId, setProductId] = useState('chatgpt');
  const [planName, setPlanName] = useState('');
  const [linkError, setLinkError] = useState<string | null>(null);
  const products = snapshot?.products ?? [];
  const product = products.find(p => p.id === productId) ?? products.find(p => p.id === 'chatgpt') ?? products[0];
  const plan = product?.plans.find(p => p.name === planName) ?? product?.plans[0];
  const error = data.error || snapshot?.error;
  const updated = snapshot?.updatedAt ? new Date(snapshot.updatedAt * 1000).toLocaleString('zh-CN', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit', hour12: false }) : null;
  async function open(site: 'comparison' | 'recharge') {
    setLinkError(null);
    try { await invoke('open_pricing_website', { site }); }
    catch { setLinkError('无法打开浏览器，请检查默认浏览器设置'); }
  }
  return <>
    <section className="pricing-card" aria-label="AI订阅比价">
      <div className="pricing-head">
        <div className="pricing-picker"><h2>AI 订阅比价</h2><select aria-label="AI软件" disabled={!products.length} value={product?.id ?? 'chatgpt'} onChange={event => { setProductId(event.target.value); setPlanName(''); }}>
          {products.length ? products.map(p => <option key={p.id} value={p.id}>{p.id === 'chatgpt' ? 'Codex / ChatGPT' : p.name}</option>) : <option value="chatgpt">Codex / ChatGPT</option>}
        </select></div>
        <span className="badge neutral">每日更新</span>
      </div>
      {product && <div className="pricing-plan"><span>{product.id === 'chatgpt' ? 'ChatGPT 订阅价格 · 含 Codex' : product.name}</span><select aria-label="订阅套餐" value={plan?.name ?? ''} onChange={event => setPlanName(event.target.value)}>{product.plans.map(p => <option key={p.name} value={p.name}>{p.name}</option>)}</select></div>}
      {!!plan?.prices.length && <div className="pricing-table-wrap"><table className="pricing-table"><thead><tr><th scope="col">地区</th><th scope="col">人民币参考</th><th scope="col">当地价格 / 税费</th></tr></thead><tbody>{plan.prices.map((price, i) => <tr key={`${price.region}-${i}`}><th scope="row">{price.region}</th><td>{price.convertedPrice || '—'}</td><td>{price.price || '—'}{price.tax && <small>{price.tax}</small>}{price.note && <small>{price.note}</small>}</td></tr>)}</tbody></table></div>}
      {!plan?.prices.length && <p className="pricing-empty" role="status">{error ? '暂无可用价格，请查看来源网站' : snapshot ? '来源暂未提供价格' : '正在获取订阅价格…'}</p>}
      <div className="pricing-footer"><span>{error ? `${error}${products.length ? '，显示上次价格' : ''}` : snapshot?.warning || (updated ? `更新于 ${updated}` : '每天首次打开时更新')}<small>来源 OpenTheRank{snapshot?.sourceUpdatedAt ? ` · 数据截至 ${snapshot.sourceUpdatedAt}` : ''}</small><small>地区订阅价，结算以商店为准</small></span><button className="text-button" onClick={() => void open('comparison')}>查看来源 ↗</button></div>
    </section>
    <section className="recharge-card" aria-label="AI充值">
      <div className="section-head"><h2>AI 充值</h2><span className="badge neutral">PriceAI</span></div>
      <div className="recharge-mark" aria-hidden="true">P<span>↗</span></div>
      <h3>为 AI 订阅充值续费</h3>
      <p>比较 ChatGPT / Codex 充值与订阅渠道的价格、库存和交付方式，前往 PriceAI 查看套餐并选择办理渠道。</p>
      <button className="primary recharge-open" onClick={() => void open('recharge')}>前往 PriceAI 充值 <span aria-hidden="true">↗</span></button>
    </section>
    {linkError && <p className="pricing-link-error" role="alert">{linkError}</p>}
  </>;
}
