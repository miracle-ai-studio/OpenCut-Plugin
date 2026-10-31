import { editorHostCapabilities } from "../../../../lib/editor-sdk";
import type { EditorApi } from "../../api";
import type { ToolInput, ToolOutput } from "../../../../lib/editor-sdk";
import { OpenCutApiError } from "../../../../lib/editor-sdk";
import type {
  AudioPreviewControl,
  AudioPreviewCreate,
  AudioPreviewSession,
} from "../types";

type Original = ToolInput<"opencut_audio_preview_recovery_stage">;
type Record = ToolOutput<"opencut_audio_preview_recovery_stage">;
type Pending = {
  api: EditorApi;
  identity: string;
  original: Original;
  record: Record | null;
  observed: boolean;
  retired: boolean;
};
const tools = [
  "opencut_audio_preview_recovery_stage",
  "opencut_audio_preview_recovery_inspect",
  "opencut_audio_preview_recovery_dispatch",
  "opencut_audio_preview_recovery_ack",
] as const;
const exact = (value: unknown): string =>
  JSON.stringify(value, (_key, child) =>
    child && typeof child === "object" && !Array.isArray(child)
      ? Object.fromEntries(
          Object.entries(child).sort(([a], [b]) => a.localeCompare(b)),
        )
      : child,
  );
const createOf = (request: Original) =>
  request.request.kind === "create"
    ? request.request.request
    : request.request.create;
const aborted = () =>
  new DOMException(
    "The original audio preview document has retired.",
    "AbortError",
  );

/** Host persistence is authoritative; no owner, lease, token, request or pointer
 * is written to browser storage. Reload discovers and retires, never resumes. */
