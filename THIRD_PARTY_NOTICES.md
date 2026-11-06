# 来源与第三方说明

## 内置专用网络内核

随包分发未经修改的 MetaCubeX/mihomo v1.19.33 Windows amd64-compatible 独立可执行文件，许可证为 GNU GPL v3；许可全文随 `network-kernel/LICENSE` 及生成的第三方许可材料交付。固定版本、官方分发来源、归档及可执行文件摘要见 `resources/network-kernel/manifest.json`。对应源码：https://github.com/MetaCubeX/mihomo/archive/refs/tags/v1.19.33.zip 。Fusion 通过本机代理及控制接口与独立进程通信，没有把内核源码复制或链接到 Rust 模块，也不修改其二进制；独立进程形式不免除该内核的分发许可要求。内核不从运行时配置指定位置下载或自行升级。

本工具的账号授权协议和 Codex 原生转发行为参考：

- 项目：Cockpit Tools
- 作者：jlcodes99 / jlcodes
- 固定参考版本：v1.3.75
- 仓库：https://github.com/jlcodes99/cockpit-tools
- OAuth 参考：https://github.com/jlcodes99/cockpit-tools/blob/v1.3.75/src-tauri/src/modules/codex_oauth.rs
- 网关参考：https://github.com/jlcodes99/cockpit-tools/blob/v1.3.75/src-tauri/src/modules/codex_local_access.rs
- 会员到期字段参考：同版本 src-tauri/src/modules/codex_account_token_refresh.rs 的 chatgpt_subscription_active_until。仅参考授权字段及账号身份校验，自行实现日期规范化和只读倒计时；不读取或复制 Cockpit 数据，不把授权快照视为实时账单。
- 启用时备份及接管流程参考：同版本 `codex_local_access_native_profiles.rs` 的 `native_activate_profile`、`codex_local_access_profile_takeover.rs` 的 `save_profile_takeover_backup`。仅参考生命周期处理思路，自行实现字段级接管与恢复；未复用其读取或备份 auth.json 的实现。
- 上游声明许可：CC BY-NC-SA 4.0（以其源仓库许可说明为准）。

修改说明：本工具重新组织为独立 Rust 核心和精简桌面界面，未包含 Cockpit 的完整 UI、数据库、账号文件、其他平台模块、Go sidecar 或应用多开/接管实现。保留浏览器 PKCE 授权、原生 Codex 路径及请求行为的来源说明，不宣称是官方 OpenAI 或 Cockpit 产品。

本工具源码采用 CC BY-NC-SA 4.0；参见 LICENSE。第三方依赖保持各自许可，具体版本见锁文件。OpenAI、Codex 和 ChatGPT 名称归其相应权利人所有。

客户端配置参考：https://learn.chatgpt.com/docs/config-file/config-reference

## 原生图片协议参考

固定官方源码版本：`3535c22b43e5d7a67322f6e85b267ef65577c227`，仅参考协议、工具开放条件和隔离夹具形状，没有整体合并或修改客户端：

- https://github.com/openai/codex/blob/3535c22b43e5d7a67322f6e85b267ef65577c227/codex-rs/codex-api/src/endpoint/images.rs
- 同版本 `codex-rs/codex-api/src/images.rs`、`codex-rs/core/src/tools/spec_plan.rs`、`codex-rs/model-provider-info/src/lib.rs`、`codex-rs/core/src/responses_metadata.rs` 和 `codex-rs/app-server/tests/suite/v2/imagegen_extension.rs`。
- 上游许可 Apache-2.0：https://github.com/openai/codex/blob/3535c22b43e5d7a67322f6e85b267ef65577c227/LICENSE 。

原生工具与用量说明：https://learn.chatgpt.com/docs/image-generation 。网关识别通道是版本相关实现细节，不视为官方长期兼容承诺。

## Uiverse 界面组件（MIT）

以下组件移植为 React 和本地 CSS，适配明暗主题、中文标签、禁用状态和账号池真实数据，不依赖远程脚本或 Tailwind：

- Copyright - 2026 Nawsome：https://uiverse.io/Nawsome/cowardly-squid-50 。固定参考 uiverse-io/galaxy `adbd2adde0a299a3956ea288fb444ec01891ca41` 的 `loaders/Nawsome_cowardly-squid-50.html`；参考多层图形错位展开的立体表现，改写为账号卡的抬升、双层错位底板、主题描边及阴影。由现有 `currentAccountId` 持续驱动，保留正面文字与操作布局，适配明暗及减少动态效果，不移植原加载器或旋转动画。沿用下方 MIT 许可。

