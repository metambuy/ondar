// The country select's groups (0.1.2, F4) — vitest under jsdom, `pnpm test`. `../api` is mocked:
// the list is what Rust would send, already sorted and grouped; the page must keep that order,
// put the main list first with no heading, and head the other two groups.
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Country, ListedCountries } from "../api";
import CountryControl from "./CountryControl";

const sent: Country[] = [
  { code: "AF", name: "Afghanistan", group: "main", station_count: 2 },
  { code: "PT", name: "Portugal", group: "main", station_count: 371 },
  { code: "RE", name: "Réunion", group: "main", station_count: 47 },
  { code: "RO", name: "Romania", group: "main", station_count: 120 },
  { code: "AD", name: "Andorra", group: "small", station_count: 9 },
  { code: "MC", name: "Monaco", group: "small", station_count: 11 },
  { code: "AX", name: "Åland Islands", group: "territory", station_count: 3 },
  { code: "GP", name: "Guadeloupe", group: "territory", station_count: 17 },
];

const reply: ListedCountries = {
  items: sent,
  fetched_at: 0,
  age_secs: 0,
  source: { kind: "fresh" },
  refreshing: false,
};

vi.mock("../api", () => ({
  stations: { listCountries: () => Promise.resolve(reply) },
  onCountriesUpdated: () => Promise.resolve(() => {}),
}));

afterEach(cleanup);

async function renderControl(onSelect = vi.fn()) {
  render(
    <CountryControl
      country="PT"
      onSelect={onSelect}
      mine={false}
      onToggleMine={() => {}}
      showGeneration={0}
      onCountries={() => {}}
    />,
  );
  await screen.findByRole("option", { name: "Portugal (371)" });
  return onSelect;
}

describe("the country select's groups", () => {
  // Fails if the page sorts on its own (a byte-wise sort puts "Romania" before "Réunion", which
  // Rust's accent-folded order does not: the plant `_handover/v012/plants/f4-page-sorts.patch`),
  // drops a group, or shows a heading over the main list.
  it("renders the groups Rust sent, in its order, the main list unheaded", async () => {
    await renderControl();
    const select = screen.getByRole("combobox");
    const children = Array.from(select.children).map((el) =>
      el.tagName === "OPTGROUP"
        ? `[${el.getAttribute("label")}] ${Array.from(el.children).map((o) => o.textContent).join(", ")}`
        : (el.textContent ?? ""),
    );
    expect(children).toEqual([
      "Afghanistan (2)",
      "Portugal (371)",
      "Réunion (47)",
      "Romania (120)",
      "[Small states] Andorra (9), Monaco (11)",
      "[Territories] Åland Islands (3), Guadeloupe (17)",
    ]);
  });

  // Fails if a grouped option stops reporting its code (the `map_select` path in `Panel`).
  it("a choice inside a group reports its code", async () => {
    const onSelect = await renderControl();
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "MC" } });
    expect(onSelect).toHaveBeenCalledWith("MC");
  });

  // Fails if an empty group still renders its heading.
  it("a group with no member renders no heading", async () => {
    reply.items = sent.filter((c) => c.group !== "territory");
    await renderControl();
    expect(screen.queryByRole("group", { name: "Territories" })).toBeNull();
    expect(screen.getByRole("group", { name: "Small states" })).toBeTruthy();
    reply.items = sent;
  });
});
