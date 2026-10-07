---
paths:
  - "src-tauri/crates/ondar-map/**"
  - "src-tauri/crates/ondar-map-build/**"
  - "src-tauri/src/map.rs"
  - "src-tauri/src/commands/map.rs"
  - "src-tauri/resources/map/**"
  - "src/panel/MapPane.tsx"
  - "scripts/fetch-natural-earth.sh"
---

# Map rules

The decisions and figures: ONDAR.md, "M4: the drawn map", the M4a and M4b sections.

- **Rust owns the map; the webview draws exactly the paths Rust sends.** Projection (spherical LAEA
  on the authalic radius 6 371.0072 km), framing, insets, level choice, clipping, station gathering
  and hit-testing are Rust's. No Leaflet, no tiles.
- **The resource is built at build time** from Natural Earth 10m v5.1.2, inputs pinned by SHA-256
  (a mismatch is refused), shipped as one committed file and rebuilt only for a rule change. It is
  never fetched at runtime; the NE inputs never enter CI. The tool: `cargo run -p ondar-map-build
  --release -- [--tables DIR] [--out FILE] [--report FILE] [--encoding deflate|raw]
  [--simplifier hybrid|vw] [--bench] [--allow-dropped-insets] [--allow-wide-labels]`, ~20 s; the
  tables are written before the ship gate, so a refused build still leaves them to read. Its rules:
  parts under 300 km on the ground group; a remote group ≥ 1 000 km² must be a listed inset,
  matched ≤ 100 km from its anchor; an admin-1 edge found once must lie within 375 m of admin 0.
- **Never frame or clamp in lon/lat.** Centres, fits, pan limits and boxes are in projected km
  (antimeridian-aware; Antarctica pole-centred).
- **The crate takes the pane as an argument**; the page is told its size and computes nothing.
- **The ladder and the clamp:** 1.5/3/6/12/24 km/pt; a view uses the coarsest level at or below its
  scale; zoom is clamped to [1.5 km/pt, fit] and the view stays inside the fit rectangle (D6).
  Coverage and the frame's index both go through `ondar_map::index`, so the frame never asks for a
  blob the tool did not store. Subdivisions show above 8 km/pt, by a per-country flag.
- **Coverage is built for every band** 140..=300 pt at 328 wide. A missing blob is skipped and
  counted in `FrameStats::missing_blobs`; a frame never fails.
- **The drawn bound:** land ≤ 0.25 pt simplified + ≤ 0.035 pt quantised; subdivisions ≤ 0.5 +
  0.035 pt. The simplifier is the per-ring hybrid (RDP if simple and within the bound, else VW).
- **No panic from bytes:** the format is v2 (v1 is refused as `Version(1)`); the loader reads
  through one bounded cursor, and a corrupt resource is an error — the app runs without a map.
  The crate's non-test code has no panic shape, no index and no `.clamp(` (a scan test refuses
  them).
- **Insets:** the bottom-right corner is the `− fit +` row's at every band; each box takes the
  largest scale at which it is inside the pane, apart from the boxes placed and ≥ 12 pt from the
  land, down to 28 × 12 pt of land, else it is dropped at that band. The tool computes the scales
  and stores them; the frame reads them (`Store::inset_boxes`) and never searches. Hawaii alone may
  drop at 178. The tool refuses a label wider than its box at 178 or 300.
- **The page draws three layers and nothing else:** neighbours; the land, one flat tone per theme
  with its hairline on the same path; subdivisions above; then the insets with whole labels. No
  filter, no opacity, no `<use>` (WebKit styles a `<use>` clone as the original). Every colour and
  width is a token. One `map_pull` in flight, once per animation frame while input is pending; a
  reply is drawn only if its `seq` is newer than the frame on screen.
- **The bar:** a country change from idle to the painted frame ≤ 100 ms p90 at 178 and 300 (RU, US,
  PT, AQ). RU at 300 reads 99 ms. Measure it after any change to the frame, the payload or the page.
- **Station coordinates are radio-browser's `geo` only.**
