// PR-193 / session-193 — vitest pins for `csv-export.ts`. The SPA's
// vitest setup runs in node (no jsdom — same posture as
// `payload-reviver.test.ts` / `invoice-list-persistence.test.ts`),
// so the three pure helpers (`escapeCsvField`, `composeCsv`,
// `csvFilenameTimestamp`) are the load-bearing surfaces under test
// here. `downloadCsv` touches `document` + `URL.createObjectURL`
// which are not available in node; we stub both globals for a
// thin smoke pin that proves the anchor-click sequence fires.

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  composeCsv,
  csvFilenameTimestamp,
  downloadCsv,
  escapeCsvField,
} from "./csv-export";

describe("escapeCsvField", () => {
  it("returns empty string for null", () => {
    expect(escapeCsvField(null)).toBe("");
  });

  it("returns empty string for undefined", () => {
    expect(escapeCsvField(undefined)).toBe("");
  });

  it("renders numbers without quoting", () => {
    expect(escapeCsvField(127_000)).toBe("127000");
    expect(escapeCsvField(0)).toBe("0");
    expect(escapeCsvField(-42)).toBe("-42");
  });

  it("renders booleans without quoting", () => {
    expect(escapeCsvField(true)).toBe("true");
    expect(escapeCsvField(false)).toBe("false");
  });

  it("passes plain strings through unchanged", () => {
    expect(escapeCsvField("abc")).toBe("abc");
    expect(escapeCsvField("Hello world")).toBe("Hello world");
  });

  it("returns empty string for empty string (no quotes)", () => {
    // Matches Excel's Save-As-CSV — empty cell is bare, not `""`.
    expect(escapeCsvField("")).toBe("");
  });

  it("quotes a field that contains a comma", () => {
    expect(escapeCsvField("a,b")).toBe('"a,b"');
  });

  it("quotes a field that contains a CR or LF", () => {
    expect(escapeCsvField("a\nb")).toBe('"a\nb"');
    expect(escapeCsvField("a\r\nb")).toBe('"a\r\nb"');
  });

  it("escapes embedded double quotes by doubling and wraps in quotes", () => {
    // Internal `"` doubles to `""`; the whole field is wrapped per
    // RFC-4180 §2.5.
    expect(escapeCsvField('a"b')).toBe('"a""b"');
    expect(escapeCsvField('"hello"')).toBe('"""hello"""');
  });

  it("preserves Hungarian accented characters verbatim (UTF-8 BOM handles encoding)", () => {
    // Per ADR-0017 / printed-invoice template the operator surface
    // is Hungarian; the BOM at the start of the composed file is
    // the load-bearing Excel-compat signal — the field itself is
    // plain UTF-8.
    expect(escapeCsvField("Árvíztűrő tükörfúrógép")).toBe(
      "Árvíztűrő tükörfúrógép",
    );
  });

  it("coerces other types via String()", () => {
    expect(escapeCsvField(new Date("2026-05-31T00:00:00Z").toISOString())).toBe(
      "2026-05-31T00:00:00.000Z",
    );
  });
});

