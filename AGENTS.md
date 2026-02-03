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

The minimum supported Rust version is 1.92.0.

## Architecture

The library has three core types in `src/`:

- **Client** (`client.rs`): Wraps a single SQLite connection. Spawns a background `std::thread` that receives commands (closures) via a `crossbeam_channel`. Results are returned through `futures_channel::oneshot`. This design makes it runtime-agnostic. Client is cheaply cloneable.

- **Pool** (`pool.rs`): Manages multiple `Client` instances with round-robin selection via an atomic counter. Provides the same API as Client plus `conn_for_each()` for executing on all connections. Defaults to CPU-count connections.

- **Error** (`error.rs`): Non-exhaustive enum wrapping `rusqlite::Error`, channel errors, and pragma failures.

All database operations use a closure-based API (e.g., `conn(|conn| { ... })`) to avoid lifetime issues with the cross-thread boundary. Both blocking and async variants exist for all operations.

## Features

All Cargo features are pass-through to `rusqlite`. The `bundled` feature (default) bundles SQLite. The library re-exports `rusqlite` for downstream use.

## Testing

Tests in `tests/tests.rs` use a `async_test!` macro that generates two variants of each async test: one running on tokio and one on smol. This ensures runtime compatibility. Tests use `tempfile` for temporary database files.
