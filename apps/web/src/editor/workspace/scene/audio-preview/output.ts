import { DEVICE_PRIME_BLOCKS } from "./buffer-limits";
import workletUrl from "./worklet.ts?worker&url";
import { PreviewDevicePort } from "./device-port";
import { WorkerPreviewPump } from "./worker-pump";
import type { DeviceReceipt } from "./device-port";
import type { EditorConnection } from "../../../../lib/editor-sdk";
import type { AudioPreviewSession } from "../types";
import { PreviewOutputClock } from "./output-clock";
import type { OutputObservation } from "./output-clock";
import type {DeviceTimestamp,PresentationClockBinding} from "./presentation-clock";
export type AudioOutputProgress = {
  consumedFrames: string;
  stalled: boolean;
  contextTime: number;
  output?: OutputObservation;
  underflowEvents?: number;
  underflowFrames?: string;
  transportTiming?: {controlMaxMs:number; readMaxMs:number; readAt:string};
  atRefresh?: boolean;
  deviceTimestamp?:DeviceTimestamp;
};
/** Native-mastered PCM presentation; the worker owns the data-plane port. */
export class PreviewAudioOutput {
  clockDiagnostics(){return {...this.clock.diagnostics(),deviceUnderflowAt:this.latestReceipt?.underflowAt??"",deviceElapsedFrames:this.latestReceipt?.elapsedRenderFrames??"",deviceCopiedFrames:this.latestReceipt?.consumedFrames??"",...this.pump?.diagnostics?.()};}
  readonly ready: Promise<void>;
  readonly sampleRate: number;
  private context: AudioContext;
  private node: AudioWorkletNode | null = null;
  private device: PreviewDevicePort | null = null;
  private pump: WorkerPreviewPump | null = null;
  private stopped = false;
  private picturePort: MessagePort | null = null;
  private clockPort:MessagePort|null=null;
  private closeClockPort() {
    if(this.clockPort){this.clockPort.onmessage=null;this.clockPort.close();this.clockPort=null;}
  }
  private closing: Promise<string> | null = null;
  private clock: PreviewOutputClock;
  private animation: number | null = null;
  private latestReceipt: DeviceReceipt | null = null;
  private latestOutput: OutputObservation = null;
  private firstReceiptAt: number | null = null;
  private lastPublished = "";
  private lastPublishedAt=-Infinity;
  private presentationBound=false;
  bindPresentationClock(port:MessagePort):PresentationClockBinding {
    if(this.stopped||!this.node||this.presentationBound)throw Error("Presentation clock is unavailable or already bound");
    this.presentationBound=true;
    this.node.port.postMessage({kind:"presentation-clock-port",token:this.token,port},[port]);
    return {token:this.token,sampleRate:this.sampleRate,graphLatencySeconds:0,directPictureCredit:true,...(this.pump?{outputPort:this.pump.bindOutputClock()}: {})};
  }
  constructor(
    readonly token: string,
    sampleRate: number,
    private readonly progress: (value: AudioOutputProgress) => void,
    private readonly failure: (error: Error) => void,
    private readonly completedBlock: () => void = () => {},
    private readonly primeBlocks = DEVICE_PRIME_BLOCKS,
  ) {
    this.context = new AudioContext({ sampleRate });
    this.sampleRate = this.context.sampleRate;
    this.clock = new PreviewOutputClock(this.sampleRate, 0);
    const activation = this.context.resume();
    this.ready = this.initialize(sampleRate, activation);
  }
  private observed(data: DeviceReceipt) {
    if (this.stopped) return;
    try {
      this.clock.observe(data);
      this.latestReceipt = data;
      this.firstReceiptAt ??= performance.now();
    } catch (error) {
      this.failure(error instanceof Error ? error : Error(String(error)));
      this.silence();
    }
  }
  private sampleOutput(atRefresh=false) {
    if (this.stopped || !this.latestReceipt) return null;
    const now = performance.now();
    const timestamp=this.context.getOutputTimestamp();
    const sample = this.clock.sample(timestamp, now);
    if (!sample && this.firstReceiptAt !== null && now - this.firstReceiptAt > 1000)
      throw Error("The audio device did not provide a valid output clock. Resume playback to recalibrate.");
    this.latestOutput = sample?.output ?? null;
    const progress: AudioOutputProgress = {
      consumedFrames: this.clock.submittedFrames,
      stalled: !sample?.started || sample.stalled,
      contextTime: this.latestReceipt.renderFrame! / this.sampleRate,
      output: this.latestOutput,
      ...(atRefresh?{atRefresh:true}:{}),
      // Calibrate the picture worker while PCM is still priming. Waiting for
      // the first audible output makes startup depend on a later UI refresh.
      ...(sample?{deviceTimestamp:{contextTime:timestamp.contextTime!,performanceTime:timestamp.performanceTime!,timeOrigin:performance.timeOrigin}}:{}),
      ...(this.latestReceipt.underflowEvents!==undefined?{
        underflowEvents:this.latestReceipt.underflowEvents,
        underflowFrames:this.latestReceipt.underflowFrames,
      }:{}),
    };
    const identity = `${progress.consumedFrames}:${progress.output?.frames ?? "pending"}:${progress.stalled}:${progress.underflowFrames}`;
    if (identity !== this.lastPublished||atRefresh&&now-this.lastPublishedAt>=50) {
      this.lastPublished = identity;
      this.lastPublishedAt=now;
      this.progress(progress);
    }
    return this.latestOutput;
  }
  private tick = () => {
    this.animation = null;
    if (this.stopped) return;
    try { this.sampleOutput(true); }
    catch (error) {
      this.failure(error instanceof Error ? error : Error(String(error)));
      this.silence();
      return;
    }
    this.animation = requestAnimationFrame(this.tick);
  }
  private async initialize(sampleRate: number, activation: Promise<void>) {
    try {
      if (
        this.sampleRate !== sampleRate ||
        ![44100, 48000].includes(sampleRate)
      )
        throw new Error(
          "The audio device sample rate does not match this service.",
        );
      await activation;
      if (typeof this.context.getOutputTimestamp !== "function")
        throw Error("This browser cannot provide the audio output clock required for synchronized preview.");
      if (this.context.state !== "running")
        throw new Error("Allow audio playback before starting scene sound.");
      await this.context.audioWorklet.addModule(workletUrl);
      if (this.stopped) return;
      this.node = new AudioWorkletNode(
        this.context,
        "opencut-audio-preview",
        { numberOfInputs: 0, numberOfOutputs: 1, outputChannelCount: [2] },
      );
      // Native PCM already includes the realtime master and its compensated
      // look-ahead. The device adapter only presents these exact samples.
      this.node.connect(this.context.destination);
      this.node.port.postMessage({ kind: "prime", blocks: this.primeBlocks });
      const clockChannel=new MessageChannel();
      this.clockPort=clockChannel.port1;
      this.clockPort.onmessage=({data}:MessageEvent<DeviceReceipt>)=>{
        if(data.kind==="progress"&&data.token===this.token)this.observed(data);
      };
      this.clockPort.start();
      this.node.port.postMessage({kind:"clock-port",token:this.token,port:clockChannel.port2},[clockChannel.port2]);
      const channel = new MessageChannel();
      this.picturePort = channel.port1;
      this.node.port.postMessage({ kind: "picture-port", token: this.token, port: channel.port2 }, [channel.port2]);
      this.device = new PreviewDevicePort(
        this.node.port,
        this.token,
        (data) => {if(!this.clockPort)this.observed(data);},
        this.failure,
        this.completedBlock,
      );
      this.node.onprocessorerror = () =>
        this.failure(new Error("Scene audio output failed."));
      this.context.onstatechange = () => {
        if (!this.stopped && this.context.state !== "running")
          this.failure(
            new Error(
              "Scene audio playback was suspended. Press Play to resume.",
            ),
          );
      };
      this.animation = requestAnimationFrame(this.tick);
    } catch (error) {
      this.silence();
      throw error;
    }
  }
  async createPump(
    connection: EditorConnection,
    updated: (s: AudioPreviewSession, p: AudioOutputProgress) => void,
    failure: (error: unknown) => void,
  ) {
    await this.ready;
    this.pump = new WorkerPreviewPump(
      this.token,
      connection,
      () => this.device!.detach(),
      (data) => {
        // The worker owns submission/stop credit. Its final validated receipt
        // fences the direct clock port before adopting the terminal cursor, so
        // a delayed earlier quantum cannot arrive after that final observation.
        if(this.closing)this.closeClockPort();
        this.observed(data);
      },
      updated,
      failure,
    );
    await this.pump.ready;
    return this.pump;
  }
  async waitForCredit(signal: AbortSignal) {
    await this.ready;
    await this.device!.waitForCredit(signal);
  }
  async push(pcm: Float32Array, signal: AbortSignal) {
    await this.ready;
    await this.device!.push(pcm, signal);
  }
  finish() {
    this.device?.finish();
  }
  limitPicture(frames: string) {
    if (this.stopped) return;
    if (!/^(0|[1-9][0-9]{0,18})$/.test(frames)) throw Error("Invalid picture consumption limit");
    this.picturePort?.postMessage({ kind: "picture-limit", token: this.token, frames });
  }
  silence() {
    if (!this.stopped) {
      if (this.pump?.ownsDevice) this.pump.freeze();
      else this.device?.silence();
    }
    this.stopped = true;
    if (this.animation !== null) cancelAnimationFrame(this.animation);
    this.animation = null;
    this.picturePort?.close();
    this.picturePort = null;
    this.closeClockPort();
    this.node?.disconnect();
  }
  stop() {
    return (this.closing ??= this.close());
  }
  private async close() {
    try {
      const frames = this.pump?.ownsDevice
        ? await this.pump.stopOutput()
        : this.device
          ? await this.device.stop()
          : "0";
      this.closeClockPort();
      // After transfer, only the worker's stop reply owns the final receipt.
      // The detached main-thread port still retains its pre-transfer snapshot.
      if (!this.pump?.ownsDevice && this.device?.lastReceipt)
        this.observed(this.device.lastReceipt);
      // Stop accepting new content, but keep the graph connected while samples
      // already submitted to the device finish. Hard silence never waits here.
      const deadline = performance.now() + 5000;
      while (!this.stopped && frames !== "0") {
        const output = this.sampleOutput();
        if (output && BigInt(output.frames) >= BigInt(frames)) break;
        if (performance.now() >= deadline)
          throw Error("The audio device did not confirm its final output position.");
        await new Promise<void>((resolve) => setTimeout(resolve, 10));
      }
      return frames;
    } finally {
      this.silence();
      await this.context.close();
    }
  }
}
