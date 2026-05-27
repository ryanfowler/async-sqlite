# async-sqlite

This file provides guidance to AI agents when working with code in this repository. Keep this file updated after making changes.

## Project Overview

async-sqlite is a Rust library providing an asynchronous, runtime-agnostic wrapper around SQLite via `rusqlite`. It works with any async runtime (tokio, smol, etc.) by using background threads internally rather than depending on a specific runtime.

## Build Commands

```bash
cargo build                    # Build the library
cargo test                     # Run all tests
cargo fmt --all -- --check     # Check formatting
cargo clippy -- -D warnings    # Lint (CI treats all warnings as errors)
```

To run a single test:
```bash
cargo test <test_name>         # e.g., cargo test test_blocking_client
```

The minimum supported Rust version is 1.95.0.

## Architecture

The library has three core types in `src/`:

- **Client** (`client.rs`): Wraps a single SQLite connection. Spawns a background `std::thread` that receives commands (closures) via a `crossbeam_channel`. Results are returned through `futures_channel::oneshot`. Queued async commands are skipped if their reply channel is already canceled, and `ClientBuilder::queue_capacity()` can bound the worker queue. This design makes it runtime-agnostic. Client is cheaply cloneable.

- **Pool** (`pool.rs`): Manages multiple `Client` instances with round-robin selection via an atomic counter. Provides the same API as Client plus `conn_for_each()` for executing on all connections. File-backed and named shared-memory pools default to CPU-count connections; anonymous in-memory pools default to one connection and reject explicit multi-connection configuration. `PoolBuilder::queue_capacity()` applies the same per-client queue bound to every pool connection.

- **Error** (`error.rs`): Non-exhaustive enum wrapping config errors, queue-full errors, `rusqlite::Error`, channel errors, panics, and pragma failures.

All database operations use a closure-based API (e.g., `conn(|conn| { ... })`) to avoid lifetime issues with the cross-thread boundary. Both blocking and async variants exist for all operations.

## Features

All Cargo features are pass-through to `rusqlite`. The `rusqlite` dependency has `default-features = false`; async-sqlite explicitly defaults to `bundled` plus rusqlite's `cache` and `ffi-sqlite-wasm-rs` defaults. The library re-exports `rusqlite` for downstream use.

## Testing

Tests in `tests/tests.rs` use a `async_test!` macro that generates two variants of each async test: one running on tokio and one on smol. This ensures runtime compatibility. Tests use `tempfile` for temporary database files.
