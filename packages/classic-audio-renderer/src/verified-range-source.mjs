import { StreamSource } from "mediabunny";

// Transport adaptation only. The pinned Mediabunny reader owns random access,
// prefetch and LRU eviction; the Rust host owns CAS verification and authority.
const CHUNK_BYTES = 65536, MAX_CACHE_BYTES = 8 * 1024 * 1024;
const sources = new WeakSet();
function fail(code, message) { const error = new Error(message); error.code = code; throw error; }
function integer(value, minimum, maximum = Number.MAX_SAFE_INTEGER) {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum)
    fail("INVALID_RESOURCE_RANGE", "Resource range is outside its admitted bounds");
  return value;
}
function sizeOf(value) {
  if (typeof value === "string" && !/^[1-9][0-9]*$/.test(value))
    fail("INVALID_RESOURCE_RANGE", "Resource length is not canonical");
  return integer(Number(value), 1);
}
export function rangeCacheBytes(maximum, resourceCount, maximumChunkBytes = CHUNK_BYTES, readAhead = false) {
  if (![CHUNK_BYTES, 512 * 1024].includes(maximumChunkBytes)) fail("INVALID_RESOURCE_RANGE", "Invalid bounded range transport");
  integer(maximum, 1); integer(resourceCount, 1, 32);
  // Two upstream read workers, bounded hex/copy transport and the cache all
  // belong to one resident budget. File length is deliberately absent here.
  const cache = Math.floor((Math.floor(maximum / resourceCount) - maximumChunkBytes * (readAhead ? 8 : 6)) / 3);
  if (cache < CHUNK_BYTES) fail("RESOURCE_BUDGET_EXCEEDED", "No bounded range cache fits the execution");
  return Math.min(MAX_CACHE_BYTES, cache);
}
export const isVerifiedRangeSource = value => sources.has(value);

