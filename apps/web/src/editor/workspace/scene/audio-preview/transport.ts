import { DEVICE_BUFFER_BLOCKS } from "./buffer-limits";
import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import type { AudioPreviewPcmHeader } from "../../../../lib/editor-sdk";
import type { AudioPreviewSession } from "../types";

const MAX_FRAME = 12 + 4096 + 32768;
const decimal = (s: unknown): s is string =>
  typeof s === "string" &&
  /^(0|[1-9][0-9]{0,19})$/.test(s) &&
  BigInt(s) <= 18446744073709551615n;
export function sameAudioIdentity(
  a: AudioPreviewSession["identity"],
  b: AudioPreviewSession["identity"],
) {
  return (
    a.project_id === b.project_id &&
    a.revision === b.revision &&
    a.sequence_id === b.sequence_id &&
    a.profile === b.profile &&
    a.sample_rate === b.sample_rate &&
    a.channels === b.channels
  );
}

/** The SDK opens the authenticated stream. This decoder checks publication,
 * continuity and resource limits without reconstructing project/source clocks. */
export async function* readPreviewPcm(
  response: Response,
  session: AudioPreviewSession,
  signal: AbortSignal,
): AsyncGenerator<Float32Array> {
  if (
    response.headers.get("content-type")?.split(";")[0].trim() !==
      "application/vnd.opencut.audio-preview" ||
    !response.body
  )
    throw new Error("This service returned an invalid scene audio stream.");
  const reader = response.body.getReader();
  let pending = new Uint8Array(0),
    sequence = 0n,
    start = BigInt(session.actual_start_sample);
  try {
    for (;;) {
      signal.throwIfAborted();
      if (pending.length >= 12) {
        const view = new DataView(
          pending.buffer,
          pending.byteOffset,
          pending.byteLength,
        );
        const headerLength = view.getUint32(4, true),
          payloadLength = view.getUint32(8, true);
        if (
          String.fromCharCode(...pending.subarray(0, 4)) !== "OCAP" ||
          headerLength < 2 ||
          headerLength > 4096 ||
          payloadLength < 8 ||
          payloadLength > 32768
        )
          throw new Error("Invalid scene audio frame envelope.");
        const size = 12 + headerLength + payloadLength;
        if (pending.length >= size) {
          const header: AudioPreviewPcmHeader = JSON.parse(
            new TextDecoder("utf-8", { fatal: true }).decode(
              pending.subarray(12, 12 + headerLength),
            ),
          );
          const payload = pending.subarray(12 + headerLength, size);
          if (
            header.pcm_stage !== "mastered" ||
            header.session_id !== session.session_id ||
            !header.identity ||
            !sameAudioIdentity(header.identity, session.identity) ||
            header.generation !== session.generation ||
            header.overlay_epoch !== session.overlay_epoch ||
            !decimal(header.sequence) ||
            BigInt(header.sequence) !== sequence ||
            !decimal(header.start_sample) ||
            BigInt(header.start_sample) !== start ||
            !Number.isInteger(header.frame_count) ||
            header.frame_count < 1 ||
            header.frame_count > 4096 ||
            payloadLength !== header.frame_count * 8 ||
            start + BigInt(header.frame_count) > BigInt(session.total_frames) ||
            !/^[a-f0-9]{64}$/.test(header.content_hash) ||
            bytesToHex(sha256(payload)) !== header.content_hash
          )
            throw new Error(
              "Scene audio frame identity, continuity or checksum is invalid.",
            );
          const pcm = new Float32Array(header.frame_count * 2),
            samples = new DataView(
              payload.buffer,
              payload.byteOffset,
              payload.byteLength,
            );
          for (let i = 0; i < pcm.length; i++) {
            pcm[i] = samples.getFloat32(i * 4, true);
            if (!Number.isFinite(pcm[i]))
              throw new Error("Scene audio contains non-finite PCM.");
          }
          pending = pending.slice(size);
          sequence++;
          start += BigInt(header.frame_count);
          signal.throwIfAborted();
          yield pcm;
          continue;
        }
      }
      const { value, done } = await reader.read();
      if (done) {
        if (pending.length || start !== BigInt(session.total_frames))
          throw new Error(
            "Scene audio stream ended before its fixed scene range.",
          );
        return;
      }
      if (value.length + pending.length > MAX_FRAME * DEVICE_BUFFER_BLOCKS)
        throw new Error("Scene audio exceeds its bounded transport budget.");
      const joined = new Uint8Array(pending.length + value.length);
      joined.set(pending);
      joined.set(value, pending.length);
      pending = joined;
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
