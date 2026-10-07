---
paths:
  - "src/**/*.{ts,tsx,css}"
  - "panel.html"
  - "vite.config.ts"
---

# TypeScript and the page

- **The one rule holds here first** (CLAUDE.md): render, animate, handle input, hold ephemeral view
  state; nothing else. Rust is the source of truth; the page mirrors events and keeps no optimistic
  parallel model.
- Strict mode, no `any`. IPC types come from `src/bindings/` (generated), calls from `src/api.ts`.
- Function components and hooks; local state by default.
- CSS modules, no framework. **Every colour and size literal lives in `src/styles/tokens.css`**, light
  and dark together; `pnpm lint` fails on one elsewhere and on an inline `style={{`
  (`scripts/check-tokens.sh`, over `src/**/*.{css,ts,tsx}` and `panel.html`).
- Every interactive element is keyboard-reachable and labelled.
- The panel's height, the map band's rect and the controls' rect come from Rust's `PanelLayout`;
  the page never measures or computes them.
- `measure.ts` and the `?measure=` drivers are debug-only and must stay inert otherwise; the
  release binary's `strings` carries no `measure[`.
- `vite.config.ts` builds the one entry, `panel.html`; its explicit input map is what ships it.
