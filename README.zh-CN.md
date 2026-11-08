# OpenCut

[English](README.md) · [简体中文](README.zh-CN.md)

OpenCut 是一套开源视频剪辑系统，由 Web 工作台和本地 Rust 引擎驱动。你可以直接在工作台剪辑，也可以让 Codex 等 MCP 客户端操作同一个工程。


## 剪辑与交付

- 整理素材、检查镜头，在多轨时间线上剪辑视频和音频。
- 添加字幕、文字、图形、蒙版、转场和关键帧。
- 预览剪辑、渲染导出，并在工作台查看任务进度。
- 撤销剪辑、比较工程修订，在工作区之间迁移工程。
- 通过 MCP、CLI、HTTP 和 SDK 自动化剪辑。[操作参考](docs/cli-mcp-capabilities.md)

## 安装到 macOS

下载与你的 Mac 芯片相符的 `OpenCut-<版本>-<芯片>.zip`，解压后双击 `OpenCut.app`。点击“安装桌面应用”可将带 OpenCut 图标的应用放入 `~/Applications`，然后点击“安装 / 启动”和“打开编辑器”。应用会安装包内运行时、启动本机服务；本机已有 Codex 时会注册 `opencut_desktop`。旧安装可点击“切换标准工作区”，选用现有的 `~/.opencut/workspace.sqlite` 数据库；旧工作区的工程留在原数据库，数据库文件会保留。窗口会显示 Metal 设备状态。升级正在运行的服务时，先点“停止服务”再点“安装 / 启动”。仅经过本地 ad-hoc 签名的网上下载版本可能需要在 macOS 中手动确认信任；使用 Developer ID 签名并公证的发行版需要 Apple 开发者账号。

安装完成后，打开 OpenCut 会自动启动已停止的服务，已就绪的服务会直接复用。“安装桌面应用”也支持替换同版本或更旧的 OpenCut，先校验新副本再替换。MCP 名称为 `opencut_desktop`，使用 stdio，固定启动命令为 `~/.opencut-plugin/opencut-mcp`，无需参数或环境变量。连接 MCP 客户端前先打开 OpenCut。

也可使用命令行安装：

从对应的 [GitHub 发行版](https://github.com/miracle-ai-studio/OpenCut-Plugin/releases) 下载桌面安装脚本，在运行 Codex 的 Mac 上执行。将 `v0.0.1-beta` 换成发行标签。脚本会按芯片选择安装包，校验文件，启动服务并注册 `opencut_desktop`。

```sh
sh install-desktop.sh install v0.0.1-beta
```

也可用已下载的平台 ZIP 离线安装，请把 `.sha256` 放在同一目录。从源码目录操作时，将命令里的 `install-desktop.sh` 换成 `scripts/install-desktop.sh`。

```sh
sh install-desktop.sh install --archive /path/to/opencut-release.zip
```

打开安装结果里的 `web_url` 进入工作台。要让 Agent 剪辑，打开新的本地 Codex 会话，让它列出 OpenCut 工程。首次安装使用 `~/.opencut/workspace.sqlite`，升级沿用已有工作区。

## 管理本机安装

| 命令 | 用途 |
|---|---|
| `sh install-desktop.sh start` | 启动工作台并返回访问地址 |
| `sh install-desktop.sh check` | 检查服务、工程列表和 Codex MCP 注册 |
| `sh install-desktop.sh doctor` | 检查运行时完整性、芯片架构和基础依赖 |
| `sh install-desktop.sh stop` | 停止本机服务 |
| `sh install-desktop.sh update v0.0.1-beta` | 升级到指定版本并重新启动服务 |

升级前等任务完成，再执行 `stop`。使用 `update --archive /path/to/opencut-release.zip` 可以从本地 ZIP 升级。升级完成后运行 `check` 验证连接。[运行手册](docs/operations.md)

## 构建桌面安装包

在对应架构的 Mac 上构建 `x86_64-apple-darwin` 或 `aarch64-apple-darwin`。准备该架构的媒体库和运行时清单后执行：

```sh
sh scripts/build-desktop-release.sh TARGET native-media.json runtime.json dist/releases
sh scripts/package-desktop-app.sh dist/releases/opencut-plugin-VERSION-TARGET.zip dist/releases
```

GitHub Actions 可在配置好的发布构建机上并行构建两个架构。[构建配置](docs/development.md) · [架构](docs/architecture.md) · [发行状态](docs/status.md)

OpenCut 是 [OpenCut](https://github.com/OpenCut-app/OpenCut) 的社区分支，由 [miracle-ai-studio](https://github.com/miracle-ai-studio) 维护。[MIT 许可证](LICENSE)