- Copyright - 2026 gharsh11032000 (Harsh Gupta): https://uiverse.io/gharsh11032000/brave-pug-20
- Copyright - 2026 vinodjangid07 (Vinod Jangid): https://uiverse.io/vinodjangid07/smart-emu-83
- Copyright - 2026 catraco (Catraco): https://uiverse.io/catraco/fluffy-quail-74
- Copyright - 2026 dexter-st: https://uiverse.io/dexter-st/breezy-zebra-94
- Copyright - 2026 mobinkakei (Mobeen): https://uiverse.io/mobinkakei/cold-bat-89
- Copyright - 2026 spotift_5948 (Spotift Twin): https://uiverse.io/spotift_5948/polite-grasshopper-45
- Copyright - 2026 themrsami (themrsami): https://uiverse.io/themrsami/lovely-yak-39

0.1.31 按钮及更新卡片（MIT）：

- Copyright - 2026 marcelodolza (Marcelo Dolza): https://uiverse.io/marcelodolza/fat-zebra-11 。保留立体层、描边流光、逐字波动、图标起飞/完成动画；用真实连接状态替换原 :focus 演示成功，仅替换首页启停按钮，适配明暗、中文与减少动态效果。
- Copyright - 2026 catraco (Catraco): https://uiverse.io/catraco/short-starfish-66 。保留圆形底座、渐变前层及按压下沉，主题色用于检查、红色用于已发现稳定更新。
- Copyright - 2026 vinodjangid07 (Vinod Jangid): https://uiverse.io/vinodjangid07/lazy-rattlesnake-93 。保留图标/标题/说明/双按钮结构与字重，替换 Cookie 内容为 Fusion 版本说明，新增居中虚化蒙版、滚动、键盘与真实更新进度。
- 侧栏 LEARN MORE 复用下方已有 cssbuttons-io/massive-mayfly-74（MIT）来源，保留圆形展开、箭头和文字，改为固定 GitHub 仓库入口。

0.1.22 挂件与侧栏改版（沿用下方 MIT 许可）：

- Copyright - 2026 SSpisso；上游变体同时署名 mi-series / Md Mainul Islam Satabddo：https://uiverse.io/SSpisso/kind-cobra-45 。用户提供 HTML/CSS；保留分段账单卡、细分隔线、左右汇总和突出底栏，改为本对话/全部对话的只读请求汇总、总 Token 与打开控制台，删除表单和付款语义，适配明暗主题。
- Copyright - 2026 nathAd17 (nathan ady)：https://uiverse.io/nathAd17/heavy-warthog-57 。参考 uiverse-io/galaxy 的 Buttons/nathAd17_heavy-warthog-57.html；保留圆角、背景扫入、圆形箭头旋转，改写为本地 CSS 和三项数值胶囊，增加按压、独立拖动、减少动态效果，不引入 Tailwind。
- Copyright - 2026 kamehame-ha (kameHame HA)：https://uiverse.io/kamehame-ha/chilly-snake-91 。参考 uiverse-io/galaxy 的 Cards/kamehame-ha_chilly-snake-91.html；保留悬停放大、同组其他项缩小/模糊与 400ms 联动，调小幅度并适配 Fusion 主题、当前页面、键盘焦点及减少动态效果。

新增组件（源代码固定于 uiverse-io/galaxy `adbd2adde0a299a3956ea288fb444ec01891ca41`）：

- Copyright - 2026 andrew-demchenk0: https://uiverse.io/andrew-demchenk0/new-kangaroo-60
- Copyright - 2026 Smit-Prajapati (Smit Prajapati): https://uiverse.io/Smit-Prajapati/great-bat-98
- Copyright - 2026 cssbuttons-io (cssbuttons.io): https://uiverse.io/cssbuttons-io/massive-mayfly-74

账号优先策略组件：Copyright - 2026 Smit-Prajapati (Smit Prajapati)，https://uiverse.io/Smit-Prajapati/quick-panther-98 。用户提供原始 HTML/CSS；保留左侧渐变光带、发光与 `0.5s cubic-bezier(0.37, 1.95, 0.66, 0.56)` 滑块过渡，改为五项策略，并适配应用明暗主题、键盘焦点与减少动态效果。沿用下方 MIT 许可。

请求记录的对话/结果下拉框也沿用上述 quick-panther-98 的发光光带及弹性滑块，使用同一主题强调色，适配动态选项数量、滚动、键盘选择和减少动态效果。沿用同一作者署名、来源和 MIT 许可。

顶部 Codex / Claude Code / API 统一工作区下拉复用同一 quick-panther-98 本地组件，保留发光光带、弹性滑块和展开过渡，适配标题位置、选择后收起、键盘操作及明暗主题；沿用同一作者署名、来源和 MIT 许可。

