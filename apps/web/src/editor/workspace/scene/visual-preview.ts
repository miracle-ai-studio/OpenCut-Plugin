import type { ToolInput } from "../../../lib/editor-sdk";
import { OpenCutApiError } from "../../../lib/editor-sdk";
import type { SessionHost } from "./session";
import type { AudioPreviewSession, AudioPreviewState } from "./types";
import type { PicturePresentation } from "./audio-preview";
import type { VisualPreviewRenderer } from "./visual-preview-renderer";
import { PictureWorkerRenderer } from "./picture-worker-renderer";
import type { CachedPicture } from "./picture-cache";
import {StillPreview} from "./still-preview";
import {sequenceContentEnd} from "./sequence-navigation";
import {subtractTime,zero} from "../../model/time";
type Renderer = Pick<PictureWorkerRenderer, "render" | "close"> & Partial<Pick<PictureWorkerRenderer,"prepareCache"|"cachedFrame"|"startSurface"|"startLiveSurface"|"surfaceTimestamp"|"stopSurface"|"suspend"|"stillFrame">>;
type BufferedPicture = Awaited<ReturnType<VisualPreviewRenderer["render"]>>|CachedPicture;
type Screen = { request(callback: FrameRequestCallback): number; cancel(id: number): void };
type PictureTime = AudioPreviewSession["presentation_time"];
const compareTime = (a: PictureTime, b: PictureTime) =>
  BigInt(a.ticks) * BigInt(b.timebase) - BigInt(b.ticks) * BigInt(a.timebase);
const pictureBytes = (picture: { width: number; height: number }) =>
  picture.width * picture.height * 4;
export type VisualEditDraftInput = {
  intent: ToolInput<"opencut_visual_preview_edit_draft_frame">["intent"];
  draftId: string;
  transaction: ToolInput<"opencut_edit_apply">;
  sequenceId: string;
  time: AudioPreviewSession["presentation_time"];
};
type PendingTransformDraft = {
  input: VisualEditDraftInput;
  api: NonNullable<ReturnType<SessionHost["api"]>>;
  session: AudioPreviewSession;
  check(): void;
  signal: AbortSignal;
  resolve(): void;
  reject(error: unknown): void;
};

export type VisualPreviewView = {
  target?:string;
  phase: "idle" | "loading" | "ready" | "failed";
  pixels: ImageData | ImageBitmap | null;
  time: number | null;
  generation: string | null;
  error: string | null;
  runtime: string | null;
  presentedFrames: number;
  discardedFrames: number;
  cacheCompleted: number;
  cacheTotal: number;
  statistics: string | null;
  lastRun?: string | null;
  surfaceGeneration?:number;
  surfaceActive?:boolean;
  editDraftId?: string | null;
  restoringDraftId?: string | null;
};
const empty = (): VisualPreviewView => ({
  phase: "idle",
  pixels: null,
  time: null,
  generation: null,
  error: null,
  runtime: null,
  presentedFrames: 0,
  discardedFrames: 0,
  cacheCompleted: 0,
  cacheTotal: 0,
  statistics: null,
  lastRun:null,
  surfaceGeneration:0,
  surfaceActive:false,
});
const token = (s: AudioPreviewSession) =>
  JSON.stringify([
    s.identity,
    s.session_id,
    s.generation,
    s.overlay_epoch,
    s.owner_id,
    s.lease_id,
  ]);
/** Latest confirmed consumer time, one serial renderer and one pending intent.
 * No wall-clock playhead, durable frame jobs or unbounded presentation queue. */
