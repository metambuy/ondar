---
paths:
  - "src-tauri/**/*.rs"
  - "src-tauri/**/Cargo.toml"
---

# Rust rules

- **No `unwrap()` / `expect()` / `panic!` on a fallible runtime operation**, and the compiler
  holds it: `src-tauri/Cargo.toml`'s `[workspace.lints]` denies `unwrap_used`, `expect_used`,
  `panic`, `todo`, `unimplemented`, `dbg_macro`, `print_stdout`, `print_stderr` and `pedantic`
  (five allows, each with its reason in that table; `clippy.toml` holds only `msrv`). Two
  exemptions: a `Mutex` is locked through the crate's `sync::lock` (poison propagation, the one
  `unwrap` in each crate), and `expect()` on thread spawn and tokio runtime construction (the OS
  refused a thread; the app cannot run). The decode thread spawns per `play`, not only at startup.
- **Every exempt site carries `#[allow(clippy::expect_used, reason = "…")]`**, the reason being
  the attribute: `rg 'allow\(clippy::expect_used' src-tauri` lists them (the thread and runtime
  spawns, and the four outside both exemptions by decision, ONDAR.md "M3a: the station directory,
  built" and the defect B review: `stream.rs::build_client`, `bounded_storage()`'s const,
  `ReqwestTransport::new`, `lib.rs`'s `.build(context)`). Describe a new one the same way or
  handle the error. Tests, examples and the build tool may panic and print (crate-root
  `cfg_attr(test, allow(…))`, a per-file `#![allow]`). Three source-scan tests refuse what clippy
  cannot: `hls/` the six panic shapes (`unreachable!` included), `adts.rs` those and indexing,
  the `ondar-map` crate those, indexing and `.clamp(`.
- **The release profile is `panic = "abort"`:** a panic anywhere ends the app.
- **One shell error type**, `OndarError` (`thiserror`), serialised `{ code, message }` with a stable
  `code`. Engine failure reasons are `types::ErrorCode`, carried in `PlaybackState::Error`.
- **Logging:** `log::` in the crates; the shell installs `tracing_subscriber` (bridging `log`), so
  one `RUST_LOG` drives both. Never `println!` (`print_stdout` / `print_stderr` deny; the two
  `eprintln!` in `log_sink.rs` run before the logger exists and carry the allow). Audio-thread
  logging is rate-limited.
- **Commands are thin:** validate → send or forward → map the error. Domain logic lives in the
  crates.
- **A new dependency:** say what it does and why std or an existing crate is not enough.
- **Bindings:** a type crossing the boundary derives `Serialize, Deserialize, TS` with
  `#[ts(export)]` — the engine's in `ondar-audio/src/types.rs`, the shell's beside the module that
  owns them; run `cargo test --workspace` and commit the generated `.ts`.
