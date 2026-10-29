# 公共接口

业务操作由 Rust 注册声明定义，CLI、HTTP、MCP、领域动作和生成 SDK 消费同一声明。不通过手写 DTO、兼容别名或另一套客户端领域模型扩展接口。

视频导出沿用 `render.video`，`options.resolution` 可显式选择 `p720`、`p1080`、`p1440`、`p2160`。新 Web 导出默认提交 `p1080`；2K 标签对应 2560×1440，4K 对应 3840×2160。横屏按相应 16:9 框、竖屏按反向框等比容纳，方形按相应短边尺寸；保留宽高比，不裁切，编码尺寸取偶数，小画布可放大。帧率不变，场景工程尺寸不被改写。历史省略分辨率及显式 `source` 继续输出原尺寸，旧请求重试不得插入新默认值。内部播放缓存继续显式使用 `preview540`，不采用导出默认值。新增枚举的当前构建/生成/验收状态见 `status.md`。

```sh
target/release/opencut --json system capabilities
target/release/opencut --json system domains
target/release/opencut --json system schema
target/release/opencut --json system domains --domain edit --schemas
```

CLI 通用入口为 `call <operation> --input <request.json>` 和 `domain <domain> <action> --input <request.json>`。HTTP 暴露相同业务语义，Agent 使用独立 Bearer 凭证。标准输入输出 MCP 使用 `mcp serve`；独立 Worker 使用同一数据库，或由连接的 `--with-worker` 管理。实际选项以 `--help` 为准。

`opencut_project_create/list/inspect` 管理工程，`opencut_edit_plan/apply` 处理原子编辑。读取固定 revision 后构造计划，提交携带 expected revision 与稳定幂等键。冲突或未知结果不能触发隐式重写请求、换 revision 或另一条后备写入口。

工程封面使用现有编辑入口的 `cover_set` 参数，CLI、HTTP、MCP 和 SDK 共用同一 Rust 类型。封面是工程修订的一部分；复制、历史恢复与工程包保留该引用。`cover: null` 恢复自动封面；`{"kind":"image","asset_id":"poster"}` 选择已绑定图片；`{"kind":"video_frame","asset_id":"video","time":{"ticks":"75","timebase":25}}` 选择源视频第 3 秒的画面。源时间不受时间线修剪、变速、标题和特效影响，必须满足 `0 <= time < 素材时长`。素材来自当前修订中的固定版本，禁止路径、外部 URL 或无授权哈希充当封面资源。删除引用中的素材必须在同一事务内明确清除或替换封面。

例如将以下请求保存为 `cover.json`，先运行 `opencut --database <数据库> --json call opencut_edit_plan --input cover.json` 检查，再用 `opencut_edit_apply` 提交完全相同的请求；领域入口为 `domain edit plan/apply`。MCP 的 `opencut_edit` 使用 `{"action":"apply","input":<同一请求>}`；展开的工具入口同样接受该请求。先通过 `system domains --domain edit --schemas` 或 MCP 工具发现读取实际参数。

```json
{
  "project_id": "my-project",
  "expected_revision": "rev_已观察到的固定修订",
  "idempotency_key": "my-cover-change",
  "label": "设置视频帧封面",
  "operations": [
    {
      "type": "cover_set",
      "cover": {"kind": "video_frame", "asset_id": "video", "time": {"ticks": "75", "timebase": 25}}
    }
  ]
}
```

上传图片复用 `opencut_upload_begin`、顺序分块、`opencut_upload_complete` 与公开导入任务结果；完成后在同一个编辑事务中先 `asset_bind`（`source.kind = project_media`，携带观察到的 `media_id` 和完整 `version`），再 `cover_set`。图片不需要放入时间线。没有独立封面写入口或浏览器封面数据库；图库缩略图仍通过授权的固定版本素材生成，不提交组合渲染任务。

外部工程通过普通工程包导入、导出及其历史恢复合同交换。旧浏览器 Classic 工程恢复不再提供公开操作或专用页面；已保存的工程仍由正常工程合同读取。

素材、字体与资源通过授权引用和版本绑定。上传采用有界顺序分块及完整哈希，导入成功后消费其发布结果。上传完成不等于已把素材放入时间线。

