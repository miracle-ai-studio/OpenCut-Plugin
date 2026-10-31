import { DEVICE_BUFFER_BLOCKS, DEVICE_PRIME_BLOCKS } from "./audio-preview/buffer-limits";
import { OpenCutApiError } from "../../../lib/editor-sdk";
import type { EditorApi } from "../api";
import type { Time } from "../../model/types";
import type { SessionHost } from "./session";
import type {DeviceTimestamp,PresentationClockSource} from "./audio-preview/presentation-clock";
import {pictureConsumptionLimit} from "./audio-preview/presentation-clock";
import type {
  AudioPreviewControl,
  AudioPreviewCreate,
  AudioPreviewSession,
  AudioPreviewState,
  GainDraft,
} from "./types";
import { PreviewAudioOutput } from "./audio-preview/output";
import type { AudioOutputProgress } from "./audio-preview/output";
import { PreviewConsumptionPump } from "./audio-preview/pump";
import type { PreviewPump } from "./audio-preview/pump";
import { checkPreviewSession } from "./audio-preview/session-identity";
import { PreviewRecovery } from "./audio-preview/recovery";
import type { PreparedPreviewOwner } from "./still-preview";
import {compareTime} from "../../model/time";
import {sequenceContentEnd} from "./sequence-navigation";

export const emptyAudioPreview = (): AudioPreviewState => ({
  phase: "idle",
  capability: null,
  session: null,
  request: null,
  error: null,
});
type Output = Pick<
  PreviewAudioOutput,
  "ready" | "sampleRate" | "push" | "waitForCredit" | "silence" | "stop"
> &
  Partial<Pick<PreviewAudioOutput, "finish" | "createPump" | "limitPicture" | "clockDiagnostics" | "bindPresentationClock">>;
type OutputFactory = (
  token: string,
  rate: number,
  progress: (value: AudioOutputProgress) => void,
  failure: (error: Error) => void,
  completedBlock: () => void,
  primeBlocks: number,
) => Output;
export type PicturePresentation = {
  takePaused?(api:EditorApi,request:AudioPreviewCreate):PreparedPreviewOwner|null;
  beforeCreate?():Promise<void>;
  hasPausedPicture?(session: AudioPreviewSession): boolean;
  prepare(session: AudioPreviewSession, signal: AbortSignal, pictureReady?: (time: Time) => void,clockSource?:PresentationClockSource): Promise<void>;
  observe(state: AudioPreviewState): void;
  consume?(session: AudioPreviewSession, frames: string, atRefresh?: boolean,deviceTimestamp?:DeviceTimestamp): void;
  timestamp?(value:DeviceTimestamp): void;
  suspend(retainPicture?:boolean): void;
  close(): void;
};
type Active = {
  recovery: PreviewRecovery;
  picture?: PicturePresentation;
  api: EditorApi;
  epoch: number;
  generation: number;
  request: AudioPreviewCreate;
  session: AudioPreviewSession | null;
  output: Output;
  abort: AbortController;
  controls: AbortController;
  progress: AudioOutputProgress;
  draining: boolean;
  seekTarget: Time | null;
  gainPending: boolean;
  transitionTask: Promise<void | { error: unknown }> | null;
  pauseAfterTransition: boolean;
  transitionPreparation: AbortController | null;
  pump: PreviewPump | null;
  preparing: boolean;
  streamStarted: boolean;
  leaseDeadline: number;
  documentDeparted: boolean;
  stopRequest: AudioPreviewControl | null;
};
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Fixed-revision playback delegate. Project evaluation, overlays and sample→Time
 * belong to the service; this client only reports actual device consumption. */
