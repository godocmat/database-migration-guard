# Contributing

Install Git, rustup, and a native Rust linker for your host. On Windows x64, install the MSVC C++ build tools and Windows SDK. Use Xcode Command Line Tools on macOS and a C toolchain on Linux. Rustup reads the pinned channel and components from `rust-toolchain.toml`. [Rust toolchain overrides](https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file)

## Native PostgreSQL parser

The pinned PostgreSQL parser builds its bundled C source with a native compiler. Bindgen loads libclang from `LIBCLANG_PATH`. Point it to the directory holding a library for your host architecture. [Bindgen requirements](https://rust-lang.github.io/rust-bindgen/requirements.html)

| Build host | Native prerequisites and libclang |
| --- | --- |
| Ubuntu 24.04 x64 | GCC, `clang-18`, and `libclang-18-dev`. Set `LIBCLANG_PATH=/usr/lib/llvm-18/lib` and `CLANG_PATH=/usr/bin/clang-18` so bindgen can find the library and Clang's resource headers. |
| Windows x64 | x64 MSVC build tools, Windows SDK, and x64 LLVM. With the default LLVM location, set `$env:LIBCLANG_PATH = "$env:ProgramFiles/LLVM/bin"` in PowerShell. |
| macOS arm64 | Xcode Command Line Tools and arm64 Homebrew LLVM 18. Set `LIBCLANG_PATH="$(brew --prefix llvm@18)/lib"`. |

The CI workflow sets `DOCS_RS=1` so `pg_query` 18.1.0 uses its bundled generated Protobuf bindings. Set it locally when `protoc` is on PATH or `REGENERATE_PROTOBUF` is set. These conditions otherwise make the dependency regenerate its bindings. [Pinned parser build script](https://github.com/pganalyze/pg_query.rs/blob/v18.1.0/build.rs)

Run these commands from the repository root.

```sh
rustc -Vv
cargo fmt --all -- --check
cargo build --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

The first build needs the registry packages in `Cargo.lock`. After `cargo fetch --locked`, pass `--offline` to the Cargo build, lint, or test command to use the local cache. Commit dependency changes and the corresponding lockfile together. [Cargo offline mode](https://doc.rust-lang.org/cargo/reference/config.html#netoffline)

Put input bytes, including encoding and line endings, through the same paths as existing callers. Preserve v1 field names and meanings when adding optional fields to reports. Update the contract tests for changes to serialization or gate behavior.
