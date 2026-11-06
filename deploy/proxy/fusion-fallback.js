// Independent Clash Verge subscription extension. Replace the two placeholders
// with existing node names locally; never import a subscription or node secrets.
// https://www.clashverge.dev/guide/script.html
// https://wiki.metacubex.one/config/proxy-groups/fallback/
function main(config) {
  const candidates = ["__FUSION_PRIMARY__", "__FUSION_BACKUP__"];
  const group = "Fusion-OpenAI-Fallback";
  const existing = new Set((config.proxies || []).map(p => p.name));
  const nodes = candidates.filter((name, i) => existing.has(name) && candidates.indexOf(name) === i);
  // Never resolve an empty candidate list to DIRECT, including older cores
  // without empty-fallback support. Existing named subscription nodes only.
  const fallback = { name: group, type: "fallback", proxies: nodes.length ? nodes : ["REJECT"],
    url: "https://www.gstatic.com/generate_204", interval: 30, timeout: 5000,
    lazy: false, "expected-status": 204, "empty-fallback": "REJECT" };
  if ((config["proxy-groups"] || []).some(p => p.name === group)) {
    throw new Error("Fusion fallback already exists; review before replacing it");
  }
  config["proxy-groups"] = [fallback, ...(config["proxy-groups"] || [])];
  // Both process and destination must match; browser and Codex direct traffic
  // keep their existing rules. No global mode or connection-close operations.
  const rules = ["chatgpt.com", "openai.com"].map(domain =>
    `AND,((PROCESS-NAME,personal-codex-pool.exe),(DOMAIN-SUFFIX,${domain})),${group}`);
  config.rules = [...rules, ...(config.rules || [])];
  return config;
}
