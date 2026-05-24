// PR-29 / session-33 — bytes-as-UTF-8 substitution for the audit-
// payload drill-down in `InvoiceDetail.svelte`.
//
// Why this module exists. The audit-ledger's typed payloads
// (`apps/aberp/src/audit_payloads.rs`) carry every NAV request /
// response envelope as `Vec<u8>` (`request_xml`, `response_xml`,
// `ack_xml`, plus the `Option<Vec<u8>>` failure / annulment
// variants). `serde_json::to_vec` serialises `Vec<u8>` as a JSON
// array of integers — NOT base64, despite an older in-file comment
// in `audit_payloads.rs` claiming otherwise (the session-32 handoff
// confirms the live behaviour is long int arrays). The PR-27 drill-
// down renderer (`formatPayload`) prints those arrays verbatim,
// which is unreadable for the most common case (Submitted /
// Recovered entries with multi-kilobyte XML bodies).
//
// Why a JSON.stringify replacer over a recursive walk. `JSON.stringify`
// already walks the object tree and invokes the replacer at every
// key / value pair (MDN: "If you return any other object during the
// stringify process, the object is recursively stringified, calling
// the replacer function on each property"). Returning a decoded
// string from the replacer substitutes the array in the output;
// returning the array unchanged lets stringify recurse into elements
// (no-op for our case — elements are integers). A hand-rolled walk
// would duplicate that traversal logic for no gain. Per CLAUDE.md
// rule 2 (simplicity first).
//
// Why no module-load self-test. The labels.ts module-load asserts
// pin DATA INVARIANTS (`LIFECYCLE_ORDER` length / dedup / set-
// equality with `LABELS`) — drift between the SPA's
// `InvoiceState` union and the runtime sort order is silent
// otherwise. The reviver here is BEHAVIOUR, and a regression
// surfaces at first glance (the operator sees int arrays instead
// of decoded XML). The CLAUDE.md rule 12 fail-loud bar is met by
// the visibility of the failure mode, not a module-load assert.
//
// Heuristic. An array is treated as bytes iff:
//   - non-empty (an empty `Vec<u8>` would also serialise to `[]`,
//     but decoding it to `""` would erase the operator-visible
//     hint that the field carried zero bytes);
//   - every element is an integer in [0, 255];
//   - the bytes decode as valid UTF-8 under `fatal: true`.
// Non-UTF-8 byte arrays (rare; would indicate a non-XML body in
// a `Vec<u8>` field — NAV always emits UTF-8 XML per v3.0 spec)
// fall back to the int-array form so no information is lost.
//
// Future drift. If a future audit payload introduces a meaningful
// `Vec<integer>` field (today none exist — every numeric field is
// scalar `u32` / `u64` / `usize`), the heuristic would over-decode.
// The introducer of that field is responsible for either a path-
// based opt-out in this module OR adjusting the heuristic. The
// trap is named here so a future reader can find it.

const utf8Decoder = new TextDecoder("utf-8", { fatal: true });

/**
 * `JSON.stringify` replacer. Substitutes any "non-empty array of
 * integers in [0, 255] that decodes as valid UTF-8" with the
 * decoded string; every other value passes through unchanged.
 *
 * The `_key` parameter is unused — the heuristic is value-shape-
 * based, not field-name-based, so the audit-payload schema can
 * grow new `Vec<u8>` fields without an opt-in list per kind.
 */
export function bytesAsUtf8Replacer(_key: string, value: unknown): unknown {
  if (!Array.isArray(value) || value.length === 0) return value;
  for (const el of value) {
    if (typeof el !== "number" || !Number.isInteger(el) || el < 0 || el > 255) {
      return value;
    }
  }
  try {
    return utf8Decoder.decode(new Uint8Array(value as number[]));
  } catch {
    return value;
  }
}
