// S426 / ADR-0082 — pure logic for the Snapshots tab.
//
// No DOM, no Svelte, no fetch — sort / filter / summarise / restore-guard
// helpers over the snapshot rows the backend returns. Pinned by
// `snapshots-list.test.ts`. The component (`Snapshots.svelte`) stays dumb:
// it calls these and renders the result.

/** One snapshot row — mirrors `serve::SnapshotListItem` (snake_case wire
 * shape). `size_human` / `age_human` are pre-rendered by the backend. */
export interface SnapshotItem {
  seq: number;
  created_at: string;
  byte_size: number;
  size_human: string;
  valid: boolean;
  invoice_count: number;
  audit_count: number;
  chain_len: number;
  age_human: string;
  validation_error: string | null;
  dir: string;
}

/** Closed-vocab validation-status facet for the filter chips. */
export type SnapshotStatusFacet = "all" | "valid" | "invalid";

/** Closed-vocab sort keys. */
export type SnapshotSortKey = "seq" | "created_at" | "byte_size";
export type SortDir = "asc" | "desc";

/** Filter by validation status. `all` passes everything through. */
export function filterSnapshots(
  items: SnapshotItem[],
  facet: SnapshotStatusFacet,
): SnapshotItem[] {
  if (facet === "all") return items.slice();
  const wantValid = facet === "valid";
  return items.filter((it) => it.valid === wantValid);
}

/** Stable sort by the given key + direction. Does not mutate the input. */
export function sortSnapshots(
  items: SnapshotItem[],
  key: SnapshotSortKey,
  dir: SortDir,
): SnapshotItem[] {
  const sorted = items.slice().sort((a, b) => {
    let cmp: number;
    if (key === "created_at") {
      cmp = a.created_at.localeCompare(b.created_at);
    } else {
      cmp = a[key] - b[key];
    }
    return dir === "asc" ? cmp : -cmp;
  });
  return sorted;
}

/** Headline counts for the tab banner. `newest_valid_seq` is the rollback
 * point retention treats as sacred (never pruned). */
export interface SnapshotSummary {
  total: number;
  valid: number;
  invalid: number;
  newest_valid_seq: number | null;
}

export function summarizeSnapshots(items: SnapshotItem[]): SnapshotSummary {
  let valid = 0;
  let newest_valid_seq: number | null = null;
  for (const it of items) {
    if (it.valid) {
      valid += 1;
      if (newest_valid_seq === null || it.seq > newest_valid_seq) {
        newest_valid_seq = it.seq;
      }
    }
  }
  return {
    total: items.length,
    valid,
    invalid: items.length - valid,
    newest_valid_seq,
  };
}

/** Client-side mirror of the backend restore guard
 * (`aberp_snapshot::ensure_restore_allowed`). Returns a human warning
 * string when the target is unsafe, or `null` when it is acceptable. This
 * is defence-in-depth — the backend STILL enforces; this just lets the UI
 * refuse before a doomed round-trip. */
export function restoreTargetWarning(to: string): string | null {
  const trimmed = to.trim();
  if (trimmed === "") {
    return "Adj meg egy cél elérési utat. / Enter a target path.";
  }
  // Any `.aberp` path component is a live tenant home — refuse it, exactly
  // as the binary does.
  const segments = trimmed.split(/[\\/]+/);
  if (segments.includes(".aberp")) {
    return (
      "A cél egy élő ~/.aberp adatbázis — állíts vissza egy külön útvonalra, " +
      "majd kézzel cseréld be. / Target is a live ~/.aberp DB — restore to a " +
      "side path, then swap it in manually."
    );
  }
  return null;
}

/** Whether the restore wizard's Submit may be enabled: a selector, a
 * non-empty safe target, and the confirm checkbox all present. */
export function canSubmitRestore(input: {
  selector: string;
  to: string;
  confirm: boolean;
}): boolean {
  return (
    input.selector.trim() !== "" &&
    input.confirm === true &&
    restoreTargetWarning(input.to) === null
  );
}
