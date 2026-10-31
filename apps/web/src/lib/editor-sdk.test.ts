import { afterEach, expect, it, vi } from "vitest";
import { createEditorClient, editorHostCapabilities, type HostCapabilities } from "./editor-sdk";
afterEach(() => vi.unstubAllGlobals());
it.each(["", "captured-credential"])(
  "shares the captured cookie/bearer stream transport (%s)",
  async (token) => {
    const fetch = vi.fn(async () => new Response("bounded-stream"));
    vi.stubGlobal("fetch", fetch);
    const signal = new AbortController().signal;
    await createEditorClient({
      baseUrl: "http://localhost:1234/",
      token,
    }).openAudioPreviewStream(
      {
        session_id: "fixed",
        generation: "2",
        overlay_epoch: "3",
        owner_id: "owner",
        lease_id: "lease",
      },
      { signal },
    );
    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("http://localhost:1234/api/preview/audio/stream");
    expect(init.signal).toBe(signal);
    expect(init.redirect).toBe("error");
    expect(new Headers(init.headers).get("Authorization")).toBe(
      token ? `Bearer ${token}` : null,
    );
    expect(init.credentials).toBe(token ? undefined : "same-origin");
    expect(JSON.parse(init.body as string)).toEqual({
      session_id: "fixed",
      generation: "2",
      overlay_epoch: "3",
      owner_id: "owner",
      lease_id: "lease",
    });
  },
);

function discovery() {
  let resolve!: (value: HostCapabilities) => void;
  let reject!: (error: Error) => void;
  const request = new Promise<HostCapabilities>((yes, no) => { resolve = yes; reject = no; });
  const api = { connectionIdentity: "service/principal/credential", sdk: {
    hostCapabilities: vi.fn(() => request),
  } };
  const value = { tools: ["opencut_audio_preview_recovery_retire"] } as HostCapabilities;
  return { api, resolve, reject, value };
}

it("shares simultaneous authenticated discovery, with independent values and a fresh later read", async () => {
  const f = discovery();
  const readers = Array.from({length: 4}, () => editorHostCapabilities(f.api));
  expect(f.api.sdk.hostCapabilities).toHaveBeenCalledTimes(1);
  f.resolve(f.value);
  const values = await Promise.all(readers);
  values[0].tools.length = 0;
  expect(values[1].tools).toEqual(f.value.tools);
  expect(f.value.tools).toHaveLength(1);
  await editorHostCapabilities(f.api);
  expect(f.api.sdk.hostCapabilities).toHaveBeenCalledTimes(2);
});

it("does not share discovery across connection objects or before authentication", async () => {
  const f = discovery();
  const separate = { ...f.api };
  const unauthenticated = { ...f.api, connectionIdentity: null };
  const readers = [f.api, separate, unauthenticated, unauthenticated].map(editorHostCapabilities);
  expect(f.api.sdk.hostCapabilities).toHaveBeenCalledTimes(4);
  f.resolve(f.value);
  await Promise.all(readers);
});

it("retires a failed shared read so the next explicit discovery can succeed", async () => {
  const f = discovery();
  const first = editorHostCapabilities(f.api), second = editorHostCapabilities(f.api);
  const failed = Promise.allSettled([first, second]);
  f.reject(new Error("connection lost"));
  expect((await failed).map(r => r.status)).toEqual(["rejected", "rejected"]);
  f.api.sdk.hostCapabilities.mockResolvedValue(f.value);
  expect(await editorHostCapabilities(f.api)).toEqual(f.value);
  expect(f.api.sdk.hostCapabilities).toHaveBeenCalledTimes(2);
});

it("rejects an old identity without clearing discovery already started for its replacement", async () => {
  const old = discovery(), next = discovery();
  const stale = editorHostCapabilities(old.api);
  const rejected = expect(stale).rejects.toThrow("connection changed");
  old.api.connectionIdentity = "service/another-principal/credential";
  old.api.sdk.hostCapabilities.mockImplementation(next.api.sdk.hostCapabilities);
  const fresh = editorHostCapabilities(old.api);
  old.resolve(old.value);
  await rejected;
  const shared = editorHostCapabilities(old.api);
  expect(old.api.sdk.hostCapabilities).toHaveBeenCalledTimes(2);
  next.resolve(next.value);
  expect(await fresh).toEqual(next.value);
  expect(await shared).toEqual(next.value);
});
