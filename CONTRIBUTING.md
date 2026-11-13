# Contributing

Install Git, rustup, and a native Rust linker for your host. On Windows x64, install the MSVC C++ build tools and Windows SDK. Use Xcode Command Line Tools on macOS and a C toolchain on Linux. Rustup reads the pinned channel and components from `rust-toolchain.toml`. [Rust toolchain overrides](https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file)

Run these commands from the repository root.

```sh
rustc -Vv
cargo fmt --all -- --check
cargo build --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

The first build needs the registry packages in `Cargo.lock`. After `cargo fetch --locked`, pass `--offline` to the Cargo build, lint, or test command to use the local cache. Commit dependency changes and the corresponding lockfile together. [Cargo offline mode](https://doc.rust-lang.org/cargo/reference/config.html#netoffline)

Use the worktree and integration order in the [implementation plan](PLAN.md#22-parallel-execution-with-codex-worktrees). Put input bytes, including encoding and line endings, through the same paths as existing callers. Preserve v1 field names and meanings when adding optional fields to reports. Update the contract tests for changes to serialization or gate behavior.
