# ADR-0017 — ABERP design language

- **Status:** Proposed
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Related:** ADR-0004 (Tauri + Svelte)
- **Influences:** situation_room ADR-0006 (design language) — same discipline, different context

## Context

ABERP's users are not casual. They are an operator running a CNC shop,
an accountant reviewing inbound and outbound invoices against the Hungarian
tax authority record, a warehouse worker scanning vignettes on the shop floor,
and a tax inspector who arrived with a deadline. The product must look like
the tool a serious person picks; not like a freemium SaaS.

The visual language must support four different modes of use:

1. **Dense reading** — invoice ledger, inventory balances, audit log review.
   Lots of rows, lots of numbers, lots of state. Discoverable at a glance.
2. **Form entry** — issuing an invoice, creating a product, registering a
   purchase order. Friction matters; alignment, validation, and keyboard
   flow matter more than air.
3. **Shop-floor scanning** — a few buttons, large hit targets, gloved hands,
   bright ambient light, possibly a worn small touchscreen on a forklift.
   Not the same medium as the desktop.
4. **Print and PDF output** — invoices, labels, vignettes. The design system
   has to extend off-screen to paper without re-inventing.

The space of analytical UIs falls into two familiar camps: the Bloomberg
clone (loud, dense, hostile to read for hours), and the generic SaaS
dashboard (airy, illustration-rich, indistinguishable from a CRM). Neither
serves ABERP's users. We take a third path, deliberately.

## Decision

ABERP's visual language is:

**Tufte information-density discipline on a warm-charcoal foundation,
surgical color reserved for meaning, monospace tabular numbers, ambient
kinetic for live data — with two derived modes (shop-floor and print) that
inherit the principles but adjust the medium.**

Five core principles, enforced by design tokens and review:

1. **Chrome is quiet; data is bright.** ~80% of the screen is shades of
   warm charcoal (low-saturation, slight warm tint, not pure black); ~20%
   carries information. Borders, labels, and structure never carry color.
2. **Color means something.** Color is reserved for categorical signal:
   invoice state (paid, overdue, NAV-rejected), inventory health (low,
   reserved, blocked), audit divergence (when ABERP and NAV disagree).
   If you see color, look. If chrome has color, it is a bug.
3. **Numbers are monospace, tabular-figured, right-aligned.** Comparing
   amounts vertically requires digit alignment. This is a legibility
   requirement, not a style preference. Same rule for HUF amounts, stock
   counts, sequence numbers, and timestamps.
4. **Animations are ambient, never theatrical.** A new value fades in
   over ~200ms. A NAV submission in flight has a quiet pulse on the
   affected row. No spinners on data, no skeleton shimmers, no celebratory
   transitions. The operator is working, not being entertained.
5. **Divergence has a signature color.** When ABERP's record disagrees
   with NAV's or with the physical scan of a vignette, the affected cell
   gets a signature violet that no other surface uses. A trained operator
   internalizes "violet means investigate" without reading a label.

### Two derived modes

**Shop-floor mode.** Same tokens, different layout density. Touch targets
≥ 48 px. Single primary action per screen. High-contrast variant of the
charcoal palette for outdoor / bright shop lighting. Reachable from the
desktop by selecting "Shop floor" or by running the binary with
`--profile=shop-floor`. Same backend, same modules.

**Print and PDF.** Invoices, labels, vignettes. Uses a light-on-white
variant of the same tokens, with the same monospace numbers and the same
discipline about meaningful color (state stamps, watermarks). The design
system covers print explicitly; we do not let print be a CSS afterthought.

### Hungarian typography

The product is first delivered in Hungarian. The body face supports full
Hungarian diacritics with proper kerning (`ő`, `ű`, `Ő`, `Ű` in particular
fail in many otherwise-good faces). This is a font-selection requirement,
not a style choice. The font choice itself is deferred to a separate small
ADR or a CHANGELOG note when it ships.

### Tokens are the enforcement mechanism