export class AudioPreview {
  private readonly recovery = new PreviewRecovery();
  private retirements=new Set<Promise<void>>();
  settled(){return Promise.all([...this.retirements]).then(()=>{});}
  private epoch = 0;
  private capabilityEpoch = 0;
  private active: Active | null = null;
  private pausedHeartbeat: ReturnType<typeof setInterval> | null = null;
  private pending: {
    recovery: PreviewRecovery;
    api: EditorApi;
    request: AudioPreviewCreate;
    control: AudioPreviewControl | null;
    session: AudioPreviewSession | null;
    picture?: PicturePresentation;
  } | null = null;
  private desired: GainDraft | null = null;
  private overlaySequence = 0n;
  private presentation:Time|null=null;
  private presentationListeners=new Set<()=>void>();
  private retiredDiagnostics:Record<string,string>={};
  subscribePresentation=(listener:()=>void)=>{
    this.presentationListeners.add(listener);
    return()=>{this.presentationListeners.delete(listener);};
  };
  /** One device-derived cursor for lightweight presentation and user actions. */
  currentTime():Time|null {
    const a=this.active;
    return a&&this.same(a)&&a.progress.output?this.presentation:null;
  }
  presentationDiagnostics() {
    const a=this.active,s=a?.session;
    if(!a)return this.retiredDiagnostics;
    if(!s)return {sessionId:"",bufferedFrames:"",consumedFrames:"",producedThroughSample:"",outputFrames:"",outputClockSource:"",underflowEvents:"",underflowFrames:""};
    return {sessionId:s.session_id,bufferedFrames:s.buffered_frames,consumedFrames:s.consumed_frames,
      producedThroughSample:s.produced_through_sample,
      outputFrames:a.progress.output?.frames??s.output?.frames??"",
      outputClockSource:a.progress.output?.source??s.output?.source??"",
      underflowEvents:String(a.progress.underflowEvents??""),underflowFrames:a.progress.underflowFrames??"",...a.output.clockDiagnostics?.()};
  }
  private notifyPresentation() {
    for(const listener of this.presentationListeners)listener();
  }
  private recordPresentation(time:Time) {
    this.presentation=time;
    this.notifyPresentation();
  }
  private present(time:Time) {
    this.publishPlayhead(time);
    this.recordPresentation(time);
  }
  constructor(
    private readonly host: SessionHost,
    private readonly publishPlayhead: (time: Time) => void,
    private readonly outputFactory: OutputFactory = (
      token,
      rate,
      progress,
      failure,
      completedBlock,
      primeBlocks,
    ) => new PreviewAudioOutput(token, rate, progress, failure, completedBlock, primeBlocks),
    private readonly beforeCreate:()=>Promise<void>=async()=>{},
  ) {}
  private publish(patch: Partial<AudioPreviewState>) {
    const state = this.host.state();
    if (state) {
      const next = {
        ...state,
        audioPreview: { ...state.audioPreview, ...patch },
      };
      this.host.publish(next);
      this.active?.picture?.observe(next.audioPreview);
      this.notifyPresentation();
    }
  }
  private same(a: Active) {
    const s = this.host.state(),
      snapshot = s && (s.historical ?? s.head);
    return (
      this.active === a &&
      a.epoch === this.epoch &&
      a.api === this.host.api() &&
      a.generation === this.host.generation() &&
      snapshot?.project.id === a.request.project_id &&
      snapshot.revision === a.request.revision &&
      s?.sequenceId === a.request.sequence_id
    );
  }
  private pictureCredit(a: Active, session: AudioPreviewSession) {
    // Validated native pictures allow at most 100ms of audio lead. A persistent
    // picture stall still fences consumption; the final picture fences EOF.
    a.output.limitPicture?.("0");
    let through = 0n;
    return (time: Time) => {
      if (!this.same(a) || a.session?.generation !== session.generation) return;
      const end = pictureConsumptionLimit(time,session);
      if (end > through) { through = end; a.output.limitPicture?.(String(end)); }
    };
  }
  private pictureClock(a:Active):PresentationClockSource|undefined {
    const output=a.output;
    return output.bindPresentationClock?{bind:port=>output.bindPresentationClock!(port)}:undefined;
  }
  private outputObserved(a: Active, output: Output, progress: AudioOutputProgress) {
    if (!this.same(a) || a.output !== output) return;
    a.progress = progress;
    if(progress.underflowEvents!==undefined&&(this.host.state()?.audioPreview.deviceUnderflowEvents!==progress.underflowEvents||this.host.state()?.audioPreview.deviceUnderflowFrames!==progress.underflowFrames))
      this.publish({deviceUnderflowEvents:progress.underflowEvents,deviceUnderflowFrames:progress.underflowFrames});
    a.pump?.observe(progress);
    if (a.session?.state === "playing" && progress.deviceTimestamp) a.picture?.timestamp?.(progress.deviceTimestamp);
    if (!a.session || !progress.output) return;
    const frames = progress.output.frames;
    a.picture?.consume?.(a.session, frames, progress.atRefresh,progress.deviceTimestamp);
    this.recordPresentation({ ticks: (BigInt(a.session.actual_start_sample) + BigInt(frames)).toString(),
      timebase: a.session.identity.sample_rate });
  }
  private continueWithoutPicture(error: unknown): boolean {
    const a = this.active;
    if (!a || !this.same(a) || !(error instanceof Error) || !("code" in error) ||
      !["VISUAL_BUDGET_EXCEEDED", "VISUAL_GPU_FAILED", "CAPABILITY_UNAVAILABLE", "UNSUPPORTED_RENDER_FEATURE"].includes(String(error.code))) return false;
    a.picture?.close();
    a.picture = undefined;
    if (a.session) a.output.limitPicture?.(a.session.total_frames);
    this.publish({pictureError: message(error)});
    return true;
  }
  pictureFailed(error: Error) {
    if (!this.continueWithoutPicture(error)) {
      this.publish({pictureError:message(error)});
      void this.pause().catch(() => {});
    }
  }
  async capability() {
    const api = this.host.api(),
      generation = this.host.generation(),
      epoch = ++this.capabilityEpoch;
    if (!api) return;
    const project = this.host.state()?.head.project.id;
    if (project) {
      try {
        await this.recovery.restore(api, project);
      } catch (error) {
        if (
          api === this.host.api() &&
          generation === this.host.generation() &&
          epoch === this.capabilityEpoch
        )
          this.publish({ phase: "unconfirmed", error: message(error) });
        return;
      }
    }
    try {
      const capability = await api.scene.audioPreviewCapability();
      if (
        api !== this.host.api() ||
        generation !== this.host.generation() ||
        epoch !== this.capabilityEpoch
      )
        return;
      // Runtime admission belongs to the service, not a playback generation.
      // Seek/pause may retire a session while this request is in flight. Its
      // result must neither disappear nor clear a newer playback failure.
      this.publish({ capability });
    } catch {
      if (
        api === this.host.api() &&
        generation === this.host.generation() &&
        epoch === this.capabilityEpoch
      )
        this.publish({ capability: null });
    }
  }
  private checkSession(
    a: Active,
    s: AudioPreviewSession,
    previous?: AudioPreviewSession,
  ) {
    checkPreviewSession(s, a.request, a.output.sampleRate, previous);
    // A local admission hint only; every Play still rechecks server authority.
    a.leaseDeadline =
      performance.now() + Math.min(30000, Number(s.lease_remaining_ms));
  }
  /** Called by the actual Play event, before the first asynchronous operation. */
  play(sampleRate?: number, picture?: PicturePresentation) {
    const api = this.host.api(),
      state = this.host.state();
    if (!api || !state)
      return Promise.reject(new Error("Open an editor project first."));
    if (this.pending)
      return Promise.reject(
        new Error("Retry the original audio session before starting another."),
      );
    if (
      this.active &&
      this.same(this.active) &&
      ["loading", "playing", "stalled"].includes(state.audioPreview.phase)
    )
      return Promise.resolve();
    const capability = state.audioPreview.capability;
    const rememberedRate = state.audioPreview.request?.sample_rate;
    const rate =
      sampleRate ??
      (rememberedRate && capability?.sample_rates.includes(rememberedRate)
        ? rememberedRate
        : [48000, 44100].find(r => capability?.sample_rates.includes(r)));
    if (
      !capability?.available ||

      capability.profile !== "classic-realtime-v31" ||
      capability.max_block_frames !== 4096 ||
      !Number.isSafeInteger(capability.max_buffered_frames) ||
      capability.max_buffered_frames < 4096 * 2 ||
      capability.max_buffered_frames > 4096 * DEVICE_BUFFER_BLOCKS ||
      capability.max_buffered_frames % 4096 !== 0 ||
      !rate ||
      ![44100, 48000].includes(rate) ||
      !capability.sample_rates.includes(rate)
    )
      return Promise.reject(
        new Error(
          capability?.unavailable_reason ??
            "Live scene audio is unavailable on this service.",
        ),
      );
    const snapshot = state.historical ?? state.head;
    const paused = this.active;
    if (
      paused &&
      this.same(paused) &&
      state.audioPreview.phase === "paused" &&
      paused.session?.state === "paused" &&
      paused.session.identity.sample_rate === rate &&
      !paused.controls.signal.aborted &&
      paused.leaseDeadline > performance.now()
    )
      return this.begin(
        api,
        paused.request,
        null,
        paused.session,
        picture ?? paused.picture,
        paused.recovery,
      );
    const sequence=snapshot.project.sequences.find(sequence=>sequence.id===state.sequenceId);
    if(!sequence)return Promise.reject(Error("Preview scene is unavailable"));
    const ended=state.audioPreview.phase==="ended"?state.audioPreview.session:null;
    const atConfirmedEnd=ended?.identity.revision===snapshot.revision&&ended.identity.sequence_id===state.sequenceId&&
      BigInt(state.playhead.ticks)*BigInt(ended.identity.sample_rate)>=BigInt(ended.total_frames)*BigInt(state.playhead.timebase);
    const request: AudioPreviewCreate = {
      project_id: snapshot.project.id,
      revision: snapshot.revision,
      sequence_id: state.sequenceId,
      start_time: atConfirmedEnd||compareTime(state.playhead,sequenceContentEnd(sequence))>=0
        ? { ticks: "0", timebase: state.playhead.timebase } : state.playhead,
      sample_rate: rate,
      idempotency_key: crypto.randomUUID(),
    };
    this.invalidate(false);
    return this.begin(api, request, null, null, picture);
  }
  retry() {
    const pending = this.pending,
      s = this.host.state(),
      api = this.host.api();
    if (!pending && api && s) return this.capability();
    if (
      !pending ||
      !s ||
      api !== pending.api ||
      (s.historical ?? s.head).revision !== pending.request.revision ||
      s.head.project.id !== pending.request.project_id ||
      s.sequenceId !== pending.request.sequence_id
    )
      return Promise.reject(
        new Error(
          "Return to the original scene and revision to recover its audio session.",
        ),
      );
    this.invalidate(false);
    return this.begin(
      api,
      pending.request,
      pending.control,
      pending.session,
      pending.picture,
      pending.recovery,
    );
  }
  private begin(
    api: EditorApi,
    request: AudioPreviewCreate,
    originalControl: AudioPreviewControl | null = null,
    preparedSession: AudioPreviewSession | null = null,
    picture?: PicturePresentation,
    recovery: PreviewRecovery = this.recovery,
  ) {
    this.stopPausedHeartbeat();
    const epoch = this.epoch;
    let a: Active;
    let output: Output;
    try {
      output = this.outputFactory(
        request.idempotency_key + ":" + epoch,
        request.sample_rate,
        (progress) => {
          this.outputObserved(a, output, progress);
        },
        (error) => {
          if (this.same(a) && a.output === output) this.fail(a, error);
        },
        () => this.completedBlock(a, output),
        Math.min(DEVICE_PRIME_BLOCKS, (this.host.state()?.audioPreview.capability?.max_buffered_frames ?? 32768) / 4096),
      );
    } catch (error) {
      this.publish({
        phase: "failed",
        request,
        session: null,
        error: message(error),
      });
      return Promise.reject(error);
    }
    // AudioContext construction still occurs in the user gesture. Transfer
    // only after it succeeds, so a failed output cannot orphan the still owner.
    const adopted = !preparedSession && !originalControl ? picture?.takePaused?.(api, request) : null;
    if (adopted) {
      request = adopted.request;
      preparedSession = adopted.session;
      recovery = adopted.recovery;
    }
    a = {
      recovery,
      picture,
      api,
      epoch,
      generation: this.host.generation(),
      request,
      session: preparedSession,
      output,
      abort: new AbortController(),
      progress: { consumedFrames: "0", stalled: true, contextTime: 0 },
      controls: new AbortController(),
      draining: false,
      seekTarget: null,
      gainPending: false,
      transitionTask: null,
      pauseAfterTransition: false,
      transitionPreparation: null,
      pump: null,
      preparing: true,
      streamStarted: false,
      leaseDeadline: 0,
      documentDeparted: false,
      stopRequest: null,
    };
    this.active = a;
    this.publish({ phase: "loading", request, session: null, error: null, pictureError: null });
    return this.create(a, originalControl, preparedSession);
  }
  private async create(
    a: Active,
    originalControl: AudioPreviewControl | null,
    preparedSession: AudioPreviewSession | null,
  ) {
    let control = originalControl,
      dispatched = false;
    try {
      await a.output.ready;
      if (!this.same(a)) return;
      await this.attachPump(a);
      if (!this.same(a)) return;
      let session = preparedSession;
      if (!session) {
        await (a.picture?.beforeCreate??this.beforeCreate)();
        if(!this.same(a))return;
        dispatched = true;
        session = await a.recovery.create(
          a.api,
          a.request,
          a.controls.signal,
          () => this.same(a),
        );
      }
      this.checkSession(a, session);
      a.session = session;
      if (!this.same(a)) {
        this.stopRemote(a);
        return;
      }
      // A replayed session may have advanced: never play stale producer state.
      if (originalControl) {
        dispatched = true;
        const result = await a.recovery.control(
          a.api,
          a.request,
          originalControl,
          a.controls.signal,
          () => this.same(a),
        );
        this.checkSession(a, result, session);
        a.session = result;
      }
      if (!this.same(a)) return;
      if (a.session.state === "stopped" || a.session.state === "failed")
        throw new Error(
          "The original scene audio session has stopped. Start a new session.",
        );
      if (!originalControl) {
        control = this.command(a, { kind: "play" });
        dispatched = true;
        a.session = await a.recovery.control(
          a.api,
          a.request,
          control,
          a.controls.signal,
          () => this.same(a),
        );
      }
      this.checkSession(a, a.session, session);
      if (!this.same(a)) {
        this.stopRemote(a);
        return;
      }
      // Picture preparation happens with an already-resumed AudioContext,
      // before any PCM stream starts. Local execution failure is known.
      dispatched = false;
      const stopPreparationHeartbeat=this.keepPreparing(a);
      try {
      if (a.picture)
        await a.picture.prepare(
          a.session,
          AbortSignal.any([a.controls.signal, AbortSignal.timeout(1800000)]),
          this.pictureCredit(a, a.session),
          this.pictureClock(a),
        );
      } catch (error) {
        if (!(this.hasTransition(a) && error instanceof Error && error.name === "AbortError") && !this.continueWithoutPicture(error)) throw error;
      } finally {
        stopPreparationHeartbeat();
      }
      if (!this.same(a)) return;
      this.pending = null;
      a.preparing = false;
      this.present(a.session.presentation_time);
      this.publish({ session: a.session, phase: "loading", error: null });
      if (this.desired) a.gainPending = true;
      if (this.hasTransition(a)) {
        const result = await this.queueTransition(a);
        if (result) throw result.error;
        return;
      }
      this.stream(a);
    } catch (e) {
      if (!this.same(a)) return;
      if (!dispatched) this.stopRemote(a);
      const unknown =
        !!originalControl || (dispatched && !(e instanceof OpenCutApiError));
      this.pending = unknown
        ? {
            recovery: a.recovery,
            api: a.api,
            request: a.request,
            control,
            session: a.session ?? preparedSession,
            picture: a.picture,
          }
        : null;
      this.fail(a, e, unknown);
      throw e;
    }
  }
  private command(
    a: Active,
    command: AudioPreviewControl["command"],
  ): AudioPreviewControl {
    const s = a.session!;
    return {
      session_id: s.session_id,
      generation: s.generation,
      owner_id: s.owner_id,
      lease_id: s.lease_id,
      control_id: crypto.randomUUID(),
      command,
    };
  }
  private async attachPump(a: Active) {
    const output = a.output;
    const updated = (
      session: AudioPreviewSession,
      _progress: AudioOutputProgress,
    ) => {
      if (!this.same(a) || a.output !== output) return;
      this.checkSession(a, session, a.session ?? undefined);
      a.session = session;
      if (!a.progress.output) this.present(session.presentation_time);
      if (a.draining) return;
      const phase =
        session.state === "ended"
          ? "ended"
          : !a.progress.output ? "loading" : a.progress.stalled
            ? "stalled"
            : "playing";
      if(phase==="ended")this.present(session.presentation_time);
      const shown=this.host.state()!.audioPreview;
      if(phase!==shown.phase||phase==="ended"||shown.session?.generation!==session.generation||shown.session?.overlay_epoch!==session.overlay_epoch) {
        this.publish({session,phase});
      } else {
        a.picture?.observe({...shown,session,phase});
      }
      if (phase === "ended") this.invalidate(false);
    };
    const failure = (error: unknown) => {
      if (this.same(a) && a.output === output) this.fail(a, error);
    };
    const pump = output.createPump
      ? await output.createPump(a.api.audioPreviewConnection, updated, failure)
      : new PreviewConsumptionPump(
          {
            open: (session, signal) =>
              a.api.sdk.openAudioPreviewStream(session, { signal }),
            control: (input, signal) =>
              a.api.scene.audioPreviewControl(input, signal),
          },
          output,
          () => this.same(a) && a.output === output,
          updated,
          failure,
        );
    if (!this.same(a) || a.output !== output) {
      pump.abort();
      return;
    }
    a.pump = pump;
    const abort = () => {
      pump.abort();
      a.controls.signal.removeEventListener("abort", abort);
    };
    a.abort.signal.addEventListener("abort", abort, { once: true });
    a.controls.signal.addEventListener("abort", abort, { once: true });
  }
  private stream(a: Active) {
    a.pump!.start(a.session!);
    a.streamStarted = true;
  }
  private completedBlock(a: Active, output: Output) {
    if (this.same(a) && a.output === output && !a.draining) a.pump?.completed();
  }
  private async report(a: Active, frames: string) {
    const previous = a.session!;
    const session = await a.pump!.reportStopped(frames);
    if (!this.same(a)) return;
    if (!session)
      throw new Error("Scene audio final consumption was not confirmed.");
    this.checkSession(a, session, previous);
    if (
      session.generation !== previous.generation ||
      session.overlay_epoch !== previous.overlay_epoch
    )
      return;
    a.session = session;
    this.present(session.presentation_time);
    return session;
  }
  private stopRemote(a: Active) {
    const s = a.session,
      old = a.stopRequest;
    if (
      s &&
      !(
        old &&
        old.session_id === s.session_id &&
        old.generation === s.generation &&
        old.owner_id === s.owner_id &&
        old.lease_id === s.lease_id
      )
    ) {
      const request = this.command(a, { kind: "stop" });
      a.stopRequest = request;
      const retired=a.recovery
        .stop(a.api, a.request, request, a.documentDeparted)
        .catch(() => {});
      this.retirements.add(retired);void retired.finally(()=>this.retirements.delete(retired));
    }
  }
  private localStop(a: Active) {
    a.picture?.suspend();
    a.output.silence();
    a.pump?.freeze();
  }
  private stopPausedHeartbeat() {
    if (this.pausedHeartbeat) clearInterval(this.pausedHeartbeat);
    this.pausedHeartbeat = null;
  }
  private keepPaused(a: Active) {
    this.stopPausedHeartbeat();
    let reporting = false;
    this.pausedHeartbeat = setInterval(() => {
      const previous = a.session;
      if (!this.same(a) || previous?.state !== "paused" || this.host.state()?.audioPreview.phase !== "paused") {
        this.stopPausedHeartbeat();
        return;
      }
      if (reporting) return;
      reporting = true;
      void a.api.scene.audioPreviewControl(this.command(a, { kind: "heartbeat" }),
        AbortSignal.any([a.controls.signal, AbortSignal.timeout(3000)]))
        .then(session => {
          if (!this.same(a) || a.session?.generation !== previous.generation || this.host.state()?.audioPreview.phase !== "paused") return;
          this.checkSession(a, session, previous);
          if (session.state !== "paused" || session.generation !== previous.generation ||
            session.overlay_epoch !== previous.overlay_epoch || session.actual_start_sample !== previous.actual_start_sample ||
            session.consumed_frames !== previous.consumed_frames ||
            BigInt(session.presentation_time.ticks) * BigInt(previous.presentation_time.timebase) !==
              BigInt(previous.presentation_time.ticks) * BigInt(session.presentation_time.timebase))
            throw new Error("Paused scene audio changed state.");
          a.session = session;
          this.publish({ session, phase: "paused" });
        })
        .catch(error => {
          if (this.same(a) && a.session?.generation === previous.generation && this.host.state()?.audioPreview.phase === "paused") this.fail(a, error);
        })
        .finally(() => { reporting = false; });
    }, 5000);
  }
  /** Native render jobs may outlive a short owner lease. Renew only this
   * unchanged unstarted generation; preparation never fabricates output. */
  private keepPreparing(a:Active):()=>void {
    let pending=false,closed=false;
    const timer=setInterval(()=>{
      const previous=a.session;
      if(closed||!this.same(a)||!previous||a.streamStarted)return;
      if(pending)return;pending=true;
      void a.api.scene.audioPreviewControl(this.command(a,{kind:"heartbeat"}),
        AbortSignal.any([a.controls.signal,AbortSignal.timeout(5000)]))
        .then(session=>{
          if(closed||!this.same(a)||a.session?.generation!==previous.generation||a.streamStarted)return;
          this.checkSession(a,session,previous);
          if(session.consumed_frames!==previous.consumed_frames||session.actual_start_sample!==previous.actual_start_sample||session.overlay_epoch!==previous.overlay_epoch)
            throw Error("Unstarted preview changed its output anchor");
          a.session=session;
        }).catch(error=>{if(!closed&&this.same(a))this.fail(a,error);})
        .finally(()=>{pending=false;});
    },5000);
    return ()=>{closed=true;clearInterval(timer);};
  }
  /** Discard this document's sound, including BFCache departure. No fabricated
   * final cursor: Stop uses only the last validated owner/lease/generation. */
  retireDocument() {
    this.invalidate(false, true);
    // BFCache can restore this same object: unknown admission/control must
    // retain its original request rather than create another session.
    this.publish(
      this.pending
        ? {
            phase: "unconfirmed",
            session: this.pending.session,
            request: this.pending.request,
          }
        : { phase: "paused", session: null, request: null, error: null },
    );
  }
  invalidate(clearPending = true, documentDeparted = false) {
    this.stopPausedHeartbeat();
    this.epoch++;
    const a = this.active;
    if(a)this.retiredDiagnostics=this.presentationDiagnostics();
    this.active = null;
    this.desired = null;
    if (clearPending) this.pending = null;
    if (a) {
      a.documentDeparted = documentDeparted;
      a.picture?.close();
      this.localStop(a);
      a.abort.abort();
      a.controls.abort();
      void a.output.stop().catch(() => {});
      this.stopRemote(a);
      void a.recovery
        .retire(a.api, a.request, documentDeparted)
        .catch(() => {});
    }
  }
  private fail(a: Active, error: unknown, unknown = false) {
    this.stopPausedHeartbeat();
    console.debug("Scene audio preview failed " + JSON.stringify({ message: message(error), code: error instanceof OpenCutApiError ? error.code : null, stack: error instanceof Error ? error.stack : null, cause:error instanceof Error?error.cause:undefined }));
    this.localStop(a);
    a.abort.abort();
    a.controls.abort();
    if (!unknown) this.stopRemote(a);
    void a.output.stop().catch(() => {});
    if (this.active === a) {
      this.retiredDiagnostics=this.presentationDiagnostics();
      a.picture?.close();
      this.active = null;
      this.epoch++;
    }
    this.publish({
      phase: unknown ? "unconfirmed" : "failed",
      session: a.session,
      error: message(error),
    });
  }
  async pause() {
    const a = this.active;
    if (!a || !this.same(a) || !a.session) {
      this.invalidate();
      this.publish({ phase: "paused" });
      return;
    }
    a.pauseAfterTransition = true;
    const result = await this.queueTransition(a);
    if (result) throw result.error;
  }
  private queueTransition(a: Active) {
    this.stopPausedHeartbeat();
    a.draining = true;
    // Suspending the picture cancels its in-flight initial preparation. No
    // device has been handed off yet, so retain the intent without aborting it.
    if (a.preparing) {
      a.picture?.suspend(a.pauseAfterTransition&&!a.seekTarget&&!a.gainPending);
      this.publish({ phase: "loading" });
      return;
    }
    a.pump?.freeze();
    a.transitionPreparation?.abort();
    this.publish({ phase: "loading" });
    if (!a.transitionTask) {
      const task = this.transition(a);
      a.transitionTask = task;
      void task.finally(() => {
        if (a.transitionTask === task) a.transitionTask = null;
      });
    }
    return a.transitionTask;
  }
  seek(time?: Time) {
    const a = this.active;
    // Pending controls and expired lease observations still need the original
    // recovery path, even when the requested paused position is unchanged.
    if (time && a?.session && this.same(a) &&
      this.host.state()?.audioPreview.phase === "paused" && a.session.state === "paused" &&
      !a.preparing && !a.transitionTask && !a.seekTarget && !a.gainPending && !a.pauseAfterTransition &&
      a.leaseDeadline > performance.now() &&
      (!a.picture || a.picture.hasPausedPicture?.(a.session) === true) &&
      compareTime(time, a.session.presentation_time) === 0) return true;
    if (
      time &&
      a?.session &&
      this.same(a) &&
      (a.preparing ||
        a.transitionTask ||
        (a.session.state === "paused" &&
          this.host.state()?.audioPreview.phase === "paused") ||
        (!a.draining &&
          ["playing", "stalled", "loading"].includes(
            this.host.state()?.audioPreview.phase ?? "",
          )))
    ) {
      a.seekTarget = time;
      void this.queueTransition(a);
      return true;
    }
    this.invalidate();
    this.publish({ phase: "paused", session: null, request: null });
    return false;
  }
  revisionChanged() {
    // A new head is not evidence that an unknown gain transaction succeeded.
    // Keep the captured base/overlay receipt identity, but stop its old sound.
    this.invalidate(false);
    this.publish({ phase: "paused" });
  }
  private async transition(a: Active) {
    let unconfirmed: {
      request: AudioPreviewControl;
      previous: AudioPreviewSession;
    } | null = null;
    try {
      const frames = await a.output.stop();
      if (!this.same(a)) return;
      a.picture?.suspend(a.pauseAfterTransition&&!a.seekTarget&&!a.gainPending);
      // Paused consumption is already confirmed; its retired pump cannot report
      // a new generation, and changing an overlay must not restart the device.
      if (a.streamStarted && a.session!.state !== "paused")
        await this.report(a, frames);
      else if (!a.streamStarted && frames !== "0")
        throw new Error("Unstarted scene preparation consumed unexpected sound.");
      if (!this.same(a)) return;
      // One latest target, one control in flight. Adopt every accepted identity
      // before the next command, but never prepare/publish a superseded target.
      while (this.same(a) && this.hasTransition(a)) {
        const time = a.seekTarget;
        a.seekTarget = null;
        const previous = a.session!;
        let command: AudioPreviewControl["command"];
        if (time) command = { kind: "seek", sequence_time: time };
        else if (a.gainPending) {
          a.gainPending = false;
          const draft = this.desired;
          command = draft
            ? {
                kind: "set_temporary_gain",
                gain: {
                  track_id: draft.trackId,
                  element_id: draft.elementId,
                  decibels_tenths: draft.tenths,
                  gesture_id: draft.id,
                  update_sequence: (++this.overlaySequence).toString(),
                },
              }
            : previous.temporary_gain
              ? {
                  kind: "clear_temporary_gain",
                  gesture_id: previous.temporary_gain.gesture_id,
                }
              : // Cancelled before dispatch: reanchor the confirmed final cursor
                // only if playing, because that old output has already closed.
                previous.state === "paused"
                ? { kind: "pause" }
                : { kind: "seek", sequence_time: previous.presentation_time };
        } else {
          a.pauseAfterTransition = false;
          command = { kind: "pause" };
        }
        const request = this.command(a, command);
        unconfirmed = { request, previous };
        const result = await a.recovery.control(
          a.api,
          a.request,
          request,
          a.controls.signal,
          () => this.same(a),
        );
        this.checkSession(a, result, previous);
        a.session = result;
        unconfirmed = null;
        if (!this.same(a)) {
          this.stopRemote(a);
          return;
        }
        if (
          a.seekTarget ||
          a.gainPending ||
          (a.pauseAfterTransition && result.state !== "paused")
        )
          continue;
        if (result.state === "paused") {
          a.pauseAfterTransition = false;
          a.abort.abort();
          this.present(result.presentation_time);
          this.publish({ session: result, phase: "paused" });
          this.keepPaused(a);
          return;
        }
        const started = await this.replaceOutput(a, result, true);
        if (!this.same(a)) return;
        if (!this.hasTransition(a)) return;
        this.localStop(a);
        a.draining = true;
        const finalFrames = await a.output.stop();
        if (!this.same(a)) return;
        if (started) await this.report(a, finalFrames);
        else if (finalFrames !== "0")
          throw new Error(
            "Superseded scene preparation consumed unexpected sound.",
          );
        a.abort.abort();
      }
    } catch (e) {
      if (this.same(a)) this.fail(a, e);
      if (unconfirmed && !(e instanceof OpenCutApiError))
        void this.retireUnconfirmedControl(
          a,
          unconfirmed.request,
          unconfirmed.previous,
        );
      // Detached seek/gain callers observe the published failure. Pause also
      // preserves its existing rejected-promise contract for the UI caller.
      return { error: e };
    }
  }
  private async retireUnconfirmedControl(
    a: Active,
    request: AudioPreviewControl,
    previous: AudioPreviewSession,
  ) {
    try {
      // Leaving a view can abort a response after the service has reanchored.
      // The original request/key recovers that receipt without adopting a new
      // owner's generation or reviving this retired document's sound/picture.
      const result = await a.api.scene.audioPreviewControl(
        request,
        AbortSignal.timeout(4000),
        a.documentDeparted ? { keepalive: true } : {},
      );
      this.checkSession(a, result, previous);
      a.session = result;
      this.stopRemote(a);
    } catch {
      // Revocation, owner/lease change or unavailable transport cannot be
      // bypassed; the original native authorization/lease fence still applies.
    }
  }
  private hasTransition(a: Active) {
    return !!(a.seekTarget || a.gainPending || a.pauseAfterTransition);
  }
  private async replaceOutput(
    a: Active,
    session: AudioPreviewSession,
    transitioning = false,
  ) {
    const superseded = () => transitioning && this.hasTransition(a);
    a.abort.abort();
    a.pump = null;
    a.streamStarted = false;
    a.session = session;
    a.abort = new AbortController();
    a.progress = { consumedFrames: "0", stalled: true, contextTime: 0 };
    const output = this.outputFactory(
      session.session_id +
        ":" +
        session.generation +
        ":" +
        session.overlay_epoch,
      session.identity.sample_rate,
      (p) => {
        this.outputObserved(a, output, p);
      },
      (e) => {
        if (this.same(a) && a.output === output) this.fail(a, e);
      },
      () => this.completedBlock(a, output),
      Math.min(DEVICE_PRIME_BLOCKS, (this.host.state()?.audioPreview.capability?.max_buffered_frames ?? 32768) / 4096),
    );
    a.output = output;
    await output.ready;
    if (!this.same(a) || superseded()) return false;
    await this.attachPump(a);
    if (!this.same(a) || superseded()) return false;
    if (a.picture) {
      const preparation = new AbortController();
      const stopPreparationHeartbeat=this.keepPreparing(a);
      if (transitioning) a.transitionPreparation = preparation;
      try {
        await a.picture.prepare(
          session,
          AbortSignal.any([
            a.controls.signal,
            preparation.signal,
            AbortSignal.timeout(1800000),
          ]),
          this.pictureCredit(a, session),
          this.pictureClock(a),
        );
      } catch (error) {
        if (superseded() && preparation.signal.aborted) return false;
        if (!this.continueWithoutPicture(error)) throw error;
      } finally {
        stopPreparationHeartbeat();
        if (a.transitionPreparation === preparation)
          a.transitionPreparation = null;
      }
    }
    if (!this.same(a) || superseded()) return false;
    a.draining = false;
    this.present(session.presentation_time);
    this.publish({ session, phase: "loading" });
    this.stream(a);
    return true;
  }
  draft(draft: GainDraft | null, preserve = false) {
    if (preserve) return;
    if (!draft && !this.desired && !this.active?.session?.temporary_gain)
      return;
    this.desired = draft;
    const a = this.active;
    if (
      draft &&
      a &&
      (draft.projectId !== a.request.project_id ||
        draft.revision !== a.request.revision ||
        draft.sequenceId !== a.request.sequence_id)
    ) {
      this.desired = null;
      return;
    }
    if (!a || !this.same(a) || !a.session) return;
    a.gainPending = true;
    void this.queueTransition(a);
  }
}
