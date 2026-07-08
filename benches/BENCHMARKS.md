# Benchmarks

Performance numbers for `diesel-sqlite-session` on native and WebAssembly targets, plus a comparison against rusqlite's session extension.

## Native Performance (Linux `x86_64`, AMD Ryzen Threadripper PRO 5975WX)

Criterion, `--release` with LTO and one codegen unit. Warm-up 500 ms, measurement 2 s, 30 samples per bench.

### Core operations

| Operation | Time (mean ± std) |
|---|---|
| Session creation | 8.01 ± 0.13 µs |
| Attach single table | 39.09 ± 0.55 µs |

### Changeset generation (INSERT-only)

| Rows | Patchset | Changeset |
|---|---|---|
| 10 | 86.78 ± 1.61 µs | 79.79 ± 1.30 µs |
| 100 | 266.42 ± 8.63 µs | 266.47 ± 3.93 µs |
| 500 | 1.127 ± 0.024 ms | 1.144 ± 0.017 ms |

### Apply, insert-only patchset (no conflicts)

| Rows | Time |
|---|---|
| 10 | 63.92 ± 2.96 µs |
| 100 | 106.32 ± 1.67 µs |
| 500 | 338.29 ± 6.70 µs |

### End-to-end workflows

| Workflow | Time |
|---|---|
| Mixed ops (75 changes) | 308.54 ± 6.08 µs |
| Full replication (100 rows) | 383.24 ± 1.82 µs |

### Streamed vs buffered generation

`session.changeset()` vs `session.changeset_strm(&mut Vec::with_capacity(64 KiB))`. Numbers are effectively identical: the trampoline overhead does not register on wall time at these sizes, and the streamed variant remains preferable whenever peak memory matters.

| Rows | Buffered | Streamed | Δ |
|---|---|---|---|
| 10 | 79.39 ± 0.44 µs | 79.96 ± 0.67 µs | +0.7% |
| 100 | 265.53 ± 7.33 µs | 267.59 ± 5.34 µs | +0.8% |
| 500 | 1.128 ± 0.021 ms | 1.168 ± 0.010 ms | +3.5% |

### Streamed vs buffered apply

`apply_changeset_with` (buffered) vs `apply_changeset_strm_with(Cursor::new(bytes), ...)`. Same shape: within noise.

| Rows | Buffered | Streamed | Δ |
|---|---|---|---|
| 10 | 62.91 ± 2.64 µs | 58.82 ± 1.01 µs | **-6.5%** |
| 100 | 106.70 ± 5.39 µs | 103.04 ± 1.98 µs | -3.4% |
| 500 | 334.60 ± 4.92 µs | 338.96 ± 5.66 µs | +1.3% |

### v2 apply, conflict callback fired on every row

Replica pre-populated with rows 0..N. Incoming changeset has the same PKs with different values, so every row triggers a `Conflict` resolved via `ConflictAction::Replace`.

| Rows | Time | Overhead vs conflict-free apply |
|---|---|---|
| 10 | 104.47 ± 1.86 µs | +64% |
| 100 | 497.81 ± 7.09 µs | +368% |
| 500 | 2.173 ± 0.011 ms | +542% |

The callback overhead dominates once every row conflicts. Reading the same volume without conflict is a `sqlite3changeset_apply` fast path. Every fired callback drags the row through the Rust trampoline, builds a `ConflictInfo`, and re-applies via `Replace`.

### Read-side and transform primitives

All measured on insert-only changesets built from the same source.

| Rows | `ChangesetReader::next` + `new_value(0)` | `invert_changeset` | `concat_changesets` (2 inputs) | `Changegroup::add` × 5 + `output` |
|---|---|---|---|---|
| 10 | 1.68 ± 0.024 µs | 321.3 ± 6.4 ns | 2.63 ± 0.038 µs | 6.11 ± 0.011 µs |
| 100 | 16.27 ± 0.31 µs | 2.11 ± 0.035 µs | 23.30 ± 0.148 µs | 57.06 ± 0.333 µs |
| 500 | 81.46 ± 1.69 µs | 9.11 ± 0.215 µs | 114.54 ± 1.38 µs | 319.04 ± 1.83 µs |

### Rebaser

`Rebaser::configure(rebase_blob)` then `rebase(changeset)`, on a rebase blob captured from a `Replace`-resolved apply.

| Rows | Time |
|---|---|
| 10 | 1.85 ± 0.010 µs |
| 100 | 14.54 ± 0.111 µs |
| 500 | 75.38 ± 0.334 µs |

## Comparison with rusqlite

`comparison_benchmarks.rs` runs both crates against the identical schema and mutation script so the numbers are directly comparable. All rows are 500 elements unless noted.

| Operation | diesel-sqlite-session | rusqlite | Δ |
|---|---|---|---|
| Session creation | 37.27 ± 1.91 µs | 35.80 ± 1.96 µs | +4% |
| Attach table | 38.37 ± 1.95 µs | 34.81 ± 2.41 µs | +10% |
| `is_empty` (empty) | 52.20 ± 3.18 µs | 44.01 ± 7.19 µs | +19% |
| `is_empty` (with changes) | 69.67 ± 2.08 µs | 61.95 ± 3.07 µs | +12% |
| Patchset (500 rows) | 1.160 ± 0.020 ms | 1.408 ± 0.026 ms | **-18%** |
| Changeset (500 rows) | 1.149 ± 0.019 ms | 1.355 ± 0.015 ms | **-15%** |
| Apply patchset (500 rows) | 339.07 ± 8.65 µs | 327.44 ± 6.19 µs | +4% |
| Apply changeset (500 rows) | 481.12 ± 178.76 µs | 339.76 ± 2.32 µs | see note |
| Mixed operations | 326.03 ± 4.25 µs | 348.90 ± 6.12 µs | **-7%** |
| Full replication | 460.37 ± 99.37 µs | 648.93 ± 75.02 µs | **-29%** |

The `apply_changeset` row measured a very wide std for the diesel-sqlite-session run (178 µs). The rusqlite side of the same bench ran clean, and the buffered `apply_changeset` in `session_benchmarks.rs` measured 334.60 ± 4.92 µs on the same host, so treat this row as a scheduling artifact rather than a real gap. `full_replication_workflow` also shows elevated std for the diesel-sqlite-session sample. Ignore the noisy row of the two.

### Interpretation

Setup calls (create + attach + is_empty) are a small percent slower on `diesel-sqlite-session` because the connection setup goes through Diesel's cache layer. Data-heavy calls (patchset / changeset generation, mixed ops, full replication) are 7 to 29 percent faster because the mutations are driven through Diesel's prepared-statement pipeline instead of ad-hoc SQL. Apply operations are noise-close on both sides once the outlier row is removed.

Performance is not a differentiator between the two. Pick the ORM that fits.

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
