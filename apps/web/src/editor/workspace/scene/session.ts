import type { CaptionContext } from "./captions/recovery";
import { VisualPreview } from "./visual-preview";
import { AudioGain } from "./audio-gain";
import { AudioPreview, emptyAudioPreview } from "./audio-preview";
import { VideoExport } from "./video-export";
import { AudioWav } from "./audio-wav";
import { SourceAudio } from "./source-audio";
import { Media } from "./media";
import { Frame } from "./frame";
import { TimelineEdit } from "./timeline-edit/controller";
import { TimelineClipboard } from "./timeline-edit/clipboard";
import { emptyTimelineEdit } from "./timeline-edit/types";
import { reconcileTimelineSelection } from "./timeline-edit/selection";
import { EditRecovery, exactRecovery } from "./edit-recovery";
import type { ToolInput } from "../../../lib/editor-sdk";
import type { EditorApi } from "../api";
import type { ProjectRef, Time } from "../../model/types";
import { zero, compareTime } from "../../model/time";
import type {
  BookmarkOperation,
  SessionState,
  Operation,
  EditReceipt,
  Snapshot,
} from "./types";

export type SessionHost = {
  api(): EditorApi | null;
  state(): SessionState | null;
  generation(): number;
  viewGeneration(): number;
  changeView(): number;
  publish(state: SessionState): void;
  acquire(projectId: string, draftOwner?: string): () => void;
  refreshTasks(): Promise<void>;
  report?(error: unknown): void;
  publicApply?(
    api: EditorApi,
    request: ToolInput<"opencut_edit_apply">,
    sequenceId: string | null,
  ): Promise<EditReceipt>;
};
/** A delegate with no project state or SDK of its own. The workspace controller
 * owns the mutually exclusive legacy/V4 session, routing and write fence. */
