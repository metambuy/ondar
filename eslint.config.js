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
  // The renderer boundary, held by the linter (2026-10-17, "enforce, don't document"). The
  // rule is CLAUDE.md's "The one rule": the webview renders; it makes no network request,
  // decodes no audio, persists nothing. Every action is an `invoke()` through src/api.ts.
  // Test files are included on purpose: `vi.mock("…")` and `vi.stubGlobal("fetch", …)` pass
  // strings, so a mock never trips these; a call does.
  {
    files: ["src/**/*.{ts,tsx}"],
    rules: {
      "no-restricted-globals": [
        "error",
        ...["fetch", "XMLHttpRequest", "WebSocket", "EventSource"].map((name) => ({
          name,
          message: `The webview is a renderer, not an application (CLAUDE.md, "The one rule"): no network in TS. Invoke a Tauri command through src/api.ts.`,
        })),
        ...["localStorage", "sessionStorage", "indexedDB"].map((name) => ({
          name,
          message: `The webview is a renderer, not an application (CLAUDE.md, "The one rule"): no persistence in TS. Rust owns state that outlives a render.`,
        })),
      ],
      "no-restricted-syntax": [
        "error",
        {
          selector: "JSXOpeningElement[name.name=/^(audio|video)$/]",
          message:
            'The webview is a renderer, not an application (CLAUDE.md, "The one rule"): no <audio> or <video>; the Rust engine plays.',
        },
        {
          selector: "NewExpression[callee.name='Audio']",
          message:
            'The webview is a renderer, not an application (CLAUDE.md, "The one rule"): no `new Audio()`; the Rust engine plays.',
        },
        {
          selector:
            "MemberExpression[object.name=/^(window|globalThis|self)$/][property.name=/^(fetch|XMLHttpRequest|WebSocket|EventSource|localStorage|sessionStorage|indexedDB)$/]",
          message:
            'The webview is a renderer, not an application (CLAUDE.md, "The one rule"): no network or persistence in TS, not through `window.` either.',
        },
      ],
      "no-restricted-imports": [
        "error",
        {
          paths: [
            {
              name: "@tauri-apps/plugin-updater",
              message:
                "Rust owns the updater (ONDAR.md, M6-lite): the page renders the prompt and reports the click through src/api.ts.",
            },
            {
              name: "@tauri-apps/plugin-process",
              message:
                "Rust owns the process (ONDAR.md, M6-lite): relaunch and exit are Tauri commands, through src/api.ts.",
            },
            {
              name: "@tauri-apps/api",
              message:
                "src/api.ts is the only file that imports @tauri-apps/api (.claude/rules/ipc.md): add the wrapper there.",
            },
          ],
          patterns: [
            {
              group: ["@tauri-apps/api/*", "@tauri-apps/plugin-*"],
              message:
                "src/api.ts is the only Rust boundary (.claude/rules/ipc.md): add the wrapper there.",
            },
          ],
        },
      ],
    },
  },
  {
    // The one file that may import the Tauri API: the boundary itself.
    files: ["src/api.ts"],
    rules: { "no-restricted-imports": "off" },
  },
);
