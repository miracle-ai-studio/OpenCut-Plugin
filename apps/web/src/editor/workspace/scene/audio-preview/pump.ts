import type { AudioPreviewControl, AudioPreviewSession } from "../types";
import type { AudioOutputProgress } from "./output";
import { readPreviewPcm } from "./transport";
import type { OutputObservation } from "./output-clock";

export type PumpTransport = {
  open(session: AudioPreviewSession, signal: AbortSignal): Promise<Response>;
  control(
    input: AudioPreviewControl,
    signal: AbortSignal,
  ): Promise<AudioPreviewSession>;
};
export type PumpOutput = {
  waitForCredit(signal: AbortSignal): Promise<void>;
  push(pcm: Float32Array, signal: AbortSignal): Promise<void>;
  finish?(): void;
};
export interface PreviewPump {
  start(session: AudioPreviewSession): void;
  observe(progress: AudioOutputProgress): void;
  completed(): void;
  freeze(): void;
  reportStopped(frames: string): Promise<AudioPreviewSession | undefined>;
  abort(): void;
}
import { checkPreviewSession, counter } from "./session-identity";
const CONTROL_TIMEOUT_MS = 3000;
// One more serialized report/renewal attempt plus scheduling margin. Subtract
// the whole report RTT: the server's lease snapshot can predate its response.
const RENEWAL_RESERVE_MS = CONTROL_TIMEOUT_MS + 2000;
/** Shared stream/credit delegate; no project time, source clock or DSP work. */
export class PreviewConsumptionPump implements PreviewPump {
  private session: AudioPreviewSession | null = null;
  private progress: AudioOutputProgress = {
    consumedFrames: "0",
    stalled: true,
    contextTime: 0,
  };
  private outputObservation: OutputObservation = null;
  private outputStalled = true;
  private sequence = 0n;
  private controlMaxMs=0;
  private readMaxMs=0;
  private readAt="";
  private timer: ReturnType<typeof setInterval> | null = null;
  private reporting: Promise<AudioPreviewSession | undefined> | null = null;
  private pendingCompletion = false;
  private reportingActive = false;
  private draining = false;
  private heartbeatAt = performance.now();
  private readonly streamAbort = new AbortController();
  private readonly controlsAbort = new AbortController();
  constructor(
    private readonly transport: PumpTransport,
    private readonly output: PumpOutput,
    private readonly live: () => boolean,
    private readonly updated: (
      s: AudioPreviewSession,
      p: AudioOutputProgress,
    ) => void,
    private readonly failure: (error: unknown) => void,
  ) {}
  private current() {
    return this.live() && !this.controlsAbort.signal.aborted;
  }
  observe(progress: AudioOutputProgress) {
    if (!counter(progress.consumedFrames))
      throw new Error("Invalid device consumed cursor.");
    if (!this.draining && this.current()) this.progress = progress;
    if ("output" in progress) this.observeOutput(progress.output ?? null, progress.stalled);
  }
  observeOutput(output: OutputObservation, stalled: boolean) {
    if (output && !counter(output.frames)) throw Error("Invalid device output cursor.");
    if(output&&this.outputObservation&&BigInt(output.frames)<BigInt(this.outputObservation.frames))return;
    if (this.current()) { this.outputObservation = output; this.outputStalled = stalled; }
  }
  completed() {
    if (this.draining || !this.current()) return;
    if (this.reportingActive) this.pendingCompletion = true;
    else void this.report().catch(() => {});
  }
  start(session: AudioPreviewSession) {
    if (this.session) throw new Error("Audio data pump already owns a stream.");
    checkPreviewSession(
      session,
      session.identity,
      session.identity.sample_rate,
    );
    if (!this.current()) throw new Error("Audio data pump has stopped.");
    this.session = session;
    this.scheduleReport();
    void this.read(session);
  }
  private scheduleReport() {
    if (this.timer) clearInterval(this.timer);
    // Completed 4096-sample blocks already report every ~85–93 ms at
    // 44.1/48 kHz. Only fill a gap when no completion arrives.
    this.timer = setInterval(() => {
      void this.report().catch(() => {});
    }, 100);
  }
  private async read(session: AudioPreviewSession) {
    const signal = this.streamAbort.signal;
    try {
      const response = await this.transport.open(session, signal);
      if (!this.current() || signal.aborted) {
        await response.body?.cancel();
        return;
      }
      const blocks = readPreviewPcm(response, session, signal);
      let readSample=BigInt(session.actual_start_sample),readBlocks=0;
      try {
        for (;;) {
          await this.output.waitForCredit(signal);
          const readBegan=performance.now();
          const { value, done } = await blocks.next();
          const readMs=performance.now()-readBegan;
          if(!done){if(readBlocks++>0&&readMs>this.readMaxMs){this.readMaxMs=readMs;this.readAt=readSample.toString();}readSample+=BigInt(value.length/2);}
          if (done) {
            if (this.current()) this.output.finish?.();
            break;
          }
          if (!this.current()) return;
          await this.output.push(value, signal);
        }
      } finally {
        await blocks.return(undefined);
      }
    } catch (error) {
      if (this.current() && !this.draining && !signal.aborted)
        this.failure(error);
    }
  }
  freeze() {
    this.draining = true;
    this.pendingCompletion = false;
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }
  async reportStopped(frames: string) {
    if (!counter(frames)) throw new Error("Invalid final device cursor.");
    this.freeze();
    while (this.reporting) await this.reporting;
    return this.report(frames);
  }
  private check(s: AudioPreviewSession, previous: AudioPreviewSession) {
    checkPreviewSession(
      s,
      previous.identity,
      previous.identity.sample_rate,
      previous,
    );
  }
  private report(stopped?: string): Promise<AudioPreviewSession | undefined> {
    if (
      !this.current() ||
      !this.session ||
      this.reportingActive ||
      (this.draining && stopped === undefined)
    )
      return Promise.resolve(undefined);
    // A block completion already reports the current device cursor. Start the
    // periodic deadline from that report instead of sending another one on an
    // unrelated timer phase. Completions and final stopped receipts stay
    // immediate; a slow in-flight report still has periodic ticks pending.
    if (!this.draining) this.scheduleReport();
    this.pendingCompletion = false;
    const previous = this.session;
    const command = (
      command: AudioPreviewControl["command"],
    ): AudioPreviewControl => ({
      session_id: previous.session_id,
      generation: previous.generation,
      owner_id: previous.owner_id,
      lease_id: previous.lease_id,
      control_id: crypto.randomUUID(),
      command,
    });
    this.reportingActive = true;
    const controlBegan=performance.now();
    const task = (async () => {
      try {
        const began = performance.now();
        const signal = AbortSignal.any([
          this.controlsAbort.signal,
          AbortSignal.timeout(CONTROL_TIMEOUT_MS),
        ]);
        const consumed=stopped??this.progress.consumedFrames;
        // The device receipt and projected output arrive through independent
        // MessagePorts. Keep an ahead-of-receipt projection pending; only the
        // Worklet receipt can advance the submitted cursor accepted by Native.
        const observed=this.outputObservation;
        const confirmedOutput=observed&&BigInt(observed.frames)<=BigInt(consumed)
          ?observed:previous.output;
        const session = await this.transport.control(
          command({
            kind: "report_consumed",
            sequence: (++this.sequence).toString(),
            consumed_frames: consumed,
            playing: stopped === undefined,
            stalled: stopped === undefined && (this.outputObservation ? this.outputStalled : this.progress.stalled),
            output: confirmedOutput,
          }),
          signal,
        );
        this.check(session, previous);
        if (
          !this.current() ||
          session.generation !== previous.generation ||
          session.overlay_epoch !== previous.overlay_epoch
        )
          return;
        this.session = session;
        if (session.state === "failed" || session.state === "stopped")
          throw new Error("Scene audio is no longer available.");
        this.controlMaxMs=Math.max(this.controlMaxMs,performance.now()-began);
        this.updated(session, {...this.progress,transportTiming:{controlMaxMs:this.controlMaxMs,readMaxMs:this.readMaxMs,readAt:this.readAt}});
        const now = performance.now();
        const leaseUrgent =
          BigInt(session.lease_remaining_ms) <=
          BigInt(Math.ceil(Math.max(0, now - began)) + RENEWAL_RESERVE_MS);
        if (
          this.current() &&
          !this.draining &&
          (leaseUrgent || now - this.heartbeatAt >= 500) &&
          (!this.pendingCompletion || leaseUrgent)
        ) {
          const heartbeat = await this.transport.control(
            command({ kind: "heartbeat" }),
            signal,
          );
          this.check(heartbeat, session);
          if (
            !this.current() ||
            this.draining ||
            heartbeat.generation !== session.generation ||
            heartbeat.overlay_epoch !== session.overlay_epoch
          )
            return;
          this.heartbeatAt = performance.now();
        }
        return session;
      } catch (error) {
        if (this.current()) this.failure(error);
        throw error;
      } finally {
        this.controlMaxMs=Math.max(this.controlMaxMs,performance.now()-controlBegan);
        this.reportingActive = false;
        this.reporting = null;
        if (this.pendingCompletion && this.current() && !this.draining)
          void this.report().catch(() => {});
      }
    })();
    if (this.reportingActive) this.reporting = task;
    return task;
  }
  abort() {
    this.freeze();
    this.streamAbort.abort();
    this.controlsAbort.abort();
  }
}
