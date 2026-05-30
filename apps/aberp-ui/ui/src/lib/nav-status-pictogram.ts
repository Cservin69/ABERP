// PR-95 / session-115 — NAV-status pictogram. Pure-module helper
// that collapses the eleven `InvoiceState` labels into a closed-vocab
// pictogram signal for the operator's NAV-ack readout, and names
// whether the pictogram is an actionable click-to-recheck affordance.
// Drives the new pictogram cell on `InvoiceList.svelte` (state column)
// and the prominent pictogram on `InvoiceDetail.svelte` (header).
//
// Supersedes the PR-80 manual `↻ Lekérés` / Poll-ack button in the
// detail action bar AND every other manual poll affordance the SPA
// previously surfaced.
//
// PR-98 / session-122B — `InFlight` split into two states. Ervin's
// rule: "Pending = operator hasn't sent it yet" (no submit attempted
// or NAV hasn't started processing) is meaningfully different from
// "Submitted = operator's done their part, NAV is working." The first
// stays muted (operator owns the next action); the second is a green-
// toned positive-in-progress (operator can rest, NAV is processing).
// The mapping flips Pending / PendingNavExists from InFlight into
// NotSubmitted; Submitted / Recovered stay in the new `Submitted`
// pictogram bucket (renamed from `InFlight`). Click-to-recheck stays
// on the new `Submitted` bucket only.
//
// Closed-vocab posture per CLAUDE.md rule 11 + ADR-0017 §"Adversarial
// review #4": the four `NavPictogramState` variants are the
// load-bearing categorical signal; the glyph carries the colour-blind-
// safe signal in addition to the per-state border colour. Pinned by
// `nav-status-pictogram.test.ts` (exhaustively over the eleven
// `InvoiceState` members + an unknown-string fallback).

import type { InvoiceState } from "./api";

/** Closed-vocab pictogram signal. Mirrors the four operator-meaningful
 * NAV-ack outcomes Ervin's PR-98 brief named verbatim:
 *   - `NotSubmitted` — invoice exists locally; operator hasn't sent it
 *                      yet (Draft / pre-submission / NAV-side Pending
 *                      without an operator-initiated submit).
 *   - `Submitted`    — operator has submitted to NAV; NAV is
 *                      processing (post-submit, pre-terminal-ack).
 *                      Green-toned positive-in-progress: the submit
 *                      succeeded; the ack is still being awaited.
 *   - `Rejected`     — terminal negative (NAV ABORTED, or operator
 *                      walked away from a stuck submission).
 *   - `Final`        — terminal positive (NAV SAVED; includes
 *                      downstream lifecycle states whose base was
 *                      SAVED — Storno, Amended). */
export type NavPictogramState =
  | "NotSubmitted"
  | "Submitted"
  | "Rejected"
  | "Final"
  // Session 162 — operational-paid superset of `Final`. An invoice that
  // is NAV-terminal-positive (SAVED → `Final`) AND carries a payment
  // record collapses to ONE visual: the bag-of-coins pictogram. Ervin's
  // ask (2026-05-29): "on paid invoices no need to stack statuses like
  // green check, Finalised, Paid. One final is enough as it supposed to
  // have all priors." Paid is a strict superset of Final (mark-as-paid
  // is `Finalized`-gated at the backend route per ADR-0039), so the
  // bag-of-coins implies the prior SAVED-ack state; the renderer drops
  // the separate `Finalized` chip + `Paid` pill when this state shows.
  | "Paid";

/** Display + behaviour affordance for one of the four pictogram states.
 * Mirrors `LabelMeta` in `labels.ts` shape-wise (glyph + tooltip), but
 * splits the tooltip into HU + EN halves and adds the `actionable`
 * boolean that names the click-to-recheck affordance. */
export interface NavPictogramMeta {
  /** The closed-vocab state. Renderer reads to pick a class /
   * affordance; never used as a label string itself. */
  state: NavPictogramState;
  /** Single unicode glyph. The categorical signal per ADR-0017
   * §"Adversarial review #4". */
  glyph: string;
  /** Concise Hungarian tooltip. Ervin's word: "really concise." */
  tooltip_hu: string;
  /** Concise English tooltip — same scope, for non-HU readers /
   * screen-reader fallback. */
  tooltip_en: string;
  /** CSS class the renderer applies for per-state border / colour
   * styling. One of: `pictogram-muted`, `pictogram-submitted`,
   * `pictogram-negative`, `pictogram-positive`. PR-98 split: the
   * pre-PR-98 `pictogram-warning` class is retired alongside the
   * collapsed `InFlight` state. */
  kind_class: string;
  /** Whether clicking the pictogram should re-poll NAV. True iff
   * `state === "Submitted"` — the only state where a fresh
   * queryTransactionStatus call can advance the displayed state.
   * False for the other three: NotSubmitted has nothing to poll
   * (the operator hasn't submitted yet); Rejected and Final are
   * terminal so a poll cannot change them. The renderer reads
   * this to pick the cursor (`pointer` vs `help`) and to wire
   * the click handler. */
  actionable: boolean;
}

