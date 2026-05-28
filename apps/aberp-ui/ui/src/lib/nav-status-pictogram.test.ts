// PR-95 / session-115 — pin tests for `navStatusPictogram`.
//
// PR-98 / session-122B — `InFlight` split into two pictogram states
// (`NotSubmitted` for the pre-submission lifecycle pair, new
// `Submitted` for the post-submit-pre-terminal NAV-processing pair).
// The mapping table is the only place every InvoiceState collapses
// into the pictogram vocab; pinning each of the eleven InvoiceState
// members + the unknown-string fallback catches a regression at
// `npm test` rather than at operator-survey time per CLAUDE.md rule
// 12 (fail loud).
//
// CLAUDE.md rule 9 — per-state coverage means a regression that
// collapsed every state to one pictogram (or returned a constant)
// cannot pass every assertion vacuously: each row asserts a distinct
// `(state, actionable, glyph)` triple.

import { describe, expect, it } from "vitest";

import {
  navStatusPictogram,
  type NavPictogramState,
} from "./nav-status-pictogram";
import type { InvoiceState } from "./api";

interface Expected {
  state: InvoiceState;
  pictogramState: NavPictogramState;
  glyph: string;
  actionable: boolean;
}

// Exhaustive over the eleven `InvoiceState` members per ADR-0036 §2.
// Same shape + ordering as the `buttonsForState` and
// `quickActionsForState` tables so a reader scanning the three
// per-state mappings side-by-side sees them as a coherent surface.
const TABLE: Expected[] = [
  // Unknown — no audit-ledger entries; nothing has been submitted.
  { state: "Unknown", pictogramState: "NotSubmitted", glyph: "◌", actionable: false },
  // Ready — draft exists locally; no submission attempted yet.
  { state: "Ready", pictogramState: "NotSubmitted", glyph: "◌", actionable: false },
  // PR-98 — `Pending` reads as "operator hasn't completed the submit
  // yet" per Ervin's operator-action mental model; the pictogram
  // stays muted ◌ to signal the operator still owns the next step.
  { state: "Pending", pictogramState: "NotSubmitted", glyph: "◌", actionable: false },
  // PR-98 — same posture as `Pending`. The pre-submission lifecycle
  // pair collapses into the operator-owns-this NotSubmitted bucket.
  { state: "PendingNavExists", pictogramState: "NotSubmitted", glyph: "◌", actionable: false },
  // PR-98 — operator submitted; NAV is processing. Green-toned
  // positive-in-progress signal; click-to-recheck is wired here only.
  { state: "Submitted", pictogramState: "Submitted", glyph: "⌛", actionable: true },
  // PR-98 — Recovered shares the post-submit-pre-terminal posture.
  { state: "Recovered", pictogramState: "Submitted", glyph: "⌛", actionable: true },
  // Finalized — terminal SAVED ack.
  { state: "Finalized", pictogramState: "Final", glyph: "✓", actionable: false },
  // Rejected — terminal ABORTED ack.
  { state: "Rejected", pictogramState: "Rejected", glyph: "⚠", actionable: false },
  // Storno — base was SAVED + has a storno chain entry; ack-wise final.
  { state: "Storno", pictogramState: "Final", glyph: "✓", actionable: false },
  // Amended — base was SAVED + has a modification chain entry; ack-wise final.
  { state: "Amended", pictogramState: "Final", glyph: "✓", actionable: false },
  // Abandoned — operator-marked terminal; NAV-side will never land.
  { state: "Abandoned", pictogramState: "Rejected", glyph: "⚠", actionable: false },
];

