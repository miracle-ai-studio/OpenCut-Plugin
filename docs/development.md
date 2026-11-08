# 开发与检查

安装、构建和启动见[README](../README.md)。Python 脚本要求 Python 3.11+。Rust 使用根 Cargo.lock，JavaScript 包使用各自 npm 锁文件；生成契约保持可审阅格式。

## 边界与行为

`.github/workflows/desktop-build.yml` 提供 macOS 发行构建和隔离验收，默认只运行 Intel；Apple Silicon 需手动启用。按实际改动范围在本地运行以下检查，发行工作流不能代替源码边界与专项行为检查。

```sh
python3 -m unittest discover -s scripts/tests -p 'test_architecture.py'
python3 scripts/check-architecture.py
python3 scripts/check-contract-boundaries.py
python3 scripts/check-source-hygiene.py
npm run boundaries:check --prefix packages/sdk-ts
npm run boundaries:check --prefix apps/web
cargo test --workspace --exclude opencut-desktop --locked
python3 -m unittest discover -s scripts/tests
python3 -m unittest discover -s scripts -p 'test_acceptance_runner.py'
npm test --prefix packages/sdk-ts
npm test --prefix apps/web
npm test --prefix packages/classic-audio-renderer
npm test --prefix packages/classic-visual-renderer
npm test --prefix packages/classic-source-art-renderer
```

按实际改动选择检查，分别报告编译、静态门禁、单元、浏览器与完整验收。文档更新不要求渲染媒体或构建 Rust。有状态浏览器和媒体检查串行使用同一自有合成工作区，复用原请求、Job 和成功产物。

## 生成契约

代码集成后构建一次，使用同一显式二进制检查 SDK、OpenAPI、工具、领域与 Web 类型：

```sh
cargo build --locked --release -p opencut-cli
node packages/sdk-ts/scripts/generate.mjs --check --skip-build --binary "$PWD/target/release/opencut"
python3 scripts/check-canonical-contract.py --binary "$PWD/target/release/opencut"
```

不复制 DTO，不修改依赖基线绕过失败。验证器修正且产品代码未变时继续使用原二进制。

## 浏览器与打包

macOS 原生预览性能验证须记录浏览器可见/无界面模式、实际浏览器身份和图形后端。无界面模式可验证功能，但其显示提交背压不能直接代表可见编辑器；出现慢帧时先用同一构建、保留工程和原门槛作可见窗口对照。可见窗口自动化也不等于物理显示时序或听感验收。验证器修正不触发产品重建或重复部署。

查看 `scripts/verify-web.py --help`，显式指定 CLI 和网页目录。素材使用合成或获准的公开样本，测试现场放在仓库外。真实 Finder、系统 IME、物理音频、人工审片和系统保存另作人工检查。

```sh
python3 scripts/package-local.py --help
python3 scripts/verify-relocated-runtime.py --help
python3 scripts/verify-package-exchange.py --help
```

包内程序、Web、SDK、运行时与许可证必须绑定准确身份。构建成功不等于完整发行验收；解包、搬迁、签名、安装与升级各自验证，见[交付状态](status.md)。

当前 macOS 装配要求 `--native-media-spec /absolute/path/to/native-media.json`。该可信构建输入使用 `opencut.native-media-input`，声明 `target`、媒体库安装 `root` 和对应源码/许可证的 `provenance_root`。后者的 `build.json` 记录五个已识别 FFmpeg 共享库的字节、禁用 GPL/nonfree/version3 的配置和静态 dav1d 来源；同时保留 `build-media-runtime.py`、`sources` 中的对应源码归档和 `licenses` 中的原文。打包只修改副本，拒绝未记录的非系统动态依赖；尚不支持的平台明确失败。

### macOS 发布构建

在原生 Intel 或 Apple Silicon 构建机上运行 `sh scripts/build-desktop-release.sh TARGET native-media.json runtime.json dist/releases`。脚本构建 Rust CLI 和 Worker、组装 Web 与原生运行时，再用发布检查生成平台 ZIP 和校验文件。构建输入中的 FFmpeg 组件及来源记录需提前满足发布内容检查。