Design tokens live in a single Svelte module (path TBD when the desktop
shell is scaffolded). Components import only from tokens. A hardcoded hex
value or pixel literal is a code-review block. Drift happens when tokens
are bypassed, not when they are used.

Token namespaces (initial sketch, refined when the first component lands):

- `color.surface.*` — backgrounds, in shades of charcoal.
- `color.text.*` — text in shades of charcoal.
- `color.signal.*` — categorical meaning. `signal.positive`, `signal.negative`, `signal.warning`, `signal.divergence` (the violet), `signal.muted`.
- `space.*` — spacing scale.
- `type.size.*`, `type.family.body`, `type.family.mono` — typography.
- `motion.fade.in`, `motion.pulse.live`, `motion.pulse.divergence` — the only motion tokens.

## Consequences

**Positive**

- High information density: a screen of audit log shows enough rows to
  be scanned without scrolling.
- Color carries meaning consistently — divergence is visible at a glance.
- The product looks like the serious tool it is.
- Print, PDF, shop-floor, and desktop share one discipline.

**Negative**

- Tufte-density is harder to design for than card-heavy SaaS layouts.
  New panels take more thought.
- The aesthetic is polarizing. Some users will want softer visuals; we
  accept this and offer a light-mode variant later if real users ask.
- A motion library is implied. Small bundle cost, accepted.

**Neutral**

- The divergence violet is a brand-level choice. If the brand changes,
  the specific hex does; the principle ("divergence has a signature color")
  does not.
- Shop-floor mode is a layout pivot, not a separate design system. The
  same tokens drive it.

## Adversarial review

- *"You haven't actually built any UI yet — this is premature."* — The
  ADR exists to constrain the first component built, not to retro-fit
  existing screens. Committing to discipline before the first component
  is cheaper than after the tenth.
- *"Shop-floor mode is hand-waved."* — Acknowledged. When the first
  shop-floor screen is built we will either ratify these constraints in a
  follow-up ADR or amend this one. We do not pretend the constraint is
  fully resolved.
- *"Print is not Tufte's medium; you'll fight CSS print engines."* — True;
  print fidelity is a real chore. The discipline of "same tokens, light
  variant" gives us a starting point. Print output for invoices may end up
  using a different renderer (e.g., a PDF library on the Rust side, not
  browser print). That decision goes in a separate ADR.
- *"Color-coded categorical state will mis-cue color-blind users."* —
  Categorical signals are never carried by color alone. Every state carries
  a glyph or label in addition. Accessibility is a constraint on the design
  language, not a footnote.
- *"Animations on financial UIs look unprofessional."* — Theatrical
  animations do. Ambient ones — a 200ms fade on a new audit entry — read
  as "the system is alive", which is what we want for a long-running tool.
- *"Why not just use a vendor design system (Material, Carbon)?"* —
  Because every other ERP looks like a vendor design system. The
  differentiation is in the discipline of density and signal; that does
  not come for free with a vendor kit.

## Alternatives considered

- **Bloomberg-clone aesthetic** — rejected: signals seriousness through
  volume, not through information. Hostile to read for hours.
- **Generic modern SaaS dashboard** — rejected: undercuts the positioning,
  low information density, indistinguishable from a CRM tool.
- **Pure light mode by default** — rejected: bright background over
  multi-hour sessions is fatiguing. A light variant remains available
  for print and for users who request it.
- **Pure black dark mode** — rejected: cold and surgical on modern
  displays; warm charcoal is easier on the eye over long sessions.
- **No motion at all** — rejected: real-time data needs liveness signals;
  static UIs feel dead when values change off-screen.
- **Vendor design system (Material, Carbon, Tailwind UI)** — rejected:
  productizes commodity, loses the discipline that differentiates ABERP.

## Open questions

- Specific font family for body and monospace, including Hungarian
  diacritic coverage. Decided when the desktop shell is scaffolded.
- Exact print-rendering path (browser print vs Rust-side PDF library).
- Light-mode variant tokens — designed only when a real request appears.
- Shop-floor mode breakpoints, hit-target tokens, and outdoor-contrast
  variants — refined with the first shop-floor screen.
