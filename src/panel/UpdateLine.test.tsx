// The update line's four states and its one report — vitest under jsdom, `pnpm test`. `../api`
// is mocked whole: nothing reaches Tauri; the update arrives through `onUpdateAvailable` as the
// `update:available` event would. The last test reads the stylesheets as text and pins the
// button's colours to their tokens (0.1.2, F2). Each test's comment states what it pins and what it would have
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

  // 2. F2 (0.1.2): the event renders one enabled button, "Update to 0.1.1", the version the
  //    event's, and no other text. Fails on the 0.1.1 line ("Ondar 0.1.1 available · Update", the
  //    grey text F2 is about), if the page composes the version from anywhere else, or if the
  //    button is missing or disabled.
  it("2. the event renders one button, Update to <version>", async () => {
    const { container } = render(<UpdateLine />);
    await settle();
    act(() => mock.emit(v011));
    const button = screen.getByRole<HTMLButtonElement>("button", { name: "Update to 0.1.1" });
    expect(button.disabled).toBe(false);
    expect(container.textContent).toBe("Update to 0.1.1");
  });

  // 3. A click calls `install` once and disables the button, relabelled "Updating…", while the
  //    promise is pending; a second click while pending is impossible (disabled), so one call.
  //    Fails if the click does not reach Rust, reaches it twice, or the button stays clickable.
  it("3. a click reports once and disables the button while installing", async () => {
    render(<UpdateLine />);
    await settle();
    act(() => mock.emit(v011));
    fireEvent.click(screen.getByRole("button", { name: "Update to 0.1.1" }));
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
    fireEvent.click(screen.getByRole("button", { name: "Update to 0.1.1" }));
    await act(async () => {
      mock.rejects[0]({ code: "updater", message: "temp directory is not on the same mount point as the app" });
    });
    expect(screen.getByText("Update failed, see log")).toBeTruthy();
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Update to 0.1.1" }).disabled).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Update to 0.1.1" }));
    expect(mock.installs).toHaveLength(2);
  });

  // 5. The same event twice renders one line, and a new event after a failure returns the line
  //    to "available". Fails if lines accumulate or a stale failure sticks.
  it("5. a repeated event renders one line and clears a failure", async () => {
    render(<UpdateLine />);
    await settle();
    act(() => mock.emit(v011));
    act(() => mock.emit(v011));
    expect(screen.getAllByRole("button")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Update to 0.1.1" }));
    await act(async () => {
      mock.rejects[0]({ code: "updater", message: "x" });
    });
    act(() => mock.emit({ version: "0.1.2", notes: "" }));
    expect(screen.getByRole("button", { name: "Update to 0.1.2" })).toBeTruthy();
    expect(screen.queryByText("Update failed, see log")).toBeNull();
  });

  // 6. F2 (0.1.2): the button's fill and label are their tokens, never literals; the fill is
  //    the system `AccentColor` where WebKit supports it, with a literal blue before that
  //    (macOS 12's WebKit has no `AccentColor`), set through `@supports` because a custom
  //    property cannot fall back by itself; neither appearance overrides it (the accent is the
  //    same). Fails if the rule takes a literal or another token, if the `@supports` block or
  //    the fallback goes, or if the dark block redefines the fill to something else.
  it("6. the button's colours are the accent tokens, with a fallback", () => {
    const panelCss = readFileSync("src/panel/panel.module.css", "utf8");
    const tokensCss = readFileSync("src/styles/tokens.css", "utf8");
    const start = panelCss.indexOf(".updateButton {");
    expect(start, "no .updateButton rule").toBeGreaterThanOrEqual(0);
    const body = panelCss.slice(start, panelCss.indexOf("}", start));
    expect(/background:\s*var\(--color-update-fill\)/.test(body)).toBe(true);
    expect(/(?:^|[\s;{])color:\s*var\(--color-update-label\)/.test(body)).toBe(true);
    const darkAt = tokensCss.indexOf("prefers-color-scheme: dark");
    const light = tokensCss.slice(0, darkAt);
    expect(/--color-update-fill:\s*rgb\(0 122 255\);/.test(light), "the fallback").toBe(true);
    expect(/--color-update-label:\s*rgb\([^)]+\);/.test(light)).toBe(true);
    expect(/@supports \(color: AccentColor\)\s*\{\s*:root\s*\{\s*--color-update-fill:\s*AccentColor;/.test(tokensCss)).toBe(true);
    expect(tokensCss.slice(darkAt).includes("--color-update-fill"), "not redefined in Ink").toBe(false);
  });
});