export async function openVerifiedRangeSource(resource, access, budget, cacheBytes, signal, { prefetch = true, maximumChunkBytes = CHUNK_BYTES, readAhead = false } = {}) {
  if (![CHUNK_BYTES, 512 * 1024].includes(maximumChunkBytes)) fail("INVALID_RESOURCE_RANGE", "Invalid bounded range transport");
  const length = sizeOf(resource?.size);
  if (typeof resource.assetId !== "string" || !resource.assetId || !/^[a-f0-9]{64}$/.test(resource.contentHash)
      || !access || ["open", "read", "release"].some(key => typeof access[key] !== "function")
      || typeof budget?.reserve !== "function") fail("INVALID_RESOURCE_RANGE", "Missing fixed range capability");
  integer(cacheBytes, CHUNK_BYTES, MAX_CACHE_BYTES);
  cacheBytes = Math.min(cacheBytes, length);
  if (typeof readAhead !== "boolean") fail("INVALID_RESOURCE_RANGE", "Invalid bounded read-ahead");
  const transportBytes = maximumChunkBytes * (readAhead ? 8 : 6);
  const releaseBudget = budget.reserve(cacheBytes * 3 + transportBytes);
  const fixed = Object.freeze({ ...resource }), pending = new Set(), lines = new Map(), loading = new Map();
  let transfer, closed = false, closing;
  const check = () => {
    if (signal?.aborted) fail("CANCELLED", "Verified source read cancelled");
    if (closed) fail("RESOURCE_SOURCE_CLOSED", "Verified source capability closed");
  };
  try {
    check(); transfer = await access.open(fixed); check();
    if (transfer?.contentHash !== fixed.contentHash
        || transfer.byteLength !== String(length) || ![CHUNK_BYTES, 512 * 1024].includes(transfer.maxChunkBytes) || transfer.maxChunkBytes > maximumChunkBytes
        || typeof transfer.transferId !== "string" || !transfer.transferId)
      fail("ASSET_HASH_MISMATCH", "Verified source descriptor changed its fixed identity");
    async function readBytes(start, count) {
      check(); integer(start, 0, length); integer(count, 1, Math.min(transfer.maxChunkBytes, length - start));
      if (pending.size >= 2) fail("RESOURCE_BUDGET_EXCEEDED", "Range reader exceeded two worker credits");
      const work = (async () => {
        const chunk = await access.read(transfer.transferId, start, count); check();
        if (chunk?.offset !== String(start) || chunk.eof !== (start + count === length))
          fail("ASSET_HASH_MISMATCH", "Verified source interval changed");
        if (chunk.bytes instanceof Uint8Array && chunk.bytes.buffer instanceof ArrayBuffer) {
          if (chunk.bytes.byteLength !== count) fail("ASSET_HASH_MISMATCH", "Verified source interval changed length");
          return chunk.bytes.slice();
        }
        if (typeof chunk.bytesHex !== "string" || chunk.bytesHex.length !== count * 2 || !/^(?:[0-9a-f]{2})+$/.test(chunk.bytesHex))
          fail("ASSET_HASH_MISMATCH", "Verified source bytes are not a bounded canonical interval");
        // Canonical lowercase/length validation remains above. Native decoding
        // avoids per-byte substrings; older engines use the same exact nibbles.
        if (typeof Uint8Array.fromHex === "function") return Uint8Array.fromHex(chunk.bytesHex);
        const bytes = new Uint8Array(count);
        for (let n = 0; n < count; n++) {
          const high = chunk.bytesHex.charCodeAt(n * 2), low = chunk.bytesHex.charCodeAt(n * 2 + 1);
          bytes[n] = ((high <= 57 ? high - 48 : high - 87) << 4) | (low <= 57 ? low - 48 : low - 87);
        }
        return bytes;
      })();
      pending.add(work);
      try { return await work; } finally { pending.delete(work); }
    }
    async function readStreamBytes(start, count) {
      if (!readAhead) return readBytes(start, count);
      check();
      // Cross-process reads have higher call overhead than a direct file read. Reuse
      // two aligned, verified transport lines beneath Mediabunny's own LRU;
      // never turn its small demux request into an unbounded network prefetch.
      const offset = Math.floor(start / transfer.maxChunkBytes) * transfer.maxChunkBytes;
      let bytes = lines.get(offset);
      if (!bytes) {
        let work = loading.get(offset);
        if (!work) {
          work = readBytes(offset, Math.min(transfer.maxChunkBytes, length - offset)).then(value => {
            check(); lines.set(offset, value);
            while (lines.size > 2) lines.delete(lines.keys().next().value);
            return value;
          }).finally(() => loading.delete(offset));
          loading.set(offset, work);
        }
        bytes = await work;
      } else { lines.delete(offset); lines.set(offset, bytes); }
      check();
      return bytes.slice(start - offset, Math.min(bytes.length, start - offset + count));
    }
    const close = () => closing ??= (async () => {
      closed = true;
      try { await Promise.allSettled([...pending]); await access.release(transfer.transferId); }
      finally { lines.clear(); loading.clear(); releaseBudget(); }
    })();
    const source = new StreamSource({ getSize: () => { check(); return length; }, maxCacheSize: cacheBytes,
      prefetchProfile: prefetch ? "fileSystem" : "none",
      read(start, end) {
        check(); integer(start, 0, length - 1); integer(end, start + 1, length);
        let offset = start;
        // A demuxer may request a large interval. Feed it bounded chunks rather
        // than allocating end-start or accumulating another complete file.
        return new ReadableStream({ async pull(controller) {
          try {
            if (offset === end) { controller.close(); return; }
            const bytes = await readStreamBytes(offset, Math.min(transfer.maxChunkBytes, end - offset));
            offset += bytes.length; controller.enqueue(bytes);
          } catch (error) { controller.error(error); }
        } }, { highWaterMark: 0 });
      },
      dispose() { void close().catch(() => {}); },
    });
    const value = Object.freeze({ resource: fixed, length, source, readBytes, close,
      transportBytes,
      residentBytes: cacheBytes * 3 + transportBytes });
    sources.add(value); return value;
  } catch (error) {
    try { if (transfer?.transferId) await access.release(transfer.transferId); }
    finally { releaseBudget(); }
    throw error;
  }
}
