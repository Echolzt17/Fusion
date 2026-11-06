# Fusion 开发指南

本指南用于在另一台 Windows 电脑检出源码并继续开发。当前开发范围包括 Windows 本机 Codex 订阅账号池、Codex / Claude Code 独立客户端与手动 API 来源、账号池专用代理，以及明确操作的可读聊天交接。技术栈为 Tauri 2、Rust、React / TypeScript；不包含账号同步、跨厂商订阅池或 API Key 自动池化。

1.0.11 增加胶囊工作区下拉、随包 Poppins 字体、账单式多订阅单选与原内核启动世代保护。代理目录仍使用原 kv 键，写入版本 2 后旧版按向前保护拒绝读取；请基于当前代码继续开发，不通过清除数据或关闭版本检查实现降级。

## 1. 准备环境

- Windows 10 / 11 x64，以及 Git。
- Node.js 与 npm。当前锁定的 Vite 7 要求 Node.js `^20.19.0 || >=22.12.0`；可使用满足该范围的 Node.js 22.12 或更新版本。
- Rust 稳定版的 MSVC 工具链，目标 `x86_64-pc-windows-msvc`。
- Visual Studio C++ Build Tools：“使用 C++ 的桌面开发”组件及 Windows SDK。
- WebView2 Runtime；UI 自动测试还使用本机 Microsoft Edge。
- 联网以首次取得 npm / Cargo 依赖。真实业务测试另需已安装的 Codex 桌面和可用的显式代理。

普通类型检查、核心测试和前端构建不需要真实账号、发布凭据或签名私钥。保持仓库中的 `package-lock.json` 和 `Cargo.lock`，不要为了首次启动随意更新依赖。

## 2. 检出与检查

```powershell
git clone https://github.com/Echolzt17/Fusion.git
cd Fusion
npm ci
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/prepare-network-kernel.ps1
npm run check
npm test
npm run web:build
```

`npm run check` 执行 TypeScript 检查和 Rust workspace 检查；`npm test` 执行独立核心测试；`npm run web:build` 生成前端产物。首次 Cargo 编译可能较慢。若需要限制核心测试并行度，可使用 `npm test -- -- --test-threads=4`。

继续已有开发分支时，先提交或保存本机改动，再执行 `git pull --ff-only`。更新源代码不等于更新已安装的软件，也不自动启用账号池。

## 3. 启动开发环境

```powershell
# 桌面开发：启动 Vite 与 Tauri
npm run dev

# 只调试前端
npm run web:dev
```

Tauri 开发版通过 IPC 调用 Rust。仅打开前端浏览器预览并不具备真实 Tauri IPC；UI 自动测试会注入公开假数据。开发桌面实际使用当前 Windows 用户的数据目录，调试前请正常退出已安装的 Fusion，并结束或保存日常 Codex 任务；点击真实启动/关闭按钮会执行正常的接入切换和客户端退出流程。

新电脑不会从源码仓库获得账号或登录状态。需要真实业务联调时，在该电脑分别完成 Codex 自身登录和 Fusion 账号授权；不要复制其他电脑的凭据管理器、`auth.json`、池数据库或含本机密钥的 provider 配置。

## 4. 目录与边界

| 位置 | 职责 |
| --- | --- |
| `crates/pool-core` | 账号、OAuth、网关、调度、上下文绑定、本地存储与统计；不依赖 Tauri |
| `src-tauri` | 桌面生命周期、Windows 窗口、IPC 权限、托盘、更新与安装保护 |
| `src` | React 界面，只通过 IPC 操作本机后端 |
| `tests/ui` | Playwright 页面与交互回归，使用假 IPC |
| `scripts` | 构建、验证与发布工具；运行发布工具不属于普通开发步骤 |
| `docs` | 架构、维护状态、验收与更新说明，以及依赖许可文本 |
| `public` | 随包提供的图标与字体资源及许可 |

先阅读根目录 `AGENTS.md`、`docs/ARCHITECTURE.md` 和 `docs/MAINTENANCE.md`。修改对应模块并保留无关工作；复用代码保留固定来源及许可说明。

