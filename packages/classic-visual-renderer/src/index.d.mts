/** Private execution bridge. Public frame DTOs are generated from Rust. */
export function encodeRgbaHex(bytes: Uint8Array): string;
export function encodeRgbaBase64(bytes: Uint8Array): string;
export type GpuRealm = { device: { destroy(): void }; observation: { gpuBackend: string; gpuDevice: string; textureFormat: string } };
export type ClassicTextFontCache = Map<string, { bytes: Uint8Array; face?: FontFace }>;
declare const preparedFontFrameBrand: unique symbol;
/** Opaque, single-use, same-invocation resource proof; never serialized. */
export type PreparedFontFrameToken = { readonly [preparedFontFrameBrand]: true };
export type PrivateVisualResourceHost = {
  outputBufferBytes?:number;
  /** Same-device owned texture; caller must close it, no pixel readback. */
  captureSurface?(frame:{readonly width:number;readonly height:number;readonly ready:Promise<void>;close():void}):void;
  beforeGpu?():Promise<void>;
  rangePrefetch?: boolean;
  rangeChunkBytes?: 65536 | 524288;
  rangeCacheLimit?: number;
  rangeReadAhead?: boolean;
  /** Optional decoder-resource lifetime, distinct from one frame's deadline. */
  rangeSignal?: AbortSignal;
  readVerifiedBytes?(transferId: string, offset: number, count: number): Uint8Array | Promise<Uint8Array>;
  preparedFontFrame?: PreparedFontFrameToken;
};
/** Prepare only already-retained fonts under the native admission; obtain a
 * fresh native frame plan before consuming the token, then always close it. */
export function preparePreviewFontFrame(fonts: readonly unknown[], identity: unknown, admission: unknown,
  realm: GpuRealm, signal: AbortSignal, cache: Map<string, Uint8Array>, decoders: VisualDecoderCache,
  fontCache: ClassicTextFontCache): Promise<{ token: PreparedFontFrameToken; close(): Promise<void> } | null>;

export function measureClassicText(ctx: CanvasRenderingContext2D, text: unknown, fontFamily: string, font: { style: string; weight: number }): { block: { maxWidth: number; height: number } };
/** Web preview may enable a local font digest worker; native hosts default off. */
export function openGpuRealm(options?: { fontHashWorker?: boolean }): Promise<GpuRealm>;
/** Opt in only the browser-owned preview realm; native/raw byte consumers retain per-frame verification. */
export function enableImmutablePreviewFonts(realm: GpuRealm): void;
/** Releases retained graphics/GPU textures and destroys the realm once. */
export function closeGpuRealm(realm: GpuRealm): void;
export class VisualDecoderCache {
  close(): Promise<void>;
  keys(): Set<string>;
  retainedBytes(): number;
  retainedEncodedBytes(): number;
  adopt(other: VisualDecoderCache,replace?:boolean): Promise<void>;
}
export function prewarmVideoSources(plan:unknown,permits:unknown,host:PrivateVisualResourceHost & {
  admission:unknown;callback(name:string,args:Record<string,unknown>):Promise<unknown>;
  readVerifiedBytes(transferId:string,offset:number,count:number):Promise<Uint8Array>;
},signal:AbortSignal,decoders:VisualDecoderCache,activeKeys:Set<string>):Promise<boolean>;
export function renderClassicFrame(plan: unknown, host: PrivateVisualResourceHost & {
  admission: unknown;
  surface?: OffscreenCanvas;
  hitQuery?: {x:number;y:number};
  observeHitTest?(observation: {identity:unknown;point:{x:number;y:number};hits:{elementId:string;alpha:number}[]}):void;
  requireResolveCached?: boolean;
  callback(name: string, args: Record<string, unknown>): Promise<unknown>;
  rgba(offset: number, bytes: Uint8Array): Promise<void>;
  observeExecution?(observation: Record<string, string>): void;
}, realm: GpuRealm, signal: AbortSignal, cache?: Map<string, Uint8Array>, decoders?: VisualDecoderCache): Promise<unknown>;
export function renderClassicTextFrame(plan: unknown, host: PrivateVisualResourceHost & {
  admission: unknown;
  surface?: OffscreenCanvas;
  hitQuery?: {x:number;y:number};
  observeHitTest?(observation: {identity:unknown;point:{x:number;y:number};hits:{elementId:string;alpha:number}[]}):void;
  requireResolveCached?: boolean;
  callback(name: string, args: Record<string, unknown>): Promise<unknown>;
  rgba(offset: number, bytes: Uint8Array): Promise<void>;
  observeExecution?(observation: Record<string, string>): void;
}, realm: GpuRealm, signal: AbortSignal, cache?: Map<string, Uint8Array>, decoders?: VisualDecoderCache, fontCache?: ClassicTextFontCache): Promise<unknown>;

