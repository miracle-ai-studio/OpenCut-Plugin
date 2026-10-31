export { requestEnvelope, OpenCutApiError, OpenCutProtocolError, OpenCutTransportError, type Envelope } from "./transport.js";
import { requestEnvelope, OpenCutApiError, OpenCutProtocolError, OpenCutTransportError, type Envelope } from "./transport.js";
export * from "./contracts.generated.js";
import { toolNames, domainNames, domainActions, picturePreparationTransport, type ToolMap, type DomainMap, type JobResultMap, type AudioPreviewPcm, type AudioPreviewTransport, type VisualPreviewTransport, type VisualPreviewPreparation, type VisualPreviewPrepared } from "./contracts.generated.js";
import { hashPreviewPicture } from "./preview-digest.js";

export type AudioPreviewPcmHeader = AudioPreviewPcm.AudioPreviewPcmHeader;
export type AudioPreviewStreamInput = AudioPreviewTransport.AudioPreviewStream;
export type ToolName = keyof ToolMap;
export type ToolInput<N extends ToolName> = ToolMap[N]["input"];
export type ToolOutput<N extends ToolName> = ToolMap[N]["output"];
export type DomainName = keyof DomainMap & string;
export type DomainAction<D extends DomainName> = keyof DomainMap[D] & string;
export type DomainInput<D extends DomainName, A extends DomainAction<D>> =
  DomainMap[D][A] extends { input: infer I } ? I : never;
export type DomainOutput<D extends DomainName, A extends DomainAction<D>> =
  DomainMap[D][A] extends { output: infer O } ? O : never;
export type Job = ToolOutput<"opencut_job_view">;
export type HostCapabilities = Extract<ToolOutput<"opencut_resource_read">["payload"], {kind: "capabilities"}>["data"];
export type ArtifactReference = Job["artifacts"][number];
export type JobResult = NonNullable<Job["result"]>;
/** Narrow the sole validated,portable result union. */
export function jobResult<K extends keyof JobResultMap>(job:Job,kind:K):JobResultMap[K] {
  if(job.state==="succeeded" && job.kind===kind && !job.result_unavailable && job.result?.kind===kind && job.result.data!=null) {
    return job.result.data as JobResultMap[K];
  }
  throw new TypeError(`Expected an available successful ${kind} result.`);
}
export class JobWaitTimeoutError extends Error {
  readonly name = "JobWaitTimeoutError";
  readonly code = "JOB_WAIT_TIMEOUT";
  constructor(readonly jobId: string) { super(`Stopped waiting for ${jobId}; the job has not been cancelled.`); }
}

export class JobCompletionError<J extends Job = Job> extends Error {
  readonly name = "JobCompletionError";
  readonly code: "JOB_FAILED" | "JOB_CANCELLED";
  constructor(readonly job: J) {
    super(`Job ${job.job_id} ended with state ${job.state}.`);
    this.code = job.state === "cancelled" ? "JOB_CANCELLED" : "JOB_FAILED";
  }
}

