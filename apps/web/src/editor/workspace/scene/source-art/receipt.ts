import type { ToolOutput } from "../../../../lib/editor-sdk";
import type { Asset } from "../types";

export const canonicalArt = (value: unknown): string => JSON.stringify(ordered(value));
function ordered(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(ordered);
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).sort(([a], [b]) => a.localeCompare(b)).map(([key, child]) => [key, ordered(child)]));
  return value;
}
function invalid(): never { throw new Error("The service returned invalid source artwork."); }
const hash = (value: unknown) => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
const text = (value: unknown) => typeof value === "string" && value.trim().length > 0 && value.length <= 1024 && !/[\u0000-\u001f\u007f-\u009f]/.test(value);

export function validateArtScope(response: Pick<ToolOutput<"opencut_source_art_get">, "project_id" | "revision" | "asset_id" | "version" | "identity">,
  selected: { projectId: string; revision: string; asset: Asset }) {
  if (!response || response.project_id !== selected.projectId ||
      response.revision !== selected.revision || response.asset_id !== selected.asset.id ||
      canonicalArt(response.version) !== canonicalArt(selected.asset.version) || !response.identity) invalid();
  const identity = response.identity;
  if (!("worker_sha256" in identity) || identity.decode_profile !== "fixed-cas-native-libav-concat-linear-skia-jpeg" ||
      identity.rasterizer !== "skia-safe-0.153.3-srgb-jpeg80" || !hash(identity.worker_sha256) || !hash(identity.module_sha256) ||
      !["source-peaks128-v1", "classic-thumbnail-1s-jpeg08-v1", "source-frame-at-jpeg08-v2"].includes(identity.algorithm) ||
      canonicalArt(Object.keys(identity).sort()) !== canonicalArt(["algorithm", "decode_profile", "decoder_libraries", "module_sha256", "platform", "rasterizer", "worker_sha256"].sort()) ||
      !identity.platform || canonicalArt(Object.keys(identity.platform).sort()) !== canonicalArt(["architecture", "kernel_release", "os", "system_build"]) ||
      !Object.values(identity.platform).every(text) || !Array.isArray(identity.decoder_libraries) || identity.decoder_libraries.length !== 5) invalid();
  const names = new Set<string>();
  for (const library of identity.decoder_libraries) {
    if (!library || !["avcodec", "avformat", "avutil", "swscale", "swresample"].includes(library.name) || names.has(library.name) ||
        canonicalArt(Object.keys(library).sort()) !== canonicalArt(["binary_sha256", "configuration_sha256", "name", "version"]) ||
        !text(library.version) || !hash(library.configuration_sha256) || !hash(library.binary_sha256)) invalid();
    names.add(library.name);
  }
}
