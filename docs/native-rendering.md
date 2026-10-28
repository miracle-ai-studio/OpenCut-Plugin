# 原生渲染替换

目标是直接替换浏览器媒体执行链路。工程格式、公开编辑和渲染入口继续由现有 Rust 应用层定义，不添加产品版本开关、并行旧后端或失败后的隐式回退。当前提交 `74afa429` 保留替换前的性能候选及独立像素失败证据。

## 实现与复用边界

- 现有 composition 编译、时间采样、资源锁和应用执行会话继续负责业务语义。
- 原生 renderer 只接收已准备的像素、几何、混合、蒙版与效果指令；通过原生 wgpu 执行现有固定 WGSL。浏览器 Canvas 的颜色、字形、路径栅格化需要显式适配，不能把相似画面当作等价。
- 原生媒体适配器负责受限解码和音频处理，复用已有 FFmpeg 进程生命周期与授权资源。GPU 输出直接进入 Rust sink 和编码器，不再通过 Playwright/Node 字符串传输。
- 宿主装配实现并保持固定修订、取消、租约、预算和发布围栏。GPU、编解码器及字体的实际身份进入执行证明；历史存储证明保持原样。
- Web 保留编辑和显示职责，预览与导出消费同一原生执行语义。来源图、暂停/seek/恢复、预览缓存和音频设备协议一并接入。

## 依赖边界说明

新增 `opencut-render-worker` 专用宿主进程，仅依赖原生适配器，不解析 CLI 命令或公开工程路径。父进程保留资源授权和发布责任，通过有界二进制管道传输验证字节、采样观察和 RGBA，独立监测 RSS、逐帧/任务期限并终止整个进程组。该进程在一次视频任务中复用 GPU 和解码器。

新增 `opencut-renderer` 原生适配器，依赖纯 `opencut-contracts` 中的几何和效果事实，不依赖应用、存储、CLI 或 Web。`wgpu` 限于该适配器，启用原生 Metal/Vulkan/DX12 与 WGSL，不启用浏览器后端；`pollster` 负责本地设备初始化，`sha2` 记录嵌入着色器身份。依赖基线按这些具体边逐项维护，不重生成允许列表。此模块是最终原生执行层，不是临时第二套业务操作。

同一适配器使用仓库已有的 `tiny-skia` 栅格化已采样的几何路径。独立文字验收发现轮廓填充的抗锯齿与 Canvas 不同，因此文字改由固定 `skia-safe` 的原生 HarfBuzz/Skia 执行；不保留第二套文字后端。使用官方 CPU textlayout 构建及默认 CPU 特性以复用其固定二进制缓存，不启用 Skia GPU，不将附带的 PDF/JPEG 接口暴露为业务操作。字体仅从验证过的字节创建，单一固定字体迭代器与空 fallback manager 拒绝系统替代字形。`ttf-parser` 核对真实字体元数据；中线按 OS/2 字体的 em 比例和 1/64 px 规则归一化。新增依赖边仅位于原生适配器，动画、布局意图、图层顺序、资源选择和字体授权仍由现有 composition/application 准备。

