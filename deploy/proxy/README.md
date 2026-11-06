# Fusion 的独立主备扩展

这不是 Fusion 内置 VPN/节点管理器。`fusion-fallback.js` 用于现有 Clash Verge 的**当前订阅扩展脚本**，只增加已有节点引用与两条“Fusion 进程 + OpenAI 域名”规则。不要作为 YAML 扩展覆盖整份规则，也不要覆盖已有自定义脚本。

本机 2026-11-05 已根据原对照数据生成 `target/fusion-fallback-20261105.js`，保持主节点指纹 `27f5c511e545`，备用 `413050943f1d`。候选只有这两个；样本不足，未宣称备用更好。生成器只读取本机命名管道代理元数据，不读取订阅正文、凭据或节点地址，不改当前选择，不清理连接。生成文件含现有节点名称，仅留本机，不进入报告或 Git。

尚未启用此扩展。先安装/观察 Fusion 1.0.6，保留第一阶段基线，再单独启用主备做对照。当前订阅扩展仍是原默认脚本；启用前记录其内容指纹并保留该小文件，核对当前订阅/主节点未变化。若已有自定义扩展，先合并，不能整份覆盖。通过 Clash Verge 保存扩展后检查最终规则和代理组；必须确认当前模式为规则模式且能识别 Fusion 进程，同时关闭代理的“切换时关闭连接”行为。不能把配置存在当作进程匹配已生效。

默认每30秒检查一次、5秒超时，使用无凭据204探测；空候选为 REJECT，并设置 empty-fallback=REJECT，不允许 DIRECT。有序 fallback 不能保证所有故障15秒内切换；短检查也不能证明模型长流健康。不要按几毫秒延迟调整顺序，不按请求轮询。

回退：恢复此订阅原扩展，再由 Clash Verge 重新生成配置；不改 Fusion 账号绑定或回放请求，不调用清理现有连接。软件和节点分别记录结果、分别回退。

隔离验证：

```powershell
node scripts/verify-fusion-fallback.mjs
python scripts/verify-mihomo-fallback.py '<已安装的 verge-mihomo.exe 绝对路径>'
```

后者只在临时目录启动独立 Mihomo，连接两个 loopback 假代理，以缩短的检查间隔验证“旧流完整结束、新请求走备用”，不访问真实订阅或上游。真实主备配置的进程命中、长流完整率和跨节点表现仍须单独验收。

参考：[Clash Verge 扩展](https://www.clashverge.dev/guide/extend.html)、[Mihomo fallback](https://wiki.metacubex.one/config/proxy-groups/fallback/)、[组配置](https://wiki.metacubex.one/config/proxy-groups/)。
