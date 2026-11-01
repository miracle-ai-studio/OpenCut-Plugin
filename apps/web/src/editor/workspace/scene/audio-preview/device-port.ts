import { DEVICE_BUFFER_BLOCKS } from "./buffer-limits";
import { counter } from "./session-identity";
export type DeviceReceipt = {
  kind: "progress";
  token: string;
  consumedFrames: string;
  copied: number;
  completedBlocks: number;
  renderFrame?: number;
  quantumFrames?: number;
  runStartFrame?: number | null;
  runStartConsumed?: string;
  phase?: "preparing" | "playing" | "buffering" | "ended";
  repeatedQuanta?:number;
  elapsedRenderFrames?:string;
  underflowEvents?: number;
  underflowAt?: string;
  underflowFrames?: string;
  pictureWaitEvents?: number;
  pictureWaitFrames?: string;
  pictureWaitMaxFrames?: string;
};
/** Single owner of the bounded transfer slots and final processor receipt. */
export class PreviewDevicePort {
  lastReceipt: DeviceReceipt | null = null;
  private sent = 0;
  private completed = 0;
  private consumed = "0";
  private stopped = false;
  private detached = false;
  private waiters: Array<() => void> = [];
  private final: Promise<string> | null = null;
  private receipt: ((value: string) => void) | null = null;
  private closing: Promise<string> | null = null;
  constructor(
    readonly port: MessagePort,
    readonly token: string,
    progress: (data: DeviceReceipt) => void,
    failure: (error: Error) => void,
    complete: () => void,
    reset = true,
  ) {
    port.onmessage = ({ data }) => {
      if (data.token !== token) return;
      if (data.kind === "stopped") {
        if (
          !this.stopped ||
          !counter(data.consumedFrames) ||
          BigInt(data.consumedFrames) < BigInt(this.consumed)
        ) {
          failure(new Error("Invalid final device cursor."));
          return;
        }
        this.receipt?.(data.consumedFrames);
        this.receipt = null;
        return;
      }
      if (this.stopped && !this.receipt) return;
      if (data.kind === "error") {
        failure(new Error(data.message));
        return;
      }
      if (data.kind !== "progress") return;
      if (
        !counter(data.consumedFrames) ||
        !Number.isInteger(data.completedBlocks) ||
        data.completedBlocks < this.completed ||
        data.completedBlocks > this.sent ||
        !Number.isInteger(data.copied) ||
        data.copied < 0 ||
        data.copied > 4096 ||
        BigInt(data.consumedFrames) < BigInt(this.consumed)
      ) {
        failure(new Error("Invalid device consumption receipt."));
        return;
      }
      const advanced = data.completedBlocks > this.completed;
      this.completed = data.completedBlocks;
      this.consumed = data.consumedFrames;
      this.lastReceipt = data;
      progress(data);
      if (this.sent - this.completed < DEVICE_BUFFER_BLOCKS) this.wake();
      if (advanced && !this.stopped) complete();
    };
    port.start();
    if (reset) port.postMessage({ kind: "reset", token });
  }
  detach() {
    if (this.sent || this.stopped || this.detached)
      throw new Error("Device ownership can transfer only before PCM starts.");
    this.detached = true;
    this.port.onmessage = null;
    return this.port;
  }
  private wake() {
    this.waiters.splice(0).forEach((resolve) => resolve());
  }
  async waitForCredit(signal: AbortSignal) {
    if (this.detached)
      throw new Error("Audio device port belongs to the data pump.");
    while (
      !this.stopped &&
      !signal.aborted &&
      this.sent - this.completed >= DEVICE_BUFFER_BLOCKS
    ) {
      await new Promise<void>((resolve) => {
        const done = () => {
          signal.removeEventListener("abort", done);
          resolve();
        };
        this.waiters.push(done);
        signal.addEventListener("abort", done, { once: true });
      });
    }
    if (this.stopped && !signal.aborted)
      await new Promise<void>((resolve) =>
        signal.addEventListener("abort", () => resolve(), { once: true }),
      );
    if (this.stopped || signal.aborted)
      throw new DOMException("Stopped", "AbortError");
  }
  async push(pcm: Float32Array, signal: AbortSignal) {
    await this.waitForCredit(signal);
    if (
      !pcm.length ||
      pcm.length % 2 ||
      pcm.length > 8192 ||
      !pcm.every(Number.isFinite) ||
      !(pcm.buffer instanceof ArrayBuffer)
    )
      throw new Error("Invalid scene audio PCM block.");
    this.sent++;
    this.port.postMessage(
      { kind: "block", token: this.token, pcm: pcm.buffer },
      [pcm.buffer],
    );
  }
  finish() {
    if (!this.stopped)
      this.port.postMessage({ kind: "end", token: this.token });
  }
  silence() {
    if (!this.stopped) {
      this.stopped = true;
      this.final = new Promise<string>((resolve) => {
        this.receipt = resolve;
      });
      this.port.postMessage({ kind: "stop" });
    }
    this.stopped = true;
    this.wake();
  }
  stop() {
    return (this.closing ??= this.close());
  }
  private async close() {
    this.silence();
    const value = await Promise.race([
      this.final!,
      new Promise<null>((resolve) => setTimeout(() => resolve(null), 500)),
    ]);
    this.receipt = null;
    this.port.close();
    if (value === null)
      throw new Error(
        "The audio device did not confirm its stopped sample cursor.",
      );
    return value;
  }
}
