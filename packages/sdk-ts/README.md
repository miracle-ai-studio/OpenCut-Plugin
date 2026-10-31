# @opencut/sdk

OpenCut 的类型化 HTTP 客户端。编辑语义、权限和任务状态由宿主维护，SDK 不拥有工程副本或原生存储。

```sh
npm ci
npm run build
npm test
npm run boundaries:check
```

```ts
import { OpenCutClient } from "@opencut/sdk";
const client = new OpenCutClient({ baseUrl: "http://127.0.0.1:8788", token });
const capabilities = await client.hostCapabilities();
const project = await client.inspectEditorProject({ project_id: "project-id", revision: null });
```

`call` 和 `callEnvelope` 按生成 ToolMap 校验工具输入/输出，`callDomain` 与 `callDomainEnvelope` 按 DomainMap 调用领域动作。`ToolInput`、`ToolOutput`、JobResultMap、事件与媒体传输类型来自 Rust 注册声明，不手写副本。

编辑使用 `opencut_edit_plan/apply`，任务读取使用 `opencut_job_view`；支持范围从宿主能力和领域发现读取。公共工程视图区分可编辑与只读输入；辅助方法不会把只读内容伪装为可编辑工程。

错误保留 API code、request identity 和 warnings。普通调用不自动重试未知写入结果；保留原请求、固定 revision 与幂等键后显式恢复。等待超时或中止观察不会取消已接受任务。下载、PCM 流及上传助手遵守专用传输合同、哈希、范围和取消约束。

`prepareVisualPreviewPicture` 在一次请求内准备并领取场景或草稿画面，返回已校验的 `header`、完整像素的 `pixels` Promise 和 `cancel`。收到帧头只允许准备下一帧，显示前必须等待像素完成长度及显式 BLAKE3 校验；中途取消不重试已经领取的帧。既有工具结果仍可用 `readVisualPreviewPicture` 单独领取，两个传输调用同一宿主用例。

仓库开发与同二进制生成检查见根 README。发布包只包含 package metadata、README、JavaScript 与类型声明。

Native preview pixels are verified with a bundled BLAKE3-256 WebAssembly SIMD
module in both Node and browser workers. Complete pixel length and digest checks
still precede presentation. Clients without WebAssembly SIMD receive
`PREVIEW_DIGEST_UNAVAILABLE`; there is no alternate algorithm or unverified path.
The module's source, pinned build recipe and provenance are in `preview-digest/`;
ordinary SDK builds verify the checked-in artifact without compiling native code.
Third-party notices are included in `LICENSE`.
