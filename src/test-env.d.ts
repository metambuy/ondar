// Node's `fs`, for the one test that reads a stylesheet's source (`MapPane.test.tsx`, test 9:
// the one-flat-tone rule is pinned on `panel.module.css` and `tokens.css` as text — vitest runs
// on Node, and Vite's `?raw` import does not apply to a `.module.css`). The project carries no
// `@types/node`, so the one function used is declared here, for the type checker only.
declare module "node:fs" {
  export function readFileSync(path: string | URL, encoding: "utf8"): string;
}