2026-11-07 顶部触发按钮融合 Li-Deheng/shy-moth-10：https://uiverse.io/Li-Deheng/shy-moth-10 ，Copyright - 2026 Li-Deheng（李德恒），MIT。源代码固定参考 uiverse-io/galaxy `adbd2adde0a299a3956ea288fb444ec01891ca41` 的 `Buttons/Li-Deheng_shy-moth-10.html`；保留胶囊外形、Poppins 600、三箭头依次流动及光晕/轮廓动画，用工作区名称替换 NEXT，颜色适配 Fusion 主题，点击展开上述 quick-panther-98 选择面板，并适配键盘、窄窗和减少动态效果。

代理与节点账单式布局视觉参考 Cksunandh/slippery-gecko-23：https://uiverse.io/Cksunandh/slippery-gecko-23 ，Copyright - 2026 Cksunandh（Sunandh ck），该参考在 Uiverse 按 MIT 发布。依据原作者公开索引预览确认的单色收据、标题/日期、明细和突出汇总层级做本地适配；未获得原始 HTML/CSS，不将本地实现称为原源码移植或逐像素复刻。将商品/金额替换为实际订阅、节点数和当前出口，不引入虚构金额、购买或付款语义。

请求记录的信息层级组件（2026-10-30 移植，沿用下方 MIT 许可）：

- Copyright - 2026 LilaRest (Lila Rest)：https://uiverse.io/LilaRest/soft-baboon-75 。原始 HTML/CSS 固定参考 uiverse-io/galaxy `adbd2adde0a299a3956ea288fb444ec01891ca41` 的 `Radio-buttons/LilaRest_soft-baboon-75.html`；保留玻璃槽内外层、圆形选项、圆球的选中/未选中位移、阴影和 `800ms cubic-bezier(1,-0.4,0,1.4)`。按两个真实分类同比缩小几何尺寸和位移，中文名称独立排版，适配主题和减少动态效果。
- Copyright - 2026 ashif_6672 (Ashif)：https://uiverse.io/ashif_6672/nice-warthog-84 。原始 HTML/CSS 由用户于 2026-10-30 提供；保留父子连接线、文件夹图标开合、`grid-template-rows 0.3s ease-in-out` 展开及 `background-color 0.2s` 反馈。改为 React 动态分类，展开按钮和查看范围按钮分离，增加键盘焦点、闭合节点 inert 及减少动态效果，颜色和尺寸适配 Fusion。没有使用早先误认的 dovatgabriel 卡组。

通知中心组件（2026-10-30 获取并本地移植，沿用下方 MIT 许可）：

- Copyright - 2026 narmesh_sah (Narmesh Kumar Sah)：https://uiverse.io/narmesh_sah/hungry-pig-40 。保留五层星形、多层尺寸、6 秒旋转与延迟结构，缩放为通知入口，未读时才播放，并适配明暗主题及减少动态效果。
- Copyright - 2026 0xnihilism (0xNihilist)：https://uiverse.io/0xnihilism/thin-crab-36 。源码参考 uiverse-io/galaxy `adbd2adde0a299a3956ea288fb444ec01891ca41` 的 `Cards/0xnihilism_thin-crab-36.html`；保留粗边框、硬阴影与单按钮结构，内容改为 Fusion 的真实通知，删除 Mark as Read 按钮，使用本地 OKAY 按钮并适配主题、中文及长文本。

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

## 官方重置卡协议参考

只参考接口路径、字段和结果枚举，未复制官方客户端实现代码：
https://github.com/openai/codex/blob/23934d31b15b1127c83535f84854e5009f7cd51d/codex-rs/backend-client/src/client/rate_limit_resets.rs
以及同版本 app-server/tests/suite/v2/rate_limit_reset_credits.rs。
该上游采用 Apache-2.0：https://github.com/openai/codex/blob/23934d31b15b1127c83535f84854e5009f7cd51d/LICENSE。

## 官方订阅档位参考

只参考官方档位标识与显示名称，不复制客户端或凭据：
https://github.com/openai/codex/blob/a29c7b97e88367044ae84c28c85451ec3e72af9f/codex-rs/protocol/src/auth.rs
以及同版本 `codex-rs/tui/src/subscription.rs`。上游采用 Apache-2.0：https://github.com/openai/codex/blob/a29c7b97e88367044ae84c28c85451ec3e72af9f/LICENSE 。

公开个人套餐及 Pro 100 / 200 / 500 档位：https://learn.chatgpt.com/docs/pricing （2026-10-29 核对）。组织套餐没有统一可比个人等级，不由产品名称或额度百分比推断等级。


## Windows 安装模板与依赖许可材料

