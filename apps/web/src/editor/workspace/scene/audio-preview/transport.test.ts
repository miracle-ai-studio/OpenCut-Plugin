import { afterEach, expect, it, vi } from "vitest";
import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import type { AudioPreviewPcmHeader } from "../../../../lib/editor-sdk";
import { setupAudioPreview } from "../audio-preview-fixture";
import { readPreviewPcm } from "./transport";

afterEach(() => vi.restoreAllMocks());
const encode = (header: AudioPreviewPcmHeader, floats: number[]) => {
  const payload = new Uint8Array(floats.length * 4),
    pv = new DataView(payload.buffer);
  floats.forEach((n, i) => pv.setFloat32(i * 4, n, true));
  const json = new TextEncoder().encode(
    JSON.stringify({
      ...header,
      content_hash: header.content_hash || bytesToHex(sha256(payload)),
    }),
  );
  const result = new Uint8Array(12 + json.length + payload.length),
    v = new DataView(result.buffer);
  result.set(new TextEncoder().encode("OCAP"));
  v.setUint32(4, json.length, true);
  v.setUint32(8, payload.length, true);
  result.set(json, 12);
  result.set(payload, 12 + json.length);
  return result;
};
const response = (chunks: Uint8Array[]) =>
  new Response(
    new ReadableStream({
      start(c) {
        chunks.forEach((chunk) => c.enqueue(chunk));
        c.close();
      },
    }),
    { headers: { "content-type": "application/vnd.opencut.audio-preview" } },
  );
async function context() {
  const f = await setupAudioPreview();
  const s = f.sessionFor({
    project_id: "project",
    revision: "fixed",
    sequence_id: "scene",
    sample_rate: 48000,
    start_time: { ticks: "0", timebase: 1 },
    idempotency_key: "request",
  });
  s.total_frames = "2";
  const header: AudioPreviewPcmHeader = {
    pcm_stage: "mastered",
    session_id: s.session_id,
    identity: s.identity,
    generation: s.generation,
    overlay_epoch: s.overlay_epoch,
    sequence: "0",
    start_sample: "0",
    frame_count: 2,
    content_hash: "",
  };
  return { f, s, header };
}
it("verifies exact identity and Float32 LE while parsing split prefix/header/payload boundaries", async () => {
  const { f, s, header } = await context();
  try {
    const frame = encode(header, [0.5, -0.5, 0.25, -0.25]);
    const chunks = Array.from(frame, (_, i) => frame.slice(i, i + 1));
    const blocks = [];
    for await (const block of readPreviewPcm(
      response(chunks),
      s,
      new AbortController().signal,
    ))
      blocks.push([...block]);
    expect(blocks).toEqual([[0.5, -0.5, 0.25, -0.25]]);
  } finally {
    f.cleanup();
  }
});
it("requires completed native mastering before yielding samples to a direct device graph", async () => {
  const { f, s, header } = await context();
  try {
    for (const pcm_stage of [undefined, "unmastered", "unknown"]) {
      const malformed = {
        ...header,
        pcm_stage,
      } as unknown as AudioPreviewPcmHeader;
      await expect(
        readPreviewPcm(
          response([encode(malformed, [0, 0, 0, 0])]),
          s,
          new AbortController().signal,
        ).next(),
      ).rejects.toThrow();
    }
  } finally {
    f.cleanup();
  }
});
it("rejects another revision/generation, checksum mismatch, source discontinuity and non-finite samples", async () => {
  const { f, s, header } = await context();
  try {
    const bad = [
      { ...header, identity: { ...header.identity, revision: "other" } },
      { ...header, overlay_epoch: "9" },
      { ...header, generation: "9" },
      { ...header, sequence: "1" },
      { ...header, start_sample: "1" },
      { ...header, content_hash: "0".repeat(64) },
    ];
    for (const h of bad)
      await expect(
        readPreviewPcm(
          response([encode(h, [0, 0, 0, 0])]),
          s,
          new AbortController().signal,
        ).next(),
      ).rejects.toThrow();
    await expect(
      readPreviewPcm(
        response([encode(header, [Infinity, 0, 0, 0])]),
        s,
        new AbortController().signal,
      ).next(),
    ).rejects.toThrow("non-finite");
  } finally {
    f.cleanup();
  }
});
it("rejects transport overflow and a truncated fixed scene instead of claiming a complete stream", async () => {
  const { f, s, header } = await context();
  try {
    await expect(
      readPreviewPcm(
        response([new Uint8Array((12 + 4096 + 32768) * 32 + 1)]),
        s,
        new AbortController().signal,
      ).next(),
    ).rejects.toThrow("budget");
    const one = encode({ ...header, frame_count: 1 }, [0, 0]);
    const stream = readPreviewPcm(
      response([one]),
      s,
      new AbortController().signal,
    );
    await stream.next();
    await expect(stream.next()).rejects.toThrow("ended before");
  } finally {
    f.cleanup();
  }
});
