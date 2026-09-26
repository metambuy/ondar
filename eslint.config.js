// @ts-check
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";

export default tseslint.config(
  {
    // src/bindings is ts-rs-generated; if lint flags it, ignore the directory rather than
    // editing generated output.
    // _handover/ is the gitignored handover area (plans, reports, logs, design extracts). CI
    // never sees it, but `eslint .` on a checkout walks it, and a stray .mjs there made the
    // local `pnpm lint` gate exit 1 on 2026-09-24 (M3c Step 0 report, "Probe, revert, checks").
    ignores: ["dist", "src-tauri/target", "src-tauri/gen", "src/bindings", "_handover"],
  },
  tseslint.configs.recommended,
  reactHooks.configs.flat.recommended,
);
