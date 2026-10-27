# 三个平台的安装包发布

[返回 README](../README.md) · [发布工作流](../.github/workflows/release.yml)

工作流在对应 GitHub runner 上构建：Windows x64、Linux x64，以及 macOS Apple Silicon / Intel 两种架构。Linux 生成 deb/rpm 和 AppImage。产物上传到同一个 **Release 草稿**；各构建保存自己的更新清单，最后由 `verify-updater` 合并四个平台的条目并核对下载地址属于本次 Release，避免并发写入覆盖。全部任务成功后再手动发布。

v0.1.0 的四个平台构建及清单合并已在 GitHub 运行成功，更新签名和公开清单已验证；不同平台实际安装及跨版本升级仍需实机验收。

## 1. 配置更新签名 Secret

本机已有签名私钥 `~/.tauri/free-api-chat.key`，对应的 `.pub` 内容已写入 `src-tauri/tauri.conf.json` 的 updater 公钥。接续核验时，使用该私钥签署本机临时测试文件，再用配置中的公钥验证，结果通过。不要重新生成一套不同密钥覆盖它。

打开 GitHub 仓库的 **Settings → Secrets and variables → Actions → New repository secret**，添加：

| 名称 | 值 |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | `~/.tauri/free-api-chat.key` 的完整文件内容；不是路径，也不是 `.pub` 内容 |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 私钥密码；本次本机密钥使用空密码，可不添加此项 |

私钥保留在本机并通过 GitHub 加密 Secret 提供给 CI，不提交到 Git、README、Issue 或聊天。`GITHUB_TOKEN` 由 Actions 提供，无需另外创建同名个人 token。

此密钥用于**更新包验签**，不等于 Apple Developer 证书或 Windows 发布者证书。macOS 构建配置了 ad-hoc 签名，仍未提供 Apple 公证；Windows 也可能显示未知发布者提示。更换 updater 公钥会影响旧客户端对新更新的验证，已安装的旧版本可能需要手动安装一次新包。

