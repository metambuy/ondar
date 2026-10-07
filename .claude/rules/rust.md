---
paths:
  - "src-tauri/**/*.rs"
  - "src-tauri/**/Cargo.toml"
---

# Rust rules

- **No `unwrap()` / `expect()` / `panic!` on a fallible runtime operation** in code reachable from
  a command or the audio thread. Two exemptions: `Mutex::lock().unwrap()` (poison propagation
  only), and `expect()` on thread spawn and tokio runtime construction (the OS refused a thread;
  the app cannot run). The decode thread spawns per `play`, not only at startup.
- **Four sites sit outside both exemptions, by decision** (ONDAR.md, "M3a: the station directory,
  built" and the defect B review): `stream.rs::build_client`'s `.build().expect(..)`,
  `bounded_storage()`'s `NonZeroUsize::new(BUFFER_BYTES).expect(..)` (a non-zero const),
  `ondar-stations`' `ReqwestTransport::new`, and `lib.rs`'s `.build(generate_context!()).expect(..)`,
  which runs once at startup before a command or the audio thread exists. Describe a new one the
  same way or handle the error. Clippy does not enforce the rule (`clippy.toml` sets only `msrv`);
  source-scan tests do, each with its own scope: `hls/` refuses the six panic shapes, `adts.rs`
  those and indexing, the `ondar-map` crate those, indexing and `.clamp(`.
- **The release profile is `panic = "abort"`:** a panic anywhere ends the app.
- **One shell error type**, `OndarError` (`thiserror`), serialised `{ code, message }` with a stable
  `code`. Engine failure reasons are `types::ErrorCode`, carried in `PlaybackState::Error`.
- **Logging:** `log::` in the crates; the shell installs `tracing_subscriber` (bridging `log`), so
  one `RUST_LOG` drives both. Never `println!`. Audio-thread logging is rate-limited.
- **Commands are thin:** validate → send or forward → map the error. Domain logic lives in the
  crates.
- **A new dependency:** say what it does and why std or an existing crate is not enough.
- **Bindings:** a type crossing the boundary derives `Serialize, Deserialize, TS` with
  `#[ts(export)]` — the engine's in `ondar-audio/src/types.rs`, the shell's beside the module that
  owns them; run `cargo test --workspace` and commit the generated `.ts`.
