# OpenCut 本地运行包

原生渲染运行包包含相邻的 `bin/opencut`、`bin/opencut-render-worker` 和 `bin/lib` 媒体库；对应源码、构建记录与许可证保存在 `THIRD_PARTY/native-media`。程序无需浏览器渲染运行时。资源目录解包所需的 Node、字体派生所需的 Python/fontTools，以及导出所需的 FFmpeg/ffprobe，是否随包提供以清单为准；未提供的依赖必须显式安装。环境诊断通过也不能代替项目渲染验收。

进入解包目录，完整辅助运行时包可直接使用包内 Python 诊断：

```sh
bin/libexec/runtimes/python/bin/python3.14 -I -B doctor.py
bin/opencut --json system capabilities
bin/opencut --json system domains
```

清单未提供 Python 的包使用已安装的 Python 3.11+ 运行 `python3 -B doctor.py`。媒体工具的原始许可证与对应源码/构建记录位于 `bin/libexec/runtimes/ffmpeg` 和 `bin/libexec/runtimes/ffprobe`，清单记录每个文件身份。

选择工作区和私有服务目录，启动本机服务：

```sh
bin/opencut --database /absolute/path/to/workspace.sqlite --json \
  server --directory /absolute/path/to/service start \
  --listen 127.0.0.1:8788 --web-root "$PWD/web"
bin/opencut --json server --directory /absolute/path/to/service status
bin/opencut --json server --directory /absolute/path/to/service stop
```

返回 ready=true 后打开 web_url。浏览器使用本机 HttpOnly 会话，Agent 使用独立 Bearer 凭证，凭证不应进入日志或版本控制。复用工程必须选择原数据库和关联资源；服务配置文件存在时会参与启动设置，核对返回的工作区与可执行程序。

包内 `contracts` 是由同一 Rust 声明导出的公共接口，`sdk` 包含独立 SDK。使用 `system domains --domain <name> --schemas` 发现实际输入、输出与权限。标准输入输出 MCP 使用同一数据库，显式选择托管或独立 Worker。

固定修订编辑先 plan 后 apply，携带 expected revision 和原始幂等键。未知结果先查询原请求或 Job，不自动改键重提。下载依赖公开 artifact handle、权限、长度与哈希，不从私有路径读取。

更新前停止原服务并按 SQLite 要求备份数据库和关联资源，再替换程序与网页。不要覆盖正在运行的媒体执行包或删除活动资源。存储回收使用正式计划和 publication fence。

`manifest.json` 记录包文件身份，`status.json` 记录交付资格及已知限制。包构建成功不等于完整功能、签名、升级或跨平台发行验收通过。

macOS 桌面安装入口按 `x86_64-apple-darwin` 和 `aarch64-apple-darwin` 分别选择发行包。分别在对应架构的原生构建机上组装 CLI、Worker、媒体库、Node 和 Python。发行构建用 Rust `--remap-path-prefix` 和原生编译器的文件路径映射去除构建机目录，再用 `scripts/package-plugin.py --runtime-archive` 校验完整运行包，最后通过 `scripts/install-desktop.sh --archive` 在同架构机器上验收。

`scripts/package-desktop-app.sh` 把同一平台 ZIP、校验文件与安装入口封入可双击的 `OpenCut.app`，使用系统 Swift/AppKit 编译入口并进行本地 ad-hoc 签名。应用首次启动显示服务与 Metal 状态；用户操作“安装 / 启动”完成安装、Codex 注册和服务启动，再打开 Web 编辑器。发行工作流为每个平台同时产出应用 ZIP 和校验文件。ad-hoc 签名不替代 Apple Developer ID、公证或真实用户桌面会话的原生画面验收。