参见 [GitHub Secrets](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/use-secrets)、[Tauri Updater](https://v2.tauri.app/plugin/updater/) 和 [Tauri GitHub 构建说明](https://v2.tauri.app/distribute/pipelines/github/)。

## 2. 一条命令准备版本并推送

先提交想随新版发布的代码，然后在项目根目录运行：

```bash
# 可先预览，不修改文件、不创建标签，也不联系远端
npm run release -- v0.1.1 --dry-run

# 确认版本后，自动更新版本文件、提交、打标签并推送
npm run release -- v0.1.1
```

也可以写 `v1.2`，脚本会补齐为 `v1.2.0`；版本必须是稳定版数字，不能使用 `v1`、前导零或 `-rc`。脚本自动同步 `package.json`、`package-lock.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`、`src-tauri/Cargo.lock`，只提交这五个版本文件。脚本不会修改模型配置、签名密钥或用户数据。

正式运行前，脚本检查是否在 `main`，工作区是否干净，本地及远端标签是否已存在，以及远端 `main` 是否有本地尚未包含的提交。工作区有改动时，先自己检查并提交需要发布的文件；脚本不会替你决定是否把它们一起提交。预览不检查远端，实际执行时才会检查。

若输入版本与当前版本相同，且该标签在本地和远端都不存在，脚本不会创建空提交，而是直接将标签指向当前提交。Git 标签可以指向已有提交，但同名标签不能重复使用；例如当前已有 `v0.1.0`，下次可用 `v0.1.1`。

脚本会推送 `main` 和新标签，触发 GitHub Actions，**但不会替你发布 Release 草稿**。如果推送失败，已完成的版本文件、提交或标签保留，脚本会说明停止阶段和对应的继续推送命令；不要重新创建同名标签或强推。

手动重跑：在 GitHub Actions 中打开 Release，选择 **Run workflow**，填入已经推送的版本标签。工作流会检出并构建该标签，不会把 `main` 分支名当作版本号。

创建 Release 时，`releaseCommitish` 使用检出后的完整提交 SHA；`tagName` 仍是版本标签。首轮远端构建曾复现 `Validation Failed: target_commitish`，工作流已按真实回执修正。已有标签无需移动。

## 3. 查看构建与草稿

打开 Actions 检查四个构建任务的日志，再到 Releases 打开对应版本草稿，核对安装包与架构：

- macOS：按 Apple Silicon / Intel 选择对应 dmg。
- Windows：msi / exe。
- Linux：deb / rpm / AppImage，以及 AppImage 的签名。
- 更新清单：`latest.json` 同时含 `darwin-aarch64`、`darwin-x86_64`、`windows-x86_64`、`linux-x86_64`，每项包含签名和下载链接。

确认最后的 `verify-updater` 任务成功，再把草稿发布为正式的最新版本。草稿和私有仓库资产无法让普通客户端匿名下载；不要把维护者的 GitHub Token 写进客户端。若源码仓库继续保持私有，需要另设公开的发行仓库，并同步调整工作流的发布目标和客户端地址。

首次运行可能暴露平台依赖或源码兼容问题，以实际日志修复。工作流不会因为本机 Linux 检查通过就宣称 Windows/macOS 已通过。

如果仍显示 **The job was not started**，先核对 GitHub 账户、组织、Actions 权限与账单状态；这是远端任务尚未启动，不是应用编译错误。优先根据实际 GitHub 日志定位原因。

## 4. 更新服务与公开源码

客户端已切换到：

```text
https://github.com/qqw52491-hue/free_api/releases/latest/download/latest.json
```

启动时检查一次，之后前台每小时检查；侧栏底部“应用更新”显示状态。发现新版后由用户点击下载，Tauri 验签、安装并重启。失败会显示错误，不再把网络失败或尚未发布当成“已是最新版本”；重启失败可以再次重启，无需重复下载。更新检查和下载安装都不需要微信桥、通道 token、个人域名或 cloudflared。

本流程的 Linux 主更新包为 AppImage。当前 deb/rpm 安装在更新面板引导到发布页手动下载，避免回退匹配 AppImage 后下载到不同安装形式的包；这不是断言 updater 2.13 不支持原生包，而是当前构建和签名清单尚未接通并验收这两条路径。Windows/macOS/AppImage 的真实升级也仍需发布后实测。

此前安装的旧版本仍携带旧更新地址，不会因为修改仓库配置就自动切换，首次需要手动安装使用新地址构建的版本。本机打包的方式也仍然可行，但需要用同一私钥签名，并把安装包、签名和正确的平台清单上传到同一个 GitHub Release；当前工作流自动完成的是 GitHub 构建路径。

微信远程控制的隧道配置与应用更新独立。本次没有停止外部隧道或改变通道自动启动设置；旧自托管发布脚本和后端路由保留兼容，桌面界面已移除旧“发布升级包”入口。

本地回归：`node scripts/test-github-updater.mjs`，覆盖在途检查共享、网络错误、下载进度、重复安装、签名失败、重启失败恢复、deb/rpm 手动安装分流，以及乱序合并、缺平台、错误版本、缺签名和非本次 Release 地址拒绝。

当前源码中的远端 MCP token 与内置 TinyFish Key 已移除，但旧提交仍包含原值。删除当前文件中的值不会清除 Git 历史。公开前需要处理旧凭据；优先在服务端作废或轮换，是否重写历史或另建只含当前快照的仓库应由维护者决定，本次没有执行这些操作。参见 [GitHub 敏感数据处理说明](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/removing-sensitive-data-from-a-repository)。

发布脚本回归：`node scripts/test-release-version.mjs` 在临时仓库和本地模拟远端验证版本同步、提交和标签、同版本标签、未提交文件、标签冲突、远端领先以及推送失败后的恢复提示。
