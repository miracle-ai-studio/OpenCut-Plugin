import { test } from "node:test";
import assert from "node:assert/strict";
import { blake3 } from "hash-wasm";
import { hashPreviewPicture } from "../dist/preview-digest.js";

const source = new Uint8Array(960 * 540 * 4 + 37);
for (let index = 0; index < source.length; index++) source[index] = (index * 17 + (index >>> 5)) & 255;

test("SIMD full digests match the independent implementation across block, chunk and offset boundaries", async () => {
  assert.equal(await hashPreviewPicture(new Uint8Array()), "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
  for (const size of [1, 63, 64, 65, 1023, 1024, 1025, 65535, 65536, 65537, 960 * 540 * 4]) {
    const bytes = source.subarray(13, 13 + size);
    assert.equal(await hashPreviewPicture(bytes), await blake3(bytes), `size=${size}`);
  }
});

test("simultaneous first calls and repeated requests cannot interleave digest state", async () => {
  const { hashPreviewPicture: fresh } = await import("../dist/preview-digest.js?concurrent");
  const inputs = Array.from({ length: 12 }, (_, index) => source.subarray(index, index + 65536 + index));
  const expected = await Promise.all(inputs.map(bytes => blake3(bytes)));
  assert.deepEqual(await Promise.all(inputs.map(bytes => fresh(bytes))), expected);
  assert.deepEqual(await Promise.all(inputs.toReversed().map(bytes => fresh(bytes))), expected.toReversed());
});

test("the final byte of a complete admitted 540p frame remains covered", async () => {
  const frame = source.slice(0, 960 * 540 * 4), expected = await blake3(frame);
  frame[frame.length - 1] ^= 1;
  assert.notEqual(await hashPreviewPicture(frame), expected);
  assert.equal(await hashPreviewPicture(frame), await blake3(frame));
});

test("abort during initialization and over-budget bytes reject without a replacement implementation", async () => {
  const { hashPreviewPicture: fresh } = await import("../dist/preview-digest.js?abort");
  const abort = new AbortController();
  const pending = fresh(source, abort.signal);
  abort.abort();
  await assert.rejects(pending, { name: "AbortError" });
  await assert.rejects(fresh(source, abort.signal), { name: "AbortError" });
  await assert.rejects(fresh(new Uint8Array(64 * 1024 * 1024 + 1)), RangeError);
  assert.equal(await fresh(source), await blake3(source));
});

test("unsupported SIMD fails explicitly and never retries with a different implementation", async () => {
  const original = WebAssembly.instantiate;
  let attempts = 0;
  WebAssembly.instantiate = async () => { attempts++; throw new WebAssembly.CompileError("SIMD unavailable"); };
  try {
    const { hashPreviewPicture: unavailable } = await import("../dist/preview-digest.js?unsupported");
    for (let index = 0; index < 2; index++)
      await assert.rejects(unavailable(source), { code: "PREVIEW_DIGEST_UNAVAILABLE" });
    assert.equal(attempts, 1);
  } finally { WebAssembly.instantiate = original; }
});