文字适配依据 [Chromium TextMetrics](https://github.com/chromium/chromium/blob/153.0.8010.12/third_party/blink/renderer/core/html/canvas/text_metrics.cc)、[归一化字体度量](https://github.com/chromium/chromium/blob/153.0.8010.12/third_party/blink/renderer/platform/fonts/simple_font_data.cc) 和 [macOS SkFont 配置](https://github.com/chromium/chromium/blob/153.0.8010.12/third_party/blink/renderer/platform/fonts/mac/font_platform_data_mac.mm)。原生实现仍须通过独立 Canvas 像素和布局门槛，不以共用库名称推断等价。执行证明另外记录操作系统构建、架构和字体平台，预览缓存包含这些事实；旧证明按其原始字段读取，不能用当前依赖常量改写历史结果。

默认 Canvas 的空格边界依照 [PlainTextNode 分词整形](https://github.com/chromium/chromium/blob/153.0.8010.12/third_party/blink/renderer/platform/fonts/plain_text_node.cc) 处理：保留词内 kerning，分别整形空格；不能通过全局关闭 kerning 修复词间差异。整行 ICU 双向级别先决定视觉顺序，再划分单词，避免逐词重新推断段落方向。[精确字体度量](https://github.com/chromium/chromium/blob/153.0.8010.12/third_party/blink/renderer/platform/fonts/skia/skia_text_metrics.cc) 使用字形路径边界，并按 Canvas 的定点转换方式调整原生字宽。当前三个文字样例尚不能证明所有 Unicode、复杂脚本、字体合成及平台组合等价，这些仍属于完整文字验收范围。

`opencut-media` 增加固定的 `ffmpeg-next` 绑定，直接使用 libav 的解复用、解码、像素转换和重采样。原生输入只借用已验证字节端口：AVIO 每次同步操作重新绑定当前授权，限制读取块、总读量和截止时间，禁止协议与嵌套文件打开；不会把路径放进公共 DTO，也不把 CLI 当内部 RPC。解码器、相邻帧和 GPU scratch 在同一执行生命周期复用，跨请求授权不复用。

交付包必须携带与验证候选一致的库、配置、哈希、许可证和来源。当前开发机 Homebrew FFmpeg 8.1.2 含 `--enable-gpl`，不能直接据此宣称现有发行包许可和搬迁安装已通过。原生库发行装配采用明确的 LGPL 构建，关闭 GPL/nonfree 组件；编码器仍通过已有原生端口装配。库 ABI 和构建身份属于执行证明，不作为用户可选的产品版本。

现有 WGSL 直接从锁定的 vendor 目录嵌入，保留 LICENSE 和 source.json，不复制或改写来源证明。移除浏览器包前需要将这些运行资产连同许可证移到最终原生资产位置，并验证哈希保持一致。

## 集成和统一验收

连续预览复用同一原生帧会话和 worker。每个播放 owner 保留解码器、GPU 和验证资源缓存，换代、撤权、停止及有界空闲会清理进程。父进程与浏览器各有固定的图片驻留预留，容量不随每帧上报的队列字节数变化；已领取但尚未结束的二进制读取也计入预留。浏览器只校验并显示原生 RGBA；点击检测在图层完成效果和蒙版后读取单个像素 alpha，按真实堆叠顺序返回。

同一受限工作进程协议支持 CPU 文字测量，复用渲染的字体布局实现，测量时不创建设备或整张画布；同一次字幕请求复用进程处理所有 cue，保留授权、期限和驻留限制。公开字幕输入不再接受客户端观察，Web 一次请求即可取得布局和原子编辑计划。内部测量事实位于应用层，具体进程与字体实现留在原生适配器，不新增业务入口或领域依赖。

当前候选已通过同一 CLI 的生成合同、65 项应用/宿主/原生检查、真实帧传输和 4 秒编辑器播放/暂停/续播/seek，以及原有文字和视频独立像素门槛；这属于限定验收，长片、4K、原生音频和完整交付仍开放。

集中实现 GPU 合成、受限解码、文字/图形/效果、音频、来源图和宿主接入，再执行一次集成构建。实现中采用静态接口/边界检查，不为细小改动反复编译或加载工作台。集成后的同一显式 CLI 生成和核对所有公共契约，浏览器及媒体验收串行使用现有自有合成工作区。

验收同时覆盖独立像素和 PCM、同修订预览/导出、冷/热启动、连续播放、跨片段 seek、取消与失效、长源峰值内存、完整导出吞吐、搬迁安装和插件调用。失败保持原测量及原请求，只重跑变化或失败的范围。当前尚在实现阶段，不能据此删除已有行为测试或标记交付完成。
