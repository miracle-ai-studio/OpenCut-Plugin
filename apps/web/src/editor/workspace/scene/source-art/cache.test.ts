import { expect, it, vi } from "vitest";
import { SourceArtCache } from "./cache";
const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
const allocation = (value: string, bytes = 4, dispose = vi.fn()) => ({ bytes, fingerprint: value,
  materialize: () => ({ bytes, fingerprint: value, value, dispose }) });

it("prioritizes newly visible artwork while serving a waiting authorization after at most three foreground loads", async () => {
  const cache = new SourceArtCache<string>({ bytes: 128, entries: 32, concurrent: 1 });
  const started: string[] = [], finish = new Map<string, () => void>();
  const acquire = (key: string, background = false) => cache.acquire(key, () => new Promise<ReturnType<typeof allocation>>(resolve => {
    started.push(key); finish.set(key, () => resolve(allocation(key)));
  }), vi.fn(), background);
  const leases = [acquire("running-probe", true), acquire("waiting-probe", true),
    ...Array.from({ length: 5 }, (_, i) => acquire(`visible-${i}`))];
  try {
    finish.get("running-probe")!(); await flush();
    expect(started).toEqual(["running-probe", "visible-0"]);
    for (const key of ["visible-0", "visible-1", "visible-2"]) { finish.get(key)!(); await flush(); }
    expect(started).toEqual(["running-probe", "visible-0", "visible-1", "visible-2", "waiting-probe"]);
    finish.get("waiting-probe")!(); await flush();
    expect(started.at(-1)).toBe("visible-3");
  } finally { cache.clear(); finish.forEach(done => done()); await flush(); leases.forEach(lease => lease.release()); }
});

it("shares active promises and waits for last-waiter cancellation acknowledgement before starting another decoder", async () => {
  const cache = new SourceArtCache<string>({ bytes: 32, entries: 8, concurrent: 2 });
  const signals: AbortSignal[] = [];
  const load = vi.fn((signal: AbortSignal) => new Promise<ReturnType<typeof allocation>>((_, reject) => {
    signals.push(signal); signal.addEventListener("abort", () => reject(new DOMException("abort", "AbortError")));
  }));
  let cancelled!: () => void;
  const cancel = vi.fn(() => new Promise<void>(resolve => { cancelled = resolve; }));
  const a = cache.acquire("a", load, cancel), duplicate = cache.acquire("a", load, cancel);
  const b = cache.acquire("b", load, vi.fn());
  const thirdLoad = vi.fn(async () => allocation("third"));
  const third = cache.acquire("c", thirdLoad, vi.fn());
  expect(a.promise).toBe(duplicate.promise); expect(load).toHaveBeenCalledTimes(2);
  a.release(); expect(signals[0].aborted).toBe(false);
  duplicate.release(); await flush();
  expect(signals[0].aborted).toBe(true); expect(cancel).toHaveBeenCalledOnce();
  expect(thirdLoad).not.toHaveBeenCalled();
  cancelled(); await flush();
  expect(await third.promise).toBe("third");
  b.release(); third.release(); cache.clear();
});

it("reauthorizes unused hot entries, evicts LRU allocations and refuses to grow past active memory", async () => {
  const cache = new SourceArtCache<string>({ bytes: 8, entries: 2, concurrent: 2 });
  const disposeA = vi.fn(), disposeB = vi.fn();
  const loadA = vi.fn(async () => allocation("A", 4, disposeA));
  const a = cache.acquire("a", loadA, vi.fn()); await a.promise; a.release(); await flush();
  const again = cache.acquire("a", loadA, vi.fn()); expect(await again.promise).toBe("A");
  expect(loadA).toHaveBeenCalledTimes(2); expect(disposeA).not.toHaveBeenCalled(); again.release(); await flush();
  const b = cache.acquire("b", async () => allocation("B", 4, disposeB), vi.fn()); await b.promise; await flush();
  const c = cache.acquire("c", async () => allocation("C", 4), vi.fn()); await c.promise;
  expect(disposeA).toHaveBeenCalledOnce();
  expect(() => cache.acquire("d", async () => allocation("D"), vi.fn())).toThrow("full");
  b.release(); c.release(); cache.clear(); expect(disposeB).toHaveBeenCalledOnce();
});

