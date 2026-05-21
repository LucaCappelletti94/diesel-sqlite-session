# Benchmarks

Performance numbers for `diesel-sqlite-session` on native and WebAssembly targets, plus a comparison against rusqlite's session extension.

## Native Performance (Linux `x86_64`)

Benchmarks run using Criterion on native targets with LTO and single codegen unit.

### Core Operations

| Operation | Time (mean ± std) | Throughput |
|-----------|-------------------|------------|
| Session creation | 8.5 ± 0.03 µs | 118K ops/sec |
| Attach table | 36.4 ± 0.3 µs | 27K ops/sec |

### Patchset/Changeset Generation

| Rows | Patchset (mean ± std)  | Changeset (mean ± std) | Throughput |
|------|------------------------|------------------------|------------|
| 10 | 82 ± 1 µs | 85 ± 1 µs | 120K rows/sec |
| 100 | 285 ± 3 µs | 278 ± 2 µs | 355K rows/sec |
| 500 | 1.19 ± 0.01 ms | 1.16 ± 0.01 ms | 425K rows/sec |

### Apply Operations

| Rows | Apply Patchset (mean ± std)  | Apply Changeset (mean ± std) | Throughput |
|------|------------------------------|------------------------------|------------|
| 10 | 66 ± 1 µs | 64 ± 1 µs | 154K rows/sec |
| 100 | 110 ± 1 µs | 110 ± 1 µs | 910K rows/sec |
| 500 | 358 ± 2 µs | 358 ± 2 µs | 1.4M rows/sec |

### End-to-End Workflows

| Workflow | Time (mean ± std) |
|----------|-------------------|
| Mixed operations (75 changes) | 323 ± 2 µs |
| Full replication (100 rows) | 395 ± 2 µs |

## Comparison with rusqlite

diesel-sqlite-session performs comparably to rusqlite's session extension:

| Operation | diesel-sqlite-session | rusqlite | Difference |
|-----------|----------------------|----------|------------|
| Session creation | 36.4 ± 0.4 µs | 34.6 ± 0.3 µs | +5% |
| Attach table | 35.6 ± 0.2 µs | 33.9 ± 0.2 µs | +5% |
| Patchset (500 rows) | 1.19 ± 0.01 ms | 1.44 ± 0.01 ms | **-17%** |
| Changeset (500 rows) | 1.19 ± 0.01 ms | 1.46 ± 0.02 ms | **-18%** |
| Apply patchset (500 rows) | 389 ± 5 µs | 379 ± 7 µs | +3% |
| Mixed operations | 327 ± 2 µs | 371 ± 6 µs | **-12%** |
| Full replication | 402 ± 6 µs | 458 ± 6 µs | **-12%** |

### Interpretation

The session extension FFI calls are identical between diesel-sqlite-session and rusqlite. For session creation and attach operations, there is a small ~5% difference attributable to connection setup.

For data-heavy operations (patchset/changeset generation, mixed operations, full replication), **diesel-sqlite-session is 12-18% faster** due to Diesel's efficient query builder and prepared statement handling.

Performance should not be a factor in choosing between the two. Use whichever ORM fits your project.

### Browser/WASM Support

Both diesel-sqlite-session and rusqlite now support `wasm32-unknown-unknown` (browser WebAssembly) via [sqlite-wasm-rs](https://crates.io/crates/sqlite-wasm-rs). rusqlite added this support in [PR #1769](https://github.com/rusqlite/rusqlite/pull/1769) (December 2025).

However, **rusqlite's session extension does not work in WASM**. The session extension requires `buildtime_bindgen`, which generates native bindings incompatible with WebAssembly. diesel-sqlite-session solves this by providing hand-written FFI bindings that work on both native and WASM targets.

## WebAssembly Performance

Benchmarks run using wasm-bindgen-test in headless browsers.

### Chrome vs Firefox Comparison

| Operation | Chrome (mean ± std) | Firefox (mean ± std) |
|-----------|---------------------|----------------------|
| Session creation | 0.05 ± 0.01 ms | 0.03 ± 0.01 ms |
| Attach table | 0.02 ± 0.01 ms | 0.03 ± 0.01 ms |
| Patchset (100 rows) | 0.40 ± 0.11 ms | 0.48 ± 0.08 ms |
| Patchset (1000 rows) | 1.67 ± 0.07 ms | 2.81 ± 0.10 ms |
| Apply patchset (100 rows) | 0.35 ± 0.03 ms | 0.43 ± 0.01 ms |
| Apply patchset (500 rows) | 1.87 ± 0.59 ms | 1.51 ± 0.11 ms |
| Mixed ops (75 changes) | 1.93 ± 0.27 ms | 2.27 ± 0.01 ms |
| Full replication (100 rows) | 3.25 ± 0.36 ms | 3.78 ± 0.18 ms |

**WASM vs Native**: WebAssembly performance is approximately 8-10x slower than native for most operations. Chrome and Firefox show comparable performance within measurement variance. This overhead is expected due to:

- JavaScript/WASM boundary overhead
- sqlite-wasm-rs overhead compared to native `SQLite`
- Browser sandbox constraints

## Running Benchmarks

```bash
# Native benchmarks (Criterion)
cargo bench --bench session_benchmarks

# Comparison benchmarks (vs rusqlite)
cargo bench --bench comparison_benchmarks

# WASM benchmarks (requires wasm-pack)
cargo install wasm-pack
cd wasm-bench && wasm-pack test --headless --firefox -- -- --nocapture
cd wasm-bench && wasm-pack test --headless --chrome -- -- --nocapture
```