## 5. 验证改动

| 改动范围 | 检查 |
| --- | --- |
| 授权、转发、调度 | `npm test` |
| UI 或 IPC | `npm run check`；相关 `npm run test:ui` 回归 |
| Windows 桌面行为 | `cargo test -p personal-codex-pool -- --test-threads=4` |
| Rust 静态检查 | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| 安装包交付 | `npm run build` 后运行 `npm run test:installer` 与 `npm run check:version -- --artifacts` |

```powershell
npm run test:ui
npm run test:stress
```

UI 测试会自动构建并启动临时本机预览服务，默认端口为 15379；通过 `FUSION_UI_TEST_PORT` 可指定其他可用端口。不要并行运行多个会重建同一 `dist` 的 UI 测试。核心与安装保护测试使用临时路径和假服务；凭据读写回归只创建并清理独立测试凭据。默认忽略的原生客户端或 WebView2 专项，须按各测试要求单独执行，不能把“忽略”记为通过。

大历史统计测量单独运行，使用自动清理的合成样本：

```powershell
cargo test -p pool-core synthetic_large_history_scan_measurement -- --ignored --nocapture --test-threads=1
```

它报告构建模式、完整冷/未变化扫描、独立暖阶段采样和可取得的测试进程内存。调试构建的样本时间不能直接当作生产客户端性能；独立阶段样本不能简单相加。

真实账号、原聊天续接、工具执行、在途排空、安装升级和挂件点击/中文输入仍需按 `docs/ACCEPTANCE.md` 在验收机确认。使用独立测试项目，不为验证而损坏真实配置或断开重要任务；记录实际结果和剩余问题。

后续接手需保留待处理项：API 网关的 Claude 文字会话来源识别、Responses 可空 error 的成功归因、Messages 缓存输入用量合计，以及挂件原生 show 与 ready 的一致性。本轮 UI/多订阅发布不将这些项视为已修复；API 切换来源先新建聊天。相关模块为 `crates/pool-core/src/api_gateway.rs` 与 `src-tauri/src/widget.rs`，真实客户端结果继续单独记录。

## 6. 安装包与签名发布

普通开发无需运行签名初始化或发布流程。`npm run build` 调用 `scripts/build-signed.ps1`，构建 Windows x64 NSIS 签名安装包；首次还会下载所需打包工具与 WebView2 离线依赖。

延续现有 Fusion 更新线时，维护机必须通过项目外的受控渠道恢复**与仓库内更新公钥匹配的原签名材料**。原机的 DPAPI 文件不能直接作为另一 Windows 用户的便携恢复材料；应使用既有离线备份恢复流程。不要在新电脑生成替代密钥、修改嵌入公钥或关闭验签来绕过缺失材料。

准备签名材料后，输出通常位于：

```text
target/x86_64-pc-windows-msvc/release/bundle/nsis/Fusion_版本号_x64-setup.exe
```

`npm run updates:release -- prepare` 执行检查、签名构建并准备发布材料，不安装软件；正式发布还需要独立的仓库写入授权和实际验收记录。版本变更须同步 npm、Cargo、Tauri 与更新说明。发布脚本不会因为普通 Git 提交自动创建 Release 或推进稳定清单。具体步骤见 `docs/UPDATES.md`。

## 7. Git 与本机数据

公开仓库从 v1.0.9 开始提供审查后的当前开发快照，并保留公开仓库已有发布历史；不会一并上传原维护机的私有 Git 历史。源码包含构建所需的锁文件、资源、测试和许可文本。

`node_modules`、`dist`、`target`、测试产物、运行数据库、日志、环境密钥、发布私钥及其备份不应提交。`.gitignore` 只能防止相应未跟踪文件被添加；提交前仍应检查 `git status` 与将要公开的差异。不要把原始请求/响应、配置全文、诊断导出、账号凭据或服务器访问材料放进仓库。

源码沿用根目录 `LICENSE` 中的 CC-BY-NC-SA-4.0 许可，第三方组件保留各自许可；参见 `THIRD_PARTY_NOTICES.md` 与 `docs/THIRD_PARTY_LICENSES.txt`。