export function renderClassicCaptionFrame(plan: unknown, host: PrivateVisualResourceHost & {
  admission: unknown;
  surface?: OffscreenCanvas;
  hitQuery?: {x:number;y:number};
  observeHitTest?(observation: {identity:unknown;point:{x:number;y:number};hits:{elementId:string;alpha:number}[]}):void;
  requireResolveCached?: boolean;
  callback(name: string, args: Record<string, unknown>): Promise<unknown>;
  rgba(offset: number, bytes: Uint8Array): Promise<void>;
  observeExecution?(observation: Record<string, string>): void;
}, realm: GpuRealm, signal: AbortSignal, cache?: Map<string, Uint8Array>, decoders?: VisualDecoderCache, fontCache?: ClassicTextFontCache): Promise<unknown>;

/** Private Classic caption preparation; public receipt types remain generated. */
export function prepareCaptionText(ctx: CanvasRenderingContext2D, original: unknown, fontFamily: string, font: { style: string; weight: number }, directive: unknown, dimensions: { width: number; height: number }): { text: unknown; wrappedText: string; measurements: { content: string; width: number }[] };

export function renderClassicGraphicFrame(plan: unknown, host: PrivateVisualResourceHost & {
  admission: unknown;
  surface?: OffscreenCanvas;
  hitQuery?: {x:number;y:number};
  observeHitTest?(observation: {identity:unknown;point:{x:number;y:number};hits:{elementId:string;alpha:number}[]}):void;
  requireResolveCached?: boolean;
  callback(name: string, args: Record<string, unknown>): Promise<unknown>;
  rgba(offset: number, bytes: Uint8Array): Promise<void>;
  observeExecution?(observation: Record<string, string>): void;
}, realm: GpuRealm, signal: AbortSignal, cache?: Map<string, Uint8Array>, decoders?: VisualDecoderCache, fontCache?: ClassicTextFontCache): Promise<unknown>;

export function renderClassicBlendFrame(plan: unknown, host: PrivateVisualResourceHost & {
  admission: unknown;
  surface?: OffscreenCanvas;
  hitQuery?: {x:number;y:number};
  observeHitTest?(observation: {identity:unknown;point:{x:number;y:number};hits:{elementId:string;alpha:number}[]}):void;
  requireResolveCached?: boolean;
  callback(name: string, args: Record<string, unknown>): Promise<unknown>;
  rgba(offset: number, bytes: Uint8Array): Promise<void>;
  observeExecution?(observation: Record<string, string>): void;
}, realm: GpuRealm, signal: AbortSignal, cache?: Map<string, Uint8Array>, decoders?: VisualDecoderCache, fontCache?: ClassicTextFontCache): Promise<unknown>;

export function renderClassicMaskFrame(plan: unknown, host: PrivateVisualResourceHost & {
  admission: unknown;
  surface?: OffscreenCanvas;
  hitQuery?: {x:number;y:number};
  observeHitTest?(observation: {identity:unknown;point:{x:number;y:number};hits:{elementId:string;alpha:number}[]}):void;
  requireResolveCached?: boolean;
  callback(name: string, args: Record<string, unknown>): Promise<unknown>;
  rgba(offset: number, bytes: Uint8Array): Promise<void>;
  observeExecution?(observation: Record<string, string>): void;
}, realm: GpuRealm, signal: AbortSignal, cache?: Map<string, Uint8Array>, decoders?: VisualDecoderCache, fontCache?: ClassicTextFontCache): Promise<unknown>;

export function renderClassicSceneFrame(plan: unknown, host: PrivateVisualResourceHost & {
  admission: unknown;
  surface?: OffscreenCanvas;
  hitQuery?: {x:number;y:number};
  observeHitTest?(observation: {identity:unknown;point:{x:number;y:number};hits:{elementId:string;alpha:number}[]}):void;
  requireResolveCached?: boolean;
  callback(name: string, args: Record<string, unknown>): Promise<unknown>;
  rgba(offset: number, bytes: Uint8Array): Promise<void>;
  observeExecution?(observation: Record<string, string>): void;
}, realm: GpuRealm, signal: AbortSignal, cache?: Map<string, Uint8Array>, decoders?: VisualDecoderCache, fontCache?: ClassicTextFontCache): Promise<unknown>;

export function prepareClassicAssSubtitle(ctx: CanvasRenderingContext2D, original: unknown, fontFamily: string, font: {style: string; weight: number}, directive: unknown, placement: {left_margin_px:number;right_margin_px:number;vertical_margin_px:number;vertical_align:"top"|"middle"|"bottom"}, dimensions: {width:number;height:number}): {text:unknown;wrappedText:string;measurements:{content:string;width:number}[]};
export { PreviewSegmentDecoder } from "./preview-segment.mjs";

export function gpuSurfacePresenter(realm:GpuRealm,canvas:OffscreenCanvas):{draw(bitmap:ImageBitmap):void;clear():void};
export function gpuSurfaceBitmap(realm:GpuRealm,frame:ImageBitmap,canvas:OffscreenCanvas):Promise<ImageBitmap>;
