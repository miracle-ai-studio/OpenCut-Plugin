import { test } from "node:test";
import assert from "node:assert/strict";
import { OpenCutClient, OpenCutApiError, picturePreparationTransport as transport } from "../dist/index.js";

const bytes = Uint8Array.of(255, 0, 0, 255, 0, 0, 255, 128);
const picture = { picture_id: "a".repeat(32), width: 2, height: 1, byte_length: bytes.length,
  content_digest: { algorithm: "blake3", value: "ca11f55b29f153340105820d5bc7fa0417bf8697604c94465a029b9b76451da2" } };
const frame = { picture, plan: { identity: { width: 2, height: 1 } } };
const header = { kind: "scene", frame };
const input = { kind: "scene", frame: { session: { session_id: "s", owner_id: "o", lease_id: "l", generation: "3", overlay_epoch: "0" } } };
function encode(value = header, pixels = bytes) {
  const metadata = new TextEncoder().encode(JSON.stringify(value));
  const wire = new Uint8Array(12 + metadata.length + pixels.length);
  wire.set(new TextEncoder().encode("OCVP"));
  const view = new DataView(wire.buffer);
  view.setUint32(4, metadata.length, true); view.setUint32(8, pixels.length, true);
  wire.set(metadata, 12); wire.set(pixels, 12 + metadata.length);
  return wire;
}
function fixture(response) {
  const calls = [];
  const client = new OpenCutClient({ baseUrl: "http://localhost:8788", token: "private", fetch: async (url, init) => {
    calls.push({ url, init }); return response();
  } });
  return { client, calls };
}
function response(body, length, extra = {}) {
  return new Response(body, { headers: { "content-type": transport.content_type, "content-length": String(length), ...extra } });
}
test("single preparation request parses split/coalesced framing and verifies exact pixels", async () => {
  const wire = encode();
  for (const size of [1, 11, 13, 31, wire.length]) {
    const f = fixture(() => response(new ReadableStream({ start(c) {
      for (let at = 0; at < wire.length; at += size) c.enqueue(wire.slice(at, at + size));
      c.close();
    } }), wire.length));
    const result = await f.client.prepareVisualPreviewPicture(input);
    assert.deepEqual(result.header, header);
    assert.deepEqual([...(await result.pixels)], [...bytes]);
    assert.equal(f.calls.length, 1);
    assert.equal(new URL(f.calls[0].url).pathname, transport.path);
    assert.equal(f.calls[0].init.method, "POST"); assert.equal(f.calls[0].init.redirect, "error");
    assert.equal(new Headers(f.calls[0].init.headers).get("Authorization"), "Bearer private");
    assert.deepEqual(JSON.parse(f.calls[0].init.body), input);
  }
});
test("claimed metadata permits the next intent before the pixel body arrives", async () => {
  const wire = encode(); let stream;
  const f = fixture(() => response(new ReadableStream({ start(c) { stream = c; c.enqueue(wire.slice(0, -bytes.length)); } }), wire.length));
  const result = await f.client.prepareVisualPreviewPicture(input);
  let completed = false; void result.pixels.then(() => { completed = true; });
  assert.deepEqual(result.header, header); assert.equal(completed, false);
  stream.enqueue(bytes); stream.close();
  assert.deepEqual([...(await result.pixels)], [...bytes]); assert.equal(f.calls.length, 1);
});
test("edit draft carries its exact typed header through the same transport", async () => {
  const value = { kind: "edit_draft", draft: { draft_id: "d", transaction_digest: "digest", frame } }, wire = encode(value);
  const f = fixture(() => response(wire, wire.length));
  const result = await f.client.prepareVisualPreviewPicture({ kind: "edit_draft", draft: { draft_id: "d", frame: input.frame } });
  assert.deepEqual(result.header, value); assert.deepEqual([...(await result.pixels)], [...bytes]);
});
test("invalid HTTP framing cancels without exposing metadata or reading a body", async () => {
  for (const change of [{ "content-type": "image/png" }, { "content-length": "0" }, { "content-length": "0012" },
    { "content-length": String(12 + transport.max_header_bytes + 64 * 1024 * 1024 + 1) }]) {
    let cancelled = false;
    const f = fixture(() => response(new ReadableStream({ cancel() { cancelled = true; } }), encode().length, change));
    await assert.rejects(f.client.prepareVisualPreviewPicture(input), /transport/);
    assert.equal(cancelled, true); assert.equal(f.calls.length, 1);
  }
});
test("malformed magic and length bounds are rejected from the prefix before allocation", async () => {
  for (const mutate of [wire => { wire[0] = 0; }, wire => { new DataView(wire.buffer).setUint32(4, transport.max_header_bytes + 1, true); },
    wire => { new DataView(wire.buffer).setUint32(8, 64 * 1024 * 1024 + 1, true); },
    wire => { new DataView(wire.buffer).setUint32(4, 0, true); }, wire => { new DataView(wire.buffer).setUint32(8, 4, true); }]) {
    const wire = encode(); mutate(wire);
    const f = fixture(() => response(wire.slice(0, 12), wire.length));
    await assert.rejects(f.client.prepareVisualPreviewPicture(input), /magic|lengths/);
    assert.equal(f.calls.length, 1);
  }
});
test("wrong kind, missing picture and changed descriptor never expose prepared metadata", async () => {
  for (const value of [{ kind: "edit_draft", draft: { frame } }, { kind: "scene", frame: {} },
    { kind: "scene", frame: { picture: { ...picture, width: 3 } } },
    { kind: "scene", frame: { picture: { ...picture, content_digest: { algorithm: "blake3", value: "bad" } } } },
    { kind: "scene", frame: { picture: { ...picture, content_digest: { algorithm: "sha256", value: picture.content_digest.value } } } },
    { kind: "scene", frame: { picture: { ...picture, content_digest: null, content_hash: picture.content_digest.value } } }]) {
    const wire = encode(value), f = fixture(() => response(wire, wire.length));
    await assert.rejects(f.client.prepareVisualPreviewPicture(input), /kind|header|descriptor/);
  }
});
test("invalid UTF-8, JSON and truncated metadata cannot release the next preparation", async () => {
  for (const change of [wire => { wire[12] = 0xff; return wire; }, wire => { wire[12] = 0; return wire; }, wire => wire.slice(0, 14)]) {
    const wire = encode(), altered = change(wire), f = fixture(() => response(altered, wire.length));
    await assert.rejects(f.client.prepareVisualPreviewPicture(input)); assert.equal(f.calls.length, 1);
  }
});
test("truncated, excess and corrupt pixel bodies fail without retrying a claimed frame", async () => {
  for (const change of [wire => wire.slice(0, -1), wire => new Uint8Array([...wire, 0]), wire => { wire[wire.length - 1] ^= 1; return wire; }]) {
    const wire = encode(), f = fixture(() => response(change(wire), wire.length));
    // Excess can arrive coalesced with the prefix, so rejection may precede metadata.
    await assert.rejects(async () => { const result = await f.client.prepareVisualPreviewPicture(input); await result.pixels; }, /length|bound|hash/);
    assert.equal(f.calls.length, 1);
  }
});
test("external abort and local cancellation unblock a stalled prefix or pixel body", async () => {
  for (const stage of ["prefix", "body", "local"]) {
    const wire = encode(), abort = new AbortController(); let cancelled = false;
    const f = fixture(() => response(new ReadableStream({ start(c) {
      if (stage !== "prefix") c.enqueue(wire.slice(0, -bytes.length));
    }, cancel() { cancelled = true; } }), wire.length));
    const pending = f.client.prepareVisualPreviewPicture(input, { signal: abort.signal });
    if (stage === "prefix") {
      const failed = assert.rejects(pending, /Retired/);
      await new Promise(setImmediate); abort.abort(Error("Retired prefix")); await failed;
    } else {
      const result = await pending, failed = assert.rejects(result.pixels, /Retired/);
      if (stage === "local") result.cancel(Error("Retired local")); else abort.abort(Error("Retired owner"));
      await failed;
    }
    assert.equal(cancelled, true); assert.equal(f.calls.length, 1);
  }
});
test("authorization errors preserve the common API error and one-shot transport policy", async () => {
  const f = fixture(() => new Response(JSON.stringify({ request_id: "r", warnings: [], data: null,
    error: { code: "AUTHORIZATION_DENIED", message: "Revoked owner", details: null } }), { status: 403 }));
  await assert.rejects(f.client.prepareVisualPreviewPicture(input), error => error instanceof OpenCutApiError && error.code === "AUTHORIZATION_DENIED");
  assert.equal(f.calls.length, 1);
});