export interface ClientOptions {
  baseUrl: string;
  token: string;
  /** Standard fetch by default; injectable for a host's network policy. */
  fetch?: typeof globalThis.fetch;
}
export interface CallOptions {
  signal?: AbortSignal;
  /** Standard fetch best-effort delivery during document departure. */
  keepalive?: boolean;
}
export type PreparedVisualPicture = {
  /** The native picture has already been claimed; pixels still require verification. */
  header: VisualPreviewPrepared.VisualPreviewPreparedPicture;
  pixels: Promise<Uint8ClampedArray<ArrayBuffer>>;
  cancel(reason?: unknown): void;
};
export interface AdmissionRetryOptions extends CallOptions {
  /** Opt-in deferral of explicit budget-lock contention; never retries an unknown outcome. */
  admissionTimeoutMs?: number;
  beforeAttempt?: () => void;
}
export interface WaitOptions extends CallOptions {
  /** Local deadline, including network time. Does not cancel the durable job. */
  timeoutMs?: number;
  pollIntervalMs?: number;
}

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function validateNativePicture(picture: unknown) {
  if (!object(picture)) throw new TypeError("Invalid native picture descriptor");
  const { byte_length: length, width, height, content_digest: digest } = picture;
  if (typeof length !== "number" || !Number.isInteger(length) || length < 4 || length > 64 * 1024 * 1024 ||
      typeof width !== "number" || !Number.isInteger(width) || width < 1 || width > 16384 ||
      typeof height !== "number" || !Number.isInteger(height) || height < 1 || height > 16384 ||
      length !== width * height * 4 || !object(digest) || digest.algorithm !== "blake3" ||
      typeof digest.value !== "string" || !/^[0-9a-f]{64}$/.test(digest.value))
    throw new TypeError("Invalid native picture descriptor");
  return { byte_length: length, width, height, content_digest: { algorithm: "blake3", value: digest.value } };
}
async function verifyNativePicture(bytes: Uint8ClampedArray<ArrayBuffer>, hash: string, signal?: AbortSignal) {
  signal?.throwIfAborted();
  const actual = await hashPreviewPicture(new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength), signal);
  signal?.throwIfAborted();
  if (actual !== hash) throw new TypeError("Native picture changed its content hash");
  return bytes;
}

function pause(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    signal.throwIfAborted();
    const abort = () => { clearTimeout(timer); reject(signal.reason); };
    const timer = setTimeout(() => { signal.removeEventListener("abort", abort); resolve(); }, ms);
    signal.addEventListener("abort", abort, { once: true });
  });
}

/** Calls are one-shot unless admission deferral is explicitly requested. No revision refreshes, implicit job cancellation or UI state. */

export class OpenCutClient {
  readonly baseUrl: string;
  #token: string;
  #fetch: typeof globalThis.fetch;