安装模板固定参考 Tauri CLI 2.12.4，commit `993038c49185307dfc895ab7657d77ca0d5ed7c7`：
https://github.com/tauri-apps/tauri/blob/993038c49185307dfc895ab7657d77ca0d5ed7c7/crates/tauri-bundler/src/bundle/windows/nsis/installer.nsi 。
Copyright Tauri Programme within The Commons Conservancy，MIT / Apache-2.0。
修改：在执行旧卸载器和替换程序前加入只读接入检查及运行进程检查；删除安装/卸载入口的默认强制关闭调用；静默模式失败即停止。

`docs/THIRD_PARTY_LICENSES.txt` 随安装包交付，包含锁定 Windows x64 依赖树（保守包含构建依赖）、npm 运行依赖及上述模板的许可与公开源代码地址。通过 `npm run licenses` 从本地锁定依赖重新生成；未修改第三方依赖源码。部分 crate 未携带根目录许可文件，补充材料固定到各 crate 的发布提交，保存在 `docs/licenses`；selectors 的 MPL 2.0 使用 Mozilla 官方许可全文。构建自动重新生成材料，不更新依赖版本。

## Microsoft Edge WebView2 离线安装程序

0.1.8 安装包携带微软原始签名的 MicrosoftEdgeWebView2RuntimeInstallerX64.exe，实际运行时载荷 156.0.4314.56。来源：https://developer.microsoft.com/microsoft-edge/webview2/ （Evergreen Standalone Installer x64）；发布文件：https://msedge.sf.dl.delivery.mp.microsoft.com/filestreamingservice/files/d6179690-e992-460b-ae64-476693dd6a91/MicrosoftEdgeWebView2RuntimeInstallerX64.exe 。二进制未修改，版权及许可归 Microsoft，适用微软 WebView2 Runtime 分发条款，不适用本项目源码许可。分发说明：https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution 。自定义 NSIS 模板沿用上述固定 Tauri 来源及许可，0.1.8 新增私有目录离线依赖安装、实际 Loader 前后检查、架构检查，不合并其他上游代码。

0.1.9沿用同一微软离线二进制；安装成功状态参考微软维护的该版本WinGet清单：https://github.com/microsoft/winget-pkgs/blob/master/manifests/m/Microsoft/EdgeWebView2Runtime/156.0.4314.56/Microsoft.EdgeWebView2Runtime.installer.yaml 。仅参考返回码数值与语义，自行实现安装后可用性复核；NSIS自定义模板改为独立压缩块，保留原Tauri来源和许可。
## 随包界面字体

Inter 4.1 by the Inter Project Authors and Source Han Sans 2.005 by Adobe are distributed as unmodified upstream WOFF2 assets under SIL Open Font License 1.1. Inter source: https://github.com/rsms/inter/tree/v4.1 (docs/font-files/InterVariable.woff2). Source Han Sans source: https://github.com/adobe-fonts/source-han-sans/tree/2.005R (Variable/WOFF2/TTF/Subset/SourceHanSansCN-VF.ttf.woff2). Copyright and full license texts accompany the files in public/fonts and the generated bundled license materials. No fonts are installed into Windows or downloaded at runtime.

The workspace button bundles the unmodified Poppins SemiBold Latin WOFF2, font version 4.004, weight 600, from the official Google Fonts stylesheet https://fonts.googleapis.com/css2?family=Poppins:wght@600&display=swap and asset https://fonts.gstatic.com/s/poppins/v24/pxiByp8kv8JHgFVrLEj6Z1xlFQ.woff2 . Copyright 2020 The Poppins Project Authors (https://github.com/itfoundry/Poppins), SIL Open Font License 1.1. The original license is included as public/fonts/Poppins-LICENSE.txt; the font is loaded locally without runtime network requests or Windows installation.

更新使用固定 tauri-plugin-updater 2.14.1（MIT OR Apache-2.0）与 minisign-verify 0.3.0（MIT）；源码未修改，锁定依赖许可由构建脚本汇总。NSIS 模板新增更新模式有界等待已安装文件释放和独立修复目录入口，保留现有维护守卫。

## 错误日志上报组件（MIT）

- Copyright - 2026 Navarog21：https://uiverse.io/Navarog21/ordinary-rat-19 。原始源码来自 uiverse-io/galaxy `Buttons/Navarog21_ordinary-rat-19.html`（Git blob `6c503547b2d898dea97cc70729c410e592bbaa1c`）。保留双描边切角、四角分段发光及两侧箭头往返动画，改为中文日志上报，适配主题、焦点和减少动态效果。
- Copyright - 2026 Yaya12085：https://uiverse.io/Yaya12085/grumpy-fox-39 。原始源码来自 uiverse-io/galaxy `Cards/Yaya12085_grumpy-fox-39.html`（Git blob `262e536c07fb280eaca8072556b1534e689e44bf`）。保留圆形图标、居中标题/说明和上下双按钮结构，内容替换为实际错误摘要、确认/取消及失败重试。沿用本文 Uiverse MIT 许可全文。
