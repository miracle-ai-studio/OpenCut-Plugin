/** In-process execution ports; these are not a public API or project schema. */
export interface AudioIdentity {
  projectId: string;
  sequenceId: string;
  revision: string;
  profile: "classic-export-v31-web-audio";
  sampleRate: number;
}
export interface CasResource { assetId: string; contentHash: string; size: number; kind: "audio" | "video" }
export interface PcmAllocation { reserve(bytes: number): () => void }
export interface VerifiedRangeSource {
  readonly resource: Readonly<CasResource>;
  readonly length: number;
  readonly source: import("mediabunny").StreamSource;
  readonly residentBytes: number;
  readBytes(offset: number, count: number): Promise<Uint8Array>;
  close(): Promise<void>;
}
export interface ContainerDecodeAllocation {
  mode: "whole" | "windowed";
  maxFrames: number;
  maxChannels: number;
  workingFrames: number;
  sourceSampleRate: number;
  targetSampleRate: number;
  maxNativeFrames: string;
  windows: { sourceStartFrame: string; sourceEndFrame: string; targetStartFrame: string }[];
}
export interface PrimaryAudioTrack { trackId: number; codec: string; sampleRate: number; channels: number }
export interface ExactSourceSample { lower: string; fractionNumerator: string; fractionDenominator: string }
export interface PitchFacts {
  mode: "unprepared-linear" | "prepared-linear" | "prepared-soundtouch";
  sourceSliceStartSample: string;
  requestedSourceFrames: string;
  actualSourceFrames: string;
  resampledFrames: string;
  outputFrames: string;
  numChannels: number;
  decodedSampleRate: number;
  targetSampleRate: number;
  tempo: { numerator: string; denominator: string };
  prefixFrames: string;
  totalChannelFrames: string;
  decodedBytes: string;
  preparedBytes: string;
  totalBytes: string;
}
export interface ClassicAudioPlan {
  identity: AudioIdentity;
  frameCount: string;
  /** Rust output selection length; frameCount remains the whole scene. */
  outputFrameCount?: string;
  range?: { startSample: string; frameCount: string } | null;
  encodedSource: { mode: "verified-range"; cacheBytes: number };
  resources: CasResource[];
  /** Stable sequence track order, then stored element order, produced by Rust. */
  voices: { elementId: string; assetId: string }[];
}
export interface SourceFormat { elementId: string; sampleRate: number; frameCount: string; channels: number; prepared: boolean }
export interface CompiledSampleBlock {
  identity: AudioIdentity;
  startSample: string;
  frameCount: number;
  voices: {
    elementId: string;
    blockOffset: number;
    frameCount: number;
    positions: ExactSourceSample[];
    /** Registry-projected amplitude, never raw dB or legacy stored gain. */
    amplitudes: number[];
  }[];
}
export interface ClassicAudioHost {
  /** Pinned actual browser build/platform/codec execution identity. */
  engineIdentity: string;
  resolveVerifiedBytes(resource: Readonly<CasResource>, signal?: AbortSignal): Promise<Uint8Array>;
  resolveRangeSource(resource: Readonly<CasResource>, budget: PcmAllocation, signal?: AbortSignal): Promise<VerifiedRangeSource>;
  /** Must enforce hard decoder memory/time limits in the isolated host realm. */
  admitDecode(resource: CasResource, primary: PrimaryAudioTrack | null): Promise<{ maxFrames: number; maxChannels: number }>;
  /** Retire decoder PCM after all consumers prepare,or after complete mixing.
   * Preview retires only concat/preparation input;ordinary rolling input stays live. */
  releaseDecodedSource(resource: Readonly<CasResource>): Promise<{ released: true }>;
  /** Container decoding always uses Rust-planned logical and resident limits. */
  admitContainerDecode(resource: CasResource, primary: PrimaryAudioTrack): Promise<ContainerDecodeAllocation>;
  /** Codec choice for native-rate signed16/float32 PCM audio, never host generation detection. */
  admitPcmAudioDecode(resource: CasResource, primary: PrimaryAudioTrack): Promise<{ mode: "whole" } | { mode: "windowed"; admission: ContainerDecodeAllocation }>;
  /** Original authored span plus actual sparse residency; dense PCM uses null. */
  compilePitchPreparation(input: { elementId: string; decodedSampleRate: number; decodedFrameCount: string; decodedChannels: number; residentFrameCount: string | null }): Promise<PitchFacts>;
  /** Original source mapping, at each exact local output sample; no gain. */
  compileSourcePositions(input: { elementId: string; localStartSample: string; frameCount: number; sourceSampleRate: number }): Promise<ExactSourceSample[]>;
  /** Calls compile_audio_block_v4; prepared sources read from zero at unit rate
   * while the gain clock remains the original element's timeline-local clock.
   * frameCount is a maximum request: Rust may return a shorter positive block
   * at the exact startSample to preserve union and voice-sample admission. */
  compileBlock(input: { startSample: string; frameCount: number; sourceFormats: SourceFormat[] }): Promise<CompiledSampleBlock>;
  createAudioContext(sampleRate: number): AudioContext;
  createOfflineContext(channels: number, frames: number, sampleRate: number): OfflineAudioContext;
  /** Idempotent actual realm/process termination and acknowledgement. */
  abortRendering(): Promise<void>;
}
export class AudioExecutionError extends Error { code: string }
export function createClassicMasteringChain(context: BaseAudioContext, destination: AudioNode): { input: GainNode; disconnect(): void };
export function renderClassicExport(plan: ClassicAudioPlan, host: ClassicAudioHost, options: { signal?: AbortSignal; maxPcmBytes: number; maxEncodedBytes?: number }): Promise<{
  buffer: AudioBuffer;
  receipt: {
    identity: AudioIdentity;
    engineIdentity: string;
    decoders: { assetId: string; profile: string; sampleRate: number; frameCount: string; channels: number; primary: PrimaryAudioTrack | null }[];
    preparation: { elementId: string; facts: PitchFacts }[];
    mastering: { profile: string; wholeSceneFrames: string; inputPeak: number; applied: boolean };
    range: { startSample: string; frameCount: string } | null;
    peakAccountedBytes: number;
  };
}>;