export class Session {
  readonly recovery: EditRecovery;
  readonly timelineEdit: TimelineEdit;
  readonly clipboard: TimelineClipboard;
  readonly audioGain: AudioGain;
  readonly audioPreview: AudioPreview;
  readonly visualPreview: VisualPreview;
  readonly sourceAudio: SourceAudio;
  readonly audioWav: AudioWav;
  readonly exporter: VideoExport;
  readonly media: Media;
  readonly frame: Frame;
  constructor(private readonly host: SessionHost) {
    const edits = host;
    this.recovery = new EditRecovery(edits, () => this.refresh());
    const writes: SessionHost = {
      ...edits,
      publicApply: (api, request, sequenceId) =>
        this.recovery.publicApply(api, request, sequenceId),
    };
    this.visualPreview = new VisualPreview(host, (error) => {
      this.audioPreview.pictureFailed(error);
    },undefined,undefined,()=>this.audioPreview.settled());
    this.audioPreview = new AudioPreview(host, (time) =>
      this.presentAudio(time),undefined,()=>this.visualPreview.retireStill(),
    );
    this.audioGain = new AudioGain(
      writes,
      () => this.refresh(),
      (draft, preserve) => this.audioPreview.draft(draft, preserve),
    );
    this.sourceAudio = new SourceAudio(writes, () => this.refresh());
    this.audioWav = new AudioWav(host);
    this.exporter = new VideoExport(host);
    this.media = new Media(writes, () => this.refresh());
    this.frame = new Frame(host);
    this.timelineEdit = new TimelineEdit(
      {
        ...edits,
        publish: (timelineEdit, primaryId) => {
          const state = host.state();
          if (!state) return;
          if (primaryId !== undefined && primaryId !== state.selectedId)
            this.audioGain.cancel();
          const current = host.state();
          if (current)
            host.publish({
              ...current,
              timelineEdit,
              ...(primaryId === undefined ? {} : { selectedId: primaryId }),
            });
        },
      },
      (api, request, signal) => api.scene.timelinePlan(request, signal),
      () => this.refresh(),
      this.recovery,
    );
    this.clipboard = new TimelineClipboard(
      {
        ...edits,
        publicApply: writes.publicApply,
        clipboard: () => host.state()!.clipboard,
        publish: (clipboard) => {
          const state = host.state();
          if (state) host.publish({ ...state, clipboard });
        },
        selectMany: (targets, primaryId) => {
          this.timelineEdit.selectMany(targets, primaryId);
          const state = host.state();
          if (
            primaryId &&
            !state?.timelineEdit.selection.some(
              (target) => target.element_id === primaryId,
            )
          )
            this.select(primaryId);
        },
      },
      () => this.refresh(),
    );
  }
  /** Register only presentation state. The controller owns the shared write fence. */
  propertyDraft(owner: string, dirty: boolean) {
    const state = this.host.state();
    if (!state) return;
    const previous = state.propertyDrafts ?? [];
    if (previous.includes(owner) === dirty) return;
    this.host.publish({ ...state, propertyDrafts: dirty ? [...previous, owner] : previous.filter(id => id !== owner) });
  }
  async generateCatalogSound(
    request: ToolInput<"opencut_catalog_resources_generate">,
    signal?: AbortSignal,
  ) {
    const { api, state } = this.active();
    if (
      state.historical ||
      state.head.project.id !== request.project_id ||
      state.gainDraft ||
      state.recovery?.record ||
      state.recovery?.error ||
      ["reading", "staging", "applying"].includes(state.recovery?.phase ?? "")
    )
      throw new Error(
        "Resolve the current edit in the original project before generating a sound.",
      );
    const release = this.host.acquire(request.project_id);
    try {
      return await api.sdk.callWithAdmissionRetry(
        "opencut_catalog_resources_generate",
        request,
        { signal },
      );
    } finally {
      release();
    }
  }
  async importCatalogResource(
    request: ToolInput<"opencut_catalog_resources_import">,
    signal?: AbortSignal,
  ) {
    const { api, state } = this.active();
    if (
      state.historical ||
      state.head.project.id !== request.project_id ||
      state.gainDraft ||
      state.recovery?.record ||
      state.recovery?.error ||
      ["reading", "staging", "applying"].includes(state.recovery?.phase ?? "")
    )
      throw new Error(
        "Resolve the current edit in the original project before acquiring a resource.",
      );
    const release = this.host.acquire(request.project_id);
    try {
      return await api.sdk.callWithAdmissionRetry(
        "opencut_catalog_resources_import",
        request,
        { signal },
      );
    } finally {
      release();
    }
  }
  async importBrandResource(
    request: ToolInput<"opencut_brand_resources_import">,
    signal?: AbortSignal,
  ) {
    const { api, state } = this.active();
    if (
      state.historical ||
      state.head.project.id !== request.project_id ||
      state.gainDraft ||
      state.recovery?.record ||
      state.recovery?.error ||
      ["reading", "staging", "applying"].includes(state.recovery?.phase ?? "")
    )
      throw new Error(
        "Resolve the current edit in the original project before acquiring a brand.",
      );
    const release = this.host.acquire(request.project_id);
    try {
      return await api.sdk.callWithAdmissionRetry(
        "opencut_brand_resources_import",
        request,
        { signal },
      );
    } finally {
      release();
    }
  }
  private active() {
    const api = this.host.api(),
      state = this.host.state();
    if (!api || !state) throw new Error("Open an editor project first.");
    return { api, state };
  }
  async open(project: ProjectRef, sequenceId?: string | null, captured?:Snapshot) {
    this.recovery.invalidate();
    this.clipboard.invalidate();
    this.timelineEdit.invalidate();
    this.audioPreview.invalidate();
    this.visualPreview.dispose();
    this.audioGain.invalidate();
    const api = this.host.api();
    if (!api) return;
    const generation = this.host.generation(),
      view = this.host.viewGeneration();
    const head = captured??await api.scene.inspect(project.id);
    const { history, navigation, revisionActors } = await api.scene.history(
      project.id,
      head.revision,
    );
    const restoredRecovery = await this.recovery.restore(api, head.project.id);
    const restoredTimeline = { attempt: this.timelineEdit.resume(api, head.project.id), recoveryError: null };
    if (
      api !== this.host.api() ||
      generation !== this.host.generation() ||
      view !== this.host.viewGeneration()
    )
      return;
    this.host.publish({
      head,
      recovery: restoredRecovery,
      timelineEdit: { ...emptyTimelineEdit(), ...restoredTimeline },
      clipboard: this.clipboard.resume(api, head.project.id),
      audioPreview: emptyAudioPreview(),
      historical: null,
      history,
      revisionActors,
      navigation,
      sequenceId: head.project.sequences.some((s) => s.id === sequenceId)
        ? sequenceId!
        : head.project.default_sequence_id,
      playhead: zero,
      selectedId: null,
      historyPlan: null,
      gainDraft: null,
      audioGain: this.audioGain.resume(api, head.project.id),
      sourceAudio: this.sourceAudio.resume(api, head.project.id, head.revision),
      audioWavCapability: null,
      audioWavCapabilityError: null,
      audioRangeWavCapability: null,
      audioRangeWavCapabilityError: null,
      audioWav: this.audioWav.resume(api, head.project.id),
      media: {
        attempt: this.media.resume(api, head.project.id),
        available: [],
        loading: false,
        error: null,
      },
      frameCapability: null,
      frameCapabilityError: null,
      frame: this.frame.resume(api, head.project.id),
    });
    // Editing and durable recovery do not depend on output backend discovery.
    // Start these bounded reads after publishing the core state, then merge only
    // availability fields into the still-current view; never replace live edits.
    const currentView = () => {
      const current = this.host.state();
      return api === this.host.api() &&
        generation === this.host.generation() &&
        view === this.host.viewGeneration() &&
        current?.head.project.id === project.id
        ? current
        : null;
    };
    const sourceView = currentView();
    if (sourceView) {
      // Rust chooses participating source versions and tracks. Accept once on
      // opening the scene so preparation can finish before playback starts.
      void api.sdk.call("opencut_source_preview_prepare", {
        project_id: head.project.id,
        revision: head.revision,
        sequence_id: sourceView.sequenceId,
        asset_id: null,
        selected_video_stream: null,
      }).then(result => {
        if (currentView() && result.jobs.length) return this.host.refreshTasks();
      }).catch(error => { if (currentView()) this.host.report?.(error); });
    }
    const loadCapabilities = async () => {
      const capability = await api.sdk.call("opencut_render_capability", {});
      const current = currentView();
      if (!current) return;
      this.host.publish({...current, frameCapability:capability, frameCapabilityError:null,
        audioWavCapability:capability, audioWavCapabilityError:null,
        audioRangeWavCapability:capability, audioRangeWavCapabilityError:null,
      });
    };
    void loadCapabilities().catch(() => {
      const current = currentView();
      if (current)
        this.host.publish({
          ...current,
          audioWavCapabilityError:
            "This service does not support scene audio WAV export.",
          audioRangeWavCapabilityError:
            "This service does not support sample range WAV export.",
          frameCapabilityError: "This service does not support scene frames.",
        });
    });
  }
  async recoverTimeline(resetPointer = false) { await this.recoverEdit(resetPointer); }
  async recoverEdit(resetPointer = false) {
    const { api, state } = this.active(),
      generation = this.host.generation(),
      view = this.host.viewGeneration();
    if (resetPointer) {
      this.recovery.resetPointer();
    }
    const restored = await this.recovery.restore(api, state.head.project.id);
    if (
      api === this.host.api() &&
      generation === this.host.generation() &&
      view === this.host.viewGeneration()
    ) {
      const current = this.host.state();
      if (current) this.host.publish({ ...current, recovery: restored });
    }
  }
  async retryOriginalEdit() {
    const { api, state } = this.active(),
      record = state.recovery?.record,
      generation = this.host.generation(),
      view = this.host.viewGeneration();
    if (!record) throw new Error("No original edit needs recovery.");
    const receipt = await this.recovery.retry();
    if (
      api === this.host.api() &&
      generation === this.host.generation() &&
      view === this.host.viewGeneration()
    ) {
      const key = record.context.intent.request.idempotency_key;
      if (receipt.kind === "public_apply") {
        this.timelineEdit.recovered(api, record.project_id, key);
        this.clipboard.recovered(api, record.project_id, key);
        this.audioGain.recovered(api, record.project_id, key);
        this.media.recovered(api, record.project_id, key);
        this.sourceAudio.recovered(api, record.project_id, key);
      }
      const current = this.host.state();
      if (current && (receipt.kind === "undo" || receipt.kind === "redo"))
        this.host.publish({ ...current, historyPlan: null });
    }
    return receipt;
  }
  async refresh(onlyIfChanged=false) {
    const { api, state } = this.active(),
      generation = this.host.generation();
    if(onlyIfChanged) {
      // The existing authorized history page supplies a tiny current-head
      // observation. Background polling need not deserialize the entire
      // document on the presentation thread every two seconds.
      const revision=await api.scene.headRevision(state.head.project.id);
      if(api!==this.host.api()||generation!==this.host.generation()||this.host.state()?.head.revision!==state.head.revision)return;
      if(revision===state.head.revision)return;
    }
    const head = await api.scene.inspect(state.head.project.id);
    if (
      generation !== this.host.generation() ||
      this.host.state()?.head.revision !== state.head.revision
    )
      return;
    if (head.revision === state.head.revision) {
      // Consume the authorized observation without repeating history/actor reads.
      // An unchanged immutable public view also preserves authored references;
      // polling must not rebuild every timeline clip during device playback.
      const current=this.host.state()!;
      if(JSON.stringify(current.head)!==JSON.stringify(head))
        this.host.publish({ ...current, head });
      return;
    }
    const observed = this.host.state()!;
    const unresolvedRecovery =
      observed.recovery?.record &&
      !["saved", "conflict"].includes(observed.recovery.phase);
    if (
      observed.gainDraft ||
      !!observed.propertyDrafts?.length ||
      unresolvedRecovery ||
      ["applying", "unconfirmed"].includes(
        observed.timelineEdit.attempt?.phase ?? "",
      )
    ) {
      this.host.publish({ ...observed, externalRevision: head.revision });
      return;
    }
    const { history, navigation, revisionActors } = await api.scene.history(
      head.project.id,
      head.revision,
    );
    if (
      generation !== this.host.generation() ||
      this.host.state()?.head.revision !== state.head.revision
    )
      return;
    const current = this.host.state()!;
    const project = (current.historical ?? head).project;
    const sequenceId = project.sequences.some(
      (s) => s.id === current.sequenceId,
    )
      ? current.sequenceId
      : project.default_sequence_id;
    if (head.revision !== current.head.revision) {
      if (!current.historical) this.timelineEdit.externalHeadChanged();
      if (!current.historical) this.clipboard.invalidate();
      this.audioGain.cancel();
      if (!current.historical) this.audioPreview.revisionChanged();
    }
    const sequence = project.sequences.find((s) => s.id === sequenceId)!;
    const timelineEdit = this.host.state()!.timelineEdit;
    const selection = reconcileTimelineSelection(
      sequence,
      sequenceId === current.sequenceId ? timelineEdit.selection : [],
    );
    const selectedId =
      sequenceId === current.sequenceId && current.selectedId &&
      (sequence.bookmarks.some((b) => b.id === current.selectedId) ||
        sequence.captions.some((c) => c.id === current.selectedId) ||
        sequence.tracks.some(
          (t) =>
            t.id === current.selectedId ||
            t.elements.some((e) => e.id === current.selectedId),
        ))
        ? current.selectedId
        : selection.at(-1)?.element_id ?? null;
    this.host.publish({
      ...current,
      audioPreview: this.host.state()!.audioPreview,
      timelineEdit: { ...timelineEdit, selection },
      clipboard: this.host.state()!.clipboard,
      selectedId,
      gainDraft:
        head.revision === current.head.revision ? current.gainDraft : null,
      head,
      externalRevision: null,
      sequenceId,
      history,
      revisionActors,
      navigation,
      historyPlan:
        head.revision === state.head.revision ? current.historyPlan : null,
    });
  }
  chooseSequence(sequenceId: string) {
    const { state } = this.active();
    if (
      !(state.historical ?? state.head).project.sequences.some(
        (s) => s.id === sequenceId,
      )
    )
      return;
    this.audioGain.invalidate();
    this.clipboard.invalidate();
    this.timelineEdit.invalidate();
    this.audioPreview.seek();
    this.host.changeView();
    this.host.publish({
      ...state,
      audioPreview: this.host.state()!.audioPreview,
      timelineEdit: this.host.state()!.timelineEdit,
      clipboard: this.host.state()!.clipboard,
      gainDraft: null,
      sequenceId,
      selectedId: null,
      playhead: zero,
      historyPlan: null,
    });
  }
  seek(playhead: Time) {
    if (this.audioPreview.seek(playhead)) return;
    const { state } = this.active();
    this.host.publish({ ...state, playhead });
  }
  /** Live user actions use the same output cursor as the picture; the saved
   * editing cursor is updated at explicit pause/seek/end boundaries. */
  currentPlayhead():Time {
    return this.audioPreview.currentTime()??this.host.state()?.playhead??zero;
  }
  /** Only service-confirmed consumer Time can present continuous sound. */
  private presentAudio(playhead: Time) {
    const state = this.host.state();
    if (state && compareTime(state.playhead, playhead) !== 0)
      this.host.publish({ ...state, playhead });
  }
  select(selectedId: string | null) {
    const { state } = this.active();
    if (state.propertyDrafts?.length) { this.host.report?.(Error("Confirm or cancel the property draft before changing selection.")); return; }
    const sequence = (state.historical ?? state.head).project.sequences.find(
      (s) => s.id === state.sequenceId,
    );
    const track = sequence?.tracks.find((t) =>
      t.elements.some(
        (e) =>
          e.id === selectedId &&
          ["video", "audio", "image", "text"].includes(e.content.kind),
      ),
    );
    if (track && selectedId) {
      this.timelineEdit.select({ track_id: track.id, element_id: selectedId });
      return;
    }
    if (selectedId !== state.selectedId) this.audioGain.cancel();
    const current = this.host.state()!;
    this.host.publish({
      ...current,
      selectedId,
      timelineEdit: { ...current.timelineEdit, selection: [] },
    });
  }
  returnToHead() {
    this.clipboard.invalidate();
    this.audioPreview.seek();
    const { state } = this.active();
    this.timelineEdit.invalidate();
    if (state.historical) this.audioGain.invalidate();
    this.host.changeView();
    this.host.publish({
      ...state,
      timelineEdit: this.host.state()!.timelineEdit,
      clipboard: this.host.state()!.clipboard,
      historical: null,
      gainDraft: null,
      sequenceId: state.head.project.sequences.some(
        (s) => s.id === state.sequenceId,
      )
        ? state.sequenceId
        : state.head.project.default_sequence_id,
      selectedId: null,
      historyPlan: null,
    });
  }
  async inspectRevision(revision: string) {
    this.clipboard.invalidate();
    this.timelineEdit.invalidate();
    this.audioPreview.seek();
    this.audioGain.invalidate();
    const { api, state } = this.active(),
      generation = this.host.generation(),
      view = this.host.changeView();
    const historical = await api.scene.inspect(state.head.project.id, revision);
    if (
      generation !== this.host.generation() ||
      view !== this.host.viewGeneration()
    )
      return;
    this.host.publish({
      ...this.host.state()!,
      historical,
      gainDraft: null,
      sequenceId: historical.project.sequences.some(
        (s) => s.id === state.sequenceId,
      )
        ? state.sequenceId
        : historical.project.default_sequence_id,
      selectedId: null,
      historyPlan: null,
    });
  }
  async write(label: string, operations: Operation[]) {
    const { api, state } = this.active();
    if (state.historical)
      throw new Error("Return to the current revision before editing.");
    const release = this.host.acquire(state.head.project.id),
      generation = this.host.generation(),
      view = this.host.viewGeneration();
    const request = api.scene.request(state.head, label, operations);
    try {
      await api.sdk.call("opencut_edit_plan", request);
      if (
        generation !== this.host.generation() ||
        view !== this.host.viewGeneration()
      )
        throw new Error("Project view changed before saving.");
      const result = await this.recovery.publicApply(
        api,
        request,
        state.sequenceId,
      );
      if (generation === this.host.generation()) await this.refresh();
      return result;
    } finally {
      release();
    }
  }
  async planMediaRemoval(assetIds: string[]) {
    const { api, state } = this.active();
    if (state.historical)
      throw new Error("Return to the current revision before editing.");
    const release = this.host.acquire(state.head.project.id),
      generation = this.host.generation(),
      view = this.host.viewGeneration();
    try {
      const request: ToolInput<"opencut_media_remove_plan"> = {
        project_id: state.head.project.id,
        expected_revision: state.head.revision,
        idempotency_key: crypto.randomUUID(),
        label: "Remove media from project",
        asset_ids: [...assetIds],
      };
      const transaction = await api.sdk.call(
        "opencut_media_remove_plan",
        request,
      );
      if (
        api !== this.host.api() ||
        generation !== this.host.generation() ||
        view !== this.host.viewGeneration() ||
        this.host.state()?.head.revision !== request.expected_revision
      )
        throw new Error("The media target changed before review.");
      if (
        transaction.project_id !== request.project_id ||
        transaction.expected_revision !== request.expected_revision ||
        transaction.idempotency_key !== request.idempotency_key ||
        transaction.label !== request.label
      )
        throw new Error(
          "The media removal plan differs from the captured request.",
        );
      return {
        transaction,
        sequenceId: state.sequenceId,
        assetIds: [...assetIds],
      };
    } finally {
      release();
    }
  }
  /** A typed application planner already owns defaults, placement and time.
   * Preserve its original request/key through the shared durable write port. */
  async applyPlanned(
    request: ToolInput<"opencut_edit_apply">,
    sequenceId: string,
    draftOwner?: string,
  ) {
    const { api, state } = this.active();
    if (
      state.historical ||
      state.head.project.id !== request.project_id ||
      state.head.revision !== request.expected_revision ||
      state.sequenceId !== sequenceId
    )
      throw new Error("The edit target changed.");
    if (draftOwner && !state.propertyDrafts?.includes(draftOwner))
      throw Error("The property draft is no longer active.");
    const generation = this.host.generation(),
      view = this.host.viewGeneration();
    const release = this.host.acquire(request.project_id, draftOwner);
    try {
      if (
        api !== this.host.api() ||
        generation !== this.host.generation() ||
        view !== this.host.viewGeneration()
      )
        throw new Error("The edit target changed.");
      const receipt = await this.recovery.publicApply(api, request, sequenceId);
      if (draftOwner) this.propertyDraft(draftOwner, false);
      if (
        api === this.host.api() &&
        generation === this.host.generation() &&
        this.host.state()?.head.project.id === request.project_id
      )
        await this.refresh();
      return receipt;
    } catch (error) {
      const current = this.host.state(), record = current?.recovery?.record;
      if (draftOwner && api === this.host.api() && generation === this.host.generation() &&
          record?.project_id === request.project_id && record.context.intent.kind === "public_apply" &&
          exactRecovery(record.context.intent.request) === exactRecovery(request))
        this.propertyDraft(draftOwner, false);
      throw error;
    } finally {
      release();
    }
  }
  async applyCaption(context: CaptionContext) {
    const { api, state } = this.active(),
      request = context.intent.request;
    if (
      state.historical ||
      state.head.project.id !== request.project_id ||
      state.head.revision !== request.expected_revision ||
      state.sequenceId !== request.sequence_id
    )
      throw new Error("The caption target changed.");
    if (
      state.recovery?.record ||
      state.recovery?.error
    )
      throw new Error("Resolve the original edit before changing captions.");
    const generation = this.host.generation(),
      view = this.host.viewGeneration(),
      release = this.host.acquire(request.project_id);
    try {
      const result = await this.recovery.execute(api, context);
      if (result.kind !== "subtitle_generate" && result.kind !== "subtitle_import")
        throw new Error("The recovered action differs from the original caption edit.");
      const receipt = result.caption;
      if (
        api === this.host.api() &&
        generation === this.host.generation() &&
        view === this.host.viewGeneration() &&
        this.host.state()?.head.project.id === request.project_id
      )
        await this.refresh();
      return receipt;
    } finally {
      release();
    }
  }
  async importFont(request:ToolInput<"opencut_font_import">) {
    if (this.recovery.blocked())
      throw new Error(
        "Resolve the original edit before importing fonts.",
      );
    const { api, state } = this.active();
    if (
      state.historical ||
      state.head.project.id !== request.project_id ||
      state.head.revision !== request.expected_revision
    )
      throw new Error("The edit target changed.");
    const generation = this.host.generation(),
      view = this.host.viewGeneration(),
      release = this.host.acquire(request.project_id);
    try {
      const result = await this.recovery.execute(api,{sequence_id:state.sequenceId,intent:{kind:"font_import",request}});
      if(result.kind!=="font_import") throw new Error("The recovered action differs from its original font import.");
      const receipt=result.transaction;
      if (
        api === this.host.api() &&
        generation === this.host.generation() &&
        view === this.host.viewGeneration() &&
        this.host.state()?.head.project.id === request.project_id
      )
        await this.refresh();
      return receipt;
    } finally {
      release();
    }
  }
  bookmark(operation: BookmarkOperation) {
    return this.write("Edit scene bookmark", [{ type: "bookmark", operation }]);
  }
  async planUndo(target_transaction_id: string, apply = false) {
    const { api, state } = this.active();
    if (state.propertyDrafts?.length)
      throw Error("Confirm or cancel the property draft before starting another edit.");
    if (state.historical)
      throw new Error("Return to the current revision before editing.");
    this.clipboard.invalidate();
    // Retire old plans without erasing the current UI selection. refresh()
    // normalizes surviving targets against the resulting immutable document.
    this.timelineEdit.invalidate(false);
    const generation = this.host.generation(),
      view = this.host.changeView();
    const input = {
      project_id: state.head.project.id,
      expected_revision: state.head.revision,
      target_transaction_id,
      idempotency_key: crypto.randomUUID(),
    };
    const plan = await api.sdk.call("opencut_edit_undo_plan", input);
    if (
      generation === this.host.generation() &&
      view === this.host.viewGeneration() &&
      this.host.state()?.head.revision === state.head.revision
    ) {
      if (this.host.state()?.propertyDrafts?.length)
        throw Error("Confirm or cancel the property draft before starting another edit.");
      const proposal = { direction: "undo" as const, input, plan, review: !apply || plan.conflicts.length > 0 };
      this.host.publish({ ...this.host.state()!, historyPlan: proposal });
      if (apply && !plan.conflicts.length && this.host.state()?.historyPlan === proposal)
        return this.applyHistory();
    }
  }
  async planRedo(undo_transaction_id: string, apply = false) {
    const { api, state } = this.active();
    if (state.propertyDrafts?.length)
      throw Error("Confirm or cancel the property draft before starting another edit.");
    if (state.historical)
      throw new Error("Return to the current revision before editing.");
    this.clipboard.invalidate();
    this.timelineEdit.invalidate(false);
    const generation = this.host.generation(),
      view = this.host.changeView();
    const input = {
      project_id: state.head.project.id,
      expected_revision: state.head.revision,
      undo_transaction_id,
      idempotency_key: crypto.randomUUID(),
    };
    const plan = await api.sdk.call("opencut_edit_redo_plan", input);
    if (
      generation === this.host.generation() &&
      view === this.host.viewGeneration() &&
      this.host.state()?.head.revision === state.head.revision
    ) {
      if (this.host.state()?.propertyDrafts?.length)
        throw Error("Confirm or cancel the property draft before starting another edit.");
      const proposal = { direction: "redo" as const, input, plan, review: !apply || plan.conflicts.length > 0 };
      this.host.publish({ ...this.host.state()!, historyPlan: proposal });
      if (apply && !plan.conflicts.length && this.host.state()?.historyPlan === proposal)
        return this.applyHistory();
    }
  }
  async olderHistory() {
    const { api, state } = this.active(),
      generation = this.host.generation(),
      connection = api.connectionIdentity;
    if (!state.history.next_cursor) return;
    const page = await api.sdk.call("opencut_project_history", {
      project_id: state.head.project.id,
      from_revision: state.history.next_cursor,
      limit: 50,
    });
    if (page.project_id !== state.head.project.id)
      throw Error("History response changed the requested project.");
    const actors = await api.scene.revisionActors(state.head.project.id, page.revisions.map(item => item.revision)).catch(() => undefined);
    if (
      generation !== this.host.generation() ||
      api !== this.host.api() ||
      api.connectionIdentity !== connection ||
      this.host.state()?.head.project.id !== state.head.project.id ||
      this.host.state()?.history.next_cursor !== state.history.next_cursor
    )
      return;
    const current = this.host.state()!;
    this.host.publish({
      ...current,
      revisionActors: { ...current.revisionActors, ...actors },
      history: {
        ...current.history,
        next_cursor: page.next_cursor,
        revisions: [
          ...current.history.revisions,
          ...page.revisions.filter(
            (r) =>
              !current.history.revisions.some(
                (old) => old.revision === r.revision,
              ),
          ),
        ],
      },
    });
  }
  async applyHistory() {
    const { api, state } = this.active(),
      proposal = state.historyPlan;
    if (
      !proposal ||
      state.historical ||
      proposal.plan.conflicts.length ||
      proposal.input.expected_revision !== state.head.revision
    )
      throw new Error(
        "Choose a conflict-free history plan for the current revision.",
      );
    const release = this.host.acquire(state.head.project.id),
      generation = this.host.generation();
    try {
      const wrapped = await this.recovery.execute(api, {
        sequence_id: state.sequenceId,
        intent:
          proposal.direction === "undo"
            ? { kind: "undo", request: proposal.input }
            : { kind: "redo", request: proposal.input },
      });
      if (wrapped.kind !== "undo" && wrapped.kind !== "redo")
        throw new Error("The service returned a different history receipt.");
      const result = wrapped.commit;
      if (generation === this.host.generation()) await this.refresh();
      return result;
    } finally {
      release();
    }
  }
  async restoreRevision(revision: string, expectedRevision?: string) {
    const { api, state } = this.active();
    if (expectedRevision && state.head.revision !== expectedRevision)
      throw new Error("The project changed. Review the restore again.");
    if (!state.history.revisions.some((item) => item.revision === revision))
      throw new Error("Choose a saved project revision to restore.");
    const release = this.host.acquire(state.head.project.id),
      generation = this.host.generation();
    try {
      const receipt = await this.recovery.execute(api, {
        sequence_id: null,
        intent: {
          kind: "restore",
          request: {
            project_id: state.head.project.id,
            expected_revision: state.head.revision,
            target_revision: revision,
            idempotency_key: crypto.randomUUID(),
            asset_versions: {},
          },
        },
      });
      if (receipt.kind !== "restore")
        throw new Error("The service returned a different restore receipt.");
      if (generation === this.host.generation()) {
        this.returnToHead();
        await this.refresh();
      }
      return receipt.transaction;
    } finally {
      release();
    }
  }
}
