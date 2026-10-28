import { test } from "node:test";
import assert from "node:assert/strict";
import { Input, ALL_FORMATS } from "mediabunny";
import { openVerifiedRangeSource } from "../src/verified-range-source.mjs";

const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};

test("hex ranges preserve every byte and canonical validation before decoding", async () => {
  const bytes = Buffer.alloc(65536); for (let i = 0; i < bytes.length; i++) bytes[i] = i % 256;
  const fixed = { assetId: "hex", contentHash: "a".repeat(64), size: String(bytes.length) };
  let encoded = bytes.toString("hex"), charged = 0;
  const capability = await openVerifiedRangeSource(fixed, {
    open: async () => ({ transferId: "hex", contentHash: fixed.contentHash, byteLength: fixed.size, maxChunkBytes: 65536 }),
    read: async () => ({ offset: "0", eof: true, bytesHex: encoded }), release: async () => {},
  }, { reserve: n => { charged += n; return () => { charged -= n; }; } }, 65536, undefined, { prefetch: false });
  try {
    assert.deepEqual(Buffer.from(await capability.readBytes(0, bytes.length)), bytes);
    for (const invalid of [encoded.toUpperCase(), encoded.slice(2), "gg" + encoded.slice(2), "\n0" + encoded.slice(2)]) {
      encoded = invalid;
      await assert.rejects(capability.readBytes(0, bytes.length), { code: "ASSET_HASH_MISMATCH" });
    }
  } finally { await capability.close(); }
  assert.equal(charged, 0);
});

for (const chunk of [65536, 524288]) test(`on-demand Mediabunny reads (${chunk}) leave no speculative worker and propagate foreground retirement`, async () => {
  const length = 4 * chunk, second = deferred(), resume = deferred();
  const fixed = { assetId: "retiring", contentHash: "a".repeat(64), size: String(length) };
  let reads = 0, released = 0, charged = 0;
  const capability = await openVerifiedRangeSource(fixed, {
    open: async () => ({ transferId: "retiring", contentHash: fixed.contentHash, byteLength: fixed.size, maxChunkBytes: chunk }),
    async read(_id, offset, count) {
      reads++;
      if (offset >= chunk) { second.resolve(); await resume.promise; }
      return { offset: String(offset), eof: offset + count === length, bytes: Uint8Array.from({ length: count }, (_, i) => (offset + i) % 251) };
    },
    release: async () => { released++; },
  }, { reserve: n => { charged += n; return () => { charged -= n; }; } }, chunk, undefined, { prefetch: false, maximumChunkBytes: chunk, readAhead: chunk === 524288 });
  const input = new Input({ source: capability.source, formats: ALL_FORMATS });
  try {
    await capability.source.getSize();
    // Exercise the pinned orchestrator without media decoding. A finished
    // request must not create the ownerless read reproduced in retirement-red.
    assert.equal((await capability.source._read(0, 1)).bytes[0], 0);
    const hit = await capability.source._read(7, 17);
    assert.deepEqual([...hit.bytes.subarray(7 - hit.offset, 17 - hit.offset)], [7,8,9,10,11,12,13,14,15,16]);
    await new Promise(resolve => setTimeout(resolve, 30));
    assert.equal(reads, chunk === 524288 ? 1 : 2);
    // Stay outside the pinned reader's128KiB worker-reuse tolerance.
    const next = capability.source._read(3 * chunk, 3 * chunk + 1);
    const rejected = assert.rejects(next, { code: "RESOURCE_SOURCE_CLOSED" });
    await second.promise;
    input.dispose();
    const closing = capability.close();
    assert.ok(charged > 0, "the pending read still owns its admitted bytes");
    resume.resolve();
    await Promise.all([closing, rejected]);
    await new Promise(resolve => setTimeout(resolve, 30));
    assert.equal(released, 1);
    assert.equal(charged, 0);
    await assert.rejects(capability.readBytes(0, 1), { code: "RESOURCE_SOURCE_CLOSED" });
  } finally { resume.resolve(); input.dispose(); await capability.close(); }
});

for (const code of ["NOT_AUTHORIZED", "ASSET_HASH_MISMATCH"]) test(`on-demand reads preserve an active ${code} failure`, async () => {
  const fixed = { assetId: "active", contentHash: "a".repeat(64), size: "65536" };
  let released = 0;
  const capability = await openVerifiedRangeSource(fixed, {
    open: async () => ({ transferId: "active", contentHash: fixed.contentHash, byteLength: fixed.size, maxChunkBytes: 65536 }),
    read: async () => { throw Object.assign(new Error(code), { code }); },
    release: async () => { released++; },
  }, { reserve: () => () => {} }, 65536, undefined, { prefetch: false });
  const input = new Input({ source: capability.source, formats: ALL_FORMATS });
  try {
    await capability.source.getSize();
    await assert.rejects(capability.source._read(0, 1), { code });
  } finally { input.dispose(); await capability.close(); }
  assert.equal(released, 1);
});
