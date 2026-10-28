# 开发与检查

安装、构建和启动见[README](../README.md)。Python 脚本要求 Python 3.11+。Rust 使用根 Cargo.lock，JavaScript 包使用各自 npm 锁文件；生成契约保持可审阅格式。

## 边界与行为

仓库不包含 GitHub Actions 流水线。按实际改动范围在本地运行以下检查。

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

查看 `scripts/verify-web.py --help`，显式指定 CLI 和网页目录。素材使用合成或获准的公开样本，测试现场放在仓库外。真实 Finder、系统 IME、物理音频、人工审片和系统保存另作人工检查。

```sh
python3 scripts/package-local.py --help
python3 scripts/verify-relocated-runtime.py --help
python3 scripts/verify-package-exchange.py --help
```

包内程序、Web、SDK、运行时与许可证必须绑定准确身份。构建成功不等于完整发行验收；解包、搬迁、签名、安装与升级各自验证，见[交付状态](status.md)。

当前 macOS 装配要求 `--native-media-spec /absolute/path/to/native-media.json`。该可信构建输入使用 `opencut.native-media-input`，声明 `target`、媒体库安装 `root` 和对应源码/许可证的 `provenance_root`。后者的 `build.json` 记录五个已识别 FFmpeg 共享库的字节、禁用 GPL/nonfree/version3 的配置和静态 dav1d 来源；同时保留 `build-media-runtime.py`、`sources` 中的对应源码归档和 `licenses` 中的原文。打包只修改副本，拒绝未记录的非系统动态依赖；尚不支持的平台明确失败。

`--binary` 可复用已构建的 CLI，`--render-worker` 指定同批 worker，缺省取 CLI 的相邻文件。`--web-root` 和 `--reuse-sdk-dist` 复用已验证的构建。可选 `--runtime-spec` 使用 `opencut.native-runtime-input`，只接受 Node/Python 安装及原始再分发许可证；不再接受旧 Chromium 构建输入。旧清单读取不构成后端回退。搬迁会改变程序和库哈希，生成契约检查使用装配后的确切 CLI，媒体验收也必须绑定实际分发身份。

需要独立 Classic 源码作为预期值的专项检查，通过 `OPENCUT_CLASSIC_SOURCE_ROOT` 指定仓库外固定对照源码的绝对路径。该路径不是产品运行依赖；测试继续校验实际源码内容与来源身份。字体目录生成使用仓库内随产品交付的固定字体 atlas。