/** Map an `InvoiceState` (or an unknown string the backend invented)
 * to the pictogram affordance. The mapping table:
 *
 *   InvoiceState        | NavPictogramState
 *   --------------------+-------------------
 *   Unknown             | NotSubmitted   (no audit entries at all)
 *   Ready               | NotSubmitted   (draft; not submitted yet)
 *   Pending             | NotSubmitted   (PR-98 — pre-submission lifecycle)
 *   PendingNavExists    | NotSubmitted   (PR-98 — pre-submission lifecycle)
 *   Submitted           | Submitted      (operator submitted; NAV processing)
 *   Recovered           | Submitted      (state reconstructed; NAV processing)
 *   Finalized           | Final          (SAVED — terminal positive)
 *   Rejected            | Rejected       (ABORTED — terminal negative)
 *   Storno              | Final          (base was SAVED; chain has a storno entry)
 *   Amended             | Final          (base was SAVED; chain has a modification entry)
 *   Abandoned           | Rejected       (operator-marked terminal — no NAV-final path remaining)
 *
 * PR-98 rationale: Ervin's mental model treats `Pending` /
 * `PendingNavExists` as "operator hasn't completed their submit yet"
 * (the lifecycle reads as an *operator-action* state) rather than as
 * "in flight at NAV." The pictogram therefore stays muted ◌ for these
 * two: it tells the operator "you still have to do something." The
 * `Submitted` / `Recovered` states are the post-submit-pre-terminal
 * pair where the operator's done their part and NAV is processing —
 * those get the green-toned positive-in-progress glyph that says
 * "you can rest; NAV is working."
 *
 * Unknown strings (backend invented a label the SPA does not model)
 * fall back to `NotSubmitted` with a muted "?" glyph per CLAUDE.md
 * rule 12 (fail visible, not silent). */
export function navStatusPictogram(
  state: InvoiceState | string,
  isPaid: boolean = false,
): NavPictogramMeta {
  const base = basePictogram(state);
  // Session 162 — Paid is a strict superset of `Final`. Collapse the
  // (Final + payment-recorded) pair into the single bag-of-coins
  // pictogram so the operator reads ONE final visual instead of the
  // pre-162 stack (`✓` pictogram + `Finalized` chip + `Paid` pill).
  // Defensive: only Final invoices can carry a payment (mark-as-paid is
  // `Finalized`-gated at the backend route per ADR-0039 §2), so an
  // `isPaid` on a non-Final base — which would indicate a backend
  // precondition breach — falls through to the base mapping rather than
  // masking the anomaly behind the Paid glyph (CLAUDE.md rule 12).
  if (isPaid && base.state === "Final") {
    return {
      state: "Paid",
      // Bag-of-coins — Ervin's named glyph ("the bag of coins
      // pictogram as I loved it"). Same 💰 the mark-as-paid action
      // uses, so the operator recognises the affordance→state pair.
      // The green-toned `pictogram-positive` border carries the
      // NAV-accepted (SAVED) signal the bag's own emoji colour cannot;
      // green-border + bag = "accepted AND paid" in one square.
      glyph: "💰",
      tooltip_hu: "Kifizetve",
      tooltip_en: "Paid",
      kind_class: "pictogram-positive",
      actionable: false,
    };
  }
  return base;
}

/** The unpaid base mapping — `InvoiceState` → one of the four
 * NAV-ladder pictogram states. Factored out of `navStatusPictogram` so
 * the operational-paid superset layers cleanly on top without
 * duplicating the eleven-state switch. */
function basePictogram(state: InvoiceState | string): NavPictogramMeta {
  switch (state) {
    case "Unknown":
    case "Ready":
    case "Pending":
    case "PendingNavExists":
      return {
        state: "NotSubmitted",
        glyph: "◌",
        tooltip_hu: "Még nincs NAV-ra küldve",
        tooltip_en: "Not submitted to NAV yet",
        kind_class: "pictogram-muted",
        actionable: false,
      };
    case "Submitted":
    case "Recovered":
      return {
        state: "Submitted",
        glyph: "⌛",
        tooltip_hu: "Beküldve, NAV feldolgozza — kattints lekéréshez",
        tooltip_en: "Submitted, NAV processing — click to re-poll",
        kind_class: "pictogram-submitted",
        actionable: true,
      };
    case "Rejected":
    case "Abandoned":
      return {
        state: "Rejected",
        glyph: "⚠",
        tooltip_hu: "Beküldve, NAV elutasította",
        tooltip_en: "Submitted, rejected by NAV",
        kind_class: "pictogram-negative",
        actionable: false,
      };
    case "Finalized":
    case "Storno":
    case "Amended":
      return {
        state: "Final",
        glyph: "✓",
        tooltip_hu: "Beküldve, NAV elfogadta",
        tooltip_en: "Submitted, accepted by NAV",
        kind_class: "pictogram-positive",
        actionable: false,
      };
    default:
      // Unknown string — backend invented a label the SPA does not
      // model. Surface the muted "?" so the divergence is visible per
      // CLAUDE.md rule 12, not silently bucketing with one of the
      // four known classes.
      return {
        state: "NotSubmitted",
        glyph: "?",
        tooltip_hu: `Ismeretlen állapot: ${state}`,
        tooltip_en: `Unknown state: ${state}`,
        kind_class: "pictogram-muted",
        actionable: false,
      };
  }
}
