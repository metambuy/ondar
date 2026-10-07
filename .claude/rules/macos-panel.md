---
paths:
  - "src-tauri/src/panel.rs"
  - "src-tauri/src/tray.rs"
  - "src-tauri/src/lib.rs"
  - "src-tauri/src/commands/panel.rs"
  - "src-tauri/Info.plist"
  - "src-tauri/tauri*.conf.json"
  - "src-tauri/Cargo.toml"
  - "src-tauri/capabilities/**"
  - "src-tauri/icons/**"
---

# macOS, the panel and the tray

Each item was measured; the record is in ONDAR.md (M2a to M2d sections).

- **Activation policy `Accessory`**, set in `panel::setup` **before** `PanelBuilder::build()`, plus
  `LSUIElement` in `Info.plist` (only a bundled build shows it; `tauri dev` cannot).
- **`tauri-nspanel`** is pinned by `rev` in `Cargo.toml`; register `tauri_nspanel::init()`. Its
  names mislead: **never call `Panel::to_window()`** (it converts the panel back and empties the
  plugin store; reach the window with `get_webview_window(label)`); `no_activate(true)` does not
  make a panel non-activating (the `NonactivatingPanel` style mask does); `show()` does not make it
  key (`make_key_window()` does).
- **Show:** lay out → `apply_frame` (one `setFrame:display:` with origin and size, while hidden) →
  emit `panel:layout` → wait for the page's commit (or the 250 ms fallback) → `show()` →
  `make_key_window()`. A resize is the same round trip with `apply_frame` last; expanding resizes
  and repositions against the tray anchor in one frame, no jump.
- **Dismissal:** hide on `WindowEvent::Focused(false)`. Never `Panel::set_event_handler` (it
  silences tao's window events); `hides_on_deactivate` stays unset.
- **One hide path, one show path** (`panel::hide`, `panel::show_at`); every caller logs `reason=`
  and `effective=`; both hop to the main thread themselves.
- **Coordinates are global logical points, top-left origin.** Convert at the boundary; never divide
  by the panel window's scale.
- **Position** from `TrayIconEvent::Click { rect }`, centred under the icon and clamped into that
  display's work area. `tauri-plugin-positioner` is not needed.
- **Vibrancy:** Tauri's `set_effects` (`Effect::Popover`) + transparency on the panel builder and
  the window, with `macos-private-api` declared explicitly in `Cargo.toml` and
  `"macOSPrivateApi": true`. `window-vibrancy` is **not** a direct dependency.
- **Tray icon:** a template image (state changes are shape changes, never colour); the event
  forwarder in `lib.rs` flips idle/playing, and the swap uses `set_icon_with_as_template`, only on
  a flip. The menu is About + Quit: About is a pane inside the popover (the standard About panel
  opens behind the frontmost app), Quit is `terminate:`. `show_menu_on_left_click(false)` is
  required; right-click logic keys off `Down`.
- **Single instance:** `tauri-plugin-single-instance`, registered first; `open Ondar.app` against
  the running app arrives as `RunEvent::Reopen`, handled in `lib.rs`; both feed `panel::show_at`.
- **Occlusion:** decode `NSWindowOcclusionState::Visible`; never read it synchronously after a show.
- **`tauri.dev.conf.json`** gives the dev instance `<id>.dev` (its own socket and data directory);
  a shell test pins the derivation. `pnpm tauri build` never merges it: bundles keep the real id.
