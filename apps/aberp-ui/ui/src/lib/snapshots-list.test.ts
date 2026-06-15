// S426 / ADR-0082 — unit tests for the Snapshots tab pure logic.

import { describe, expect, it } from "vitest";
import {
  canSubmitRestore,
  filterSnapshots,
  restoreTargetWarning,
  sortSnapshots,
  summarizeSnapshots,
  type SnapshotItem,
} from "./snapshots-list";

function snap(over: Partial<SnapshotItem> = {}): SnapshotItem {
  return {
    seq: 1,
    created_at: "2026-06-15T10:00:00Z",
    byte_size: 1024,
    size_human: "1.0 KiB",
    valid: true,
    invoice_count: 5,
    audit_count: 10,
    chain_len: 10,
    age_human: "2h",
    validation_error: null,
    dir: "/snaps/snap-1",
    ...over,
  };
}

describe("filterSnapshots", () => {
  const rows = [
    snap({ seq: 1, valid: true }),
    snap({ seq: 2, valid: false }),
    snap({ seq: 3, valid: true }),
  ];

  it("all passes everything", () => {
    expect(filterSnapshots(rows, "all")).toHaveLength(3);
  });
  it("valid keeps only valid", () => {
    const out = filterSnapshots(rows, "valid");
    expect(out.map((r) => r.seq)).toEqual([1, 3]);
  });
  it("invalid keeps only invalid", () => {
    const out = filterSnapshots(rows, "invalid");
    expect(out.map((r) => r.seq)).toEqual([2]);
  });
  it("does not mutate the input", () => {
    const before = rows.slice();
    filterSnapshots(rows, "valid");
    expect(rows).toEqual(before);
  });
});

describe("sortSnapshots", () => {
  const rows = [
    snap({ seq: 1, byte_size: 300, created_at: "2026-06-15T08:00:00Z" }),
    snap({ seq: 2, byte_size: 100, created_at: "2026-06-15T12:00:00Z" }),
    snap({ seq: 3, byte_size: 200, created_at: "2026-06-15T10:00:00Z" }),
  ];

  it("seq desc is newest first", () => {
    expect(sortSnapshots(rows, "seq", "desc").map((r) => r.seq)).toEqual([3, 2, 1]);
  });
  it("seq asc", () => {
    expect(sortSnapshots(rows, "seq", "asc").map((r) => r.seq)).toEqual([1, 2, 3]);
  });
  it("byte_size asc", () => {
    expect(sortSnapshots(rows, "byte_size", "asc").map((r) => r.byte_size)).toEqual([
      100, 200, 300,
    ]);
  });
  it("created_at desc", () => {
    expect(sortSnapshots(rows, "created_at", "desc").map((r) => r.seq)).toEqual([2, 3, 1]);
  });
  it("does not mutate the input", () => {
    const before = rows.map((r) => r.seq);
    sortSnapshots(rows, "seq", "asc");
    expect(rows.map((r) => r.seq)).toEqual(before);
  });
});

describe("summarizeSnapshots", () => {
  it("counts and finds the newest valid seq", () => {
    const rows = [
      snap({ seq: 1, valid: true }),
      snap({ seq: 2, valid: false }),
      snap({ seq: 3, valid: true }),
      snap({ seq: 4, valid: false }),
    ];
    const s = summarizeSnapshots(rows);
    expect(s.total).toBe(4);
    expect(s.valid).toBe(2);
    expect(s.invalid).toBe(2);
    // Newest VALID is seq 3 (4 is invalid).
    expect(s.newest_valid_seq).toBe(3);
  });
  it("null newest-valid when none valid", () => {
    const s = summarizeSnapshots([snap({ seq: 1, valid: false })]);
    expect(s.newest_valid_seq).toBeNull();
    expect(s.valid).toBe(0);
  });
  it("empty list", () => {
    const s = summarizeSnapshots([]);
    expect(s).toEqual({ total: 0, valid: 0, invalid: 0, newest_valid_seq: null });
  });
});

describe("restoreTargetWarning", () => {
  it("warns on empty", () => {
    expect(restoreTargetWarning("")).not.toBeNull();
    expect(restoreTargetWarning("   ")).not.toBeNull();
  });
  it("refuses a live ~/.aberp path (prod)", () => {
    expect(restoreTargetWarning("/Users/x/.aberp/prod/aberp.duckdb")).not.toBeNull();
  });
  it("refuses any tenant home, not just prod", () => {
    expect(restoreTargetWarning("/Users/x/.aberp/dev/aberp.duckdb")).not.toBeNull();
  });
  it("refuses windows-style .aberp path", () => {
    expect(restoreTargetWarning("C:\\Users\\x\\.aberp\\prod\\aberp.duckdb")).not.toBeNull();
  });
  it("allows a side path", () => {
    expect(restoreTargetWarning("/Users/x/recovery/aberp.duckdb")).toBeNull();
  });
});

describe("canSubmitRestore", () => {
  it("requires selector, confirm, and a safe target", () => {
    expect(
      canSubmitRestore({ selector: "42", to: "/tmp/r/aberp.duckdb", confirm: true }),
    ).toBe(true);
  });
  it("blocks without confirm", () => {
    expect(
      canSubmitRestore({ selector: "42", to: "/tmp/r/aberp.duckdb", confirm: false }),
    ).toBe(false);
  });
  it("blocks empty selector", () => {
    expect(
      canSubmitRestore({ selector: "  ", to: "/tmp/r/aberp.duckdb", confirm: true }),
    ).toBe(false);
  });
  it("blocks an unsafe target even with confirm", () => {
    expect(
      canSubmitRestore({ selector: "42", to: "/Users/x/.aberp/prod/aberp.duckdb", confirm: true }),
    ).toBe(false);
  });
});
