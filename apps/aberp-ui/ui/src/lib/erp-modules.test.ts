// PR-78 / session 101 — vitest pin for the closed-vocab ERP module
// registry + the two-area usage-frequency split. Invariants per
// ADR-0041 §7:
//
//   1. Registry shape — every entry carries non-empty id /
//      label_hu / label_en / glyph / routes; module ids unique;
//      route ids unique across the registry; area is a closed-vocab
//      value.
//   2. Total route coverage — every value of `AppRoute` appears in
//      exactly one module's `routes` list. Adding an `AppRoute`
//      variant without a registry home fails here loudly.
//   3. `moduleForRoute` lookups — every existing route returns its
//      typed owning module.
//   4. Area split — each route's area matches the ADR-0041 §2
//      table; `areaForRoute` agrees with `moduleForRoute(route)?.area`.
//   5. Area helpers — `modulesInArea` preserves order and partitions
//      the registry; `defaultRouteForArea` returns the first route
//      of the first module in that area.
//
// These are the *load-bearing* invariants the chrome consumes; the
// rest of the registry (label text, glyph character) is data that
// can evolve without breaking the chrome.

import { describe, expect, it } from "vitest";

import {
  AREA_LABELS,
  MODULES,
  areaForRoute,
  defaultRouteForArea,
  modulesInArea,
  moduleForRoute,
  type ErpArea,
  type ErpModuleId,
} from "./erp-modules";
import type { AppRoute } from "./router";

// Every value of `AppRoute` must be enumerated here exactly once.
// This array IS the test source-of-truth — a new AppRoute variant
// without a corresponding entry here causes a TS narrowing failure
// in the typed `EXPECTED_OWNER` / `EXPECTED_AREA` records below, so
// the pin can never silently drift away from the router's actual
// closed vocab.
const ALL_APP_ROUTES: AppRoute[] = [
  "invoices",
  "partners",
  "tenant",
  "nav-credentials",
];

// The expected module-id for each AppRoute. Hand-pinned so the
// grouping is verified against the ADR §2 table, not against the
// registry's own self-consistent restatement of it. If a future PR
// regroups a route, this table changes alongside the registry — and
// the diff makes the regrouping visible at PR review time.
const EXPECTED_OWNER: Record<AppRoute, ErpModuleId> = {
  invoices: "invoicing",
  partners: "master-data",
  tenant: "settings",
  "nav-credentials": "settings",
};

// The expected area for each AppRoute. The two-area usage-frequency
// split: operational holds the daily workflow; maintenance holds
// the configuration + master-data routes one level removed.
const EXPECTED_AREA: Record<AppRoute, ErpArea> = {
  invoices: "operational",
  partners: "maintenance",
  tenant: "maintenance",
  "nav-credentials": "maintenance",
};

// Every area must have a stable bilingual label and at least one
// module — the chrome's topbar gear/back affordance assumes both.
const ALL_AREAS: ErpArea[] = ["operational", "maintenance"];