手动触发 `Build OpenCut Desktop` 默认运行 Intel，勾选 `apple_silicon` 才同时运行 ARM64。专用构建机分别使用 `macOS`、`X64` 或 `ARM64` 及 `opencut-release` 标签，并配置 rustup、Node.js、Python 3.11+ 和验收用 FFmpeg。设置 `OPENCUT_RELEASE_INPUTS` 指向可信输入根目录，在对应目标三元组子目录中放置 `native-media.json` 和 `runtime.json`；设置 `OPENCUT_CHROME` 为 Chrome 可执行文件的绝对路径。当前媒体运行时、应用最低系统声明和安装器统一要求 macOS 26 或更新版本。

工作流先用 `verify-plugin-release.py` 对确切 ZIP 做隔离安装、MCP 和原生导出验收，再串行运行 `apps/web/scripts/verify-desktop-release.mjs --archive ZIP --workspace 同一验收目录`，检查实际界面、连续画面、独立 PCM 对照、暂停/继续/跳转、撤销/重做、重新加载和服务重启。后者默认无界面；本机可加 `--headed` 保留可见浏览器模式。两者复用合成工程、检查点和原有导出，不写用户工作区；证据保存在仓库外。通过后上传平台产物及安装脚本，供后续发版使用。

`--binary` 可复用已构建的 CLI，`--render-worker` 指定同批 worker，缺省取 CLI 的相邻文件。`--web-root` 和 `--reuse-sdk-dist` 复用已验证的构建。可选 `--runtime-spec` 使用 `opencut.native-runtime-input`，接受 FFmpeg/ffprobe、Node/Python 安装及原始再分发许可证；FFmpeg/ffprobe 还要求 `corresponding_sources` 列出组件根目录内的对应源码归档与构建记录（至少两个文件），发布清单保持这些相对路径和文件身份；不再接受旧 Chromium 构建输入。旧清单读取不构成后端回退。搬迁会改变程序和库哈希，生成契约检查使用装配后的确切 CLI，媒体验收也必须绑定实际分发身份。

需要独立 Classic 源码作为预期值的专项检查，通过 `OPENCUT_CLASSIC_SOURCE_ROOT` 指定仓库外固定对照源码的绝对路径。该路径不是产品运行依赖；测试继续校验实际源码内容与来源身份。字体目录生成使用仓库内随产品交付的固定字体 atlas。


### 0.0.1-beta 发布重置

使用 Python 3.11+，可设置 `PYTHON=python3.12`。每个构建机分别提供本架构的可信媒体库和工具输入，不提交机器路径，不通过改名复用另一架构的二进制。发行目录统一为 `dist/releases/0.0.1-beta`。

手动触发时 `publish` 默认关闭；推送到专用 `release/0.0.1-beta` 分支仅启动构建，不发布或删除 Release。所选 macOS 平台构建和隔离验收成功才创建预发布草稿；上传并校验每个文件的 GitHub SHA-256、大小和清单，再发布新 Release；仅本次手动勾选 `cleanup_old_releases` 时，才删除旧 `v0.5.0-beta.*` Release 及标签。源码 Git 历史不重写。

只校验目录：`python3.12 scripts/publish-release.py --directory dist/releases/0.0.1-beta`。手动发布增加 `--publish --commit <已验收的40位提交>`，说明来自 `docs/release-notes.md`；仅明确需要清理旧版时另加 `--remove-previous-releases`。缺少所选架构或文件不符会在远端写入前失败；上传失败不会删除旧 Release。

从旧版本重置编号时，先停止服务，再执行 `sh install-desktop.sh install v0.0.1-beta`，保留选定工作区。后续普通 `update` 仍禁止降级。

已构建 CLI/Worker 的再装配可显式使用 `opencut.native-media-reuse-input`，字段为 `target`、保留运行包的 `root` 和固定 `manifest_sha256`。装配器核验原生依赖及全部来源文件身份，逐字节保留原构建证明、许可证和源码；仅在新的 `runtime-reuse.json` 中记录来源。此输入不含编译头文件，不能交给 Rust 编译入口，也不接受重复再打包链。它不会把旧 CLI/Worker 冒充新程序；新程序始终来自 `--binary` 与 `--render-worker`。

本轮默认只要求 Intel。Apple Silicon 的构建和设备验证已由用户延后；后续手动触发工作流并勾选 `apple_silicon`，或对完整已验证目录增加 `--include-apple-silicon`，才会把 ARM64 纳入发布必需资产。