describe("composeCsv", () => {
  it("emits BOM + header CRLF + trailing CRLF on empty rows", () => {
    const csv = composeCsv(["A", "B"], []);
    // ﻿ + "A,B" + \r\n
    expect(csv).toBe("﻿A,B\r\n");
  });

  it("joins rows with CRLF and terminates the last row with CRLF", () => {
    const csv = composeCsv(
      ["A", "B"],
      [
        ["1", "2"],
        ["3", "4"],
      ],
    );
    expect(csv).toBe("﻿A,B\r\n1,2\r\n3,4\r\n");
  });

  it("quotes per-cell when the value contains separators", () => {
    const csv = composeCsv(["Name", "Note"], [["Acme, Inc.", 'has "quotes"']]);
    expect(csv).toBe('﻿Name,Note\r\n"Acme, Inc.","has ""quotes"""\r\n');
  });

  it("renders null and undefined cells as empty fields", () => {
    const csv = composeCsv(["A", "B", "C"], [[null, undefined, "x"]]);
    expect(csv).toBe("﻿A,B,C\r\n,,x\r\n");
  });

  it("preserves Hungarian accented characters in headers and cells", () => {
    const csv = composeCsv(
      ["Megjegyzés"],
      [["Árvíztűrő"], ["tükörfúrógép"]],
    );
    expect(csv).toBe(
      "﻿Megjegyzés\r\nÁrvíztűrő\r\ntükörfúrógép\r\n",
    );
  });

  it("starts every file with the UTF-8 BOM (\\uFEFF)", () => {
    const csv = composeCsv(["A"], [["1"]]);
    // First char must be U+FEFF — Excel reads UTF-8 only when this
    // is present.
    expect(csv.charCodeAt(0)).toBe(0xfeff);
  });

  it("handles mixed numeric and string cells", () => {
    const csv = composeCsv(
      ["Invoice", "Total"],
      [["2026-000001", 127_000]],
    );
    expect(csv).toBe("﻿Invoice,Total\r\n2026-000001,127000\r\n");
  });
});

describe("csvFilenameTimestamp", () => {
  it("pads to fixed width so chronological sort matches lexicographic sort", () => {
    const out = csvFilenameTimestamp(new Date(2026, 0, 5, 3, 7, 9));
    // YYYYMMDD-HHMMSS — `0` padding on every sub-field.
    expect(out).toBe("20260105-030709");
  });

  it("uses local clock fields (not UTC) — matches what the operator sees", () => {
    // Constructed via Date(YYYY, MM_zero_indexed, DD, HH, MM, SS) so
    // the helper's local-getters reflect the same inputs back.
    const out = csvFilenameTimestamp(new Date(2026, 11, 31, 23, 59, 59));
    expect(out).toBe("20261231-235959");
  });
});

describe("downloadCsv", () => {
  // The SPA's vitest setup has no jsdom layer (see header). We stub
  // the two browser globals the helper touches so it runs in node
  // and we can assert the sequence (anchor created, download attr
  // set, click fired, anchor removed, URL revoked).
  const originalDocument = (globalThis as { document?: Document }).document;
  const originalURL = globalThis.URL;

  afterEach(() => {
    if (originalDocument === undefined) {
      delete (globalThis as { document?: Document }).document;
    } else {
      (globalThis as { document?: Document }).document = originalDocument;
    }
    globalThis.URL = originalURL;
    vi.useRealTimers();
  });

  it("creates an anchor, sets href + download, clicks, and revokes the URL", () => {
    const click = vi.fn();
    const appendChild = vi.fn();
    const removeChild = vi.fn();
    const anchor: Record<string, unknown> = { click };
    (globalThis as { document?: unknown }).document = {
      createElement: vi.fn(() => anchor),
      body: { appendChild, removeChild },
    };
    const createObjectURL = vi.fn(() => "blob:fake-url");
    const revokeObjectURL = vi.fn();
    globalThis.URL = {
      ...originalURL,
      createObjectURL,
      revokeObjectURL,
    } as unknown as typeof URL;

    vi.useFakeTimers();
    downloadCsv("aberp-test.csv", "﻿A,B\r\n1,2\r\n");

    expect(createObjectURL).toHaveBeenCalledTimes(1);
    expect(anchor.href).toBe("blob:fake-url");
    expect(anchor.download).toBe("aberp-test.csv");
    expect(appendChild).toHaveBeenCalledWith(anchor);
    expect(click).toHaveBeenCalledTimes(1);
    expect(removeChild).toHaveBeenCalledWith(anchor);

    // Revoke is deferred 1000ms so the click has time to dispatch
    // before the URL is freed.
    expect(revokeObjectURL).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1000);
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:fake-url");
  });
});