  constructor(options: ClientOptions) {
    const url = new URL(options.baseUrl);
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
      throw new TypeError("baseUrl must be an HTTP(S) URL without credentials, query or fragment.");
    }
    this.baseUrl = url.href.replace(/\/$/, "");
    this.#token = options.token;
    this.#fetch = options.fetch ?? globalThis.fetch.bind(globalThis);
  }

  /** Editor helpers keep shared generated contracts and one-shot retry semantics. */
  projectSummary(input: ToolInput<"opencut_project_summary">, options: CallOptions = {}) { return this.call("opencut_project_summary", input, options); }
  timelineQuery(input: ToolInput<"opencut_project_timeline_query">, options: CallOptions = {}) { return this.call("opencut_project_timeline_query", input, options); }
  planAgentEdit(input: ToolInput<"opencut_edit_agent_plan">, options: CallOptions = {}) { return this.call("opencut_edit_agent_plan", input, options); }
  planRecipe(input: ToolInput<"opencut_edit_recipe_plan">, options: CallOptions = {}) { return this.call("opencut_edit_recipe_plan", input, options); }
  renderRevision(input: ToolInput<"opencut_render_submit">, options: CallOptions = {}) { return this.call("opencut_render_submit", input, options); }
  searchContent(input: ToolInput<"opencut_media_content_search">, options: CallOptions = {}) { return this.call("opencut_media_content_search", input, options); }
  sourceEvidence(input: ToolInput<"opencut_source_art_evidence">, options: CallOptions = {}) { return this.call("opencut_source_art_evidence", input, options); }
  planOutputEvidence(input: ToolInput<"opencut_quality_evidence_plan">, options: CallOptions = {}) { return this.call("opencut_quality_evidence_plan", input, options); }
  inspectOutputQuality(input: ToolInput<"opencut_quality_inspect">, options: CallOptions = {}) { return this.call("opencut_quality_inspect", input, options); }
  createReadOnlyProject(input: ToolInput<"opencut_project_readonly_create">, options: CallOptions = {}) { return this.call("opencut_project_readonly_create", input, options); }
  async inspectEditorProject(input:ToolInput<"opencut_project_inspect">,options:CallOptions={}) {
    const view=await this.call("opencut_project_inspect",input,options);
    if(view.kind!=="editor")throw new Error("Project is read-only;its source is preserved.");
    return view.snapshot;
  }
  async inspectReadOnlyProject(input:ToolInput<"opencut_project_inspect">,options:CallOptions={}) {
    const result=await this.call("opencut_project_inspect",input,options);
    if(result.kind!=="preserved")throw new Error("Project is not a preserved-effects document.");
    return result.view;
  }
  readOnlyProjectManifest(input: ToolInput<"opencut_project_readonly_manifest">, options: CallOptions = {}) { return this.call("opencut_project_readonly_manifest", input, options); }
  catalogSoundRecipe(input: ToolInput<"opencut_catalog_resources_recipe">, options: CallOptions = {}) { return this.call("opencut_catalog_resources_recipe", input, options); }
  chooseProjectPackageLocation(input: ToolInput<"opencut_project_package_choose_location">, options: CallOptions = {}) { return this.call("opencut_project_package_choose_location", input, options); }
  exportProjectPackage(input: ToolInput<"opencut_project_package_export">, options: CallOptions = {}) { return this.call("opencut_project_package_export", input, options); }
  inspectProjectPackage(input: ToolInput<"opencut_project_package_inspect">, options: CallOptions = {}) { return this.call("opencut_project_package_inspect", input, options); }
  importProjectPackage(input: ToolInput<"opencut_project_package_import">, options: CallOptions = {}) { return this.call("opencut_project_package_import", input, options); }
  planPreset(input: ToolInput<"opencut_plugin_preset_plan">, options: CallOptions = {}) { return this.call("opencut_plugin_preset_plan", input, options); }
  planProjectPlugin(input: ToolInput<"opencut_project_plugin_plan">, options: CallOptions = {}) { return this.call("opencut_project_plugin_plan", input, options); }
  async call<N extends ToolName>(name: N, input: ToolInput<N>, options: CallOptions = {}): Promise<ToolOutput<N>> {
    return (await this.callEnvelope(name, input, options)).data;
  }

  /** Repeat only explicit STORAGE_BUDGET_BUSY rejection, with the same captured input.
   * Network/protocol errors, capacity faults and revision conflicts remain one-shot. */
  async callWithAdmissionRetry<N extends ToolName>(name: N, input: ToolInput<N>, options: AdmissionRetryOptions = {}): Promise<ToolOutput<N>> {
    const timeout = options.admissionTimeoutMs ?? 5000;
    if (!Number.isSafeInteger(timeout) || timeout < 1 || timeout > 30000) throw new TypeError("Admission retry deadline must be 1 to 30000 milliseconds.");
    const captured = JSON.parse(JSON.stringify(input)) as ToolInput<N>;
    const deadline = performance.now() + timeout;
    const signal = options.signal ?? new AbortController().signal;
    for (;;) {
      signal.throwIfAborted();
      options.beforeAttempt?.();
      try { return await this.call(name, captured, { signal, keepalive: options.keepalive }); }
      catch (error) {
        if (!(error instanceof OpenCutApiError) || error.code !== "STORAGE_BUDGET_BUSY" || performance.now() >= deadline) throw error;
        await pause(Math.min(100, Math.max(1, deadline - performance.now())), signal);
        if (performance.now() >= deadline) throw error;
      }
    }
  }

  /** Call one typed action through the compact domain surface. */
  async callDomain<D extends DomainName, A extends DomainAction<D>>(
    domain: D, action: A, input: DomainInput<D, A>, options: CallOptions = {},
  ): Promise<DomainOutput<D, A>> {
    return (await this.callDomainEnvelope(domain, action, input, options)).data;
  }

  async callDomainEnvelope<D extends DomainName, A extends DomainAction<D>>(
    domain: D, action: A, input: DomainInput<D, A>, options: CallOptions = {},
  ): Promise<Envelope<DomainOutput<D, A>>> {
    if (!(domainNames as readonly string[]).includes(domain) ||
      !((domainActions as Record<string, readonly string[]>)[domain] ?? []).includes(action)) {
      throw new TypeError(`Unknown OpenCut domain action: ${domain}.${action}`);
    }
    const body = JSON.stringify({ action, input });
    return this.#request<DomainOutput<D, A>>(`/api/tools/opencut_${domain}`, "POST", body, "application/json", options);
  }

  /** Typed additive discovery; the original v1 capabilities tool schema stays frozen. */
  async hostCapabilities(options: CallOptions = {}): Promise<HostCapabilities> {
    const resource = await this.call("opencut_resource_read", { uri: "opencut://system/capabilities" }, options);
    if (resource.payload.kind !== "capabilities") throw new OpenCutProtocolError("Expected the host capabilities resource.", 200);
    return resource.payload.data;
  }

  async callEnvelope<N extends ToolName>(name: N, input: ToolInput<N>, options: CallOptions = {}): Promise<Envelope<ToolOutput<N>>> {
    if (!(toolNames as readonly string[]).includes(name)) throw new TypeError(`Unknown OpenCut tool: ${name}`);
    options.signal?.throwIfAborted();
    // Serialize once before awaiting. Caller mutations cannot change an in-flight write.
    const body = JSON.stringify(input);
    return this.#request<ToolOutput<N>>(`/api/tools/${name}`, "POST", body, "application/json", options);
  }

  /** Send one raw chunk. On an unknown outcome, inspect the confirmed offset before retrying. */
  async uploadChunk(uploadId: string, offset: string, bytes: Uint8Array, options: CallOptions = {}): Promise<ToolOutput<"opencut_upload_inspect">> {
    options.signal?.throwIfAborted();
    if (!/^upload_[a-f0-9]{32}$/.test(uploadId) || !/^(0|[1-9][0-9]{0,10})$/.test(offset) ||
      bytes.byteLength < 1 || bytes.byteLength > 4194304 ||
      BigInt(offset) + BigInt(bytes.byteLength) > 17179869184n) throw new TypeError("Invalid upload identity or chunk range.");
    const body = new Uint8Array(bytes);
    const hash = Array.from(new Uint8Array(await globalThis.crypto.subtle.digest("SHA-256", body)),
      b => b.toString(16).padStart(2, "0")).join("");
    const query = new URLSearchParams({ size_bytes: String(body.byteLength), content_hash: hash });
    return (await this.#request<ToolOutput<"opencut_upload_inspect">>(
      `/api/uploads/${uploadId}/chunks/${offset}?${query}`, "PUT", body, "application/octet-stream", options,
    )).data;
  }

  #request<T>(path: string, method: string, body: BodyInit, contentType: string,
    options: CallOptions): Promise<Envelope<T>> {
    return requestEnvelope(this.#fetch, this.baseUrl, this.#token, path, {
      method, body, signal: options.signal, headers: { "Content-Type": contentType },
      ...(options.keepalive === undefined ? {} : { keepalive: options.keepalive }),
    });
  }

  /** Submission/cancellation return the same portable task view as observation. */
  submitJob(input: ToolInput<"opencut_job_submit">, options: CallOptions = {}): Promise<Job> {
    return this.call("opencut_job_submit", input, options);
  }
  cancelJob(jobId:string,options:CallOptions={}):Promise<Job> {
    return this.call("opencut_job_request_cancel",{job_id:jobId},options);
  }
  waitForJob(jobId:string,options:WaitOptions={}):Promise<Job> {
    return this.wait(jobId,signal=>this.call("opencut_job_view",{job_id:jobId},{signal}),options);
  }
  artifactUrl(reference: ArtifactReference): string {
    const id = `artifact:${reference.job_id}:${reference.content_hash}`;
    if (reference.artifact_id !== id || !/^artifact:job_[a-f0-9]{32}:[a-f0-9]{64}$/.test(id)) throw new TypeError("Invalid artifact reference");
    return `${this.baseUrl}/api/artifacts/${encodeURIComponent(id)}`;
  }
  projectMediaUrl(resource: ToolOutput<"opencut_project_media_inspect">): string {
    if (!resource.project_id || !/^media_[a-f0-9]{32}$/.test(resource.media_id) ||
      resource.asset.uri !== `opencut-media://${resource.media_id}`) {
      throw new TypeError("Invalid project media resource");
    }
    return `${this.baseUrl}/api/projects/${encodeURIComponent(resource.project_id)}/media/${resource.media_id}/content`;
  }
  /** Return authorized, hash-verified project media as a streaming response. */
  downloadProjectMedia(resource: ToolOutput<"opencut_project_media_inspect">, options: CallOptions = {}): Promise<Response> {
    return this.#downloadBytes(this.projectMediaUrl(resource), "project media", options);
  }
  /** Recompute the scoped URL; a descriptor cannot select an external URL or global CAS path. */
  projectResourceUrl(resource: ToolOutput<"opencut_project_resource_inspect">): string {
    const ids = [resource.project_id, resource.revision, resource.resource_id];
    if (ids.some(id => !id.trim() || new TextEncoder().encode(id).length > 1024 || /[\u0000-\u001f\u007f-\u009f]/u.test(id)) ||
      !["asset", "font"].includes(resource.kind) ||
      !/^[a-f0-9]{64}$/.test(resource.version.content_hash) ||
      resource.version.id !== `av_${resource.version.content_hash}`) throw new TypeError("Invalid project resource descriptor");
    const query = (id: string) => encodeURIComponent(id).replace(/[!'()*]/g, c => `%${c.charCodeAt(0).toString(16).toUpperCase()}`);
    const path = `/api/project-resources/content?project_id=${query(resource.project_id)}&revision=${query(resource.revision)}&kind=${resource.kind}&resource_id=${query(resource.resource_id)}`;
    if (resource.content_path !== path) throw new TypeError("Project resource path differs from its scoped identity");
    return `${this.baseUrl}${path}`;
  }
  downloadProjectResource(resource: ToolOutput<"opencut_project_resource_inspect">, options: CallOptions = {}): Promise<Response> {
    return this.#downloadBytes(this.projectResourceUrl(resource), "project resource", options);
  }
  /** Return a streaming response. The server verifies the published hash before serving bytes. */
  downloadArtifact(reference: ArtifactReference, options: CallOptions = {}): Promise<Response> {
    return this.#downloadBytes(this.artifactUrl(reference), "artifact", options);
  }
  /** Claim the current owner generation's continuous fMP4 stream. Cancel the response body to disconnect. */
  openPreviewStream(session: Pick<ToolOutput<"opencut_preview_create">, "session_id" | "generation" | "owner_id" | "lease_id">, options: CallOptions = {}): Promise<Response> {
    const { session_id, generation, owner_id, lease_id } = session;
    return this.#downloadBytes(`${this.baseUrl}/api/preview/stream`, "preview stream", options,
      JSON.stringify({ session_id, generation, owner_id, lease_id }));
  }
  /** Open the bounded pre-master PCM stream for one fixed audio generation.
   * Consumer reports return server-authored Time; queued frames are not playback. */
  openAudioPreviewStream(session: Pick<ToolOutput<"opencut_audio_preview_create">, "session_id" | "generation" | "overlay_epoch" | "owner_id" | "lease_id">, options: CallOptions = {}): Promise<Response> {
    const { session_id, generation, overlay_epoch, owner_id, lease_id } = session;
    return this.#downloadBytes(`${this.baseUrl}/api/preview/audio/stream`, "audio preview stream", options,
      JSON.stringify({ session_id, generation, overlay_epoch, owner_id, lease_id }));
  }
  /** Read and verify the one native picture described by the frame result. */
  async readVisualPreviewPicture(session: ToolInput<"opencut_visual_preview_frame">["session"], picture: ToolOutput<"opencut_visual_preview_frame">["picture"], options: CallOptions & {
    /** Valid scoped response headers establish the host's single-use claim.
     * This permits the next native intent, not presentation of unverified bytes. */
    onClaimed?: () => void;
  } = {}): Promise<Uint8ClampedArray<ArrayBuffer>> {
    const descriptor = validateNativePicture(picture), length = descriptor.byte_length;
    const input: VisualPreviewTransport.VisualPreviewPictureRead = { session, picture_id: picture.picture_id };
    const response = await this.#downloadBytes(`${this.baseUrl}/api/preview/picture/stream`, "native preview picture", options, JSON.stringify(input));
    if (response.headers.get("content-type") !== "application/vnd.opencut.rgba" || response.headers.get("content-length") !== String(length)) {
      await response.body?.cancel(); throw new TypeError("Native picture transport changed its descriptor");
    }
    const reader = response.body?.getReader();
    if (!reader) throw new TypeError("Native picture bytes unavailable");
    const bytes = new Uint8ClampedArray(length); let offset = 0;
    try {
      options.signal?.throwIfAborted();
      options.onClaimed?.();
      for (;;) {
        options.signal?.throwIfAborted();
        const chunk = await reader.read();
        if (chunk.done) break;
        if (chunk.value.length > length - offset) throw new TypeError("Native picture exceeded its byte bound");
        bytes.set(chunk.value, offset); offset += chunk.value.length;
      }
      if (offset !== length) throw new TypeError("Native picture changed length");
      return await verifyNativePicture(bytes, descriptor.content_digest.value, options.signal);
    } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
  }
  /** Execute the existing scene/draft frame use case and claim its pixels in one request.
   * Resolves at validated metadata; the separate pixels promise never exposes partial bytes. */
  async prepareVisualPreviewPicture(input: VisualPreviewPreparation.VisualPreviewPicturePrepare, options: CallOptions = {}): Promise<PreparedVisualPicture> {
    const local = new AbortController();
    const signal = options.signal ? AbortSignal.any([options.signal, local.signal]) : local.signal;
    signal.throwIfAborted();
    // Capture the kind before awaiting, just as the serialized request is captured.
    const kind = input.kind;
    const response = await this.#downloadBytes(`${this.baseUrl}${picturePreparationTransport.path}`, "native picture preparation", { ...options, signal }, JSON.stringify(input));
    const declared = response.headers.get("content-length");
    const length = Number(declared);
    if (response.headers.get("content-type") !== picturePreparationTransport.content_type ||
        !declared || !/^[1-9][0-9]*$/.test(declared) || !Number.isSafeInteger(length) || length < picturePreparationTransport.prefix_bytes + 5 ||
        length > picturePreparationTransport.prefix_bytes + picturePreparationTransport.max_header_bytes + 64 * 1024 * 1024) {
      void response.body?.cancel().catch(() => {}); throw new TypeError("Invalid native picture preparation transport");
    }
    const reader = response.body?.getReader();
    if (!reader) throw new TypeError("Native picture bytes unavailable");
    let chunk: Uint8Array = new Uint8Array(0), position = 0, received = 0;
    let ended = false, cancelled = false;
    let idle: ReturnType<typeof setTimeout> | undefined;
    const cancel = () => {
      if (cancelled || ended) return;
      cancelled = true;
      // Cancellation closes the reader synchronously, but an underlying source
      // may never settle its cleanup promise. It cannot hold up retirement.
      void reader.cancel(signal.reason).catch(() => {});
    };
    const progress = () => {
      clearTimeout(idle);
      idle = setTimeout(() => local.abort(Object.assign(
        new Error(`Native picture transfer made no progress for 5 seconds (${received}/${length} bytes)`),
        { code: "PREVIEW_PICTURE_TRANSPORT_STALLED" },
      )), 5_000);
    };
    const read = async () => {
      signal.throwIfAborted();
      let interrupted!: () => void;
      const stopped = new Promise<never>((_resolve, reject) => {
        interrupted = () => reject(signal.reason);
        signal.addEventListener("abort", interrupted, { once: true });
      });
      try {
        const item = await Promise.race([reader.read(), stopped]);
        signal.throwIfAborted();
        if (item.done) { ended = true; clearTimeout(idle); }
        else if (item.value.length) progress();
        return item;
      } finally { signal.removeEventListener("abort", interrupted); }
    };
    signal.addEventListener("abort", cancel, { once: true });
    if (signal.aborted) cancel();
    // Native preparation has its own deadline. This bounds only an already
    // accepted response, and advances only when body bytes actually arrive.
    progress();
    const fill = async (target: Uint8Array<ArrayBuffer> | Uint8ClampedArray<ArrayBuffer>) => {
      let offset = 0;
      while (offset < target.length) {
        signal.throwIfAborted();
        if (position === chunk.length) {
          const next = await read();
          signal.throwIfAborted();
          if (next.done) throw new TypeError("Native picture changed length");
          if (next.value.length > length - received) throw new TypeError("Native picture exceeded its byte bound");
          chunk = next.value; position = 0; received += chunk.length;
          if (!chunk.length) continue;
        }
        const count = Math.min(target.length - offset, chunk.length - position);
        target.set(chunk.subarray(position, position + count), offset);
        position += count; offset += count;
      }
    };
    const cleanup = () => {
      clearTimeout(idle);
      signal.removeEventListener("abort", cancel);
      cancel();
      reader.releaseLock();
    };
    try {
      const prefix = new Uint8Array(picturePreparationTransport.prefix_bytes);
      await fill(prefix);
      if (String.fromCharCode(...prefix.subarray(0, 4)) !== picturePreparationTransport.magic)
        throw new TypeError("Invalid native picture preparation magic");
      const view = new DataView(prefix.buffer);
      const headerLength = view.getUint32(4, true), pixelLength = view.getUint32(8, true);
      if (!headerLength || headerLength > picturePreparationTransport.max_header_bytes ||
          pixelLength < 4 || pixelLength > 64 * 1024 * 1024 ||
          prefix.length + headerLength + pixelLength !== length)
        throw new TypeError("Invalid native picture preparation lengths");
      const metadata = new Uint8Array(headerLength);
      await fill(metadata);
      const value: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(metadata));
      if (!object(value) || value.kind !== kind || !["scene", "edit_draft"].includes(kind))
        throw new TypeError("Native picture changed preparation kind");
      const frame = kind === "scene" ? value.frame : object(value.draft) ? value.draft.frame : null;
      if (!object(frame) || !object(frame.picture)) throw new TypeError("Invalid native picture preparation header");
      const picture = validateNativePicture(frame.picture);
      if (picture.byte_length !== pixelLength) throw new TypeError("Native picture changed its descriptor length");
      signal.throwIfAborted();
      const pixels = (async () => {
        try {
          const bytes = new Uint8ClampedArray(pixelLength);
          await fill(bytes);
          if (position !== chunk.length) throw new TypeError("Native picture exceeded its byte bound");
          chunk = new Uint8Array(0);
          for (;;) {
            const tail = await read();
            signal.throwIfAborted();
            if (tail.done) break;
            if (tail.value.length) throw new TypeError("Native picture exceeded its byte bound");
          }
          return await verifyNativePicture(bytes, picture.content_digest.value, signal);
        } finally { cleanup(); }
      })();
      void pixels.catch(() => {});
      return { header: value as PreparedVisualPicture["header"], pixels, cancel: reason => local.abort(reason) };
    } catch (error) {
      cleanup(); throw error;
    }
  }
  /** Download a locked font authorized by the current preview owner and generation. */
  downloadVisualPreviewFont(input: ToolInput<"opencut_visual_preview_font_authorize">, options: CallOptions = {}): Promise<Response> {
    return this.#downloadBytes(`${this.baseUrl}/api/preview/visual-font/stream`, "visual preview font", options, JSON.stringify(input));
  }
  /** Read one bounded interval; identity and authority remain on the host. */
  async readVisualPreviewResource(input: ToolInput<"opencut_visual_preview_resource_read">, options: CallOptions = {}): Promise<{ offset: string; eof: boolean; bytes: Uint8Array }> {
    if (!/^(0|[1-9][0-9]*)$/.test(input.offset) || !Number.isInteger(input.count) || input.count < 1 || input.count > 512 * 1024)
      throw new TypeError("Invalid bounded visual resource range");
    const response = await this.#downloadBytes(`${this.baseUrl}/api/preview/visual-resource/read`, "visual resource range", options, JSON.stringify(input));
    const offset = response.headers.get("x-opencut-resource-offset"), eof = response.headers.get("x-opencut-resource-eof");
    if (offset !== input.offset || (eof !== "true" && eof !== "false") || response.headers.get("content-type") !== "application/octet-stream") {
      await response.body?.cancel();
      throw new TypeError("Visual resource interval changed identity");
    }
    const reader = response.body?.getReader();
    if (!reader) throw new TypeError("Visual resource bytes unavailable");
    const bytes = new Uint8Array(input.count); let length = 0;
    try {
      for (;;) {
        options.signal?.throwIfAborted();
        const item = await reader.read();
        if (item.done) break;
        if (item.value.length > input.count - length) throw new TypeError("Visual resource range exceeded its bound");
        bytes.set(item.value, length); length += item.value.length;
      }
      if (length !== input.count) throw new TypeError("Visual resource range changed length");
      return { offset, eof: eof === "true", bytes };
    } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
  }
  async #downloadBytes(url: string, kind: string, options: CallOptions, body?: string): Promise<Response> {
    options.signal?.throwIfAborted();
    let response: Response;
    try {
      response = await this.#fetch(url, { redirect: "error", signal: options.signal,
        ...(options.keepalive === undefined ? {} : { keepalive: options.keepalive }),
        method: body === undefined ? "GET" : "POST", body,
        headers: { Authorization: `Bearer ${this.#token}`, ...(body === undefined ? {} : { "Content-Type": "application/json" }) } });
    } catch (cause) {
      if (options.signal?.aborted) throw options.signal.reason;
      throw new OpenCutTransportError(cause);
    }
    if (!response.ok) {
      let value: unknown;
      try { value = await response.json(); }
      catch (cause) {
        if (options.signal?.aborted) throw options.signal.reason;
        if (!(cause instanceof SyntaxError)) throw new OpenCutTransportError(cause);
        throw new OpenCutProtocolError(`Invalid ${kind} error response.`, response.status);
      }
      if (object(value) && typeof value.request_id === "string" && object(value.error) && typeof value.error.code === "string" && typeof value.error.message === "string" && Array.isArray(value.warnings) && value.warnings.every(w => typeof w === "string")) {
        throw new OpenCutApiError(value.error.code, value.error.message, value.error.details, response.status, value.request_id, value.warnings);
      }
      throw new OpenCutProtocolError(`Invalid ${kind} response.`, response.status);
    }
    return response;
  }
  private async wait<J extends Job>(jobId: string, read: (signal: AbortSignal) => Promise<J>, options: WaitOptions): Promise<J> {
    const timeout = options.timeoutMs ?? 30_000;
    const interval = options.pollIntervalMs ?? 200;
    if (!Number.isFinite(timeout) || timeout < 0 || timeout > 2_147_483_647
      || !Number.isFinite(interval) || interval < 1 || interval > 2_147_483_647) {
      throw new RangeError("Invalid job wait timeout or polling interval.");
    }
    options.signal?.throwIfAborted();
    if (timeout === 0) throw new JobWaitTimeoutError(jobId);
    const local = new AbortController();
    const signal = options.signal ? AbortSignal.any([options.signal, local.signal]) : local.signal;
    const timer = setTimeout(() => local.abort(new JobWaitTimeoutError(jobId)), timeout);
    try {
      for (;;) {
        const job = await read(signal);
        if (job.state === "succeeded") return job;
        if (job.state === "failed" || job.state === "cancelled") throw new JobCompletionError(job);
        await pause(interval, signal);
      }
    } finally { clearTimeout(timer); }
  }
}
