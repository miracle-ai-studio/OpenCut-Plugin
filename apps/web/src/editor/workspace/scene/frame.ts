import { OpenCutApiError } from "../../../lib/editor-sdk";
import type { Job } from "../../../lib/editor-sdk";
import type { EditorApi } from "../api";
import type { SessionHost } from "./session";
import type { FrameAttempt, FrameRequest, SessionState } from "./types";
import { compareTime } from "../../model/time";

import { sameTarget, verifiedResult, readPng } from "./frame/png";
const message = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

export function frameAvailability(state: SessionState) {
  const capability = state.frameCapability;
  if (!capability) return state.frameCapabilityError ?? "Scene frame availability is being checked.";
  return capability.frame.available ? null : capability.frame.unavailable_reason ?? "Scene frames are unavailable on this service.";
}

/** Commands and display loading only. WorkspaceState.jobs/TaskFeed remain the
 * sole task authority; seek never computes renderer time or resource clocks. */
export class Frame {
  private attempts = new WeakMap<EditorApi, Map<string, FrameAttempt>>();
  private epoch = 0;
  private download: AbortController | null = null;
  private capabilityEpoch = 0;
  private owned: { api: EditorApi; attempt: FrameAttempt } | null = null;
  private captureWaiter: {
    key: string;
    resolve: (blob: Blob | null) => void;
    reject: (error: unknown) => void;
  } | null = null;
  constructor(private readonly host: SessionHost) {}
  private active() {
    const api = this.host.api(),
      state = this.host.state();
    if (!api || !state) throw new Error("Open a project first.");
    return { api, state };
  }
  resume(api: EditorApi, projectId: string) {
    return this.attempts.get(api)?.get(projectId) ?? null;
  }
  private publish(api: EditorApi, attempt: FrameAttempt) {
    if (
      this.captureWaiter?.key === attempt.request.idempotency_key &&
      ["failed", "unconfirmed", "retryable"].includes(attempt.phase)
    )
      this.finishCapture(null, new Error(attempt.error ?? "Snapshot did not complete."));
    let projects = this.attempts.get(api);
    if (!projects) {
      projects = new Map();
      this.attempts.set(api, projects);
    }
    if (["submitting", "unconfirmed", "retryable"].includes(attempt.phase))
      projects.set(attempt.request.project_id, attempt);
    else projects.delete(attempt.request.project_id);
    const state = this.host.state();
    if (
      api === this.host.api() &&
      state?.head.project.id === attempt.request.project_id &&
      state.frame?.request.idempotency_key === attempt.request.idempotency_key
    ) {
      if (attempt.jobId) this.owned = { api, attempt };
      this.host.publish({ ...state, frame: attempt });
    }
  }
  private matches(request: FrameRequest) {
    const state = this.host.state(),
      view = state && (state.historical ?? state.head);
    return (
      !!state &&
      view?.project.id === request.project_id &&
      view.revision === request.revision &&
      state.sequenceId === request.sequence_id &&
      compareTime(state.playhead, request.time) === 0
    );
  }
  async refreshCapability() {
    const { api, state } = this.active(),
      generation = this.host.generation(),
      epoch = ++this.capabilityEpoch;
    try {
      const capability = await api.sdk.call("opencut_render_capability", {});
      const current = this.host.state();
      if (
        epoch === this.capabilityEpoch &&
        generation === this.host.generation() &&
        api === this.host.api() &&
        current?.head.project.id === state.head.project.id
      )
        this.host.publish({
          ...current,
          frameCapability: capability,
          frameCapabilityError: capability
            ? null
            : "This service does not support scene frames.",
        });
    } catch (error) {
      const current = this.host.state();
      if (
        epoch === this.capabilityEpoch &&
        generation === this.host.generation() &&
        api === this.host.api() &&
        current?.head.project.id === state.head.project.id
      )
        this.host.publish({
          ...current,
          frameCapability: null,
          frameCapabilityError: "This service does not support scene frames.",
        });
    }
  }
  /** Called on view/time change or canvas unmount. Cancel only this canvas's
   * known unfinished frame, retaining unknown acceptance for explicit replay. */
  invalidate() {
    this.epoch++;
    this.finishCapture(null);
    this.download?.abort();
    this.download = null;
    const owned = this.owned;
    this.owned = null;
    if (owned?.attempt.jobId && owned.attempt.phase === "accepted")
      void owned.api.sdk
        .cancelJob(owned.attempt.jobId)
        .then(() => this.host.refreshTasks())
        .catch(() => {});
    const state = this.host.state(),
      attempt = state?.frame;
    if (
      state &&
      attempt &&
      !["submitting", "unconfirmed", "retryable"].includes(attempt.phase)
    )
      this.host.publish({ ...state, frame: null });
  }
  private begin() {
    const { api, state } = this.active();
    if (
      state.frame &&
      ["submitting", "unconfirmed"].includes(state.frame.phase)
    )
      throw new Error("Confirm the original scene frame request first.");
    const reason = frameAvailability(state);
    if (reason) throw new Error(reason);
    this.invalidate();
    const view = state.historical ?? state.head;
    const attempt: FrameAttempt = {
      request: {
        project_id: view.project.id,
        revision: view.revision,
        sequence_id: state.sequenceId,
        time: structuredClone(state.playhead),
        idempotency_key: crypto.randomUUID(),
      },
      phase: "submitting",
      jobId: null,
      error: null,
    };
    this.host.publish({ ...this.host.state()!, frame: attempt });
    this.publish(api, attempt);
    return attempt;
  }
  async start() {
    this.begin();
    return this.execute();
  }
  /** Bind the click's view before an asynchronous save picker. Rendering and
   * downloading still use the canvas's single request and existing task feed. */
  prepareCapture() {
    const { api, state } = this.active(),
      generation = this.host.generation(),
      epoch = this.epoch,
      view = state.historical ?? state.head,
      target = {
        project_id: view.project.id,
        revision: view.revision,
        sequence_id: state.sequenceId,
        time: structuredClone(state.playhead),
        idempotency_key: "",
      };
    return (): Promise<Blob | null> => {
      if (
        api !== this.host.api() ||
        generation !== this.host.generation() ||
        epoch !== this.epoch ||
        !this.matches(target)
      )
        return Promise.resolve(null);
      const attempt = this.begin();
      return new Promise((resolve, reject) => {
        this.captureWaiter = { key: attempt.request.idempotency_key, resolve, reject };
        void this.execute().catch((error) => {
          if (this.captureWaiter?.key === attempt.request.idempotency_key)
            this.finishCapture(null, error);
        });
      });
    };
  }
  private finishCapture(blob: Blob | null, error?: unknown) {
    const waiter = this.captureWaiter;
    this.captureWaiter = null;
    if (error) waiter?.reject(error);
    else waiter?.resolve(blob);
  }
  retry() {
    if (this.host.state()?.frame?.phase === "submitting")
      throw new Error("Wait for the original scene frame request.");
    return this.execute();
  }
  private async execute() {
    const { api, state } = this.active();
    let attempt = state.frame;
    if (
      !attempt ||
      !["submitting", "unconfirmed", "retryable"].includes(attempt.phase)
    )
      throw new Error("Choose a scene frame request first.");
    const wasUnknown = attempt.phase === "unconfirmed",
      epoch = this.epoch,
      generation = this.host.generation();
    attempt = { ...attempt, phase: "submitting", error: null };
    this.publish(api, attempt);
    try {
      const job = await api.sdk.call("opencut_render_submit", {kind:"frame",request:attempt.request});
      if (!job.job_id || !sameTarget(job, attempt.request))
        throw new Error("Scene frame returned an unexpected task identity.");
      attempt = {
        ...attempt,
        phase: "accepted",
        jobId: job.job_id,
        error: null,
      };
      this.publish(api, attempt);
      if (
        epoch !== this.epoch ||
        generation !== this.host.generation() ||
        api !== this.host.api() ||
        !this.matches(attempt.request) ||
        this.host.state()?.frame?.request.idempotency_key !==
          attempt.request.idempotency_key
      ) {
        if (["queued", "running"].includes(job.state))
          await api.sdk.cancelJob(job.job_id);
        this.publish(api, {
          ...attempt,
          phase: "failed",
          error:
            "This scene frame belongs to a previous view. Render the current frame again.",
        });
      }
      if (generation === this.host.generation() && api === this.host.api())
        await this.host.refreshTasks();
      return job.job_id;
    } catch (error) {
      // An accepted receipt remains known even if cancellation/feed refresh failed.
      this.publish(api, {
        ...attempt,
        phase: attempt.jobId
          ? "failed"
          : wasUnknown || !(error instanceof OpenCutApiError)
            ? "unconfirmed"
            : "retryable",
        error: message(error),
      });
      throw error;
    }
  }
  async cancel() {
    const { api, state } = this.active(),
      attempt = state.frame;
    if (!attempt?.jobId)
      throw new Error(
        "Confirm the original scene frame request before cancelling its task.",
      );
    this.download?.abort();
    this.finishCapture(null);
    this.download = null;
    this.epoch++;
    await api.sdk.cancelJob(attempt.jobId);
    this.publish(api, {
      ...attempt,
      phase: "failed",
      error:
        "Scene frame cancellation requested. Check Tasks for its final state.",
    });
    await this.host.refreshTasks();
  }
  rejectImage() {
    const { api, state } = this.active();
    if (state.frame)
      this.publish(api, {
        ...state.frame,
        phase: "failed",
        error: "The scene frame image could not be displayed.",
      });
  }
  rejectTask(job: Job) {
    const { api, state } = this.active(),
      attempt = state.frame;
    if (!attempt?.jobId || attempt.jobId !== job.job_id) return;
    this.download?.abort();
    this.download = null;
    this.epoch++;
    this.publish(api, {
      ...attempt,
      phase: "failed",
      error:
        job.result_unavailable?.message ??
        job.failure?.message ??
        "Scene frame task did not complete.",
    });
  }
  retryDownload(jobs: readonly Job[]) {
    const { api, state } = this.active(),
      attempt = state.frame;
    if (
      !attempt?.jobId ||
      attempt.phase !== "failed" ||
      !this.matches(attempt.request) ||
      !jobs.some(
        (job) =>
          job.job_id === attempt.jobId &&
          job.state === "succeeded" &&
          !job.result_unavailable,
      )
    )
      throw new Error("The saved frame is unavailable in this view.");
    this.publish(api, { ...attempt, phase: "accepted", error: null });
    return this.observe(jobs);
  }
  async observe(jobs: readonly Job[]) {
    const { api, state } = this.active(),
      attempt = state.frame;
    if (
      !attempt?.jobId ||
      attempt.phase !== "accepted" ||
      !this.matches(attempt.request)
    )
      return null;
    const job = jobs.find((row) => row.job_id === attempt.jobId);
    if (!job || ["queued", "running"].includes(job.state)) return null;
    if (job.state !== "succeeded" || job.result_unavailable) {
      this.publish(api, {
        ...attempt,
        phase: "failed",
        error:
          job.result_unavailable?.message ??
          job.failure?.message ??
          "Scene frame task did not complete.",
      });
      return null;
    }
    const epoch = this.epoch,
      generation = this.host.generation(),
      request = new AbortController();
    this.download = request;
    this.publish(api, { ...attempt, phase: "downloading", error: null });
    try {
      const settings = (state.historical ?? state.head).project.settings;
      if (settings.profile !== "classic_global")
        throw new Error(
          "Scene frames currently require Classic scene settings.",
        );
      const { result, artifact } = verifiedResult(
        job,
        attempt.request,
        settings.settings.canvas_size.width,
        settings.settings.canvas_size.height,
      );
      const blob = await readPng(api, artifact, result, request.signal);
      if (
        request.signal.aborted ||
        epoch !== this.epoch ||
        generation !== this.host.generation() ||
        api !== this.host.api() ||
        !this.matches(attempt.request) ||
        this.host.state()?.frame?.request.idempotency_key !==
          attempt.request.idempotency_key
      )
        return null;
      this.publish(api, { ...attempt, phase: "ready", error: null });
      if (this.captureWaiter?.key === attempt.request.idempotency_key)
        this.finishCapture(blob);
      return { blob, result };
    } catch (error) {
      if (
        !request.signal.aborted &&
        epoch === this.epoch &&
        generation === this.host.generation() &&
        api === this.host.api()
      )
        this.publish(api, {
          ...attempt,
          phase: "failed",
          error: message(error),
        });
      return null;
    } finally {
      if (this.download === request) this.download = null;
    }
  }
}
