# ADR-0041 — ERP module architecture: two-tier USAGE-FREQUENCY separation (operational area vs maintenance area), closed-vocab modules within each, additive SPA-shell slice in PR-78, backend namespacing phased into PR-79+

- **Status:** Accepted
- **Date:** 2026-05-27
- **Deciders:** Ervin (with session-101 mid-flight refinement folded
  in — see §0 below for the refinement summary)
- **Class:** Build-phase strategic ADR — the opening move on the
  ERP-platform reframe. Pins the *concept* (what an area IS, what
  a module IS, what the initial closed-vocab sets ARE, what shape
  the SPA shell takes, what shape the backend EVENTUALLY takes) so
  subsequent module-shaped PRs slot in cleanly. PR-78 implements
  only §3 (the SPA shell). §5 (backend namespacing) is documented-
  but-unbuilt and binds future PRs.
- **Related:**
  - **`project_aberp_ui_milestone`** — primary-UI-is-the-SPA
    posture from Sn-56 (2026-05-23). The area + module shell is
    the chrome around that primary UI; CLI affordances continue
    to live outside it.
  - **`project_aberp_ux_roadmap` Tier 3 — dashboard nav-replacement
    pushback.** That memo explicitly REFUSES "replace the operator-
    clear 4-item nav with a KPI dashboard". This ADR honors that
    refusal: every existing route still works in the same hash form
    (`#/invoices`, `#/partners`, `#/tenant`, `#/nav-credentials`),
    reachable from chrome that groups them into areas + modules
    without deleting any. The maintenance area concept is the
    grouping move, not a dashboard replacement.
  - **ADR-0006 — module boundaries (the pre-PR-1 module sketch).**
    The conceptual lineage: ABERP was always envisioned as a multi-
    module product. The first ~thirteen sessions hardened the
    invoicing slice end-to-end; this ADR is the cash-out of "now
    we treat that slice as a peer of future ones" rather than as
    the whole product.
  - **ADR-0017 — design language ("ambient, never theatrical").**
    The chrome chosen below (single-area sidebar + small topbar
    gear for area swap + faint surface shift between areas) MUST
    remain quiet chrome, not a marketing-deck navigator. Area
    captions are caps + monospace + muted; the gear is a small
    secondary control, not the focal point.
  - **CLAUDE.md rule 3 (surgical changes)** + **rule 13 (delete
    before optimize, scope discipline).** The PR-78 build is
    additive-only, SPA-only, zero backend change. Future modules
    are added when they have real routes — not as empty
    placeholders.
  - **CLAUDE.md rule 7 (surface conflicts, don't average them)** +
    **rule 12 (fail loud).** Both the area set AND the module set
    are CLOSED vocabs. `moduleForRoute(routeId)` /
    `areaForRoute(routeId)` return a typed value or refuse (deny-
    default). An unknown route in the registry is a type error at
    compile time, not a silent fall-through to a "misc" bucket.
  - **PR-53 / session-73** — the existing hash router + 4-item
    flat sidebar. This ADR reframes that sidebar into an area-
    scoped two-level one; the router contract is unchanged.
  - **PR-68 / session-90** — the `/` search-focus + `j` / `k` row-
    nav keyboard layer. The chrome reframe MUST preserve PR-68's
    behaviour: hotkeys only fire on the list-view body, not on the
    sidenav or the topbar gear button, and added tab-stops do not
    become typing targets that would suppress `j` / `k`.

## §0 — The session-101 refinement that shaped this ADR

The first draft of this ADR landed the three known modules
(Invoicing, Master Data, Settings) as PEERS in a single two-
level sidebar. Ervin pushed back mid-session: that's NOT the
split he wants.

The actual split is by **usage frequency**, not by topical
peerhood:

> "these also should be elevated to a settings or master data
> section not to interfere with the day to day workflow so should
> be some master data maintenance dashboard or layout and then the
> modules."

So Master Data + Settings are NOT co-equal with Invoicing in the
chrome. They're ONE LEVEL REMOVED — accessed intentionally, NOT
mixed into the operational sidebar where the operator works
invoices all day. This ADR therefore introduces **areas** as the
top-level grouping above modules, and the SPA shell renders ONE
area's modules at a time, with an explicit topbar affordance to
swap.

This is still ADDITIVE (every existing hash route works
unchanged), but the chrome's grouping pulls configuration OUT of
the daily workflow path rather than listing it as a co-equal
sidebar item.

## Context

### Where ABERP is today (2026-05-27)

Thirteen sessions of backend + UI hardening (PR-23 onward) have
shipped a self-contained AR-Invoicing slice: typestate-pinned
issuance ladder, NAV transport with audit-ledger evidence,
tamper-evident ledger + bundle exports, partners CRUD with
typeahead, multi-bank-account `seller.toml`, EUR-side
invoicing, live NAV-test submission verified end-to-end.

The SPA carries four top-level routes (PR-53 / PR-54 /
session-73 / session-74), all listed flat in a single sidebar:

  1. `#/invoices`        — invoice list / detail / issue / storno
                            / modification (the AR core).
  2. `#/partners`        — saved-buyer CRUD + the typeahead's
                            owner.
  3. `#/tenant`          — `seller.toml` editor (identity + bank
                            accounts).
  4. `#/nav-credentials` — NAV technical-user keychain blob editor.

The operator working invoices all day looks at the sidebar and
sees four equally-weighted nav items. Three of those (Partners,
Tenant, NAV Credentials) are operator-rare-touch — typically
configured once at onboarding and edited only when something
breaks. They dilute the daily workflow chrome.

Backend HTTP routes are all under `/api/...` flat
(`/api/invoices`, `/api/partners`, `/api/seller-info`,
`/api/seller/banks`, `/api/nav/...`, etc.).

### Why we are reframing now (the strategic direction)

Ervin (CEO of Áben Consulting KFT., the canonical first tenant)
named the next arc explicitly: **ABERP should evolve from "an
invoicing app" into "an ERP where Invoicing is one module among
others"**, both in chrome and in backend organization. The
session-101 refinement (§0) sharpened that further: the daily-
driver workflow modules belong in the operational area; the
configuration-and-master-data modules belong in a maintenance
area accessed intentionally.

Naming the boundary NOW (and additively reframing the chrome
around it NOW) makes each future module a same-shaped
contribution rather than a "figure it out per module"
reinvention.

The risk of NOT naming the boundary now: the next operational
module added under the current flat shell either (a) becomes a
5th top-level nav item ad-hoc, blurring "which is workflow /
which is config" forever, or (b) gets shoved as a route under
one of the existing four. Both are kinds of swallowed errors per
CLAUDE.md rule 12.

### What this ADR does and does not commit

**Does:**
  - Defines what an ERP **area** IS (§1.1) and what an ERP
    **module** IS (§1.2) — both as closed-vocab concepts.
  - Names the initial area set + module set + maps the existing
    4 routes to areas and modules (§2).
  - Picks the **SPA shell shape** (single-area sidebar + topbar
    gear for area swap, §3) and justifies the pick against the
    alternatives.
  - Sketches the **backend module boundary** that future PRs will
    materialise (§5) — route namespacing, crate organization, a
    capability registry.
  - Lays out the **migration phasing** (§6) — PR-78 ships SPA
    only; PR-79+ namespaces the backend; later PRs add real
    modules.
  - Names what is OUT of scope so future sessions don't drift the
    boundary (§7).

**Does NOT:**
  - Touch the backend in PR-78. Zero `/api/...` route renamed,
    zero handler moved, zero file under `apps/aberp/src/` or
    `crates/` changed.
  - Add placeholder modules with no routes. The closed module
    vocab today is THREE entries (Invoicing, Master Data,
    Settings) because those are the three module-shaped
    groupings that EXIST as real routes. Future modules
    (Inventory, Accounting, Procurement) lift the vocab one
    entry at a time, each behind a PR that ships at least one
    real route.
  - Replace the operator-clear nav. Every existing route remains
    reachable; every existing hash form remains deep-linkable.
    The chrome adds a grouping layer; it does NOT remove one.
  - Introduce a dashboard, KPI cards, or a "landing module
    picker" (the roadmap Tier 3 pushback applies).
  - Ship a dedicated `#/maintenance` landing route in PR-78.
    The maintenance area is its own sidebar layout; a tile
    dashboard at the area's entry point is a future widening
    (§3 footnote).

## Decision

### §1 — What an area IS, what a module IS

#### §1.1 — Areas (the top-level USAGE-FREQUENCY split)

An **area** is a closed-vocab grouping of modules by how often
the operator works inside it. There are TWO areas:

  - **`operational`** — the daily-driver workflow. The modules
    the operator lives in all day. Today: Invoicing. Future:
    Inventory, Accounting, Procurement. These modules are
    front-and-center: the chrome's primary sidebar shows the
    operational area's modules by default and on every fresh
    load.
  - **`maintenance`** — configuration + master data, deliberately
    one level removed from the operational nav so it does not
    clutter the day-to-day. Today: Master Data (Partners),
    Settings (Tenant config, NAV credentials). Future: products,
    price lists, tax-rate templates, integrations. Accessed
    intentionally via a topbar affordance, NOT co-listed with
    operational modules.

The area set is a CLOSED vocab. Adding a third area (e.g.
`reporting`, `analytics`, `admin`) is an explicit ADR
amendment; the deny-default in `areaForRoute` refuses to invent
new areas at runtime.

#### §1.2 — Modules (the within-area cohesive functional areas)

A **module** is a cohesive functional area of ABERP within an
area. Each module declares:

  - **`id`** — the canonical kebab-case identifier (closed vocab;
    enum-like in TS, lifts to a Rust `enum` when §5 lands). The
    id is the same string SPA-side and backend-side; the SPA's
    `ErpModuleId` and the (future) backend's route-namespace
    segment MUST match exactly.
  - **`area`** — which area this module belongs to. `operational`
    or `maintenance`.
  - **`label_hu`** + **`label_en`** — bilingual display names
    (mirrors the ADR-0038 / ADR-0040 bilingual operator-message
    posture). Hungarian first because Áben Consulting KFT.'s
    primary operators read Hungarian first.
  - **`glyph`** — a one-character or short-string visual marker
    (the design language is monospace + dark-theme; a glyph is
    a sigil, not an icon font dependency). Today: a printable
    single-char Unicode mark. CLAUDE.md rule 2: no icon library
    until a real need names one.
  - **`routes`** — an ORDERED list of route refs. A route ref is
    `{ id: AppRoute, label: string }` (i18n on the route label is
    a future widening; today the route labels stay English to
    match the existing flat sidebar).

**Identity invariants:**
  - Module ids are unique.
  - Route ids are unique across the *entire* registry — a given
    `AppRoute` belongs to exactly ONE module.
  - Every module declares exactly one area.
  - `moduleForRoute(id)` and `areaForRoute(id)` are total over
    `AppRoute` by construction; pins enforce this at build time.

### §2 — The initial area + module set + route grouping

| Area          | Module id      | label_hu      | label_en    | glyph | Routes                                |
| ------------- | -------------- | ------------- | ----------- | ----- | ------------------------------------- |
| `operational` | `invoicing`    | Számlázás     | Invoicing   | §     | `invoices`                            |
| `maintenance` | `master-data`  | Törzsadatok   | Master Data | ¶     | `partners`                            |
| `maintenance` | `settings`     | Beállítások   | Settings    | ◌     | `tenant`, `nav-credentials`           |

**Area display labels** (consumed by the chrome's topbar gear,
back-button, sidebar caption):

| Area          | label_hu       | label_en      |
| ------------- | -------------- | ------------- |
| `operational` | Munka          | Operational   |
| `maintenance` | Karbantartás   | Maintenance   |

**Why this split:**

  - **Invoicing is operational** because the operator literally
    spends their workday issuing, viewing, retrying, exporting
    invoices. It is THE daily driver today and remains so under
    every plausible near-term scope.
  - **Master Data is maintenance** because partner records (and
    future products / price lists / tax templates) are reference
    data the operator configures and then *reads* during the
    operational workflow. They are not the workflow.
  - **Settings is maintenance** because seller-identity config,
    bank accounts, and NAV credentials are configure-once-and-
    forget. The operator should not see them on the daily path.

**Routes outside any module:** none today. Every existing route
maps to exactly one module per the table above. The pins in §8
enforce total coverage of `AppRoute` + agreement between
`areaForRoute` and `moduleForRoute(route)?.area`.

**Future modules (named here so PR-78's id namespace doesn't
collide with them; NOT built):**

  - `inventory`     → operational  (items, stock, warehouses;
                                    deferred per roadmap Tier 3).
  - `accounting`    → operational  (journals, GL; deferred per
                                    roadmap Tier 3).
  - `procurement`   → operational  (POs, supplier invoices;
                                    deferred; AP-side).
  - `dashboard`     → operational  (a future landing surface IF
                                    Ervin opts in; roadmap Tier
                                    3 allows the addition, refuses
                                    it as a nav-replacement).
  - `products`      → maintenance  (when CNC-job-line-item PR
                                    lands).
  - `price-lists`   → maintenance.
  - `tax-templates` → maintenance.

These names are *reserved* by this ADR — a future PR that wants
to add e.g. an `inventory` module uses this id verbatim so the
chrome and (eventually) backend stay aligned.

### §3 — SPA shell shape: single-area sidebar + topbar gear (option A')

**Options considered:**

  - **(a) Two-level sidebar — modules as top groups, routes
        nested.** All three modules in one sidebar, Settings
        listed as a peer at the bottom. *Rejected per §0 — does
        not honor the usage-frequency split.*
  - **(a') Single-area sidebar + topbar gear for area swap.**
        The sidebar renders ONE area's modules at a time (two
        levels: module headers with their routes nested
        beneath). A small "⚙ Maintenance" button in the topbar
        navigates to the maintenance area; when in maintenance,
        the same button flips to "← Operational" to return.
        The sidebar caption labels the active area.
  - **(b) Module switcher in chrome + per-module route list.**
        A horizontal module switcher in the topbar. Each module
        view shows only its own routes. *Rejected — hides
        operational modules from each other when more than one
        ships; the operator working Invoicing today and
        Inventory tomorrow would lose the side-by-side
        visibility.*
  - **(c) Landing "module picker" home.** The `#/` default
        routes to a tile-grid landing page; clicking a module
        tile drills into its first route. *Rejected — hides ALL
        routes behind a click; explicitly the kind of
        nav-replacement the roadmap Tier 3 refuses.*
  - **(d) Co-equal three-module sidebar.** The first draft of
        this ADR. *Rejected per §0 — Settings + Master Data
        clutter the operational sidebar with operator-rare-touch
        items.*

**Picked: (a') single-area sidebar + topbar gear for area swap.**
Justification:

  - **Honors the §0 refinement.** Operational modules live in
    the primary sidebar; maintenance modules are one click away
    behind the gear. The operator working invoices all day
    never sees Settings / NAV Credentials in their chrome unless
    they explicitly enter maintenance.
  - **Operator-clarity preservation in operational mode.** All
    operational routes are visible at first glance; today
    that's just Invoices, but the moment a second operational
    module lands (e.g. Inventory), the sidebar shows both
    side-by-side without the operator having to navigate.
  - **Maintenance is its own layout, not a buried submenu.**
    When the operator hits the gear, the sidebar's contents
    swap — they're no longer in the operational view, they're
    in the maintenance view with its own area caption + its
    own module list. The chrome makes the area transition
    visible (the surface tone shifts faintly per §3 styling).
    This matches Ervin's "master data maintenance dashboard or
    layout" framing.
  - **Bookmarks + deep links still work.** A bookmarked
    `#/tenant` still mounts Tenant Settings directly; the
    chrome derives `activeArea = areaForRoute("tenant") =
    "maintenance"` and renders the maintenance sidebar on first
    paint. No URL rewrite, no redirect.
  - **Minimum disruption.** Two visible chrome additions only
    (the topbar gear button + a sidebar area caption); the
    sidebar's internal two-level structure is the same one the
    original draft proposed. CLAUDE.md rule 3.
  - **Keyboard nav compatibility.** Pick (a')'s sidenav items
    remain `<a>` tags exactly as today; the gear is a `<button>`.
    Neither is a typing target per PR-68's `isTypingTarget`, so
    the `/` + `j` / `k` hotkeys on list views are unaffected.
    No new keychord competes with PR-68's chord state.

**Visual contract (binds PR-78):**

  - **Topbar gear button** — small, secondary-text, monospace
    + uppercase + letter-spacing, sits to the LEFT of the
    backend-status pill. Two states:
      - In operational area: `⚙ MAINTENANCE`.
      - In maintenance area: `← OPERATIONAL` (slightly
        stronger text colour so the way back is the visual
        primary in maintenance mode).
  - **Sidebar caption** — caps + monospace + strong text colour
    at the top of the sidebar, labels the active area. Above
    the area's modules.
  - **Sidebar surface tone** — the maintenance area sidebar uses
    a faintly different surface variable
    (`--color-surface-base` if defined, falls back to
    `--color-surface-raised`) so the operator recognises "I am
    in the configuration area, not in my daily workflow". The
    shift is subtle — strong enough to be a cue, not strong
    enough to feel like a different app.
  - **Module header rows** — `glyph` + `label_en` (English in
    the chrome today; bilingual labels are stored but the EN
    label renders — a future i18n widening will pick HU when
    the OS locale is `hu`).
  - **Module headers are NOT clickable navigation targets**
    (they are not routes; they group routes). Default expanded;
    PR-78 does NOT ship a collapse toggle (CLAUDE.md rule 2 —
    add when density bites, not speculatively).
  - **Route rows** — same `.sidenav__item` look as today (no
    glyph, indented under the module header).
    `aria-current="page"` on the active route, unchanged from
    today.
  - **"Parent-module-of-active-route" is marked subtly** (slightly
    stronger text colour on the module header), via a CSS class
    derived from `moduleForRoute(route).id`.

**Footnote — future per-area landing dashboard.** Ervin's §0
language ("master data maintenance dashboard or layout") leaves
room for an explicit landing dashboard inside the maintenance
area — a tile grid showing all maintenance modules with their
routes, mounted at e.g. `#/maintenance`. PR-78 deliberately does
NOT ship this. The sidebar IS the area's layout today; an
explicit landing dashboard becomes valuable when maintenance
holds 5+ modules and the operator wants a glanceable overview.
At that point, a future PR adds an `AppRoute = "maintenance-
home"` (or similar), wires it as the area's `defaultRouteForArea`
target, and the chrome's gear navigates there instead of to the
first module's first route. The registry helper already isolates
this decision in `defaultRouteForArea`; no chrome refactor
needed.

### §4 — Pure-module helper API (binds PR-78)

`apps/aberp-ui/ui/src/lib/erp-modules.ts` exports:

  - `type ErpArea = "operational" | "maintenance"`
  - `type ErpModuleId = "invoicing" | "master-data" | "settings"`
  - `interface ErpRouteRef { id: AppRoute; label: string }`
  - `interface ErpModule { id; area; label_hu; label_en; glyph; routes }`
  - `const AREA_LABELS: Record<ErpArea, { hu; en }>`
  - `const MODULES: ErpModule[]`
  - `function moduleForRoute(route): ErpModule | null`
  - `function areaForRoute(route): ErpArea`
  - `function modulesInArea(area): ErpModule[]`
  - `function defaultRouteForArea(area): AppRoute | null`

All pure-data + pure lookups. No DOM, no fetch, no Svelte
dependency. Pinned by `erp-modules.test.ts`.

### §5 — Backend module boundary (FUTURE — documented, not built in PR-78)

When the backend slice of this initiative lands (PR-79+), it
materialises three changes; PR-78 ships NONE of them:

  1. **Route namespacing.** Routes move from flat `/api/<thing>`
     to `/api/<module-id>/<thing>`. Note the AREA namespace is
     NOT in the URL — the area concept is a chrome-side
     usage-frequency grouping, not a backend authorization
     boundary. The module id IS the URL prefix:
       - `/api/invoices/...`           → `/api/invoicing/invoices/...`
       - `/api/partners/...`           → `/api/master-data/partners/...`
       - `/api/seller-info`            → `/api/settings/seller-info`
       - `/api/seller/banks`           → `/api/settings/seller/banks`
       - `/api/nav/credentials/...`    → `/api/settings/nav/credentials/...`
       - NAV operational routes (`/api/nav/submit`, `/api/nav/poll`)
         stay under `/api/invoicing/nav/...` because they
         exercise the invoicing flow, not the settings flow.
     The exact landing PR MUST ship redirect / compat shims (the
     old paths return 308 to the new ones, or the handlers
     register under both paths during a deprecation window) so
     the SPA can migrate independently of the backend cut.
  2. **Crate / module organization.** The `apps/aberp/src/` files
     today are flat (`serve.rs`, `nav_xml.rs`, `partners.rs`,
     `setup_seller_info.rs`, etc.). A natural reorganization is
     a `modules/<module-id>/` subtree per module, each carrying
     its handler entry points + its module-private types. Cross-
     module primitives (audit ledger, typestate, secrets, tenant
     resolution) stay in `crates/` exactly as today — those ARE
     the platform, by design.
  3. **Module capability registry.** A typed Rust `enum ErpModule
     { Invoicing, MasterData, Settings, ... }` mirrors the SPA's
     `ErpModuleId`. Each module declares a `Capabilities` block:
     which database stores it owns, which keychain entries it
     reads/writes, which other modules it depends on (for the
     dependency-DAG validation a future PR adds). The registry's
     value is type-system enforcement that a module's handlers
     don't reach across module lines outside the declared
     capabilities.

**§5 is INTENT, not contract.** A future ADR (likely ADR-0042 or
ADR-0043, whichever the backend-PR session draws first) will
pin §5 the way ADR-0040 pinned the multi-bank schema — at the
moment the backend code starts to encode it. PR-78 is unblocked
because the SPA's module ids are chosen to *match* the §5
namespace exactly, so the backend cut doesn't force a SPA-side
rename later.

### §6 — Migration phasing

| PR    | Slice                                         | Risk      | Touches backend? |
| ----- | --------------------------------------------- | --------- | ---------------- |
| PR-78 | SPA shell: area + module registry + single-area sidebar + topbar gear | Very low (additive, behind no flag, no backend call) | No |
| PR-79 | Backend route namespacing — invoicing module  | Medium (handler relocation + compat shim + SPA fetch URL bump) | Yes |
| PR-80 | Backend route namespacing — master-data + settings modules | Medium (same shape as PR-79, smaller surface) | Yes |
| PR-81 | Backend file-system reorganization — `modules/<id>/` subtree | Medium-low (rename + import path) | Yes (zero-behavior) |
| PR-82 | Module capability registry + dependency DAG validation | Medium (introduces a new compile-time check) | Yes |
| later | First brand-new operational module (likely `inventory` driven by Ervin's CNC-job-line-item need) | Per-PR | Yes |
| later | Per-area landing dashboard if/when maintenance grows past ~5 modules | Low | No (SPA-only) |

PR-78 (this ADR's build slice) is **fully reversible** — undoing
it is a `git revert` of the SPA chrome diff. PR-79+ involve
backend routes and are SQUARELY in "needs a compat shim"
territory.

### §7 — Out of scope (explicit refusals + deferrals)

  - **No KPI dashboard, no landing module picker.** Roadmap Tier 3
    pushback applies. If Ervin asks for one later, it's a *fifth
    something* (either a dashboard module with one route, or a
    `#/` landing the operator can opt out of), not a replacement.
  - **No empty-module placeholders.** A `Future modules` table in
    §2 names what's reserved; those modules MUST NOT appear in
    the chrome until they ship a real route.
  - **No backend changes in PR-78.** §5 + §6 are documented; the
    backend cut is its own session(s).
  - **No `#/maintenance` landing route in PR-78.** The maintenance
    sidebar layout IS the area's layout today. The §3 footnote
    leaves an explicit landing dashboard as a future widening
    when maintenance has 5+ modules.
  - **No HU-locale toggle in PR-78.** Bilingual labels are
    *stored* on every module and area, but the chrome renders
    `label_en` today because every existing screen is English-
    labeled.
  - **No glyph icon library.** Single Unicode marks today; if a
    real icon set is wanted later, that's a separate ADR with a
    design-language amendment.
  - **No nav collapse toggle.** Default expanded. Add when density
    bites, not speculatively (CLAUDE.md rule 2).
  - **No route-label i18n.** Route labels in the sidenav stay
    English today (e.g. "Invoices", "Partners"). Lift when the
    rest of the chrome lifts.
  - **No keyboard shortcut for the area swap in PR-78.** A
    future PR could bind a hotkey (e.g. `g m` for "go to
    maintenance") via PR-68's parser, but PR-78 doesn't add one
    — the brief said no PR-68 expansion.

### §8 — Test posture (binds PR-78)

The pure-module helper `erp-modules.ts` ships with Vitest pins
covering:

  1. **Registry shape**: every entry carries non-empty `id`,
     `label_hu`, `label_en`, `glyph`, `routes`, and a closed-
     vocab `area`. Module ids are unique; route ids are unique
     across the registry.
  2. **Total route coverage**: every value of `AppRoute` (the
     existing closed union from `router.ts`) appears in exactly
     one module's `routes` list. A new `AppRoute` variant added
     without a registry entry fails this pin loudly.
  3. **`moduleForRoute` lookups**: every existing route returns
     the typed module that owns it; the helper is exhaustive (no
     `null` return for any `AppRoute`).
  4. **Area split**: each route's area matches the §2 table;
     `areaForRoute(route)` agrees with
     `moduleForRoute(route)?.area`; `AREA_LABELS` has non-empty
     bilingual labels for every area.
  5. **Area helpers**: `modulesInArea` preserves registry order
     within each area and partitions the registry exactly
     (no module appears in zero or two areas);
     `defaultRouteForArea` returns the first route of the first
     module in that area.

The Svelte chrome itself is not directly unit-pinned in PR-78
(it's chrome, not logic); the existing `npm run check` + the
already-passing PR-68 keyboard-nav pins continue to guarantee
the hotkey layer isn't broken.

### §9 — Failure mode if a future PR adds a route without updating the registry

Concrete failure: dev adds `AppRoute = "..." | "inventory-items"`
in `router.ts`, forgets to add it to a module's `routes` list in
`erp-modules.ts`. The §8 pin "every AppRoute appears in exactly
one module" fails at `npm test`, naming the orphan route. The
build does not silently sweep the new route into a "misc"
bucket — there is no misc bucket. Additionally,
`areaForRoute("inventory-items")` would return the defensive
fallback `"operational"`, so the new route would chrome-mount in
the operational sidebar with no parent module header — visibly
wrong, not silently wrong.

## Consequences

  - The chrome reframe lands with zero operator-flow churn: every
    existing hash route still works, the active route still
    highlights, every existing test continues to pass. The only
    visible change is that Settings + NAV Credentials + Partners
    are no longer in the operational sidebar — they're one
    intentional click away behind the topbar gear, in their own
    maintenance layout.
  - Future modules add cleanly: extend the `ErpModuleId` union,
    add a registry entry with its chosen `area`, add the route(s)
    to `router.ts` (which already requires a typed widening).
    Three coordinated edits, each loud at compile / test time if
    missed.
  - The backend cut (PR-79+) inherits a stable module-id
    vocabulary from this ADR; SPA fetch URLs become
    `/api/<module-id>/...` mirroring the registry's module ids.
    The SPA's mental model and the operator's HTTP-trace mental
    model converge. The area concept stays SPA-side only — it's a
    chrome usage-frequency grouping, not a backend authorization
    boundary.
  - The ADR explicitly refuses four drift modes that would
    otherwise creep in over the next several sessions: dashboard-
    replaces-nav, empty-module-placeholders, backend-routes-flat-
    forever, and the co-equal-three-module sidebar the first draft
    landed on. Each is now a written refusal someone has to argue
    against, not a silent default.
  - The cost: a small chrome refactor + one new pure-module
    helper + one ADR. PR-78's actual diff is small; the strategic
    value is the named boundary, not the lines of code.

## Pushback

  - **"Two areas with one operational module each feels like
    over-architecture."**
    The point of doing it NOW rather than at module five is that
    the grouping is cheap to establish when modules are few;
    retrofitting it across a sprawled flat nav would be a bigger
    PR with more operator-flow risk. The area concept's value
    becomes visible the moment a SECOND operational module lands
    (Inventory) — at that point the operator wants Inventory +
    Invoicing side-by-side in the sidebar, NOT diluted by
    Settings / NAV Credentials.
  - **"Why not just rename `tenant` → `settings/tenant` in the
    hash too?"**
    Considered, refused. Hash routes are operator-visible URLs
    and existing muscle memory / bookmarks reach for `#/tenant`.
    Renaming the hash also forces a backend coordination PR
    that PR-78 explicitly avoids. The hash form is the API; the
    chrome is the presentation; this ADR keeps them orthogonal
    until §5 / §6 land.
  - **"Why not put the maintenance modules in a dropdown under a
    single 'Settings' sidebar item instead of swapping the entire
    sidebar?"**
    A dropdown would still co-list maintenance in the operational
    chrome (it's *in* the sidebar), violating the usage-frequency
    separation. The full area swap is the cleanest "one level
    removed" answer: the operator doesn't see maintenance until
    they intentionally enter it.
  - **"The topbar gear could be missed by new operators."**
    The button is small but it's persistent (always in the
    topbar, always to the left of the backend-status pill), it
    carries an explicit "MAINTENANCE" label (not just an icon),
    and it has a tooltip explaining what it opens. Operators who
    need to reach Settings will find it via discovery in seconds;
    operators who don't need to reach it benefit from a less
    cluttered daily workflow.
  - **"Sidebar surface-tone shift for maintenance is too subtle."**
    Acknowledged. PR-78 ships it subtle by design (ADR-0017 —
    ambient, never theatrical); if Ervin reports the cue is
    insufficient during operator validation, a future PR makes
    it stronger (e.g. a coloured top border on the maintenance
    sidebar). One-CSS-rule change, no logic change.