describe("MODULES registry shape", () => {
  it("every module carries non-empty id, labels, glyph, routes, area", () => {
    for (const m of MODULES) {
      expect(m.id.length).toBeGreaterThan(0);
      expect(m.label_hu.trim().length).toBeGreaterThan(0);
      expect(m.label_en.trim().length).toBeGreaterThan(0);
      expect(m.glyph.length).toBeGreaterThan(0);
      expect(m.routes.length).toBeGreaterThan(0);
      // Closed-vocab assertion: every module's area is one of the
      // known ErpArea values. Catches a typo at registry-write time.
      expect(ALL_AREAS).toContain(m.area);
      for (const r of m.routes) {
        expect(r.id.length).toBeGreaterThan(0);
        expect(r.label.trim().length).toBeGreaterThan(0);
      }
    }
  });

  it("module ids are unique", () => {
    const ids = MODULES.map((m) => m.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("route ids are unique across the entire registry", () => {
    // A route can only belong to ONE module (ADR-0041 §1 identity
    // invariant). Catches a paste-error that double-claims a route
    // when adding a new module.
    const allRoutes: string[] = [];
    for (const m of MODULES) {
      for (const r of m.routes) allRoutes.push(r.id);
    }
    expect(new Set(allRoutes).size).toBe(allRoutes.length);
  });
});

describe("total route coverage", () => {
  it("every AppRoute is claimed by exactly one module", () => {
    // ADR-0041 §7 + §8: deny-default. A new AppRoute variant added
    // to router.ts without a registry home fails here naming the
    // orphan, so a future contributor can't silently sweep a new
    // route into a "misc" bucket (there is no misc bucket).
    for (const route of ALL_APP_ROUTES) {
      const claimants = MODULES.filter((m) =>
        m.routes.some((r) => r.id === route),
      );
      expect(
        claimants.length,
        `route "${route}" should be claimed by exactly one module`,
      ).toBe(1);
    }
  });

  it("the grouping matches ADR-0041 §2 verbatim", () => {
    // Hand-pinned table. Catches a regrouping that the registry
    // alone wouldn't surface (e.g. moving `partners` to `settings`
    // would pass the totality pin above but break this one).
    for (const route of ALL_APP_ROUTES) {
      const owner = MODULES.find((m) =>
        m.routes.some((r) => r.id === route),
      );
      expect(owner?.id).toBe(EXPECTED_OWNER[route]);
    }
  });
});

describe("moduleForRoute lookup", () => {
  it("returns the owning module for every AppRoute", () => {
    for (const route of ALL_APP_ROUTES) {
      const m = moduleForRoute(route);
      expect(m).not.toBeNull();
      expect(m?.id).toBe(EXPECTED_OWNER[route]);
    }
  });

  it("returned module's routes list actually contains the queried route", () => {
    // Defence-in-depth: moduleForRoute could in principle return a
    // module by accident (e.g. an off-by-one in a future refactor).
    // Pin that the returned module's routes ACTUALLY includes the
    // route we asked about.
    for (const route of ALL_APP_ROUTES) {
      const m = moduleForRoute(route);
      expect(m?.routes.some((r) => r.id === route)).toBe(true);
    }
  });
});

describe("area split (operational vs maintenance)", () => {
  it("each AppRoute lives in the expected area", () => {
    for (const route of ALL_APP_ROUTES) {
      expect(areaForRoute(route)).toBe(EXPECTED_AREA[route]);
    }
  });

  it("areaForRoute agrees with moduleForRoute(route)?.area", () => {
    for (const route of ALL_APP_ROUTES) {
      expect(areaForRoute(route)).toBe(moduleForRoute(route)?.area);
    }
  });

  it("AREA_LABELS has a non-empty HU + EN label for every area", () => {
    for (const a of ALL_AREAS) {
      const label = AREA_LABELS[a];
      expect(label.hu.trim().length).toBeGreaterThan(0);
      expect(label.en.trim().length).toBeGreaterThan(0);
    }
  });
});

describe("modulesInArea + defaultRouteForArea", () => {
  it("modulesInArea preserves registry order within each area", () => {
    const op = modulesInArea("operational");
    const mt = modulesInArea("maintenance");
    expect(op.map((m) => m.id)).toEqual(["invoicing"]);
    expect(mt.map((m) => m.id)).toEqual(["master-data", "settings"]);
  });

  it("modulesInArea partitions the registry (union covers every module, no overlap)", () => {
    const union = [
      ...modulesInArea("operational"),
      ...modulesInArea("maintenance"),
    ];
    expect(union.length).toBe(MODULES.length);
    expect(new Set(union.map((m) => m.id)).size).toBe(MODULES.length);
  });

  it("defaultRouteForArea returns the first route of the first module in that area", () => {
    // Operational entry point: Invoicing → Invoices.
    // Maintenance entry point: Master Data → Partners.
    // These are the routes the chrome's area-swap button navigates
    // to when the operator enters an area without a specific
    // destination in mind.
    expect(defaultRouteForArea("operational")).toBe("invoices");
    expect(defaultRouteForArea("maintenance")).toBe("partners");
  });
});
