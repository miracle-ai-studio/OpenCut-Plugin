import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { OpenCutClient } from "../dist/index.js";
const session = { session_id: "session", generation: "3", overlay_epoch: "0", owner_id: "owner", lease_id: "lease" };
const bytes = new Uint8Array([255, 0, 0, 255, 0, 0, 255, 128]);
const picture = { picture_id: "a".repeat(32), width: 2, height: 1, byte_length: 8, content_hash: createHash("sha256").update(bytes).digest("hex") };
function fixture(response) {
  const calls = [];
  const client = new OpenCutClient({ baseUrl: "http://localhost:8788", token: "private", fetch: async (url, init) => {
    calls.push({ url, init }); return response();
  } });
  return { client, calls };
}
const headers = { "content-type": "application/vnd.opencut.rgba", "content-length": "8" };
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
  for (const change of [{ width: 0 }, { width: 1.5 }, { height: 16385 }, { byte_length: 7 }, { byte_length: 64 * 1024 * 1024 + 4 }, { content_hash: "bad" }])
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
