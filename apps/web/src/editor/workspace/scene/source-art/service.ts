import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex, hexToBytes } from "@noble/hashes/utils.js";
import type { ToolInput, ToolOutput } from "../../../../lib/editor-sdk";
import type { EditorApi } from "../../api";
import type { Asset } from "../types";
import { SourceArtCache } from "./cache";
import type { ArtAllocation } from "./cache";
import { canonicalArt, validateArtScope } from "./receipt";
export { canonicalArt } from "./receipt";
import { allocateWaveformDisplay } from "./display";
import type { WaveformDisplaySelection, WaveformDisplayResponse } from "./display";

export type SourceArtRequest = ToolInput<"opencut_source_art_get">;
export type SourceArtResponse = ToolOutput<"opencut_source_art_get">;
export type SourceArtQuery = SourceArtRequest["query"];
export type DisplayArt = (
  | { kind: "waveform"; amplitudes: Float32Array; channels: number; sampleRate: number }
  | { kind: "thumbnail"; url: string; width: number; height: number }
  | { kind: "classic_waveform"; canvas: WaveformDisplayResponse["canvas"]; bars: WaveformDisplayResponse["bars"]; temporaryGain: number | null }
  | { kind: "no_frame" }) & { executionIdentity: string };
export type ArtSelection = { projectId: string; revision: string; asset: Asset; query: SourceArtQuery };
export type ArtRequestSelection = ArtSelection | WaveformDisplaySelection;
export const artSelectionKey = (selection: ArtRequestSelection) => canonicalArt([selection.projectId, selection.revision,
  selection.asset.id, selection.asset.version, "display" in selection ? selection.display : selection.query]);

type ArtLease = { promise: Promise<DisplayArt>; release(): void };
type AuthorizationGroup = { users: number; timer: ReturnType<typeof setTimeout> | null; executionIdentity?: string;
  pending?: ArtLease; listeners: Set<(error: unknown) => void>; selection: ArtSelection };
type Owner = { cache: SourceArtCache<DisplayArt>; scope: string; groups: Map<string, AuthorizationGroup> };
const caches = new WeakMap<EditorApi, Owner>();
function clearOwner(owner: Owner, error?: unknown) {
  const listeners = [...owner.groups.values()].flatMap(group => [...group.listeners]);
  for (const group of owner.groups.values()) { if (group.timer) clearTimeout(group.timer); group.pending?.release(); }
  owner.groups.clear(); owner.cache.clear();
  if (error) for (const listener of listeners) listener(error);
}
export function clearSourceArt(api: EditorApi) {
  const owner = caches.get(api);
  // Keep the API's scheduler while cancelled decoders await acknowledgement.
  // A refresh clears content, not the outstanding execution credit.
  if (owner) clearOwner(owner);
}
function invalid(): never { throw new Error("The service returned invalid source artwork."); }
const hash = (value: unknown) => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);

export function allocateArt(response: SourceArtResponse, selected: ArtSelection): ArtAllocation<DisplayArt> {
  validateArtScope(response, selected);
  const data = response.data;
  const identity = canonicalArt([response.version, response.identity]);
  const metadataBytes = new TextEncoder().encode(identity).length;
  if (selected.query.kind === "waveform") {
    if (data.kind !== "waveform" || response.identity.algorithm !== "source-peaks128-v1" ||
        data.sample_rate !== selected.query.sample_rate || data.bucket_size !== 128 ||
        !Number.isInteger(data.channels) || data.channels < 1 || data.channels > 64 ||
        !/^(0|[1-9][0-9]{0,19})$/.test(data.total_samples) || BigInt(data.total_samples) > 18446744073709551615n ||
        canonicalArt(data.projection) !== canonicalArt(selected.query.projection) ||
        !Array.isArray(data.amplitudes) || data.amplitudes.length > 4096 || data.amplitudes.length !== selected.query.projection.bins ||
        !data.amplitudes.every(value => Number.isFinite(value) && value >= 0 && Number.isFinite(Math.fround(value)))) invalid();
    const amplitudes = Float32Array.from(data.amplitudes);
    const bytes = metadataBytes + 128 + amplitudes.byteLength;
    const fingerprint = canonicalArt([identity, bytesToHex(sha256(new Uint8Array(amplitudes.buffer))), data.channels, data.sample_rate]);
    return { bytes, fingerprint, materialize: () => ({ bytes,
      fingerprint, value: { kind: "waveform", amplitudes, channels: data.channels, sampleRate: data.sample_rate, executionIdentity: identity }, dispose() {} }) };
  }
  if (response.identity.algorithm !== "classic-thumbnail-1s-jpeg08-v1") invalid();
  if (data.kind === "thumbnail_no_frame") {
    if (data.reason !== "no_sample_at_one_second") invalid();
    return { bytes: metadataBytes, fingerprint: canonicalArt([identity, data]), materialize: () => ({
      bytes: metadataBytes, fingerprint: canonicalArt([identity, data]), value: { kind: "no_frame", executionIdentity: identity }, dispose() {} }) };
  }
  if (data.kind !== "thumbnail_ready" || !Number.isInteger(data.width) || data.width < 1 || data.width > 1280 ||
      !Number.isInteger(data.height) || data.height < 1 || data.height > 720 || !hash(data.content_hash) ||
      typeof data.jpeg_bytes_hex !== "string" || !data.jpeg_bytes_hex.length || data.jpeg_bytes_hex.length > 2 * 1024 * 1024 ||
      !/^(?:[a-f0-9]{2})+$/.test(data.jpeg_bytes_hex)) invalid();
  const jpeg = hexToBytes(data.jpeg_bytes_hex);
  if (jpeg[0] !== 255 || jpeg[1] !== 216 || bytesToHex(sha256(jpeg)) !== data.content_hash) invalid();
  const bytes = metadataBytes + 128 + jpeg.byteLength + data.width * data.height * 4;
  const fingerprint = canonicalArt([identity, data.content_hash, data.width, data.height]);
  return { bytes, fingerprint, materialize: () => {
    const url = URL.createObjectURL(new Blob([jpeg], { type: "image/jpeg" }));
    return { bytes, fingerprint, value: { kind: "thumbnail", url, width: data.width, height: data.height, executionIdentity: identity }, dispose: () => URL.revokeObjectURL(url) };
  } };
}

