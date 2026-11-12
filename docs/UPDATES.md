# Fusion 发布与安全更新

固定仓库为 https://github.com/Echolzt17/Fusion。当前候选测试版为1.0.14-rc.1，稳定版为1.0.13。本次仅上传候选安装包及对应源码，不修改updates/preview.json或updates/stable.json，不把候选自动推送给日常客户端。已安装客户端仍沿用稳定更新线。

## 使用者

候选版从对应GitHub预发布页手动下载。安装前请在Fusion正常关闭账号池并退出；账号池关闭完成后Codex保持退出。不要直接替换正在运行的EXE。覆盖安装保留账号数据，不迁移账号或跨厂商上下文。真实验收状态见MAINTENANCE和ACCEPTANCE。

稳定更新的检查/下载/签名与版本保护保持。当前正式更新清单拒绝预发布版本；候选包独立核验Minisign、可信注释与版本绑定，不为上传候选放宽正式客户端保护或默认验收门槛。

## 维护者

1. 同步npm/Cargo/Tauri版本、说明和公开源码，保留锁文件、资源、测试与许可；业务文件必须等于审查后的本机源码。
2. 使用项目外受保护的原签名材料与仓库写入授权，不打印或提交私钥、密码、令牌或DPAPI。新维护机见DEVELOPMENT。
3. 当前源码以远端公开main为唯一父，保留两条现有更新清单，不推送维护机私有Git历史、数据、日志或诊断。
4. 候选标为prerelease，不替换既有版本附件或改写稳定发现清单。公开下载字节核对、源码上传和实际客户端验收分别记录。
5. 将来正式发布需明确授权，按既有稳定版本/签名/验收流程准备新的正式包；不把rc文件改名冒充正式构建。

正式端点为 https://raw.githubusercontent.com/Echolzt17/Fusion/main/updates/stable.json。更新签名不是Windows Authenticode证书，不生成替代公钥或关闭验签。
