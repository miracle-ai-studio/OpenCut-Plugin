import { DEVICE_BUFFER_BLOCKS, DEVICE_PRIME_BLOCKS } from "./buffer-limits";
import { PreviewPcmQueue } from "./queue";

declare class AudioWorkletProcessor {
  readonly port: MessagePort;
}
declare function registerProcessor(
  name: string,
  processor: typeof AudioWorkletProcessor,
): void;
declare const currentFrame: number;

class PreviewOutputProcessor extends AudioWorkletProcessor {
  private queue = new PreviewPcmQueue();
  private lastRenderFrame=-1;
  private lastOutput:Float32Array[]=[];
  private lastOutputToken:string|null=null;
  private repeatedQuanta=0;
  private token: string | null = null;
  private playing = false;
  private primed = false;
  private ended = false;
  private runStartFrame: number | null = null;
  private runStartConsumed = "0";
  private contentEndFrame = -1;
  private completedBlocks = 0;
  private lastPhase: string | null = null;
  private primeBlocks = DEVICE_PRIME_BLOCKS;
  private pictureLimit: bigint | undefined;
  private picturePort: MessagePort | null = null;
  private pictureToken: string | null = null;
  private underflowEvents = 0;
  private underflowAt: string[] = [];
  private underflowFrames = 0n;
  private pictureWaitEvents = 0;
  private pictureWaitFrames = 0n;
  private pictureWaitRunFrames = 0n;
  private pictureWaitMaxFrames = 0n;
  private firstContentFrame:number|null=null;
  private clockPort:MessagePort|null=null;
  private clockToken:string|null=null;
  private presentationClockPort:MessagePort|null=null;
  private presentationClockToken:string|null=null;
  private admitPictureCredit(limit:{token?:unknown;kind?:unknown;frames?:unknown},expected:string|null){
    if(limit.kind!=="picture-limit"||limit.token!==expected||(this.token!==null&&this.token!==expected)||typeof limit.frames!=="string"||!/^(0|[1-9][0-9]{0,18})$/.test(limit.frames))return;
    const next=BigInt(limit.frames);if(this.pictureLimit===undefined||next>this.pictureLimit)this.pictureLimit=next;
  }
  private reportClock(receipt:object) {
    const observation={...receipt,repeatedQuanta:this.repeatedQuanta,underflowAt:this.underflowAt.join(","),elapsedRenderFrames:String(this.firstContentFrame===null?0:Math.max(0,this.contentEndFrame-this.firstContentFrame)),
      pictureWaitEvents:this.pictureWaitEvents,pictureWaitFrames:String(this.pictureWaitFrames),pictureWaitMaxFrames:String(this.pictureWaitMaxFrames)};
    this.port.postMessage(observation);
    if(this.token===this.clockToken)this.clockPort?.postMessage(observation);
    if(this.token===this.presentationClockToken)this.presentationClockPort?.postMessage(observation);
  }
  constructor() {
    super();
    this.port.onmessage = ({ data }) => {
      if (data.kind === "prime" && !this.playing) {
        if (Number.isInteger(data.blocks) && data.blocks >= 2 && data.blocks <= DEVICE_BUFFER_BLOCKS) this.primeBlocks = data.blocks;
      } else if(data.kind==="presentation-clock-port") {
        if(this.presentationClockPort||!data.port||typeof data.token!=="string")return;
        this.presentationClockToken=data.token;this.presentationClockPort=data.port;
        this.presentationClockPort!.onmessage=({data:limit})=>this.admitPictureCredit(limit,this.presentationClockToken);
        this.presentationClockPort!.start();
      } else if(data.kind==="clock-port") {
        if(this.clockPort||!data.port||typeof data.token!=="string")return;
        this.clockToken=data.token;this.clockPort=data.port;
      } else if (data.kind === "picture-port") {
        if (this.picturePort || !data.port) return;
        this.pictureToken = data.token;
        this.picturePort = data.port;
        this.picturePort!.onmessage = ({ data: limit }) => this.admitPictureCredit(limit,this.pictureToken);
        this.picturePort!.start();
      } else if (data.kind === "reset") {
        this.token = data.token;
        this.queue.reset(this.token);
        this.playing = true;
        this.primed = false;
        this.ended = false;
        this.runStartFrame = null;
        this.runStartConsumed = "0";
        this.contentEndFrame = -1;
        this.firstContentFrame=null;
        this.completedBlocks = 0;
        this.lastPhase = null;
        this.underflowEvents = 0;
        this.underflowAt = [];
        this.underflowFrames = 0n;
        this.pictureWaitEvents = 0;
        this.pictureWaitFrames = 0n;
        this.pictureWaitRunFrames = 0n;
        this.pictureWaitMaxFrames = 0n;
        if (this.token !== this.pictureToken) this.pictureLimit = undefined;
      } else if (data.kind === "stop") {
        this.playing = false;
        this.port.postMessage({
          kind: "stopped",
          token: this.token,
          consumedFrames: this.queue.consumedFrames,
        });
        if(this.token===this.presentationClockToken)this.presentationClockPort?.postMessage({kind:"stopped",token:this.token,consumedFrames:this.queue.consumedFrames});
        this.queue.reset(null);
      } else if (data.kind === "block") {
        try {
          this.queue.append(data.token, new Float32Array(data.pcm));
          if (this.queue.freeBlocks <= DEVICE_BUFFER_BLOCKS - this.primeBlocks) this.primed = true;
        } catch (error) {
          this.playing = false;
          this.queue.reset(null);
          this.port.postMessage({
            kind: "error",
            token: this.token,
            message: String(error),
          });
        }
      } else if (data.kind === "end" && data.token === this.token) { this.ended = true; this.primed = true; }
    };
  }
  process(_inputs: Float32Array[][], outputs: Float32Array[][]) {
    const output=outputs[0];
    if(!output?.[0]||!output[1])return true;
    if(output[0].length!==output[1].length||output[0].length>4096||currentFrame<this.lastRenderFrame)
      throw Error("Invalid audio device render quantum.");
    // A device quantum is an idempotent pull. Some hosts request the same
    // frame twice; serve the identical PCM instead of consuming another block
    // at the same output position. Never invent a new device timestamp.
    if(currentFrame===this.lastRenderFrame){
      this.repeatedQuanta++;
      for(let c=0;c<2;c++){
        if(this.playing&&this.lastOutputToken===this.token&&this.lastOutput[c]?.length===output[c].length)output[c].set(this.lastOutput[c]);
        else output[c].fill(0);
      }
      return true;
    }
    this.lastRenderFrame=currentFrame;
    const result=this.render(output);
    for(let c=0;c<2;c++){
      if(this.lastOutput[c]?.length!==output[c].length)this.lastOutput[c]=new Float32Array(output[c].length);
      this.lastOutput[c].set(output[c]);
    }
    this.lastOutputToken=this.token;
    return result;
  }
  private render(output:Float32Array[]) {
    if (!this.playing || !this.primed) {
      this.pictureWaitRunFrames = 0n;
      output[0].fill(0);
      output[1].fill(0);
      if (this.playing) {
        const phase = this.queue.consumedFrames === "0" ? "preparing" : "buffering";
        if(phase==="buffering"&&!this.ended)this.underflowFrames+=BigInt(output[0].length);
        if (phase !== this.lastPhase) {
          this.lastPhase = phase;
          this.reportClock({ kind: "progress", token: this.token, copied: 0,
            consumedFrames: this.queue.consumedFrames, completedBlocks: this.completedBlocks,
            renderFrame: currentFrame, quantumFrames: output[0].length,
            runStartFrame: this.runStartFrame, runStartConsumed: this.runStartConsumed, phase,
            underflowEvents:this.underflowEvents,underflowFrames:String(this.underflowFrames) });
        }
      }
      return true;
    }
    const receipt = this.queue.render(output[0], output[1], this.pictureLimit);
    if (receipt.copied > 0) {
      this.firstContentFrame??=currentFrame;
      if (this.contentEndFrame !== currentFrame) {
        this.runStartFrame = currentFrame;
        this.runStartConsumed = (BigInt(receipt.consumedFrames) - BigInt(receipt.copied)).toString();
      }
      this.contentEndFrame = currentFrame + receipt.copied;
    }
    this.completedBlocks = receipt.completedBlocks;
    const pictureBlocked = this.pictureLimit !== undefined && BigInt(receipt.consumedFrames) >= this.pictureLimit && this.queue.freeBlocks < DEVICE_BUFFER_BLOCKS;
    // Buffered PCM can still be silenced by picture credit. Count that gap
    // separately from FIFO starvation, excluding startup and the short EOF pad.
    const pictureWait = pictureBlocked && this.firstContentFrame !== null
      ? BigInt(output[0].length - receipt.copied) : 0n;
    if (pictureWait > 0n) {
      if (this.pictureWaitRunFrames === 0n) this.pictureWaitEvents++;
      this.pictureWaitFrames += pictureWait;
      this.pictureWaitRunFrames += pictureWait;
      if (this.pictureWaitRunFrames > this.pictureWaitMaxFrames)
        this.pictureWaitMaxFrames = this.pictureWaitRunFrames;
    } else this.pictureWaitRunFrames = 0n;
    const phase = receipt.copied < output[0].length ? this.ended && !pictureBlocked ? "ended" : "buffering" : "playing";
    this.lastPhase = phase;
    // Recover from a real FIFO underrun with the same bounded startup margin.
    // A single arriving block must not alternate sound/silence on every pull.
    // EOF and a picture-credit pause retain their exact short-tail behavior.
    if (receipt.copied < output[0].length && !this.ended && this.queue.freeBlocks === DEVICE_BUFFER_BLOCKS) {
      this.primed = false;
      if(receipt.consumedFrames!=="0") {this.underflowEvents++;if(this.underflowAt.length<16)this.underflowAt.push(receipt.consumedFrames);this.underflowFrames+=BigInt(output[0].length-receipt.copied);}
    }
    this.reportClock({ kind: "progress", token: this.token, ...receipt,
      renderFrame: currentFrame, quantumFrames: output[0].length,
      runStartFrame: this.runStartFrame, runStartConsumed: this.runStartConsumed, phase,
      underflowEvents:this.underflowEvents,underflowFrames:String(this.underflowFrames) });
    return true;
  }
}
registerProcessor("opencut-audio-preview", PreviewOutputProcessor);
