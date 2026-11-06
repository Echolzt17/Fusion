//! Public subscription prices only. No account client, credentials or AI requests
//! are involved; one refresh attempt per local calendar day is persisted on disk.
use chrono::Local;
use dom_query::{Document, Selection};
use serde::{Deserialize, Serialize};
use std::{future::Future, io::{Read, Write}, path::PathBuf, time::Duration};
use tokio::sync::Mutex;

const SOURCE: &str = "https://opentherank.com/zh/ai-pricing/";
const CHATGPT: &str = "https://opentherank.com/zh/ai-pricing/chatgpt/";
const MAX_HTML: usize = 2 * 1024 * 1024;
const MAX_CACHE: u64 = 1024 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingSnapshot {
    pub source_url: String,
    pub checked_date: Option<String>,
    pub updated_at: Option<i64>,
    pub source_updated_at: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub warning: Option<String>,
    pub products: Vec<Product>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub id: String,
    pub name: String,
    pub source_url: String,
    pub description: String,
    pub plans: Vec<Plan>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Plan {
    pub name: String,
    pub prices: Vec<Price>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    pub region: String,
    pub price: String,
    pub converted_price: String,
    pub tax: String,
    pub note: Option<String>,
}
#[derive(Deserialize, Serialize)]
struct Cache {
    version: u8,
    snapshot: PricingSnapshot,
}
pub struct Pricing {
    root: PathBuf,
    snapshot: Mutex<Option<PricingSnapshot>>,
}
struct Fetched {
    products: Vec<Product>,
    source_updated_at: Option<String>,
    warning: Option<String>,
}
impl Pricing {
    pub fn new(root: PathBuf) -> Self {
        Self { root, snapshot: Mutex::new(None) }
    }
    pub async fn load(&self) -> Result<PricingSnapshot, String> {
        let today = Local::now().format("%Y-%m-%d").to_string();
        self.load_with(today, fetch_prices).await
    }
    async fn load_with<F, Fut>(&self, today: String, fetch: F) -> Result<PricingSnapshot, String>
    where F: FnOnce() -> Fut, Fut: Future<Output = Result<Fetched, String>> {
        // Keep the lock through fetch + persistence: startup, focus and visibility
        // notifications all join the same daily attempt instead of racing it.
        let mut state = self.snapshot.lock().await;
        if state.is_none() {
            *state = Some(self.read_cache()?.unwrap_or_else(|| PricingSnapshot {
                source_url: SOURCE.into(), ..Default::default()
            }));
        }
        let snapshot = state.as_mut().unwrap();
        if snapshot.checked_date.as_deref() == Some(today.as_str()) {
            return Ok(snapshot.clone());
        }
        snapshot.checked_date = Some(today);
        match fetch().await {
            Ok(fetched) => {
                snapshot.products = fetched.products;
                snapshot.source_updated_at = fetched.source_updated_at;
                snapshot.updated_at = Some(Local::now().timestamp());
                snapshot.error = None;
                snapshot.warning = fetched.warning;
            }
            Err(error) => {
                snapshot.error = Some(error); // retain last good prices
                snapshot.warning = None;
            }
        }
        if self.write_cache(snapshot).is_err() {
            let message = "价格缓存保存失败，本次运行已保留数据";
            snapshot.warning = Some(match snapshot.warning.take() {
                Some(warning) => format!("{warning}；{message}"),
                None => message.into(),
            });
        }
        Ok(snapshot.clone())
    }
    fn read_cache(&self) -> Result<Option<PricingSnapshot>, String> {
        let file = match std::fs::File::open(self.root.join("ai-pricing.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("无法读取价格缓存".into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_CACHE + 1).read_to_end(&mut bytes).map_err(|_| "无法读取价格缓存")?;
        if bytes.len() as u64 > MAX_CACHE { return Ok(None); }
        let Ok(cache) = serde_json::from_slice::<Cache>(&bytes) else { return Ok(None); };
        if cache.version > 1 { return Err("价格缓存来自较新版本，请升级 Fusion".into()); }
        if cache.version != 1 || cache.snapshot.source_url != SOURCE || !valid_products(&cache.snapshot.products) {
            return Ok(None);
        }
        Ok(Some(cache.snapshot))
    }
    fn write_cache(&self, snapshot: &PricingSnapshot) -> Result<(), String> {
        std::fs::create_dir_all(&self.root).map_err(|_| "无法保存价格缓存")?;
        let bytes = serde_json::to_vec(&Cache { version: 1, snapshot: snapshot.clone() })
            .map_err(|_| "无法编码价格缓存")?;
        let mut file = tempfile::NamedTempFile::new_in(&self.root).map_err(|_| "无法保存价格缓存")?;
        file.write_all(&bytes).map_err(|_| "无法保存价格缓存")?;
        file.as_file().sync_all().map_err(|_| "无法保存价格缓存")?;
        file.persist(self.root.join("ai-pricing.json")).map_err(|_| "无法保存价格缓存")?;
        Ok(())
    }
}
async fn fetch_prices() -> Result<Fetched, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .user_agent(concat!("Fusion/", env!("CARGO_PKG_VERSION"), " (public price comparison)"))
        .build().map_err(|_| "价格查询初始化失败")?;
    let html = fetch_html(&client, SOURCE).await?;
    let mut fetched = parse_overview(&html)?;
    // The overview supplies all products in one request. Only the default Codex /
    // ChatGPT selection needs its detailed tier table; its failure must not erase
    // an otherwise complete overview or another product's prices.
    match fetch_html(&client, CHATGPT).await.and_then(|html| parse_chatgpt(&html)) {
        Ok(plans) => {
            if let Some(product) = fetched.products.iter_mut().find(|p| p.id == "chatgpt") {
                product.plans = plans;
                product.description = "ChatGPT 订阅地区价格（含 Codex），来源为 App Store。".into();
            }
        }
        Err(_) => fetched.warning = Some("ChatGPT 套餐详情暂未更新，显示来源网站的基准套餐价格".into()),
    }
    Ok(fetched)
}
async fn fetch_html(client: &reqwest::Client, url: &str) -> Result<String, String> {
    // Both callers are fixed HTTPS URLs; the client uses the system proxy and
    // normal certificate validation, with no account authorization or cookies.
    let mut response = client.get(url).send().await.map_err(|_| "价格网站暂时无法连接")?;
    if !response.status().is_success() { return Err("价格网站暂不可用".into()); }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "价格数据下载中断")? {
        if bytes.len() + chunk.len() > MAX_HTML { return Err("价格页面超过允许大小".into()); }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| "价格页面编码无法识别".into())
}
fn text(selection: &Selection<'_>) -> String {
    selection.text().split_whitespace().collect::<Vec<_>>().join(" ")
}
fn parse_overview(html: &str) -> Result<Fetched, String> {
    let document = Document::from(html);
    let mut products = Vec::new();
    for card in document.select("main a[href^='/zh/ai-pricing/']").iter() {
        let name = text(&card.select("h2")).trim_end_matches(" 各地区定价").to_string();
        let path = card.attr_or("href", "");
        let Some(id) = path.strip_prefix("/zh/ai-pricing/").and_then(|v| v.strip_suffix('/')) else { continue; };
        if name.is_empty() || !valid_id(id) { continue; }
        let mut prices = Vec::new();
        for row in card.select(".pt-table tbody tr").iter().take(64) {
            let region = text(&row.select(".pt-region"));
            let converted_price = text(&row.select(".pc-pill"));
            let price = text(&row.select(".pc-local")).trim_start_matches('≈').trim().to_string();
            let tax = text(&row.select(".pt-tax-text"));
            if region.is_empty() || price.is_empty() || converted_price.is_empty() { continue; }
            prices.push(Price { region, price, converted_price, tax, note: None });
        }
        if prices.is_empty() { continue; }
        products.push(Product {
            id: id.into(), name, source_url: format!("https://opentherank.com{path}"),
            description: format!("{}；以下为来源首页展示的基准套餐。", text(&card.select("p").first())),
            plans: vec![Plan { name: "来源首页基准套餐".into(), prices }],
        });
    }
    if products.len() < 2 || !products.iter().any(|p| p.id == "chatgpt") || !valid_products(&products) {
        return Err("价格网站结构已变化".into());
    }
    let footer = text(&document.select(".otr-foot__note"));
    let source_updated_at = footer.split_once("更新于 ").map(|(_, v)| v.trim_end_matches('。').to_string());
    Ok(Fetched { products, source_updated_at, warning: None })
}
fn parse_chatgpt(html: &str) -> Result<Vec<Plan>, String> {
    let document = Document::from(html);
    let table = document.select("table[data-vs-plans]").first();
    let columns: Vec<_> = table.select("thead th").iter().enumerate()
        .filter_map(|(index, header)| header.attr("data-vs-col").map(|_| (index, text(&header))))
        .take(8).collect();
    let mut plans: Vec<_> = columns.iter().map(|(_, name)| Plan { name: name.clone(), prices: Vec::new() }).collect();
    for row in table.select("tbody tr.ctl-row").iter().take(64) {
        let region = text(&row.select("[data-vs-name]").first());
        let tax = text(&row.select(".ctl-tax .ctl-cell-clamp").first());
        let cells = row.select("td");
        let cells: Vec<_> = cells.iter().collect();
        if region.is_empty() { continue; }
        for (plan, (index, _)) in plans.iter_mut().zip(&columns) {
            let Some(cell) = cells.get(*index) else { continue; };
            let converted_price = text(&cell.select(".ctl-pill"));
            let price = text(&cell.select(".ctl-local")).trim_start_matches('≈').trim().to_string();
            if price.is_empty() || converted_price.is_empty() { continue; }
            let note = cell.select("[data-price-note]").attr("aria-label")
                .and_then(|label| label.split('·').nth(2).map(|v| v.trim().to_string()))
                .filter(|v| !v.is_empty());
            plan.prices.push(Price { region: region.clone(), price, converted_price, tax: tax.clone(), note });
        }
    }
    if plans.is_empty() || plans.iter().any(|p| p.name.is_empty() || p.prices.is_empty()) {
        return Err("ChatGPT 套餐表结构已变化".into());
    }
    Ok(plans)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn valid_products(products: &[Product]) -> bool {
    products.len() <= 64 && products.iter().all(|p| valid_id(&p.id)
        && p.source_url == format!("{SOURCE}{}/", p.id)
        && p.name.chars().count() <= 100 && p.description.chars().count() <= 500
        && p.plans.len() <= 8 && p.plans.iter().all(|plan| plan.name.chars().count() <= 100
            && plan.prices.len() <= 64 && plan.prices.iter().all(|price|
                [&price.region, &price.price, &price.converted_price, &price.tax].iter().all(|s| s.chars().count() <= 500)
                && price.note.as_ref().is_none_or(|s| s.chars().count() <= 500))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    const OVERVIEW: &str = r#"<main>
      <a href="/zh/ai-pricing/chatgpt/"><h2>ChatGPT 各地区定价</h2><p>Plus 和 Pro</p><table class="pt-table"><tbody><tr>
      <td class="pt-region">加拿大</td><td><b class="pc-pill">¥120/mo</b><span class="pc-local">≈ CA$25/mo</span></td><td class="pt-tax-text">5-15% GST/HST 不同</td></tr></tbody></table></a>
      <a href="/zh/ai-pricing/claude/"><h2>Claude 各地区定价</h2><p>Pro &amp; Max</p><table class="pt-table"><tbody><tr>
      <td class="pt-region">日本</td><td><b class="pc-pill">¥130/mo</b><span class="pc-local">≈ ¥3,000/mo</span></td><td class="pt-tax-text">含 10% 税</td></tr></tbody></table></a>
      </main><footer><p class="otr-foot__note">价格来源于商店。更新于 2026年11月5日 14:26 UTC。</p></footer>"#;
    const DETAILS: &str = r#"<table data-vs-plans="[]"><thead><tr><th>地区</th><th data-vs-col="plus">Plus</th><th>对比美国</th><th data-vs-col="pro">Pro 20x</th></tr></thead><tbody>
      <tr class="ctl-row"><td><span data-vs-name>加拿大</span></td><td><button data-price-note aria-label="$18/mo · CA$25/mo · 推测价格"><span class="ctl-pill">¥120</span><span class="ctl-local">≈ CA$25/mo</span></button></td><td>-10%</td><td><span class="ctl-pill">¥1200</span><span class="ctl-local">≈ CA$250/mo</span></td><td class="ctl-tax"><div class="ctl-cell-clamp">5-15% GST/HST 不同</div></td></tr></tbody></table>"#;

    #[test]
    fn parses_only_price_cards_and_preserves_region_plan_tax_and_notes() {
        let data = parse_overview(OVERVIEW).unwrap();
        assert_eq!(data.products.len(), 2);
        assert!(data.products[1].description.contains("Pro & Max"));
        assert_eq!(data.products[0].plans[0].prices[0].price, "CA$25/mo");
        assert_eq!(data.source_updated_at.as_deref(), Some("2026年11月5日 14:26 UTC"));
        let plans = parse_chatgpt(DETAILS).unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[1].name, "Pro 20x");
        assert_eq!(plans[1].prices[0].price, "CA$250/mo");
        assert_eq!(plans[0].prices[0].tax, "5-15% GST/HST 不同");
        assert_eq!(plans[0].prices[0].note.as_deref(), Some("推测价格"));
        assert!(parse_overview("<html>maintenance</html>").is_err());
        assert!(parse_chatgpt("<table>unavailable</table>").is_err());
    }
    #[tokio::test]
    async fn daily_refresh_survives_restart_and_failure_preserves_prior_prices() {
        let directory = tempfile::tempdir().unwrap();
        let service = Pricing::new(directory.path().to_path_buf());
        let initial = service.load_with("2026-11-06".into(), || async { parse_overview(OVERVIEW) }).await.unwrap();
        assert_eq!(initial.products.len(), 2);
        let calls = Arc::new(AtomicUsize::new(0));
        let failing = || { let calls = calls.clone(); async move { calls.fetch_add(1, Ordering::SeqCst); Err("离线，保留上次价格".into()) } };
        let restarted = Pricing::new(directory.path().to_path_buf());
        let cached = restarted.load_with("2026-11-06".into(), failing).await.unwrap();
        assert_eq!(cached.updated_at, initial.updated_at);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let failed = restarted.load_with("2026-11-07".into(), failing).await.unwrap();
        assert_eq!(failed.products[0].plans[0].prices[0].price, "CA$25/mo");
        assert_eq!(failed.updated_at, initial.updated_at);
        assert!(failed.error.is_some());
        let again = Pricing::new(directory.path().to_path_buf());
        again.load_with("2026-11-07".into(), failing).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let recovered = again.load_with("2026-11-08".into(), || async { parse_overview(OVERVIEW) }).await.unwrap();
        assert!(recovered.error.is_none());
    }
    #[tokio::test]
    async fn concurrent_open_events_share_one_refresh_even_without_old_data() {
        let directory = tempfile::tempdir().unwrap();
        let service = Pricing::new(directory.path().to_path_buf());
        let calls = AtomicUsize::new(0);
        let fetch = || async { calls.fetch_add(1, Ordering::SeqCst); tokio::task::yield_now().await; Err("暂不可用".into()) };
        let (a,b) = tokio::join!(service.load_with("2026-11-06".into(), fetch), service.load_with("2026-11-06".into(), fetch));
        assert!(a.unwrap().products.is_empty());
        assert!(b.unwrap().error.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    #[ignore = "real public site check, no credentials or account client involved"]
    async fn live_public_prices_parse_through_system_proxy() {
        let directory = tempfile::tempdir().unwrap();
        let result = Pricing::new(directory.path().to_path_buf()).load().await.unwrap();
        assert!(result.error.is_none(), "{}", result.error.unwrap_or_default());
        assert!(result.products.len() >= 20);
        let product = result.products.iter().find(|p| p.id == "chatgpt").unwrap();
        assert!(product.plans.len() >= 3);
        assert!(product.plans.iter().all(|p| p.prices.len() >= 10));
        println!("public prices: {} products, {} ChatGPT plans, {} regions", result.products.len(), product.plans.len(), product.plans[0].prices.len());
    }
}