export class VisualPreview {
  private editRestoration: { draftId: string; promise: Promise<boolean> } | null = null;
  private view: VisualPreviewView = empty();
  private listeners = new Set<() => void>();
  private epoch = 0;
  private renderer: Renderer | null = null;
  private surfaceGeneration=0;
  private surfaceCanvas:HTMLCanvasElement|null=null;
  private surfaceTransferred=false;
  private surfaceMode=false;
  private surfaceTimestampAt=-Infinity;
  setPlaybackCanvas(canvas:HTMLCanvasElement|null,generation:number) {
    if(generation!==this.surfaceGeneration)return;
    if(canvas!==this.surfaceCanvas){this.surfaceCanvas=canvas;this.surfaceTransferred=false;}
  }
  private abort = new AbortController();
  private desired: AudioPreviewSession | null = null;
  private presentedTime: AudioPreviewSession["presentation_time"] | null =
    null;
  private currentToken: string | null = null;
  private outputTime: AudioPreviewSession["presentation_time"] | null = null;
  private running: Promise<void> | null = null;
  private transformDraftId: string | null = null;
  private transformPending: PendingTransformDraft | null = null;
  private transformDraining = false;
  private transformAbort: AbortController | null = null;
  private transformSerial = 0;
  private transformSignalCleanup: (() => void) | null = null;
  private authorityTimer: ReturnType<typeof setInterval> | null = null;
  private watchdog: ReturnType<typeof setTimeout> | null = null;
  private buffered: BufferedPicture[] = [];
  private nextPreparation: PictureTime | null = null;
  private bufferSession: AudioPreviewSession | null = null;
  private pictureReady: ((time: PictureTime) => void) | null = null;
  private filling = false;
  private presentedBytes = 0;
  private renderLeadMs = 150;
  private presentationFrame: number | null = null;
  private cacheReady = false;
  private cacheFrameSeconds = 0;
  private lags: number[] = [];
  private missedCachedFrames = 0;
  private skippedIntervals:{from:number;to:number}[]=[];
  private maxFrameGapMs = 0;
  private lastPresentedOutput: PictureTime | null = null;
  private still:StillPreview;
  private stillSerial=0;
  private refineTimer:ReturnType<typeof setTimeout>|null=null;
  private retainedPause=false;
  constructor(
    private readonly host: SessionHost,
    private readonly failure: (e: Error) => void,
    private readonly factory: () => Renderer = () =>
      new PictureWorkerRenderer(),
    private readonly screen: Screen = {
      request: (callback) => requestAnimationFrame(callback),
      cancel: (id) => cancelAnimationFrame(id),
    },
    private readonly beforeStill:()=>Promise<void>=async()=>{},
  ) {this.still=new StillPreview(host,error=>{this.clear();this.publish({phase:"failed",pixels:null,error:error.message});});}
  snapshot = () => this.view;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private publish(patch: Partial<VisualPreviewView>) {
    // Repeated device/authority observations often describe the same empty or
    // loading picture. A no-op must not create a new external-store snapshot
    // and synchronously force React through another render/commit cycle.
    if (Object.entries(patch).every(([key, value]) => Object.is(this.view[key as keyof VisualPreviewView], value))) return;
    if (
      "pixels" in patch &&
      this.view.pixels &&
      patch.pixels !== this.view.pixels &&
      "close" in this.view.pixels
    )
      this.view.pixels.close();
    this.view = { ...this.view, ...patch };
    for (const listener of this.listeners) listener();
  }
  private clear(retainPicture=false) {
    const keep=retainPicture&&(this.view.surfaceActive||!!this.view.pixels),identity=this.currentToken;
    this.retainedPause=keep;
    if(this.refineTimer)clearTimeout(this.refineTimer);this.refineTimer=null;
    this.renderer?.stopSurface?.(!keep);
    this.surfaceMode=false;
    if (this.presentationFrame !== null) this.screen.cancel(this.presentationFrame);
    this.presentationFrame = null;
    for (const item of this.buffered)
      if ("close" in item.pixels) item.pixels.close();
    this.buffered = [];
    this.nextPreparation = null;
    this.bufferSession = null;
    this.pictureReady = null;
    this.presentedBytes = 0;
    this.cacheReady = false;
    this.cacheFrameSeconds = 0;
    this.lags = [];
    this.missedCachedFrames = 0;
    this.skippedIntervals=[];
    this.maxFrameGapMs = 0;
    this.lastPresentedOutput = null;
    this.dropTransformDraft();
    if (this.authorityTimer) clearInterval(this.authorityTimer);
    this.authorityTimer = null;
    this.presentedTime = null;
    this.abort.abort();
    this.abort = new AbortController();
    this.desired = null;
    this.currentToken = keep?identity:null;
    this.outputTime = null;
    if (this.watchdog) clearTimeout(this.watchdog);
    this.watchdog = null;
    if(!keep)this.publish({ pixels: null, time: null, generation: null, presentedFrames: 0, discardedFrames: 0,surfaceActive:false });
  }
  private stop(retain=true) {
    const lastRun=(this.cacheReady||this.surfaceMode)&&this.view.presentedFrames>0?JSON.stringify({
      generation:this.view.generation,lastFrameTime:this.view.time,presentedFrames:this.view.presentedFrames,
      discardedFrames:this.view.discardedFrames,...JSON.parse(this.surfaceMode?(this.view.statistics??"{}"):this.presentationStatistics()),
    }):null;
    this.clear();
    const renderer=this.renderer;
    if(retain&&renderer?.suspend)void this.serialize(()=>renderer.suspend!()).catch(()=>{renderer.close();if(this.renderer===renderer)this.renderer=null;});
    else {renderer?.close();this.renderer=null;}
    this.publish({...empty(),lastRun,surfaceGeneration:this.renderer?this.surfaceGeneration:0});
  }
  dispose(){this.epoch++;this.stillSerial++;void this.still.close(true).catch(()=>{});this.stop(false);}
  retireStill():Promise<void>{
    this.stillSerial++;this.abort.abort();
    this.publish({phase:this.view.pixels?"ready":"idle"});
    return this.serialize(()=>this.still.close());
  }
  showStill():Promise<void>{
    const state=this.host.state(),api=this.host.api(),snapshot=state&&(state.historical??state.head);
    if(!state||!snapshot||!api||["loading","playing","stalled","unconfirmed"].includes(state.audioPreview.phase))return Promise.resolve();
    const sequence=snapshot.project.sequences.find(s=>s.id===state.sequenceId);if(!sequence)return Promise.resolve();
    let time=state.playhead;
    const end=sequenceContentEnd(sequence);
    if(compareTime(time,end)>=0n&&snapshot.project.settings.profile==="classic_global"){
      const fps=snapshot.project.settings.settings.fps;
      time=subtractTime(end,{ticks:String(fps.denominator),timebase:fps.numerator});if(compareTime(time,zero)<0n)time=zero;
    }
    const serial=++this.stillSerial,epoch=this.epoch;
    this.abort.abort();this.abort=new AbortController();const signal=this.abort.signal;
    if(this.refineTimer)clearTimeout(this.refineTimer);this.refineTimer=null;
    this.publish({phase:"loading",error:null});
    return this.serialize(async()=>{
      if(signal.aborted||serial!==this.stillSerial||epoch!==this.epoch)return;
      await this.beforeStill();
      if(signal.aborted||serial!==this.stillSerial||epoch!==this.epoch)return;
      const current=this.host.state(),currentSnapshot=current&&(current.historical??current.head);
      if(!current||this.host.api()!==api||currentSnapshot?.revision!==snapshot.revision||
        current.sequenceId!==state.sequenceId||compareTime(current.playhead,state.playhead)!==0n||
        ["loading","playing","stalled","unconfirmed"].includes(current.audioPreview.phase))return;
      // A paused sound session already owns this native picture. Opening a
      // separate still owner would duplicate resources and detach edit tools.
      const paused=current.audioPreview.phase==="paused"?current.audioPreview.session:null;
      if(paused&&(paused.state!=="paused"||paused.identity.project_id!==snapshot.project.id||
        paused.identity.revision!==snapshot.revision||paused.identity.sequence_id!==state.sequenceId||
        compareTime(paused.presentation_time,state.playhead)!==0n))
        throw new Error("The paused preview no longer matches this frame.");
      const session=paused??await this.still.read(time);
      if(signal.aborted||serial!==this.stillSerial||epoch!==this.epoch)return;
      this.currentToken=token(session);
      await this.render(session,signal,epoch);
    }).catch(error=>{
      if(!signal.aborted&&serial===this.stillSerial&&epoch===this.epoch){this.publish({phase:"failed",pixels:null,error:error instanceof Error?error.message:String(error)});throw error;}
    });
  }
  private dropTransformDraft() {
    this.transformSerial++;
    this.transformSignalCleanup?.();
    this.transformSignalCleanup = null;
    this.transformDraftId = null;
    this.transformAbort?.abort();
    this.transformAbort = null;
    this.transformPending?.reject(
      new DOMException("Transform draft replaced", "AbortError"),
    );
    this.transformPending = null;
  }
  /** One running read-only draft and one latest request share the original GPU
   * queue. Rust validates the explicit gesture or static-property intent. */
  pausedEditSession(): AudioPreviewSession | null {
    const state = this.host.state();
    if (!state || state.historical || ["loading","playing","stalled","unconfirmed"].includes(state.audioPreview.phase)) return null;
    const session = state.audioPreview.phase === "paused" && state.audioPreview.session
      ? state.audioPreview.session : this.still.currentPaused(state.playhead);
    return session && this.currentToken === token(session) &&
      session.identity.project_id === state.head.project.id && session.identity.revision === state.head.revision &&
      session.identity.sequence_id === state.sequenceId && compareTime(session.presentation_time,state.playhead) === 0n ? session : null;
  }
  previewEditDraft(
    input: VisualEditDraftInput,
    signal: AbortSignal,
  ): Promise<void> {
    const api = this.host.api(),
      state = this.host.state(),
      session = this.pausedEditSession();
    if (
      !api ||
      !state ||
      state.historical ||
      !session ||
      this.currentToken !== token(session) ||
      (this.view.phase !== "ready" && !(this.view.phase === "loading" &&
        this.transformDraftId === input.draftId && this.view.pixels)) ||
      state.head.project.id !== input.transaction.project_id ||
      state.head.revision !== input.transaction.expected_revision ||
      state.sequenceId !== input.sequenceId ||
      JSON.stringify(state.playhead) !== JSON.stringify(input.time)
    )
      return Promise.reject(
        new Error(
          "Pause the live preview on the original scene before transforming content.",
        ),
      );
    const captured = structuredClone(input),
      epoch = this.epoch,
      identity = api.connectionIdentity,
      expectedToken = token(session),
      serial = ++this.transformSerial;
    const check = () => {
      signal.throwIfAborted();
      const current = this.host.state(),
        active = this.pausedEditSession();
      if (
        serial !== this.transformSerial ||
        epoch !== this.epoch ||
        this.transformDraftId !== captured.draftId ||
        api !== this.host.api() ||
        identity !== api.connectionIdentity ||
        !current ||
        current?.historical ||
        !active ||
        token(active) !== expectedToken ||
        current.head.project.id !== captured.transaction.project_id ||
        current.head.revision !== captured.transaction.expected_revision ||
        current.sequenceId !== captured.sequenceId ||
        JSON.stringify(current.playhead) !== JSON.stringify(captured.time)
      )
        throw new DOMException("Transform draft target changed", "AbortError");
    };
    this.transformDraftId = captured.draftId;
    this.editRestoration = null;
    this.desired = null;
    this.transformAbort?.abort();
    this.transformPending?.reject(
      new DOMException("Transform draft replaced", "AbortError"),
    );
    const result = new Promise<void>((resolve, reject) => {
      this.transformPending = {
        api,
        session,
        input: captured,
        check,
        signal,
        resolve,
        reject,
      };
    });
    this.transformSignalCleanup?.();
    const onAbort = () => {
      if (serial === this.transformSerial)
        this.clearEditDraft(captured.draftId);
    };
    signal.addEventListener("abort", onAbort, { once: true });
    this.transformSignalCleanup = () =>
      signal.removeEventListener("abort", onAbort);
    if (signal.aborted) onAbort();
    if (!this.transformDraining) {
      this.transformDraining = true;
      void this.serialize(async () => {
        try {
          while (this.transformPending) {
            const pending = this.transformPending;
            this.transformPending = null;
            const abort = new AbortController();
            this.transformAbort = abort;
            let picture: Awaited<ReturnType<Renderer["render"]>> | null = null;
            try {
              pending.check();
              const renderer = this.renderer;
              if (!renderer) throw new Error("Live preview is unavailable.");
              picture = await renderer.render(
                pending.api,
                {
                  session_id: pending.session.session_id,
                  generation: pending.session.generation,
                  overlay_epoch: pending.session.overlay_epoch,
                  owner_id: pending.session.owner_id,
                  lease_id: pending.session.lease_id,
                },
                // Let the current HTTP phase finish. Pointer cancellation
                // prevents the next phase/publication; it does not pretend to
                // have stopped the host's blocking compile.
                this.abort.signal,
                undefined,
                {
                  draftId: pending.input.draftId,
                  transaction: pending.input.transaction,
                  intent: pending.input.intent,
                  time: pending.input.time,
                  intentSignal: AbortSignal.any([pending.signal, abort.signal]),
                },
              );
              pending.check();
              if (
                BigInt(picture.frame.plan.identity.time.ticks) *
                  BigInt(pending.input.time.timebase) !==
                BigInt(pending.input.time.ticks) *
                  BigInt(picture.frame.plan.identity.time.timebase)
              )
                throw new Error(
                  "Transform draft returned another paused time.",
                );
              this.publish({
                pixels: picture.pixels,
                phase: "ready",
                editDraftId: pending.input.draftId,
                restoringDraftId: null,
                time: picture.frame.time_seconds,
                generation: pending.session.generation,
                runtime: JSON.stringify(picture.runtime),
                error: null,
              });
              picture = null;
              pending.resolve();
            } catch (error) {
              pending.reject(error);
            } finally {
              if (picture && "close" in picture.pixels) picture.pixels.close();
              if (this.transformAbort === abort) this.transformAbort = null;
            }
          }
        } finally {
          this.transformDraining = false;
        }
      });
    }
    return result;
  }
  /** Retire obsolete work without queuing a baseline between pointer moves.
   * The owning gesture must subsequently publish its latest value or restore. */
  supersedeEditDraft(draftId: string): void {
    if (this.transformDraftId !== draftId && this.editRestoration?.draftId !== draftId) return;
    this.dropTransformDraft();
    this.transformDraftId = draftId;
    this.editRestoration = null;
  }
  clearEditDraft(draftId: string): Promise<boolean> {
    if (this.transformDraftId !== draftId)
      return this.editRestoration?.draftId === draftId ? this.editRestoration.promise : Promise.resolve(false);
    this.dropTransformDraft();
    this.presentedTime = null;
    this.desired = null;
    const scene = this.host.state(), session = this.pausedEditSession(),
      epoch = this.epoch, api = this.host.api(), serial = this.transformSerial;
    if (!scene || !session || !api) return Promise.resolve(false);
    const expected = token(session), time = JSON.stringify(scene.playhead);
    const current = () => {
      const live = this.host.state(), active = this.pausedEditSession();
      return epoch === this.epoch && serial === this.transformSerial && api === this.host.api() &&
        !!live && !!active && token(active) === expected && this.currentToken === expected &&
        live.head.revision === scene.head.revision && live.sequenceId === scene.sequenceId &&
        JSON.stringify(live.playhead) === time;
    };
    // Keep the last valid pixels while restoring, but never label draft pixels
    // as the restored original frame. Completion follows the real renderer.
    this.publish({ phase: "loading", restoringDraftId: draftId });
    const promise = this.serialize(async () => {
      if (!current()) return false;
      await this.render(session, this.abort.signal, epoch, true);
      return current() && this.view.phase === "ready" && this.view.editDraftId == null && this.view.restoringDraftId == null;
    }).catch(error => {
      if (current()) {
        const failure = error instanceof Error ? error : Error(String(error));
        this.publish({ phase: "failed", error: failure.message }); this.failure(failure);
      }
      return false;
    });
    this.editRestoration = { draftId, promise };
    return promise;
  }
  /** An explicit paused-frame query copies one actual post-mask alpha texel
   * per layer. It never infers opacity from the opaque finished composition. */
  async hitTest(point: { x: number; y: number }) {
    const api = this.host.api(),
      state = this.host.state(),
      session = state?.audioPreview.session,
      epoch = this.epoch;
    if (
      this.transformDraftId ||
      !api ||
      !state ||
      state.audioPreview.phase !== "paused" ||
      !session ||
      this.currentToken !== token(session) ||
      this.view.phase !== "ready"
    )
      throw new Error(
        "Pause the live preview before selecting picture content.",
      );
    const expectedToken = token(session),
      snapshot = state.historical ?? state.head;
    const check = () => {
      const current = this.host.state(),
        active = current?.audioPreview.session;
      if (
        this.transformDraftId !== null ||
        epoch !== this.epoch ||
        api !== this.host.api() ||
        current?.audioPreview.phase !== "paused" ||
        !active ||
        token(active) !== expectedToken ||
        current.head.project.id !== state.head.project.id ||
        (current.historical ?? current.head).revision !== snapshot.revision ||
        current.sequenceId !== state.sequenceId ||
        JSON.stringify(current.playhead) !== JSON.stringify(state.playhead)
      )
        throw new DOMException("Canvas hit query replaced", "AbortError");
    };
    return this.serialize(async () => {
      check();
      this.abort.signal.throwIfAborted();
      const renderer = this.renderer;
      if (!renderer) throw new Error("Live preview is unavailable.");
      const result = await renderer.render(
        api,
        {
          session_id: session.session_id,
          generation: session.generation,
          overlay_epoch: session.overlay_epoch,
          owner_id: session.owner_id,
          lease_id: session.lease_id,
        },
        this.abort.signal,
        point,
      );
      try {
        check();
        const observation = result.hitTest;
        if (
          !observation ||
          observation.point.x !== point.x ||
          observation.point.y !== point.y
        )
          throw new Error(
            "The renderer omitted the requested content hit observation.",
          );
        return {
          projectId: snapshot.project.id,
          revision: snapshot.revision,
          sequenceId: state.sequenceId,
          time: structuredClone(result.frame.plan.identity.time),
          hits: observation.hits,
        };
      } finally {
        if ("close" in result.pixels) result.pixels.close();
      }
    });
  }
  /** Reserve the renderer synchronously. Waiting before reserving permits two
   * paused queries (or a query and a replacement) to enter the same GPU realm. */
  private serialize<T>(work: () => Promise<T>): Promise<T> {
    const previous = this.running;
    const task = (async () => {
      await previous;
      return work();
    })();
    const reservation = task.then(
      () => {},
      () => {},
    );
    this.running = reservation;
    void reservation.then(() => {
      if (this.running !== reservation) return;
      this.running = null;
      const state = this.host.state()?.audioPreview;
      if (this.desired && state) this.observe(state, this.epoch);
    });
    return task;
  }