`opencut_render_submit` 统一提交渲染，`opencut_job_view` 读取公开任务。事件、取消及产物操作从相应领域发现。已接受、已排队或生产进度均不证明成功、可下载或已经输出到设备。

下载和连续媒体流使用宿主公布的专用传输合同，不假设所有传输端点都是 JSON 工具。通过公开结果、artifact handle、长度和哈希验证实际产物，不读取私有路径。

质量检查通过公开 artifact identity 选择已发布的配置化整段/范围视频产物。服务端按原始存储类型还原准备与证明，公开任务名称用于对外分类；检查继续核验原修订、产物权限和当前发布状态，不重写原执行证明。

当前工作区的画面预览请求只提供播放会话、精确时间、分辨率、准备意图及可选点击点；解码、文字/图形栅格和命中观察由原生执行返回。完成帧给出像素长度、尺寸、SHA-256 及一次性 `picture_id`。`/api/preview/picture/prepare` 接受 Rust 声明的 `scene` 或 `edit_draft`，调用原帧用例并在响应前领取像素；响应为 `application/vnd.opencut.picture`：12 字节前缀依次为 ASCII `OCVP`、u32 little-endian JSON 长度和 RGBA 长度，随后是最多 2 MiB 的生成帧头和最多 64 MiB 的 straight RGBA8。头信息及像素每个 64 KiB 块重新检查授权、owner、lease、generation 和 overlay。SDK 在验证帧头后返回已领取事实，像素另以 Promise 返回，完整长度和 SHA-256 校验前不可呈现。既有工具结果仍可通过 `/api/preview/picture/stream` 单独领取；两个传输共用同一读取者和业务用例。新帧意图废止尚未读取的旧描述符；已开始读取的内存继续计费到读取者释放。当前候选已通过限定的原生 HTTP 和编辑器动线，完整范围及候选身份以[状态记录](status.md)为准。

文字画布计划返回原生完成的布局和 CPU 测量身份。普通字幕、严格 ASS 和 Classic ASS 规划由 Rust 在同一请求内完成固定字体测量及编辑计划，公开请求不接受客户端 `observations`。缺少字体仍返回原有字体需求与警告；应用编辑继续使用既有固定修订、幂等的普通原子提交入口。

SDK 保留结构化错误、request identity、取消信号和未知写入结果；观察超时不终止任务。浏览器使用同源 HttpOnly 会话，Agent 使用独立主体。读取、编辑、任务、资源和管理权限分别校验，执行和结果返回前保持授权围栏。能力发现不是执行许可。

[生成与检查](development.md)使用同一显式已构建 CLI。数据格式版本、数据库迁移编号和第三方包版本是实际兼容合同，必须保持准确。

帧解码准入 DTO 的可选 `decoderSurfaces` 由 Rust 执行准备提供：缺省为旧合同的 `4`，`2` 为画布传输，`1` 为直接视频帧。它是只读执行事实；不允许调用方藉此修改项目或放大准入。媒体图与工作面合同在字幕重排、预览和导出绑定中必须保持一致，旧记录缺省语义不变。

播放缓存每代仍重新取得原生授权；同一修订、同一运行时和相同窗口回执可以复用已下载内容及热解码器。暂停保留解码器最多 30 秒，切到需要释放资源的执行方式、缓存身份变化或关闭工作区时释放。界面区分核验缓存、恢复已有预览与真正的冷准备。缩略图与波形仍共用单解码队列，既有可见区域加载、合并请求、取消与容量限制继续生效。

实时音频传输声明 `pcm_stage=mastered`，每个 OCAP 头及能力发现均由共享 Rust 类型生成。客户端核验后直接呈现母带处理后的 Float32 PCM，不再增加压缩器、输出增益或同一预读延迟；缺少或未知处理阶段明确拒绝。该事实描述输出处理阶段，不增加后端选择或修改工程、历史证明及设备消费合同。当前候选验证范围见状态记录。

来源图身份保留历史浏览器证明的精确读取，并声明原生 worker、解码库、CPU 栅格和平台身份。新来源图执行及当前 Web 回执仅接受原生身份；缓存绑定实际执行身份，不能把旧证明重新标记为原生结果。缩略图、任意源时间选帧和波形仍使用现有共享操作，原生来源波形保留全部声道、128 样本峰值桶及实际解码 EOF，不应用元素增益或母带处理。当前集成的构建和行为验收以交付状态为准。