describe("navStatusPictogram", () => {
  for (const { state, pictogramState, glyph, actionable } of TABLE) {
    it(`maps state=${state} → ${pictogramState} (${glyph}, actionable=${actionable})`, () => {
      const meta = navStatusPictogram(state);
      expect(meta.state).toBe(pictogramState);
      expect(meta.glyph).toBe(glyph);
      expect(meta.actionable).toBe(actionable);
    });
  }

  it("covers every InvoiceState union member", () => {
    // Counter-pin per CLAUDE.md rule 9: a future InvoiceState member
    // added to api.ts would be flagged by TypeScript exhaustiveness on
    // the navStatusPictogram switch (no default arm reachable on the
    // typed union), but this runtime pin asserts the test table also
    // grows in lockstep so the four-class collapse stays load-bearing.
    const stateNames = TABLE.map((row) => row.state).sort();
    const expected: InvoiceState[] = [
      "Abandoned",
      "Amended",
      "Finalized",
      "Pending",
      "PendingNavExists",
      "Ready",
      "Recovered",
      "Rejected",
      "Storno",
      "Submitted",
      "Unknown",
    ];
    expect(stateNames).toEqual(expected);
  });

  it("actionable is true ONLY for the new Submitted pictogram state", () => {
    // PR-98 invariant: the pictogram doubles as a click-to-recheck
    // affordance ONLY for the post-submit-pre-terminal `Submitted`
    // bucket (where a fresh queryTransactionStatus call CAN advance
    // the displayed state). In the other three classes, polling
    // cannot help: NotSubmitted has nothing on NAV yet (operator
    // owns the next step); Rejected and Final are terminal so a
    // poll cannot change them. A regression that re-enabled
    // actionable on `NotSubmitted` would lead the operator to click
    // a no-op pictogram on a Draft / Pending invoice and see
    // nothing happen.
    for (const { state, pictogramState, actionable } of TABLE) {
      const expected = pictogramState === "Submitted";
      expect(
        actionable,
        `state=${state} (pictogramState=${pictogramState}): actionable must be ${expected}`,
      ).toBe(expected);
    }
  });

  it("every state returns a non-empty bilingual tooltip + glyph + kind_class", () => {
    // Closed-vocab guard: every pictogram has a glyph, a HU tooltip, an
    // EN tooltip, and a kind_class. A regression that omitted any
    // would render a half-formed pictogram — the operator would see a
    // pictogram without a tooltip or without a glyph.
    for (const { state } of TABLE) {
      const meta = navStatusPictogram(state);
      expect(meta.glyph.length).toBeGreaterThan(0);
      expect(meta.tooltip_hu.length).toBeGreaterThan(0);
      expect(meta.tooltip_en.length).toBeGreaterThan(0);
      expect(meta.kind_class.length).toBeGreaterThan(0);
    }
  });

  it("unknown string falls back to NotSubmitted with the '?' glyph", () => {
    // Backend invented a label this SPA does not model. CLAUDE.md
    // rule 12 — surface the divergence visibly (muted "?" pictogram +
    // the invented string in the tooltip) instead of silently bucketing
    // with one of the four known classes. Mirrors `labelMeta`'s
    // unknown-state fallback posture in `labels.ts`.
    const meta = navStatusPictogram("SomeFutureState" as InvoiceState);
    expect(meta.state).toBe("NotSubmitted");
    expect(meta.glyph).toBe("?");
    expect(meta.actionable).toBe(false);
    expect(meta.tooltip_hu).toContain("SomeFutureState");
    expect(meta.tooltip_en).toContain("SomeFutureState");
    expect(meta.kind_class).toBe("pictogram-muted");
  });

  it("the new Submitted pictogram glyph is distinct from the Final glyph", () => {
    // PR-98 visual-distinction invariant: the post-submit-pre-terminal
    // state must not collide with the terminal-positive Final glyph.
    // The brief: "submitted-not-final should be positive/green (the
    // actual submit succeeded), pending stays neutral." Both pictograms
    // ride green CSS tones, so the glyph difference is the load-bearing
    // visual signal. A regression that reused `✓` for both would erase
    // the at-a-glance distinction between "NAV processing" and "NAV
    // accepted."
    expect(navStatusPictogram("Submitted").glyph).not.toBe(
      navStatusPictogram("Finalized").glyph,
    );
    expect(navStatusPictogram("Finalized").glyph).toBe("✓"); // SAVED
    expect(navStatusPictogram("Submitted").glyph).toBe("⌛"); // NAV processing (PR-98)
    expect(navStatusPictogram("Rejected").glyph).toBe("⚠"); // ABORTED
  });

  it("pictogram kind_class is one of the four closed-vocab CSS classes", () => {
    // Closed-vocab over the CSS classes the renderer maps to. Adding
    // a fifth visual signal would require lifting the closed vocab in
    // lockstep with the renderer's stylesheet; this pin catches a
    // drift that introduces an unmapped class. PR-98 retires the
    // pre-PR-98 `pictogram-warning` and adds `pictogram-submitted`
    // (the new green-toned positive-in-progress kind).
    const allowed = new Set([
      "pictogram-muted",
      "pictogram-submitted",
      "pictogram-negative",
      "pictogram-positive",
    ]);
    for (const { state } of TABLE) {
      const meta = navStatusPictogram(state);
      expect(
        allowed.has(meta.kind_class),
        `state=${state} → kind_class ${meta.kind_class} is not in the closed vocab`,
      ).toBe(true);
    }
  });
});