  start(): PicturePresentation {
    this.stillSerial++;
    // Retire the static owner at Play intent, while sound output initializes.
    // Admission still awaits the original fence, including any failure.
    const retiring=this.still.close();
    void retiring.catch(()=>{});
    const beforeCreate=()=>retiring;
    const paused = this.host.state()?.audioPreview;
    const reuse =
      paused?.phase === "paused" &&
      paused.session &&
      this.currentToken === token(paused.session);
    this.epoch++;
    const epoch = this.epoch;
    if (reuse) {
      this.clear();
      this.publish({...empty(),surfaceGeneration:this.surfaceGeneration});
    } else {this.stop();if(!this.renderer||!this.surfaceGeneration)this.surfaceGeneration++;this.publish({surfaceGeneration:this.surfaceGeneration});}
    const valid = () => epoch === this.epoch;
    const timestamp = (value:DeviceTimestamp) => {
      if(!valid()||!this.surfaceMode)return;
      const now=performance.now();
      if(now-this.surfaceTimestampAt>=50){this.surfaceTimestampAt=now;this.renderer?.surfaceTimestamp?.(value);}
    };
    return {
      beforeCreate,
      timestamp,
      prepare: async (session, signal, pictureReady,clockSource) => {
        if (!valid()) throw new DOMException("Preview replaced", "AbortError");
        this.clear();
        this.currentToken = token(session);
        this.pictureReady = pictureReady ?? null;
        this.bufferSession = pictureReady ? session : null;
        const state = this.host.state(),
          snapshot = state && (state.historical ?? state.head);
        if (
          !snapshot ||
          snapshot.project.id !== session.identity.project_id ||
          snapshot.revision !== session.identity.revision
        )
          throw new Error(
            "Preview session differs from the fixed project view",
          );
        const sequence = snapshot.project.sequences.find(
          (s) => s.id === session.identity.sequence_id,
        );
        if (!sequence)
          throw new Error(
            "Preview scene is unavailable in the fixed project view",
          );
        this.publish({ phase: "loading", error: null });
        // Drain old generation before replacing its GPU resources.
        const renderSignal = AbortSignal.any([signal, this.abort.signal]);
        await this.serialize(async () => {
          renderSignal.throwIfAborted();
          if (!valid())
            throw new DOMException("Preview replaced", "AbortError");
          if (pictureReady) {
            this.renderer ??= this.factory();
            if(this.renderer.startLiveSurface||(this.renderer.prepareCache&&this.renderer.cachedFrame)) {
              const api=this.host.api();if(!api)throw Error("Preview service disconnected");
              // Show the requested frame before waiting for cold background
              // publication. This read-only still path reuses a nearby segment
              // or renders this frame only; it never starts the playback clock.
              let plan:Awaited<ReturnType<PictureWorkerRenderer["prepareCache"]>>|undefined;
              if(!this.renderer.startLiveSurface){
              await this.render(session,renderSignal,epoch);
              if(this.refineTimer)clearTimeout(this.refineTimer);this.refineTimer=null;
              renderSignal.throwIfAborted();
              this.publish({phase:"loading"});
              plan=await this.renderer.prepareCache!(api,{
                session_id:session.session_id,generation:session.generation,overlay_epoch:session.overlay_epoch,
                owner_id:session.owner_id,lease_id:session.lease_id,
              },renderSignal,(cacheCompleted,cacheTotal)=>{
                if(valid()&&!renderSignal.aborted)this.publish({cacheCompleted,cacheTotal});
              },session.presentation_time);
              renderSignal.throwIfAborted();
              if(!valid()||api!==this.host.api()||plan.project_id!==snapshot.project.id||plan.revision!==snapshot.revision||plan.sequence_id!==sequence.id)
                throw new DOMException("Preview cache target changed","AbortError");
              this.cacheReady=true;
              this.cacheFrameSeconds=plan.frame_step_seconds;
              }
              this.publish({presentedFrames:0,discardedFrames:0});
              const startSurface=this.renderer.startLiveSurface??this.renderer.startSurface;
              if(startSurface) {
                if(!clockSource)throw Error("Cached presentation requires the device clock port");
                const node=this.surfaceCanvas;
                if(!node||typeof node.transferControlToOffscreen!=="function")throw Object.assign(Error("Worker canvas presentation is unavailable"),{code:"CAPABILITY_UNAVAILABLE"});
                let canvas:OffscreenCanvas|undefined;
                if(!this.surfaceTransferred) {
                  if(plan){node.width=plan.width;node.height=plan.height;}canvas=node.transferControlToOffscreen();this.surfaceTransferred=true;
                }
                this.surfaceMode=true;this.surfaceTimestampAt=-Infinity;
                const expected=token(session);
                const result=await startSurface.call(this.renderer,api,session,canvas,clockSource,renderSignal,(report,retired)=>{
                  if(!valid())return;
                  if(retired) {
                    const current=this.host.state(),fixed=current&&(current.historical??current.head);
                    if(this.view.phase==="idle"&&fixed?.project.id===snapshot.project.id&&fixed.revision===snapshot.revision&&current?.sequenceId===sequence.id)
                      this.publish({lastRun:JSON.stringify({generation:report.generation,lastFrameTime:report.time,
                      presentedFrames:report.presentedFrames,discardedFrames:report.discardedFrames,runtime:JSON.parse(report.runtime),...JSON.parse(report.statistics)})});
                    return;
                  }
                  if(!this.surfaceMode||renderSignal.aborted||this.currentToken!==expected)return;
                  pictureReady(report.readyThrough);
                  this.publish({phase:"ready",target:JSON.stringify([session.identity.project_id,session.identity.revision,session.identity.sequence_id]),surfaceActive:true,pixels:null,time:report.time,generation:report.generation,runtime:report.runtime,
                    presentedFrames:report.presentedFrames,discardedFrames:report.discardedFrames,statistics:report.statistics,error:null});
                },error=>{
                  if(valid()&&!renderSignal.aborted&&this.currentToken===expected){this.stop();this.publish({phase:"failed",error:error.message});this.failure(error);}
                },sequence.tracks.flatMap(track=>track.elements.map(element=>element.timeline_range.start)));
                renderSignal.throwIfAborted();if(!valid())throw new DOMException("Cached presentation replaced","AbortError");
                pictureReady(result.readyThrough);
                return;
              }
            }
            await this.fillPictures(session, renderSignal, epoch, this.cacheReady ? 8 : 2);
          }
          else await this.render(session, renderSignal, epoch);
        });
      },
      suspend: (retainPicture=false) => {
        if (valid()) this.clear(retainPicture);
      },
      hasPausedPicture: session => valid() && this.currentToken === token(session) &&
        !this.transformDraftId && !this.editRestoration &&
        this.view.phase === "ready" && this.view.generation === session.generation &&
        (this.view.surfaceActive || !!this.view.pixels),
      consume: (session, frames, atRefresh,deviceTimestamp) => {
        if (!valid() || this.currentToken !== token(session)) return;
        // Device output observation only fences presentation. It never compiles IR
        // or substitutes for Rust's confirmed consumer time.
        this.outputTime = {
          ticks: (
            BigInt(session.actual_start_sample) + BigInt(frames)
          ).toString(),
          timebase: session.identity.sample_rate,
        };
        if(this.surfaceMode) {
          if(deviceTimestamp)timestamp(deviceTimestamp);
          return;
        }
        if (this.pictureReady) {
          // The output adapter already sampled its device clock in this
          // refresh callback. Present against that fresh observation now;
          // nesting another rAF would display yesterday's audio position.
          if(atRefresh) {
            if(this.presentationFrame!==null)this.screen.cancel(this.presentationFrame);
            this.presentationFrame=null;
            this.presentBuffered();
          } else this.presentPictures();
          this.requestPictures(epoch);
          return;
        }
        // A late picture does not revoke an already authorized visible frame.
        // Replacement/seek/authority failure clears it through the lifecycle.
      },
      observe: (state) => {
        if (valid()) this.observe(state, epoch);
      },
      close: () => {
        if (valid()) this.stop();
      },
    };
  }
  private observe(state: AudioPreviewState, epoch: number) {
    const session = state.session;
    if (state.phase === "failed" || state.phase === "unconfirmed") {
      this.stop();
      return;
    }
    if (!session) {
      if (state.phase !== "loading") this.stop();
      return;
    }
    // Loading stops old presentation synchronously (seek/gain replacement).
    if (state.phase === "loading" && this.currentToken !== token(session)) {
      this.clear();
      return;
    }
    if (this.currentToken !== token(session)) {
      const retain=this.retainedPause&&state.phase==="paused"&&this.view.target===JSON.stringify([session.identity.project_id,session.identity.revision,session.identity.sequence_id]);
      this.clear(retain);
      this.currentToken = token(session);
      this.retainedPause=false;
    }
    if (!["playing", "stalled", "paused", "ended"].includes(state.phase))
      return;
    if(this.surfaceMode&&this.pictureReady&&state.phase!=="paused"){this.bufferSession=session;return;}
    if (this.pictureReady && state.phase !== "paused") {
      this.bufferSession = session;
      this.presentPictures();
      this.requestPictures(epoch);
      return;
    }
    if (this.transformDraftId && state.phase !== "paused") {
      this.dropTransformDraft();
      this.presentedTime = null;
      this.publish({ pixels: null, phase: "loading" });
    }
    if (state.phase === "paused" && !this.authorityTimer) {
      const api = this.host.api(),
        identity = token(session);
      let checking = false;
      this.authorityTimer = setInterval(() => {
        if (checking || !api || epoch !== this.epoch || this.currentToken !== identity) return;
        checking = true;
        // Heartbeat checks the original execution authority and live lease
        // without rendering a new, unclaimed source-resolution picture.
        const authority = api.scene.audioPreviewControl({
          session_id: session.session_id,
          generation: session.generation,
          owner_id: session.owner_id,
          lease_id: session.lease_id,
          control_id: crypto.randomUUID(),
          command: { kind: "heartbeat" },
        }, AbortSignal.any([this.abort.signal, AbortSignal.timeout(3000)]))
          .then(current => {
            // Control also returns overlay and fixed project identity. They
            // must still match the already displayed picture's complete scope.
            if (token(current) !== identity || current.state !== "paused")
              throw Error("Paused preview session changed during authorization");
          });
        void authority
          .catch((error) => {
            if (epoch === this.epoch && this.currentToken === identity) {
              this.epoch++;
              this.stop();
              this.publish({
                phase: "failed",
                error: error instanceof Error ? error.message : String(error),
              });
            }
          })
          .finally(() => {
            checking = false;
          });
      }, 1000);
    }
    if (state.phase !== "paused" && this.authorityTimer) {
      clearInterval(this.authorityTimer);
      this.authorityTimer = null;
    }
    if (this.transformDraftId && state.phase === "paused") return;
    this.desired = session;
    if (this.watchdog) clearTimeout(this.watchdog);
    // A stalled/paused consumer does not advance; playing without a fresh
    // observation loses its picture instead of displaying a misleading clock.
    if (state.phase === "playing")
      this.watchdog = setTimeout(
        () => this.publish({ pixels: null, phase: "loading" }),
        150,
      );
    if (!this.running) {
      void this.serialize(() => this.drain(epoch));
    }
  }
  private async drain(epoch: number) {
    while (this.desired && epoch === this.epoch && !this.transformDraftId) {
      const session = this.desired;
      this.desired = null;
      const shown = this.presentedTime,
        requested = session.presentation_time;
      // A consumer report may renew the lease without advancing the cursor.
      // Re-executing that same confirmed frame wastes decode/GPU and FIFO time.
      if (
        shown &&
        BigInt(requested.ticks) * BigInt(shown.timebase) <=
          BigInt(shown.ticks) * BigInt(requested.timebase)
      )
        continue;
      const signal = this.abort.signal;
      try {
        await this.render(session, signal, epoch);
      } catch (error) {
        if (epoch !== this.epoch || signal.aborted) return;
        if (
          error instanceof OpenCutApiError &&
          ["PREVIEW_FRAME_STALE", "PREVIEW_GENERATION_CONFLICT"].includes(
            error.code,
          )
        )
          continue;
        const e = error instanceof Error ? error : new Error(String(error));
        this.epoch++;
        this.clear();
        this.renderer?.close();
        this.renderer = null;
        this.publish({ phase: "failed", error: e.message });
        this.failure(e);
        return;
      }
    }
  }
  private presentPictures() {
    // Startup needs a drawable first frame before PCM is handed off. Once the
    // output clock starts, all completions/observations coalesce at display time.
    if (!this.outputTime) { this.presentBuffered(); return; }
    if (this.presentationFrame !== null) return;
    const epoch = this.epoch;
    this.presentationFrame = this.screen.request(() => {
      this.presentationFrame = null;
      if (epoch === this.epoch) this.presentBuffered();
    });
  }
  private presentBuffered() {
    const session = this.bufferSession;
    if (!session || !this.pictureReady) return;
    const consumer = this.outputTime ?? session.presentation_time;
    let chosen: BufferedPicture | undefined;
    let discarded = 0;
    while (
      this.buffered.length &&
      compareTime(this.buffered[0].frame.plan.identity.time, consumer) <= 0n
    ) {
      if (chosen) {
        if ("close" in chosen.pixels) chosen.pixels.close();
        discarded++;
      }
      chosen = this.buffered.shift();
    }
    // Hold the last same-generation picture across a missed display deadline.
    // Its actual timestamp remains unchanged; holding never counts as a new frame.
    if (!chosen) { this.recordLag(consumer); return; }
    const at = chosen.frame.plan.identity.time;
    if (
      compareTime(consumer, at) * 1000n >
      125n * BigInt(consumer.timebase) * BigInt(at.timebase)
    ) {
      if ("close" in chosen.pixels) chosen.pixels.close();
      this.publish({ discardedFrames: this.view.discardedFrames + discarded + 1 });
      return;
    }
    if(this.cacheReady&&this.view.time!==null&&this.cacheFrameSeconds>0) {
      const missed=Math.max(0,Math.round((chosen.frame.time_seconds-this.view.time)/this.cacheFrameSeconds)-1);
      this.missedCachedFrames+=missed;
      if(missed&&this.skippedIntervals.length<64)this.skippedIntervals.push({from:this.view.time,to:chosen.frame.time_seconds});
    }
    if(this.outputTime&&this.lastPresentedOutput) {
      const previous=this.lastPresentedOutput;
      const gap=Number(compareTime(this.outputTime,previous))*1000/(this.outputTime.timebase*previous.timebase);
      this.maxFrameGapMs=Math.max(this.maxFrameGapMs,gap);
    }
    this.lastPresentedOutput=this.outputTime;
    this.presentedTime = at;
    this.recordLag(consumer);
    // The view keeps a drawable copy; the owner retains its bitmap for theme
    // and layout repaint. Charge both against the same presentation admission.
    this.presentedBytes = pictureBytes(chosen.pixels) * 2;
    this.publish({
      phase: "ready",
      editDraftId: null,
      restoringDraftId: null,
      pixels: chosen.pixels,
      time: chosen.frame.time_seconds,
      generation: session.generation,
      runtime: JSON.stringify(chosen.runtime),
      error: null,
      presentedFrames: this.view.presentedFrames + 1,
      discardedFrames: this.view.discardedFrames + discarded,
      statistics: this.presentationStatistics(),
    });
    this.pictureReady(this.buffered.at(-1)?.frame.next_time ?? chosen.frame.next_time);
  }
  private recordLag(consumer:PictureTime) {
    if(!this.outputTime||!this.presentedTime)return;
    const lag=Number(compareTime(consumer,this.presentedTime))*1000/(consumer.timebase*this.presentedTime.timebase);
    if(lag>=0&&Number.isFinite(lag)) {
      if(this.lags.length>=4096)this.lags.shift();
      this.lags.push(lag);
    }
  }
  private presentationStatistics() {
    const sorted=[...this.lags].sort((a,b)=>a-b);
    const percentile=(p:number)=>sorted[Math.max(0,Math.ceil(sorted.length*p)-1)]??null;
    return JSON.stringify({observations:sorted.length,lagP95Ms:percentile(.95),lagP99Ms:percentile(.99),maxLagMs:sorted.at(-1)??null,
      missedCachedFrames:this.missedCachedFrames,maxFrameGapMs:this.maxFrameGapMs,skippedIntervals:this.skippedIntervals});
  }
  private audioHasHeadroom(session: AudioPreviewSession) {
    // Sound owns the realtime deadline. Do not start another expensive picture
    // while the native PCM FIFO is nearly empty. Consumption/authorization and
    // exact picture freshness fences are unchanged; subsequent reports retry.
    return !this.outputTime ||
      BigInt(session.buffered_frames) >= 2n * 4096n ||
      BigInt(session.produced_through_sample) >= BigInt(session.total_frames);
  }
  private requestPictures(epoch: number) {
    const session = this.bufferSession;
    if (
      !session ||
      !this.pictureReady ||
      this.filling ||
      this.running ||
      this.buffered.length >= 8 ||
      (!this.cacheReady&&!this.audioHasHeadroom(session))
    )
      return;
    const signal = this.abort.signal;
    this.filling = true;
    // Both adapters run outside the UI thread. Keep the bounded queue supplied
    // without waiting for another control-plane observation after each frame.
    // Per-frame PCM headroom, authority and memory admission still apply.
    void this.serialize(() => this.fillPictures(session, signal, epoch, 8, 8))
      .catch((error) => {
        if (signal.aborted || epoch !== this.epoch) return;
        const e = error instanceof Error ? error : new Error(String(error));
        if ("code" in e && e.code === "PREVIEW_FRAME_STALE") {
          // A late picture is disposable; it is not an audio transport failure.
          this.nextPreparation = this.outputTime ?? this.bufferSession?.presentation_time ?? null;
          return;
        }
        console.debug(
          "Preview picture preparation failed " +
            JSON.stringify({
              message: e.message,
              code: "code" in e ? e.code : null,
            }),
        );
        this.stop();
        this.publish({ phase: "failed", error: e.message });
        this.failure(e);
      })
      .finally(() => {
        this.filling = false;
      });
  }
  private async fillPictures(
    session: AudioPreviewSession,
    signal: AbortSignal,
    epoch: number,
    capacity: number,
    maximumFrames = Infinity,
  ) {
    const api = this.host.api();
    if (!api) throw Error("Preview service disconnected");
    this.renderer ??= this.factory();
    let preparedFrames = 0;
    while (
      preparedFrames < maximumFrames &&
      this.buffered.length < capacity &&
      this.pictureReady &&
      epoch === this.epoch
    ) {
      signal.throwIfAborted();
      if (!this.cacheReady&&!this.audioHasHeadroom(this.bufferSession ?? session)) return;
      const consumer = this.outputTime ?? session.presentation_time;
      // Consumption reports reach Rust after the actual device receipt. Skip
      // work already behind that receipt instead of repeatedly spending the
      // remaining picture credit rendering an obsolete frame.
      let next =
        this.nextPreparation && compareTime(this.nextPreparation, consumer) < 0n
          ? consumer
          : this.nextPreparation;
      // Device credit ends at the last contiguous picture. Jumping to an
      // execution-latency estimate leaves a hole that the stopped device can
      // never cross; continue from the next native CFR boundary instead.
      if (
        next &&
        (BigInt(next.ticks) * BigInt(session.identity.sample_rate) >=
          BigInt(session.total_frames) * BigInt(next.timebase) ||
          compareTime(next, consumer) * 1000n >
            900n * BigInt(next.timebase) * BigInt(consumer.timebase))
      )
        return;
      const retained = this.buffered.reduce(
        (bytes, item) => bytes + pictureBytes(item.pixels),
        this.presentedBytes,
      );
      const frameBytes = this.presentedBytes;
      if (this.buffered.length && retained + frameBytes > 64 * 1024 * 1024)
        return;
      let result: BufferedPicture;
      const began = performance.now();
      try {
        result = this.cacheReady && this.renderer.cachedFrame
          ? await this.renderer.cachedFrame(api,next??consumer,signal)
          : await this.renderer.render(
          api,
          {
            session_id: session.session_id,
            generation: session.generation,
            overlay_epoch: session.overlay_epoch,
            owner_id: session.owner_id,
            lease_id: session.lease_id,
          },
          signal,
          undefined,
          undefined,
          { time: next, reservedBytes: retained },
        );
      } catch (error) {
        if (error instanceof Error && "code" in error && error.code === "PREVIEW_FRAME_STALE") {
          this.publish({ runtime: JSON.stringify({ deferred: error.code, ...("previewMetrics" in error ? error.previewMetrics as object : {}), elapsedMs: performance.now() - began }) });
          // A missed deadline needs more lead, not less. Keep it strictly
          // inside the native one-second preparation admission.
          this.renderLeadMs = Math.min(800, Math.max(this.renderLeadMs, Math.ceil((performance.now() - began) * 1.4 + 30)));
        }
        // Resident pictures share the original frame admission. Treat a full
        // queue as backpressure, then retry within picture credit after consumption.
        // A frame that cannot fit with an empty queue still reports failure.
        if (
          !signal.aborted &&
          epoch === this.epoch &&
          this.buffered.length > 0 &&
          error instanceof Error &&
          "code" in error &&
          error.code === "VISUAL_BUDGET_EXCEEDED"
        )
          return;
        throw error;
      }
      if (
        signal.aborted ||
        epoch !== this.epoch ||
        this.currentToken !== token(session) ||
        api !== this.host.api()
      ) {
        if ("close" in result.pixels) result.pixels.close();
        return;
      }
      if (
        !result.frame.prepare_ahead ||
        !result.frame.next_time ||
        compareTime(result.frame.next_time, result.frame.plan.identity.time) <=
          0n
      ) {
        if ("close" in result.pixels) result.pixels.close();
        throw Error("Invalid sequential picture preparation receipt");
      }
      preparedFrames++;
      // Include worker messaging and UI scheduling, not just execution inside
      // the worker. A single fast result must not erase the measured lead and
      // make the following frame miss its deadline again.
      const elapsed = performance.now() - began;
      this.renderLeadMs = Math.min(800, Math.max(150,
        Math.ceil(elapsed * 1.4 + 30), Math.ceil(this.renderLeadMs * .9)));
      this.nextPreparation = result.frame.next_time;
      this.buffered.push(result);
      // Readiness describes already decoded queued pixels, including pictures
      // whose device-clock presentation time has not arrived yet.
      this.pictureReady(result.frame.next_time);
      this.presentPictures();
    }
  }
  private async render(
    session: AudioPreviewSession,
    signal: AbortSignal,
    epoch: number,
    source=false,
  ) {
    const api = this.host.api();
    if (!api) throw new Error("Preview service disconnected");
    const identity = token(session);
    this.renderer ??= this.factory();
    const result = await (!source&&this.renderer.stillFrame?this.renderer.stillFrame(api,session,signal):this.renderer.render(
      api,
      {
        session_id: session.session_id,
        generation: session.generation,
        overlay_epoch: session.overlay_epoch,
        owner_id: session.owner_id,
        lease_id: session.lease_id,
      },
      signal,
    ));
    const discard = () => {
      if ("close" in result.pixels) result.pixels.close();
    };
    if (signal.aborted) {
      discard();
      signal.throwIfAborted();
    }
    if (
      epoch !== this.epoch ||
      this.currentToken !== identity ||
      api !== this.host.api()
    ) {
      discard();
      return;
    }
    if (this.transformDraftId) {
      discard();
      return;
    }
    const latest = this.host.state()?.audioPreview.session;
    // Compare rational public Times exactly, never divide ticks to evaluate IR.
    if (latest && token(latest) === identity) {
      const c = this.outputTime ?? latest.presentation_time,
        t = result.frame.plan.identity.time;
      // Leave one main-thread presentation quantum inside the 150ms gate.
      const maxLag = this.outputTime ? 125n : 150n;
      const delta =
        BigInt(c.ticks) * BigInt(t.timebase) -
        BigInt(t.ticks) * BigInt(c.timebase);
      if (
        delta < 0n ||
        delta * 1000n > maxLag * BigInt(c.timebase) * BigInt(t.timebase)
      ) {
        discard();
        return;
      }
    }
    this.presentedTime = result.frame.plan.identity.time;
    this.publish({
      phase: "ready",
      editDraftId: null,
      restoringDraftId: null,
      presentedFrames: this.view.presentedFrames + 1,
      target:JSON.stringify([session.identity.project_id,session.identity.revision,session.identity.sequence_id]),
      runtime: JSON.stringify(result.runtime),
      pixels: result.pixels,
      surfaceActive:false,
      time: result.frame.time_seconds,
      generation: session.generation,
      error: null,
    });
    if(!source&&"cacheKey" in result.runtime){
      if(this.refineTimer)clearTimeout(this.refineTimer);
      this.refineTimer=setTimeout(()=>{
        this.refineTimer=null;
        if(signal.aborted||epoch!==this.epoch||this.currentToken!==identity)return;
        void this.serialize(()=>this.render(session,signal,epoch,true)).catch(error=>{
          if(!signal.aborted&&epoch===this.epoch&&this.currentToken===identity){this.publish({phase:"failed",pixels:null,error:error instanceof Error?error.message:String(error)});this.failure(error instanceof Error?error:Error(String(error)));}
        });
      },300);
    }
  }
}
import type {DeviceTimestamp} from "./audio-preview/presentation-clock";
