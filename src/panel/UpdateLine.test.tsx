// The update line's four states and its one report — vitest under jsdom, `pnpm test`. `../api`
// is mocked whole: nothing reaches Tauri; the update arrives through `onUpdateAvailable` as the
// `update:available` event would. The last test reads the stylesheets as text and pins the line's
// colour to a system label token. Each test's comment states what it pins and what it would have
// to see to fail.
import { readFileSync } from "node:fs";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { UpdateAvailable } from "../api";
import UpdateLine from "./UpdateLine";

const mock = vi.hoisted(() => {
  let listener: ((u: UpdateAvailable) => void) | null = null;
  const installs: (() => void)[] = [];
  const rejects: ((e: unknown) => void)[] = [];
  return {
    /** Deliver an `update:available` event to the mounted line. */
    emit: (u: UpdateAvailable) => {
      if (listener === null) throw new Error("no listener registered");
      listener(u);
    },
    installs,
    rejects,
    reset: () => {
      listener = null;
      installs.length = 0;
      rejects.length = 0;
    },
    onUpdateAvailable: (cb: (u: UpdateAvailable) => void) => {
      listener = cb;
      return Promise.resolve(() => {});
    },
    /** Each call returns a promise the test settles through `installs` (resolve) or `rejects`. */
    install: () =>
      new Promise<void>((resolve, reject) => {
        installs.push(resolve);
        rejects.push(reject);
      }),
  };
});

vi.mock("../api", () => ({
  onUpdateAvailable: mock.onUpdateAvailable,
  updater: { install: mock.install },
}));

const v011: UpdateAvailable = { version: "0.1.1", notes: "" };

beforeEach(() => mock.reset());
afterEach(cleanup);

const settle = () => act(async () => {});

describe("the update line", () => {
  // 1. Before any event the line renders nothing at all: no text, no button. Fails if the
  //    component renders a placeholder or a disabled button while waiting.
  it("1. renders nothing until Rust says an update is available", async () => {
    const { container } = render(<UpdateLine />);
    await settle();
    expect(container.innerHTML).toBe("");
  });

  // 2. The event renders "Ondar 0.1.1 available" and an enabled Update button; the version is
  //    the event's. Fails if the page composes the version from anywhere else or the button is
  //    missing or disabled.
  it("2. the event renders the version and an Update button", async () => {
    render(<UpdateLine />);
    await settle();
    act(() => mock.emit(v011));
    expect(screen.getByText("Ondar 0.1.1 available")).toBeTruthy();
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Update" }).disabled).toBe(false);
  });

  // 3. A click calls `install` once and disables the button, relabelled "Updating…", while the
  //    promise is pending; a second click while pending is impossible (disabled), so one call.
  //    Fails if the click does not reach Rust, reaches it twice, or the button stays clickable.
  it("3. a click reports once and disables the button while installing", async () => {
    render(<UpdateLine />);
    await settle();
    act(() => mock.emit(v011));
    fireEvent.click(screen.getByRole("button", { name: "Update" }));
    expect(mock.installs).toHaveLength(1);
    const button = screen.getByRole<HTMLButtonElement>("button", { name: "Updating…" });
    expect(button.disabled).toBe(true);
    fireEvent.click(button);
    expect(mock.installs).toHaveLength(1);
  });

  // 4. A rejection (`{ code: "updater" }`) shows "Update failed, see log" and re-enables the
  //    button for a retry. Fails if the failure is hidden behind the "available" text, or the
  //    button stays disabled.
  it("4. a rejection shows the failure and allows a retry", async () => {
    render(<UpdateLine />);
    await settle();
    act(() => mock.emit(v011));
    fireEvent.click(screen.getByRole("button", { name: "Update" }));
    await act(async () => {
      mock.rejects[0]({ code: "updater", message: "temp directory is not on the same mount point as the app" });
    });
    expect(screen.getByText("Update failed, see log")).toBeTruthy();
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Update" }).disabled).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Update" }));
    expect(mock.installs).toHaveLength(2);
  });

  // 5. The same event twice renders one line, and a new event after a failure returns the line
  //    to "available". Fails if lines accumulate or a stale failure sticks.
  it("5. a repeated event renders one line and clears a failure", async () => {
    render(<UpdateLine />);
    await settle();
    act(() => mock.emit(v011));
    act(() => mock.emit(v011));
    expect(screen.getAllByText("Ondar 0.1.1 available")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Update" }));
    await act(async () => {
      mock.rejects[0]({ code: "updater", message: "x" });
    });
    act(() => mock.emit({ version: "0.1.2", notes: "" }));
    expect(screen.getByText("Ondar 0.1.2 available")).toBeTruthy();
    expect(screen.queryByText("Update failed, see log")).toBeNull();
  });

  // 6. The line's colour is one of the two system label tokens, which `tokens.css` defines for
  //    both appearances (the measured secondary label, light and dark). The line sits on the
  //    popover's material, so there is no fixed background to compute a ratio against; the
  //    contrast is Apple's pair, checked by eye in Sand and Ink at acceptance (chat decision 5,
  //    2026-10-14). Fails if the rule takes a map, chip or control token, a literal, or a token
  //    the dark block does not define.
  it("6. the line's colour is a system label token defined in both appearances", () => {
    const panelCss = readFileSync("src/panel/panel.module.css", "utf8");
    const tokensCss = readFileSync("src/styles/tokens.css", "utf8");
    const start = panelCss.indexOf(".updateLine {");
    expect(start, "no .updateLine rule").toBeGreaterThanOrEqual(0);
    const body = panelCss.slice(start, panelCss.indexOf("}", start));
    const colour = /(?:^|[\s;{])color:\s*var\((--[\w-]+)\)/.exec(body)?.[1];
    expect(["--color-label", "--color-label-secondary"]).toContain(colour);
    const darkAt = tokensCss.indexOf("prefers-color-scheme: dark");
    const defined = (css: string) => new RegExp(`${colour}:\\s*[^;]+;`).test(css);
    expect(defined(tokensCss.slice(0, darkAt)), "Sand").toBe(true);
    expect(defined(tokensCss.slice(darkAt)), "Ink").toBe(true);
  });
});
