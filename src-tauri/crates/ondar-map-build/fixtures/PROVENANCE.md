# Fixtures — ondar-map-build

- `step0-corners-161.tsv` — M4b Step 0's corner table at 328 × 161 (2026-10-02), the `fit`, `inset` and
  `corner` lines for the ten countries with insets, copied verbatim from
  `_handover/m4b-step0/q4b-pane-328x161-pad20.tsv` (the `pane_sweep` probe, `probe.patch`, on `f7a3fd8`
  and the shipped resource `e2775f81…`). Review P6: the tool's corner table at 161 with the controls'
  rect disabled is compared against it (`world::input_tests::corner_table_at_161_matches_step0`,
  `#[ignore]`, needs the NE inputs). IN and US framed on partial land there (`missing_blobs` 11 and 48
  at the fit, the `fit` lines' 7th field) and are excluded from the comparison.
  The four labels M4b commit 4b (`2e9ec11`) renamed in `insets.tsv` are renamed here too (M4c,
  2026-10-08: Canary Islands → Canaries, French Guiana → Fr. Guiana, Guadeloupe & Martinique →
  Antilles, Andaman & Nicobar → Andamans); the test looks lines up by label. No figure changed.
