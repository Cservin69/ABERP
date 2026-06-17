// S443 / ADR-0092 — pure presentation helpers for the QC verdict chip.
// The colour mapping + the bilingual label live here (no Svelte, no
// Tauri) so vitest can pin them and every component (RecordInspection,
// the WO inspection list, the Material-Traceability inspection history)
// renders the chip from one source of truth.
//
// Colour mapping (per the ADR-0092 SPA brief):
//   pass               → green   (--color-signal-positive)
//   minor, major       → yellow  (--color-signal-warning)
//   critical           → red     (--color-signal-negative)
//   calibration_stale  → grey    (--color-signal-muted)
//
// Pinned by `verdict.test.ts`.

import type { Verdict } from "./api";

/** S443 — categorical chip class for a verdict. The four CSS modifier
 * classes map onto the four signal tokens; components carry the
 * matching `.verdict-chip--*` rules. A verdict the closed vocab
 * doesn't cover falls back to the muted modifier so an unknown value
 * renders visibly (CLAUDE.md rule 12) rather than unstyled. */
export function verdictChipClass(verdict: Verdict | string): string {
  switch (verdict) {
    case "pass":
      return "verdict-chip verdict-chip--pass";
    case "minor":
    case "major":
      return "verdict-chip verdict-chip--warning";
    case "critical":
      return "verdict-chip verdict-chip--critical";
    case "calibration_stale":
      return "verdict-chip verdict-chip--stale";
    default:
      return "verdict-chip verdict-chip--stale";
  }
}

/** S443 — human label for a verdict. Bilingual HU / EN to match the
 * neighbouring chips; falls back to the raw string for an unknown
 * value (a SPA older than the backend). */
export function verdictLabel(verdict: Verdict | string): string {
  switch (verdict) {
    case "pass":
      return "Megfelelt / Pass";
    case "minor":
      return "Kisebb eltérés / Minor";
    case "major":
      return "Jelentős eltérés / Major";
    case "critical":
      return "Kritikus / Critical";
    case "calibration_stale":
      return "Kalibráció lejárt / Calibration stale";
    default:
      return verdict;
  }
}
