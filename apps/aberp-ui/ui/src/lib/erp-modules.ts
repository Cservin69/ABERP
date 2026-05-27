// PR-78 / session 101 — closed-vocab ERP module registry, organized
// by USAGE FREQUENCY into two AREAS (ADR-0041 §1):
//
//   - "operational" — the daily-driver workflow. Today: Invoicing.
//                     Future: Inventory, Accounting, Procurement.
//                     Front-and-center: the operator lives here all
//                     day.
//   - "maintenance" — master-data + settings, deliberately ONE
//                     LEVEL REMOVED from the operational nav so it
//                     does not clutter the day-to-day. Today: Master
//                     Data (Partners), Settings (Tenant, NAV
//                     credentials). Future: products, price lists,
//                     tax-rate templates. Accessed intentionally via
//                     the topbar's gear affordance, not co-listed
//                     with operational modules.
//
// This module is intentionally pure-data + a small set of lookup
// helpers. No DOM, no Svelte, no fetch. The chrome in App.svelte
// consumes `MODULES` + `modulesInArea` to render the area-scoped
// sidebar and uses `moduleForRoute` to derive the active area + the
// parent-module-of-the-active-route.
//
// Pinned by `erp-modules.test.ts`.

import type { AppRoute } from "./router";

/** Closed-vocab union of usage-frequency areas (ADR-0041 §1).
 * Two-tier separation: operational = daily driver, maintenance =
 * configuration + master data. The chrome shows ONE area at a time;
 * an explicit topbar affordance swaps between them. */
export type ErpArea = "operational" | "maintenance";

/** Closed-vocab union of every known ERP module id. Lifts to a Rust
 * `enum ErpModule` when the backend cut lands (PR-79+ per ADR-0041
 * §5); the string forms here MUST match the future Rust variant
 * snake/kebab names so backend route namespacing (`/api/<module-id>/...`)
 * mirrors the SPA chrome's grouping. CLAUDE.md rule 7 — deny-default:
 * a new module is an explicit one-line widening here, not a silent
 * fall-through. */
export type ErpModuleId = "invoicing" | "master-data" | "settings";

/** A route reference inside a module. `id` is the typed `AppRoute`
 * slug (the router's closed vocab); `label` is the chrome's display
 * string for the sidenav row. Today labels stay English to match
 * the existing flat sidebar (PR-53 / session-73). */
export interface ErpRouteRef {
  id: AppRoute;
  label: string;
}

/** A registered ERP module. See ADR-0041 §1 + §2 for the per-field
 * contract. `area` decides whether the module appears in the
 * operational sidebar or behind the maintenance gear. `glyph` is a
 * single printable Unicode mark; no icon-library dependency by
 * design (CLAUDE.md rule 2). */
export interface ErpModule {
  id: ErpModuleId;
  area: ErpArea;
  label_hu: string;
  label_en: string;
  glyph: string;
  routes: ErpRouteRef[];
}

/** Display title for each area, used by the chrome (sidebar
 * section caption, gear-button label, "back to ..." link text). */
export const AREA_LABELS: Record<ErpArea, { hu: string; en: string }> = {
  operational: { hu: "Munka", en: "Operational" },
  maintenance: { hu: "Karbantartás", en: "Maintenance" },
};

/** The registry. Order is the display order in the sidebar within
 * each area (top to bottom). Within operational: Invoicing only
 * today. Within maintenance: Master Data (referenced from invoicing)
 * before Settings (operator-rare-touch). Each module's `routes`
 * order is the display order within that module's sub-list.
 *
 * Adding a module: extend `ErpModuleId` above, add the entry here
 * with the chosen `area`. The route-coverage pin in
 * `erp-modules.test.ts` will fail loudly if a new `AppRoute` was
 * added without a registry home. */
export const MODULES: ErpModule[] = [
  {
    id: "invoicing",
    area: "operational",
    label_hu: "Számlázás",
    label_en: "Invoicing",
    glyph: "§",
    routes: [{ id: "invoices", label: "Invoices" }],
  },
  {
    id: "master-data",
    area: "maintenance",
    label_hu: "Törzsadatok",
    label_en: "Master Data",
    glyph: "¶",
    routes: [{ id: "partners", label: "Partners" }],
  },
  {
    id: "settings",
    area: "maintenance",
    label_hu: "Beállítások",
    label_en: "Settings",
    glyph: "◌",
    routes: [
      { id: "tenant", label: "Tenant settings" },
      { id: "nav-credentials", label: "NAV credentials" },
    ],
  },
];

/** Look up the module that owns a given route. Total over `AppRoute`
 * by construction — the coverage pin enforces this. Returns the
 * matched `ErpModule` for in-chrome rendering of "this route's
 * parent module" and (transitively) the active area.
 *
 * Returns `null` ONLY if the registry has been edited inconsistently
 * (a route exists in `AppRoute` but no module claims it). The pin
 * catches that at build time, so callers in production code do not
 * need to handle the null path — but the type is honest about the
 * possibility rather than throwing, so a future hand-edited registry
 * bug surfaces as a missing-parent-header in chrome (visible) rather
 * than a runtime exception (silent crash). */
export function moduleForRoute(route: AppRoute): ErpModule | null {
  for (const m of MODULES) {
    for (const r of m.routes) {
      if (r.id === route) return m;
    }
  }
  return null;
}

/** Derive the active area for the route the operator is currently
 * on. The chrome uses this to (a) decide which area's modules to
 * render in the sidebar and (b) decide which area the topbar's
 * area-swap button targets. Falls back to "operational" for
 * unknown routes — `parseRoute` already routes unknowns to the
 * default `invoices` route, so this branch is defence-in-depth. */
export function areaForRoute(route: AppRoute): ErpArea {
  return moduleForRoute(route)?.area ?? "operational";
}

/** Return every module belonging to a given area, preserving the
 * registry's declared order. Used by the sidebar to render the
 * active area's contents only. */
export function modulesInArea(area: ErpArea): ErpModule[] {
  return MODULES.filter((m) => m.area === area);
}

/** The first (display-order) route the chrome should navigate to
 * when the operator enters an area via the topbar's area-swap
 * affordance. By convention: the first route of the first module in
 * that area. Today: operational → "invoices", maintenance →
 * "partners". Returns `null` if the area has no modules at all (a
 * registry inconsistency a future pin would catch).
 *
 * This is the chrome's "entry point" answer for each area. If a
 * future PR adds a per-area landing dashboard (e.g. a tile grid for
 * maintenance), the dashboard route would either become the entry
 * point here or live alongside as an explicit `/maintenance` route
 * — ADR-0041 §3 explicitly leaves that as a future widening, not
 * required for PR-78. */
export function defaultRouteForArea(area: ErpArea): AppRoute | null {
  const modules = modulesInArea(area);
  if (modules.length === 0) return null;
  const firstRoute = modules[0].routes[0];
  return firstRoute?.id ?? null;
}