export class PreviewRecovery {
  private readonly pending = new WeakMap<EditorApi, Map<string, Pending>>();
  private readonly capabilities = new WeakMap<EditorApi, Promise<boolean>>();
  private readonly atomicRetirement = new WeakMap<EditorApi, boolean>();
  private readonly retiring = new WeakMap<Pending, Promise<void>>();
  private readonly retired = new WeakMap<EditorApi, Set<string>>();
  private readonly restores = new WeakMap<
    EditorApi,
    Map<string, Promise<void>>
  >();
  private enabled(api: EditorApi) {
    let value = this.capabilities.get(api);
    if (!value) {
      value = editorHostCapabilities(api)
        .then((c) => {
          this.atomicRetirement.set(
            api,
            c.tools.includes("opencut_audio_preview_recovery_retire"),
          );
          return tools.every((t) => c.tools.includes(t));
        });
      this.capabilities.set(api, value);
      void value.catch(() => {
        if (this.capabilities.get(api) === value) this.capabilities.delete(api);
      });
    }
    return value;
  }
  private identity(api: EditorApi) {
    if (!api.connectionIdentity)
      throw new Error(
        "Authenticate the connection before recovering audio preview.",
      );
    return api.connectionIdentity;
  }
  private check(pending: Pending) {
    if (pending.api.connectionIdentity !== pending.identity)
      throw new Error("The authenticated audio preview connection changed.");
  }
  private pendingKey(api: EditorApi, project: string) {
    return JSON.stringify([api.connectionIdentity, project]);
  }
  private pendingFor(api: EditorApi, project: string) {
    return this.pending.get(api)?.get(this.pendingKey(api, project)) ?? null;
  }
  private retain(pending: Pending) {
    let projects = this.pending.get(pending.api);
    if (!projects) {
      projects = new Map();
      this.pending.set(pending.api, projects);
    }
    projects.set(
      this.pendingKey(pending.api, createOf(pending.original).project_id),
      pending,
    );
  }
  private retiredKey(api: EditorApi, create: AudioPreviewCreate) {
    return exact([api.connectionIdentity, create]);
  }
  private forget(pending: Pending) {
    const project = createOf(pending.original).project_id;
    if (this.pendingFor(pending.api, project) === pending)
      this.pending
        .get(pending.api)
        ?.delete(this.pendingKey(pending.api, project));
  }
  private isRetired(api: EditorApi, create: AudioPreviewCreate) {
    return this.retired.get(api)?.has(this.retiredKey(api, create)) ?? false;
  }
  private markRetired(api: EditorApi, create: AudioPreviewCreate) {
    let retired = this.retired.get(api);
    if (!retired) {
      retired = new Set();
      this.retired.set(api, retired);
    }
    retired.add(this.retiredKey(api, create));
  }
  private retireOriginal(pending: Pending) {
    let retiring = this.retiring.get(pending);
    if (!retiring) {
      this.check(pending);
      pending.retired = true;
      retiring = pending.api.sdk
        .call("opencut_audio_preview_recovery_retire", pending.original, {
          keepalive: true,
        })
        .then(() => this.forget(pending));
      this.retiring.set(pending, retiring);
      void retiring.catch(() => {
        if (this.retiring.get(pending) === retiring)
          this.retiring.delete(pending);
      });
    }
    return retiring;
  }
  private async ack(pending: Pending, retire: boolean, keepalive = false) {
    this.check(pending);
    // Ordinary cancellation can race both owner cleanup and a late Stage
    // response. Share the native retirement fence instead of restaging each.
    if (retire && this.atomicRetirement.get(pending.api))
      return this.retireOriginal(pending);
    if (!pending.record) {
      // A lost Stage response still has an exact original request in this
      // document. Stage it with the original key to recover its opaque slot.
      pending.record = await pending.api.sdk.call(
        "opencut_audio_preview_recovery_stage",
        pending.original,
        { keepalive },
      );
    }
    this.check(pending);
    await pending.api.sdk.call(
      "opencut_audio_preview_recovery_ack",
      {
        project_id: pending.record.project_id,
        recovery_id: pending.record.recovery_id,
        disposition: retire ? "retire" : "observed",
      },
      { keepalive },
    );
    this.forget(pending);
  }
  /** Called on project opening before any new admission. Its promise is shared
   * with Play, so a slow restore cannot retire a newly created session. */
  restore(api: EditorApi, project: string) {
    const key = JSON.stringify([api.connectionIdentity, project]);
    let projects = this.restores.get(api);
    if (!projects) {
      projects = new Map();
      this.restores.set(api, projects);
    }
    let restoring = projects.get(key);
    if (!restoring) {
      restoring = this.discoverAndRetire(api, project);
      projects.set(key, restoring);
      void restoring.catch(() => {
        if (projects!.get(key) === restoring) projects!.delete(key);
      });
    }
    return restoring;
  }
  private async discoverAndRetire(api: EditorApi, project: string) {
    if (!(await this.enabled(api))) return;
    const identity = this.identity(api);
    const found = await api.sdk.call(
      "opencut_audio_preview_recovery_inspect",
      { project_id: project },
    );
    if (api.connectionIdentity !== identity)
      throw new Error("The authenticated audio preview connection changed.");
    if (found.record) {
      if (
        found.record.project_id !== project ||
        !found.record.recovery_id
      )
        throw new Error(
          "The service returned an unexpected preview recovery scope.",
        );
      await api.sdk.call("opencut_audio_preview_recovery_ack", {
        project_id: project,
        recovery_id: found.record.recovery_id,
        disposition: "retire",
      });
    }
  }
  private async invoke(
    api: EditorApi,
    original: Original,
    signal: AbortSignal | undefined,
    current: () => boolean,
    keepalive = false,
  ): Promise<AudioPreviewSession> {
    const create = createOf(original);
    const stopping =
      original.request.kind === "control" &&
      original.request.request.command.kind === "stop";
    if (!(await this.enabled(api))) {
      if (!current()) throw aborted();
      return original.request.kind === "create"
        ? api.scene.audioPreviewCreate(original.request.request, signal)
        : api.scene.audioPreviewControl(original.request.request, signal, {
            keepalive,
          });
    }
    await this.restore(api, create.project_id);
    const identity = this.identity(api);
    if (!current() || (!stopping && this.isRetired(api, create)))
      throw aborted();
    let pending = this.pendingFor(api, create.project_id);
    if (
      pending &&
      (pending.api !== api ||
        pending.identity !== identity ||
        exact(pending.original) !== exact(original))
    ) {
      // Only a completed, locally observed response may be cleared without
      // retiring its native generation. Unknown requests block replacement.
      const retired =
        pending.retired ||
        this.isRetired(pending.api, createOf(pending.original));
      if (!pending.observed && !retired)
        throw new Error(
          "Recover the original audio preview request before starting another control.",
        );
      await this.ack(pending, retired);
      pending = null;
    }
    if (!pending) {
      pending = {
        api,
        identity,
        original: structuredClone(original),
        record: null,
        observed: false,
        retired: false,
      };
      this.retain(pending);
    }
    this.check(pending);
    if (!pending.record) {
      try {
        pending.record = await api.sdk.call(
          "opencut_audio_preview_recovery_stage",
          pending.original,
          { signal, keepalive },
        );
      } catch (error) {
        // Known admission failures did not stage this request. Storage/IO
        // failures can follow a committed write and remain unknown outcomes.
        if (
          error instanceof OpenCutApiError &&
          ![
            "STORAGE_ERROR",
            "IO_ERROR",
            "INTERNAL_ERROR",
            "SERIALIZATION_ERROR",
          ].includes(error.code)
        )
          this.forget(pending);
        throw error;
      }
    }
    const record = pending.record;
    this.check(pending);
    if (
      record.project_id !== create.project_id ||
      record.revision !== create.revision ||
      record.sequence_id !== create.sequence_id ||
      record.kind !== original.request.kind ||
      !record.recovery_id
    )
      throw new Error(
        "The service returned an unexpected original preview request.",
      );
    if (
      !current() ||
      pending.retired ||
      (!stopping && this.isRetired(api, create))
    ) {
      await this.ack(pending, true, keepalive);
      throw aborted();
    }
    const session = await api.sdk.call(
      "opencut_audio_preview_recovery_dispatch",
      { project_id: record.project_id, recovery_id: record.recovery_id },
      { signal, keepalive },
    );
    this.check(pending);
    if (
      !current() ||
      pending.retired ||
      (!stopping && this.isRetired(api, create))
    ) {
      await this.ack(pending, true, keepalive);
      throw aborted();
    }
    pending.observed = true;
    // A lost acknowledgement is retried before the next control; it does not
    // turn a received native result into another unknown playback operation.
    await this.ack(pending, false, keepalive).catch(() => {});
    return session;
  }
  create(
    api: EditorApi,
    request: AudioPreviewCreate,
    signal: AbortSignal,
    current: () => boolean,
  ) {
    return this.invoke(
      api,
      { request: { kind: "create", request } },
      signal,
      current,
    );
  }
  control(
    api: EditorApi,
    create: AudioPreviewCreate,
    request: AudioPreviewControl,
    signal: AbortSignal | undefined,
    current: () => boolean,
    keepalive = false,
  ) {
    return this.invoke(
      api,
      { request: { kind: "control", create, request } },
      signal,
      current,
      keepalive,
    );
  }
  /** Cancellation marks this original admission before asynchronous work.
   * It never dispatches its retained command to discover the outcome. */
  async retire(
    api: EditorApi,
    create: AudioPreviewCreate,
    keepalive = false,
  ) {
    this.markRetired(api, create);
    const pending = this.pendingFor(api, create.project_id);
    if (
      pending?.api === api &&
      createOf(pending.original).idempotency_key === create.idempotency_key
    ) {
      pending.retired = true;
      if (keepalive && this.atomicRetirement.get(api))
        return this.retireOriginal(pending);
      if (await this.enabled(api)) await this.ack(pending, true, keepalive);
    }
  }
  async stop(
    api: EditorApi,
    create: AudioPreviewCreate,
    request: AudioPreviewControl,
    keepalive = false,
  ) {
    this.markRetired(api,create);
    let pending = this.pendingFor(api, create.project_id);
    // Cleanup must also work after an idle owner's lease expires. The native
    // retirement fence uses the exact original request and cannot stop a newer
    // acquired generation; dispatching Stop would require a still-live lease.
    if (await this.enabled(api) && this.atomicRetirement.get(api)) {
      if (
        pending &&
        createOf(pending.original).idempotency_key !== create.idempotency_key
      )
        throw aborted();
      if (!pending) {
        pending = {
          api,
          identity: this.identity(api),
          original: { request: { kind: "control", create, request } },
          record: null,
          observed: false,
          retired: true,
        };
        this.retain(pending);
      }
      return this.retireOriginal(pending);
    }
    if (
      pending?.api === api &&
      createOf(pending.original).idempotency_key === create.idempotency_key &&
      !pending.observed
    )
      return this.retire(api, create, keepalive);
    // Stop is itself durably staged; a delayed response never adopts it locally.
    try {
      await this.control(
        api,
        create,
        request,
        undefined,
        () => true,
        keepalive,
      );
    } catch (error) {
      await this.retire(api, create, keepalive);
      throw error;
    }
  }
}
