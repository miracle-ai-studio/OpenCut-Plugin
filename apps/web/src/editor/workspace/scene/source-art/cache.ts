// Match the host's background decoder credit, leaving foreground preview room.
export const SOURCE_ART_LIMITS = Object.freeze({ bytes: 64 * 1024 * 1024, entries: 256, concurrent: 1 });
export type MaterializedArt<T> = { value: T; bytes: number; fingerprint: string; dispose(): void };
export type ArtAllocation<T> = { bytes: number; fingerprint: string; materialize(): MaterializedArt<T> };
type Load<T> = (signal: AbortSignal) => Promise<ArtAllocation<T>>;
type Entry<T> = {
  key: string; users: number; used: number; cancelled: boolean; background: boolean;
  state: "queued" | "running" | "ready"; abort: AbortController; cancel(): void | Promise<void>;
  termination?: Promise<void>;
  load: Load<T>; value?: MaterializedArt<T>; promise: Promise<T>;
  resolve(value: T): void; reject(reason: unknown): void;
};
const cancelled = () => new DOMException("Source art cancelled", "AbortError");

/** Derived display data only. Reattaching an unused entry reauthorizes through
 * the loader; sharing an active request never increases its native work count. */
export class SourceArtCache<T> {
  private entries = new Map<string, Entry<T>>();
  private running = 0;
  private bytes = 0;
  private clock = 0;
  private foregroundRun = 0;
  private scope: string | null = null;
  constructor(private readonly limits: { bytes: number; entries: number; concurrent: number } = SOURCE_ART_LIMITS) {}
  activate(scope: string) {
    if (this.scope !== scope) { this.clear(); this.scope = scope; }
  }
  acquire(key: string, load: Load<T>, cancel: () => void | Promise<void>, background = false) {
    let entry = this.entries.get(key);
    if (entry?.users) { entry.users++; entry.used = ++this.clock; return this.lease(entry); }
    const retained = entry?.value;
    if (!entry && this.entries.size >= this.limits.entries) {
      this.evictUntil(() => this.entries.size < this.limits.entries);
      if (this.entries.size >= this.limits.entries) throw new Error("Source artwork cache is full.");
    }
    let resolve!: (value: T) => void, reject!: (reason: unknown) => void;
    const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail; });
    // Cancellation can happen before a component attaches its await handler.
    void promise.catch(() => {});
    entry = { key, users: 1, used: ++this.clock, state: "queued", abort: new AbortController(),
      cancelled: false, background, load, cancel, value: retained, promise, resolve, reject };
    // A renewed authorization joins the queue at its current request time,
    // rather than inheriting an older retained entry's scheduling position.
    this.entries.delete(key); this.entries.set(key, entry); this.pump();
    return this.lease(entry);
  }
  private lease(entry: Entry<T>) {
    let released = false;
    return { promise: entry.promise, release: () => {
      if (released) return; released = true;
      if (--entry.users > 0 || entry.cancelled) return;
      if (entry.state !== "ready") this.remove(entry);
    } };
  }
  private remove(entry: Entry<T>) {
    if (this.entries.get(entry.key) === entry) this.entries.delete(entry.key);
    if (entry.value) { this.bytes -= entry.value.bytes; entry.value.dispose(); entry.value = undefined; }
    if (entry.state !== "ready" && !entry.cancelled) {
      entry.cancelled = true; entry.abort.abort(); entry.reject(cancelled());
      if (entry.state === "running") entry.termination = Promise.resolve().then(() => entry.cancel()).then(() => {}, () => {});
    }
  }
  private evictUntil(done: () => boolean, excluding?: Entry<T>) {
    const unused = [...this.entries.values()].filter(e => e !== excluding && !e.users && e.state === "ready")
      .sort((a, b) => a.used - b.used);
    for (const entry of unused) { if (done()) break; this.remove(entry); }
  }
  private pump() {
    while (this.running < this.limits.concurrent) {
      const queued = [...this.entries.values()].filter(entry => entry.state === "queued");
      const foreground = queued.find(entry => !entry.background), background = queued.find(entry => entry.background);
      // New visible content can pass queued renewal probes, but at most three
      // foreground loads can pass a waiting probe. Never preempt a decoder or
      // release its credit before cancellation is acknowledged.
      const entry = background && (!foreground || this.foregroundRun >= 3) ? background : foreground;
      if (!entry) break;
      this.foregroundRun = entry.background || !background ? 0 : Math.min(3, this.foregroundRun + 1);
      entry.state = "running"; this.running++;
      void this.run(entry);
    }
  }
  private async run(entry: Entry<T>) {
    try {
      const allocation = await entry.load(entry.abort.signal);
      if (entry.cancelled || this.entries.get(entry.key) !== entry) return;
      if (!Number.isSafeInteger(allocation.bytes) || allocation.bytes < 0 || allocation.bytes > this.limits.bytes)
        throw new Error("Source artwork exceeds the display budget.");
      if (entry.value?.fingerprint === allocation.fingerprint) {
        entry.state = "ready"; entry.resolve(entry.value.value); return;
      }
      const retainedBytes = entry.value?.bytes ?? 0;
      this.evictUntil(() => this.bytes - retainedBytes + allocation.bytes <= this.limits.bytes, entry);
      if (this.bytes - retainedBytes + allocation.bytes > this.limits.bytes)
        throw new Error("Active source artwork exceeds the display budget.");
      if (entry.value) { this.bytes -= entry.value.bytes; entry.value.dispose(); entry.value = undefined; }
      entry.value = allocation.materialize(); this.bytes += entry.value.bytes;
      entry.state = "ready"; entry.resolve(entry.value.value);
    } catch (error) {
      if (!entry.cancelled) { entry.reject(error); this.remove(entry); }
    } finally { await entry.termination; this.running--; this.pump(); }
  }
  clear() { for (const entry of [...this.entries.values()]) this.remove(entry); }
}