it("rejects an active-byte overflow before materializing and disposes all ready content on scope change", async () => {
  const cache = new SourceArtCache<string>({ bytes: 8, entries: 8, concurrent: 2 });
  cache.activate("project:revision1");
  const dispose = vi.fn(), first = cache.acquire("a", async () => allocation("A", 8, dispose), vi.fn()); await first.promise;
  const make = vi.fn(() => ({ value: "B", bytes: 1, fingerprint: "B", dispose() {} }));
  const next = cache.acquire("b", async () => ({ bytes: 1, fingerprint: "B", materialize: make }), vi.fn());
  await expect(next.promise).rejects.toThrow("Active"); expect(make).not.toHaveBeenCalled();
  cache.activate("project:revision2"); expect(dispose).toHaveBeenCalledOnce(); first.release();
});

it("does not publish late completed work from a cleared scope or dispatch cancelled queue entries", async () => {
  const cache = new SourceArtCache<string>({ bytes: 8, entries: 8, concurrent: 1 });
  let resolve!: (value: ReturnType<typeof allocation>) => void;
  const cancel = vi.fn(), materialize = vi.fn(allocation("late").materialize);
  const first = cache.acquire("a", () => new Promise(accept => { resolve = accept; }), cancel);
  const load = vi.fn(async () => allocation("queued")), queuedCancel = vi.fn();
  const queued = cache.acquire("b", load, queuedCancel); queued.release(); cache.clear();
  resolve({ bytes: 4, fingerprint: "late", materialize }); await flush();
  await expect(first.promise).rejects.toHaveProperty("name", "AbortError");
  expect(materialize).not.toHaveBeenCalled(); expect(load).not.toHaveBeenCalled(); expect(queuedCancel).not.toHaveBeenCalled();
  expect(cancel).toHaveBeenCalledOnce();
});

it("serves a new visible request before four slow authorization groups can renew ahead of it", async () => {
  const cache = new SourceArtCache<string>({ bytes: 32, entries: 8, concurrent: 2 });
  const keys = ["proof1", "proof2", "proof3", "proof4"];
  const seeded = keys.map(key => cache.acquire(key, async () => allocation(key), vi.fn()));
  await Promise.all(seeded.map(lease => lease.promise)); seeded.forEach(lease => lease.release()); await flush();
  const started: string[] = [], completes = new Map<string, () => void>();
  const slow = (key: string) => () => new Promise<ReturnType<typeof allocation>>(resolve => {
    started.push(key); completes.set(key, () => resolve(allocation(key)));
  });
  const probes = keys.map(key => cache.acquire(key, slow(key), vi.fn()));
  const visibleLoad = vi.fn(async () => { started.push("visible"); return allocation("visible"); });
  const visible = cache.acquire("visible", visibleLoad, vi.fn());
  try {
    expect(started).toEqual(["proof1", "proof2"]);
    completes.get("proof1")!(); completes.get("proof2")!(); await flush();
    expect(started).toEqual(["proof1", "proof2", "proof3", "proof4"]);
    probes[0].release(); probes[1].release();
    // The first groups' next authorization interval arrives while the other
    // two groups still occupy both decoders. Their old cache keys stay hot.
    const renewals = ["proof1", "proof2"].map(key => cache.acquire(key, slow(`renew-${key}`), vi.fn()));
    completes.get("proof3")!(); await flush();
    expect(await visible.promise).toBe("visible");
    expect(started.indexOf("visible")).toBe(4);
    expect(started.indexOf("renew-proof1")).toBeGreaterThan(started.indexOf("visible"));
    renewals.forEach(lease => lease.release());
  } finally {
    cache.clear(); completes.forEach(complete => complete()); await flush();
    probes.forEach(lease => lease.release()); visible.release();
  }
});

it("keeps one default loader and admits over 32 MiB while refusing over 64 MiB of active display data", async () => {
  const cache = new SourceArtCache<string>();
  let finish!: (value: ReturnType<typeof allocation>) => void;
  const first = cache.acquire("large-visible", () => new Promise(resolve => { finish = resolve; }), vi.fn());
  const materialize = vi.fn(allocation("overflow", 25 * 1024 * 1024).materialize);
  const nextLoad = vi.fn(async () => ({ bytes: 25 * 1024 * 1024, fingerprint: "overflow", materialize }));
  const next = cache.acquire("next-visible", nextLoad, vi.fn());
  expect(nextLoad).not.toHaveBeenCalled();
  finish(allocation("large-visible", 40 * 1024 * 1024));
  expect(await first.promise).toBe("large-visible");
  await expect(next.promise).rejects.toThrow("Active source artwork exceeds the display budget");
  expect(materialize).not.toHaveBeenCalled();
  first.release(); cache.clear();
});
