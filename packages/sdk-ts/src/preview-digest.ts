import { previewDigestWasm } from "./preview-digest.generated.js";

type DigestInstance = {
  memory: WebAssembly.Memory;
  input_ptr(): number;
  reset(): void;
  update(length: number): void;
  finish(): number;
};
const chunkBytes = 65_536;
let initialized: Promise<DigestInstance> | undefined;

function instance(): Promise<DigestInstance> {
  return initialized ??= (async () => {
    try {
      const bytes = Uint8Array.from(atob(previewDigestWasm), c => c.charCodeAt(0));
      const result = await WebAssembly.instantiate(bytes, {});
      return result.instance.exports as unknown as DigestInstance;
    } catch (cause) {
      throw Object.assign(new Error("Native picture verification requires WebAssembly SIMD", { cause }),
        { code: "PREVIEW_DIGEST_UNAVAILABLE" });
    }
  })();
}

/** The same full unkeyed BLAKE3 digest as the host. Only the implementation
 * changes; no pixel, algorithm, authority or publication contract changes.
 * Calls share one bounded instance. There is no await between reset and finish,
 * so concurrent callers cannot interleave mutable hashing state. */
export async function hashPreviewPicture(bytes: Uint8Array, signal?: AbortSignal): Promise<string> {
  signal?.throwIfAborted();
  if (bytes.byteLength > 64 * 1024 * 1024) throw new RangeError("Native picture exceeds digest admission");
  const wasm = await instance();
  signal?.throwIfAborted();
  const input = new Uint8Array(wasm.memory.buffer, wasm.input_ptr(), chunkBytes);
  wasm.reset();
  for (let offset = 0; offset < bytes.byteLength; offset += chunkBytes) {
    const chunk = bytes.subarray(offset, offset + chunkBytes);
    input.set(chunk);
    wasm.update(chunk.byteLength);
  }
  const digest = new Uint8Array(wasm.memory.buffer, wasm.finish(), 32);
  return Array.from(digest, value => value.toString(16).padStart(2, "0")).join("");
}
