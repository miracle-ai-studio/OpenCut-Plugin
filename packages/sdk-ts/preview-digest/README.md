# Browser preview digest

This private SDK build adapter compiles the already reviewed BLAKE3 1.8.7
algorithm with its upstream `wasm32_simd` feature. It performs only unkeyed
BLAKE3-256 over bounded bytes: no composition, decoding, clocks, resources,
authority, filesystem, network, WASI or other host imports. It is not a public
API and cannot render a picture. `hash-wasm` is retained only as an independent
test oracle; production has one digest implementation and no fallback chain.

The small standalone Cargo build intentionally targets WebAssembly separately
from the native application workspace, without changing native Cargo features,
modules, binaries or their reviewed dependency baseline. Its complete source,
locked crates and build recipe are checked against `provenance.json`. BLAKE3,
arrayvec, cfg-if and constant_time_eq license notices are in the SDK LICENSE
and the identical public Web license asset. No new third-party algorithm is
introduced. The SDK entry point and complete RGBA digest contract are unchanged.

Run `rustup target add wasm32-unknown-unknown` for the repository's pinned Rust
compiler, then `npm run digest:generate --prefix packages/sdk-ts`. A reproducible
rebuild/check is `npm run digest:check --prefix packages/sdk-ts`. Ordinary SDK
builds validate the recorded source and artifact hashes without invoking Rust.
The generated TypeScript embeds only the verified Wasm bytes, so both the
published Node SDK and the Web worker use the same portable artifact. Tests
compare independent known digests, offsets, chunk boundaries, concurrent calls,
abort and trailing-byte corruption. Current clients require WebAssembly SIMD;
initialization failure is explicit and does not choose another backend.

The Wasm adapter has a 64 KiB input buffer and 256 KiB maximum linear memory.
Input is capped at the existing 64 MiB picture admission. One instance is shared
per SDK realm; a complete hash runs synchronously after initialization, so JS
calls cannot interleave its reset/update/finalize state. No memory grows and no
whole-frame buffer is allocated in Wasm. The 64 KiB stack and fixed source/output
storage are included in that cap. The wrapper never trusts an external pointer.