function readArt(owner: Owner, api: EditorApi, selection: ArtRequestSelection, authorization = false) {
  const key = artSelectionKey(selection), requestId = crypto.randomUUID();
  return owner.cache.acquire((authorization ? "authorization:" : "") + key, async signal => {
    const scope = { project_id: selection.projectId, revision: selection.revision, asset_id: selection.asset.id, request_id: requestId };
    let allocation: ArtAllocation<DisplayArt>, executionIdentity: string;
    if ("display" in selection) {
      if (typeof api.scene.waveformDisplay !== "function") throw new Error("Classic waveform display is unavailable on this service.");
      const response = await api.scene.waveformDisplay({ ...scope, ...selection.display }, signal);
      signal.throwIfAborted(); allocation = allocateWaveformDisplay(response, selection);
      executionIdentity = canonicalArt([response.version, response.identity]);
    } else {
      if (typeof api.scene.sourceArt !== "function") throw new Error("Source artwork is unavailable on this service.");
      const response = await api.scene.sourceArt({ ...scope, query: selection.query }, signal);
      signal.throwIfAborted(); allocation = allocateArt(response, selection);
      executionIdentity = canonicalArt([response.version, response.identity]);
    }
    signal.throwIfAborted();
    // The probe validates the same public receipt but retains neither another
    // JPEG/bitmap nor a second full waveform collection.
    return authorization ? { bytes: new TextEncoder().encode(executionIdentity).length, fingerprint: executionIdentity, materialize: () => ({
      bytes: new TextEncoder().encode(executionIdentity).length, fingerprint: executionIdentity,
      value: { kind: "no_frame", executionIdentity } as DisplayArt, dispose() {} }) } : allocation;
  }, async () => {
    if (typeof api.scene.cancelSourceArt === "function") await api.scene.cancelSourceArt({
      project_id: selection.projectId, revision: selection.revision, request_id: requestId });
  }, authorization);
}

function scheduleAuthorization(owner: Owner, api: EditorApi, key: string, group: AuthorizationGroup) {
  if (group.timer || group.pending || !group.users || owner.groups.get(key) !== group) return;
  group.timer = setTimeout(() => {
    group.timer = null;
    if (!group.users || owner.groups.get(key) !== group) return;
    try {
      const lease = readArt(owner, api, group.selection, true); group.pending = lease;
      void lease.promise.then(art => {
        lease.release(); group.pending = undefined;
        if (art.executionIdentity !== group.executionIdentity && owner.groups.get(key) === group) {
          clearOwner(owner, new Error("Source artwork renderer changed. Refresh the project.")); return;
        }
        scheduleAuthorization(owner, api, key, group);
      }, error => {
        lease.release(); group.pending = undefined;
        if (owner.groups.get(key) === group) clearOwner(owner, error);
      });
    } catch (error) { clearOwner(owner, error); }
  }, 1000);
}

export function acquireSourceArt(api: EditorApi, selection: ArtRequestSelection, onInvalidated: (error: unknown) => void = () => {}) {
  const scope = canonicalArt([selection.projectId, selection.revision]);
  let owner = caches.get(api);
  if (!owner) { owner = { cache: new SourceArtCache(), scope, groups: new Map() }; caches.set(api, owner); }
  if (owner.scope !== scope) { clearOwner(owner); owner.scope = scope; }
  owner.cache.activate(scope);
  const lease = readArt(owner, api, selection);
  const sampleRate = "display" in selection ? selection.display.sample_rate : selection.query.kind === "waveform" ? selection.query.sample_rate : null;
  const query: SourceArtQuery = sampleRate ? { kind: "waveform", sample_rate: sampleRate,
    projection: { kind: "source", range: null, bins: 1 } } : { kind: "thumbnail" };
  const key = canonicalArt([selection.asset.id, selection.asset.version, query]);
  let group = owner.groups.get(key);
  if (!group) { group = { users: 0, timer: null, listeners: new Set(), selection: {
    projectId: selection.projectId, revision: selection.revision, asset: selection.asset, query } }; owner.groups.set(key, group); }
  group.users++; group.listeners.add(onInvalidated);
  const capturedOwner = owner, capturedGroup = group;
  void lease.promise.then(art => {
    if (capturedOwner.groups.get(key) !== capturedGroup) return;
    if (capturedGroup.executionIdentity && capturedGroup.executionIdentity !== art.executionIdentity) {
      clearOwner(capturedOwner, new Error("Source artwork renderer changed. Refresh the project.")); return;
    }
    capturedGroup.executionIdentity = art.executionIdentity;
    scheduleAuthorization(capturedOwner, api, key, capturedGroup);
  }, () => {});
  let released = false;
  return { promise: lease.promise, release: () => {
    if (released) return; released = true; lease.release(); capturedGroup.listeners.delete(onInvalidated);
    if (--capturedGroup.users === 0) {
      if (capturedGroup.timer) clearTimeout(capturedGroup.timer);
      capturedGroup.pending?.release();
      if (capturedOwner.groups.get(key) === capturedGroup) capturedOwner.groups.delete(key);
    }
  } };
}
