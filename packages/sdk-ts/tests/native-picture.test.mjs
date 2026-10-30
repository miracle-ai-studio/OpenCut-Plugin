import { test } from "node:test";
import assert from "node:assert/strict";
import { OpenCutClient } from "../dist/index.js";
const session = { session_id: "session", generation: "3", overlay_epoch: "0", owner_id: "owner", lease_id: "lease" };
const bytes = new Uint8Array([255, 0, 0, 255, 0, 0, 255, 128]);
const picture = { picture_id: "a".repeat(32), width: 2, height: 1, byte_length: 8, content_digest: { algorithm: "blake3", value: "ca11f55b29f153340105820d5bc7fa0417bf8697604c94465a029b9b76451da2" } };
function fixture(response) {
  const calls = [];
  const client = new OpenCutClient({ baseUrl: "http://localhost:8788", token: "private", fetch: async (url, init) => {
    calls.push({ url, init }); return response();
  } });
  return { client, calls };
}
const headers = { "content-type": "application/vnd.opencut.rgba", "content-length": "8" };
test("540p digest verifies the complete payload and rejects corruption beyond the first64KiB", async () => {
  const length = 960 * 540 * 4;
  const expected = Uint8Array.from({ length }, (_, n) => n * 37 % 251);
  // Independent Noble BLAKE3 vector, also checked by the native test program.
  const descriptor = { ...picture, width: 960, height: 540, byte_length: length,
    content_digest: { algorithm: "blake3", value: "a0df3135546b83ed5aead8ecf34b17987adb6029f66861adcc460e93fa344ce1" } };
  for (const corrupt of [false, true]) {
    const data = expected.slice();
    if (corrupt) data[length - 1] ^= 1;
    const f = fixture(() => new Response(new ReadableStream({ start(controller) {
      controller.enqueue(data.subarray(0, 65537));
      controller.enqueue(data.subarray(65537)); controller.close();
    } }), { headers: { ...headers, "content-length": String(length) } }));
    const result = f.client.readVisualPreviewPicture(session, descriptor);
    if (corrupt) await assert.rejects(result, /content hash/);
    else assert.deepEqual(new Uint8Array((await result).buffer), expected);
  }
});
test("native picture consumes scoped bounded chunks and verifies length/hash before returning RGBA", async () => {
  const f = fixture(() => new Response(new ReadableStream({ start(c) { c.enqueue(bytes.slice(0, 3)); c.enqueue(bytes.slice(3)); c.close(); } }), { headers }));
  const signal = new AbortController().signal;
  const result = await f.client.readVisualPreviewPicture(session, picture, { signal });
  assert.ok(result instanceof Uint8ClampedArray); assert.deepEqual([...result], [...bytes]);
  const { url, init } = f.calls[0];
  assert.equal(new URL(url).pathname, "/api/preview/picture/stream");
  assert.equal(init.method, "POST"); assert.equal(init.signal, signal); assert.equal(init.redirect, "error");
  assert.equal(new Headers(init.headers).get("Authorization"), "Bearer private");
  assert.deepEqual(JSON.parse(init.body), { session, picture_id: picture.picture_id });
  assert.equal(f.calls.length, 1);
});
test("invalid picture geometry, hash and admission are rejected before transport", async () => {
  const f = fixture(() => { throw Error("Must not fetch"); });
  for (const change of [{ width: 0 }, { width: 1.5 }, { height: 16385 }, { byte_length: 7 }, { byte_length: 64 * 1024 * 1024 + 4 },
    { content_digest: { algorithm: "blake3", value: "bad" } },
    { content_digest: { algorithm: "sha256", value: picture.content_digest.value } },
    { content_digest: { value: picture.content_digest.value } },
    { content_digest: undefined, content_hash: picture.content_digest.value }])
    await assert.rejects(f.client.readVisualPreviewPicture(session, { ...picture, ...change }), /descriptor/);
  assert.equal(f.calls.length, 0);
});
test("changed transport metadata cancels without reading or exposing pixels", async () => {
  for (const change of [{ "content-type": "image/png" }, { "content-length": "9" }]) {
    let cancelled = false;
    const f = fixture(() => new Response(new ReadableStream({ cancel() { cancelled = true; } }), { headers: { ...headers, ...change } }));
    await assert.rejects(f.client.readVisualPreviewPicture(session, picture), /descriptor/);
    assert.equal(cancelled, true);
  }
});
test("truncation, excess and corrupt pixels cannot pass the descriptor", async () => {
  for (const data of [bytes.slice(0, 7), new Uint8Array(9), new Uint8Array(8)]) {
    const f = fixture(() => new Response(data, { headers }));
    await assert.rejects(f.client.readVisualPreviewPicture(session, picture), /length|bound|hash/);
    assert.equal(f.calls.length, 1);
  }
});
test("abort retires an active picture reader and never retries its single-use ticket", async () => {
  const abort = new AbortController(); let cancelled = false;
  const f = fixture(() => new Response(new ReadableStream({
    pull(c) { c.enqueue(bytes.slice(0, 4)); abort.abort(Error("Retired owner")); },
    cancel() { cancelled = true; },
  }), { headers }));
  await assert.rejects(f.client.readVisualPreviewPicture(session, picture, { signal: abort.signal }), /Retired owner/);
  assert.equal(cancelled, true); assert.equal(f.calls.length, 1);
  await assert.rejects(f.client.readVisualPreviewPicture(session, picture, { signal: abort.signal }), /Retired owner/);
  assert.equal(f.calls.length, 1);
});
test("a valid scoped response releases the next intent before pixels are complete", async () => {
  let controller, claimed;
  const claim = new Promise(resolve => { claimed = resolve; });
  const f = fixture(() => new Response(new ReadableStream({ start(c) { controller = c; } }), { headers }));
  let completed = false;
  const pending = f.client.readVisualPreviewPicture(session, picture, { onClaimed: claimed }).then(value => { completed = true; return value; });
  await claim;
  assert.equal(completed, false);
  controller.enqueue(bytes); controller.close();
  assert.deepEqual([...(await pending)], [...bytes]);
  assert.equal(f.calls.length, 1);
  assert.deepEqual(JSON.parse(f.calls[0].init.body), { session, picture_id: picture.picture_id });
});
test("invalid or aborted response never releases an unclaimed intent", async () => {
  let claims = 0;
  for (const changed of [{ ...headers, "content-length": "9" }, headers]) {
    const abort = new AbortController();
    const f = fixture(() => {
      if (changed === headers) abort.abort(Error("Retired response"));
      return new Response(bytes, { headers: changed });
    });
    await assert.rejects(f.client.readVisualPreviewPicture(session, picture, { signal: abort.signal, onClaimed: () => { claims++; } }));
  }
  assert.equal(claims, 0);
});
test("a failing claim observer closes its reader without retrying the ticket", async () => {
  let cancelled = false;
  const f = fixture(() => new Response(new ReadableStream({ cancel() { cancelled = true; } }), { headers }));
  await assert.rejects(f.client.readVisualPreviewPicture(session, picture, { onClaimed: () => { throw Error("Retired pipeline"); } }), /Retired pipeline/);
  assert.equal(cancelled, true); assert.equal(f.calls.length, 1);
});
