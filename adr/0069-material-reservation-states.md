# ADR-0069 — Material reservation: append-only `stock_reservations` ledger, available-to-promise = stock − open reservations, four-state lifecycle. Files ADR-0061 Open Question #2.

- **Status:** Proposed
- **Date:** 2026-06-06
- **Deciders:** Ervin (via S265 auto-quoting ground-zero brief)
- **Supersedes:** none — **files ADR-0061 Open Question #2** (the reservation model ADR-0061 deferred).
- **Related:** ADR-0061 (inventory ledger — this ADR extends it; OQ#2 is the explicit deferral this closes), ADR-0062 (work orders — Release becomes reservation-aware), ADR-0067 (DEAL saga — the reserve caller), ADR-0008 (audit ledger), the design doc [`docs/design/auto-quoting-ground-zero.md`](../docs/design/auto-quoting-ground-zero.md), and [[no-sql-specific]], [[trust-code-not-operator]], [[hulye-biztos]].

## Context

ADR-0061 shipped the inventory ledger and **explicitly deferred reservations** (its Open Question #2): *"Trigger: first sales-order or work-order-queue feature that needs to mark stock 'allocated, not yet consumed.'"* The DEAL saga (ADR-0067) is exactly that feature — a DEAL must earmark a deal's materials so a second DEAL between agreement and WO-Release cannot silently eat them.

ADR-0061's adversarial review already sketched the shape: *"reservations would split [Release] into Reserve (positive on a virtual 'reserved' balance) + Consume (negative on actual stock)."* This ADR makes that concrete.

The key insight: **a reservation is not a stock movement.** Reserved material is still physically on the shelf — `stock_qty` does not change. What changes is *promisability*. So a reservation needs its own ledger, parallel to `stock_movements`, and a derived quantity — **available-to-promise (ATP)** — that the DEAL saga checks instead of raw `stock_qty`.

## Decision

**A second append-only ledger `stock_reservations`, parallel to `stock_movements`. ATP = `stock_qty` − SUM(open reservations). A reservation moves through a four-state lifecycle: `on_hand → reserved → committed → consumed`.**

### 1. The ledger

```text
stock_reservations (
  reservation_id   ULID PRIMARY KEY,    -- prefix `rsv_`
  tenant_id        ULID NOT NULL,
  product_id       ULID NOT NULL,       -- prefix `prd_`
  qty              DECIMAL(18,6) NOT NULL, -- positive; the amount earmarked
  state            VARCHAR NOT NULL,    -- closed-vocab ReservationState
  ref_kind         VARCHAR NOT NULL,    -- closed-vocab; `quote` | `work_order`
  ref_id           VARCHAR NOT NULL,    -- the quote_id (at reserve) / wo_id (at commit)
  reserved_at      TIMESTAMP NOT NULL,
  closed_at        TIMESTAMP,           -- set when state reaches consumed/cancelled
  operator         VARCHAR NOT NULL,
  idempotency_key  VARCHAR NOT NULL,
  notes            VARCHAR
);
```

Append-only like `stock_movements` — no in-place qty edits. A state transition appends a new row referencing the original (or, equivalently, a closing row); the **open** set is reservations whose latest state is `reserved` or `committed`. (Implementation may carry state on a single mutable `state` column guarded by the app-layer transition rules, matching how `dispatches.state` works in ADR-0064 — the ledger discipline is "no qty UPDATE, no DELETE"; the `state` column flip is the documented exception, same as every other state-machine table.)

### 2. Available-to-promise (ATP) — the new gate

```
ATP(prd) = products.stock_qty(prd) − SUM(qty WHERE product_id = prd AND state IN (reserved, committed))
```

The DEAL saga checks **ATP**, not `stock_qty`. Computed at reservation write time (re-summing open reservations), so concurrent DEALs serialize correctly — the same concurrency-safe posture ADR-0061 §adversarial pins for `stock_qty` (re-sum at write, don't increment a cached delta). No ATP cache in v1 (artisan volume; the sum is microseconds). A virtual view surfaces ATP next to `stock_qty` on the products list.

### 3. The four-state lifecycle

| State | Meaning | `stock_qty` | counts against ATP |
|---|---|---|---|
| `on_hand` | not reserved — the default; no reservation row | unchanged | no (it IS the ATP) |
| `reserved` | DEAL earmarked it (ref = quote_id) | unchanged | **yes** |
| `committed` | WO Released against it (ref upgraded to wo_id); physical pull pending | unchanged | yes |
| `consumed` | `BomConsumption` stock_movement written; reservation closed | **decremented** | no (gone) |

`reserved → committed → consumed` is one reservation's life. `reserved → cancelled` and `committed → cancelled` release the earmark (DEAL declined/rolled back) without touching `stock_qty`. In the single-operator shop `committed` may collapse into `consumed` near-instantly (WO Release both locks and pulls); the state exists so a future two-step pick flow (commit at Release, consume at physical pull) has a home.

`ReservationState` is closed-vocab, snake_case, round-trip pinned (same posture as `MovementReason`).

### 4. WO-Release becomes reservation-aware (the ADR-0062 amendment)

Today ADR-0062 WO-Release emits `BomConsumption` blind. With reservations, Release:

1. finds the open reservation for this WO's BOM line (by `ref_id = wo_id`, set when the DEAL's quote converted to this WO);
2. transitions it `reserved`/`committed → consumed`;
3. emits the `BomConsumption` stock_movement (ADR-0061) — same transaction.

If **no** reservation exists (a WO created outside the DEAL path — manual WO), Release falls back to today's blind consume against `stock_qty` directly. So the amendment is additive: DEAL-originated WOs consume their reservation; manual WOs behave as before.

### 5. Audit

Two new `mes.*` kinds (the inventory family, consistent with ADR-0061 — inventory events stay together regardless of which module triggers them):

| EventKind | Storage string | Emitted |
|---|---|---|
| `StockReserved` | `mes.stock_reserved` | on `→ reserved` |
| `StockReservationConsumed` | `mes.stock_reservation_consumed` | on `→ consumed` at WO-Release |

A `→ cancelled` release is signalled by the absence of a consume plus the `deal.rolled_back` entry (mirroring ADR-0064's Cancelled-dispatch posture — no dedicated kind in v1; add `StockReservationCancelled` additively if release volume surfaces).

### 6. Scope — implementation is S274

This ADR is the spec; S274 builds it and wires the DEAL saga's reservation seam (left as a no-op stub in S273). Per design doc §15.

## Consequences

- **Deals can't cannibalize each other's materials.** A reservation decrements ATP, so a second DEAL between agreement and Release sees the earmark and routes to procurement (ADR-0068) or pauses — it cannot silently consume promised stock.
- **`stock_qty` stays the physical truth.** Reservations never touch it; only `consumed` (via `BomConsumption`) decrements physical stock. The ADR-0061 ledger invariant (`stock_qty = SUM(qty_delta)`) is untouched — reservations are a *separate* ledger.
- **ATP is a new operator-visible quantity.** The products list shows `stock_qty` (physical) and `ATP` (promisable) side by side; a product fully reserved shows ATP 0 with stock_qty > 0 — the operator sees "we have it but it's spoken for."
- **WO-Release is now reservation-aware but backward-compatible.** Manual WOs consume blind as before; DEAL-WOs consume their reservation.
- **A new recovery surface.** Orphaned reservations (DEAL paused then abandoned, crash mid-saga) decrement ATP forever if never closed. A `tools/list-stale-reservations` (open reservations older than N days with no WO progress) is the operator's cleanup lever — flagged as an Open Question, not built v1.

## Adversarial review

- *"Why a separate ledger and not a `reserved_qty` column on products like `stock_qty`?"* A single cached column loses the per-reservation provenance (which quote/WO holds the earmark) needed to release exactly the right amount on rollback. The ledger lets a specific reservation be found and closed; a column would force "release some qty" with no idempotent target. Same reasoning ADR-0061 uses for the movements ledger over a bare balance.
- *"ATP re-summing open reservations on every reservation write is O(open reservations per product)."* Bounded at artisan volume (single-digit open reservations per material). Microseconds, like ADR-0061's `stock_qty` re-sum. Cache only if volume makes it real.
- *"Orphaned reservations silently starve ATP."* Real — the held-but-abandoned DEAL case (ADR-0067 pause-seam declined-but-not-cleaned, or a crash). Mitigation: open reservations are operator-visible (ATP < stock_qty on the list), and the `tools/list-stale-reservations` lever recovers them. `deal.rolled_back` closes the common abandon path. A reservation TTL is a future option, not v1 (a TTL that auto-releases mid-deal is its own footgun).
- *"`committed` collapses into `consumed` instantly in v1 — why model it at all?"* Because WO-Release-then-physical-pull is genuinely two events the moment the shop has a picking step, and retrofitting a state into a shipped ledger is painful. Modeling it now (even if transitions are instant) costs one enum variant and saves a migration. This is the opposite of speculative abstraction — it is a state machine the domain already has, collapsed for v1 convenience.
- *"A manual WO consuming blind while a DEAL-WO consumes a reservation is two code paths in Release."* True, and the branch is explicit: "open reservation for this wo_id? consume it : consume blind." One conditional, not two implementations. The fallback keeps manual WOs working unchanged (surgical-change discipline).

## Alternatives considered

- **`reserved_qty` denormalized column on products** (no ledger). Rejected — loses per-reservation provenance; can't release the exact earmark on rollback idempotently.
- **Reserve as a negative `stock_movement` and un-reserve as positive.** Rejected — corrupts `stock_qty` (reserved material is physically present; a negative movement would say it's gone) and pollutes the physical-truth ledger with non-physical entries. Reservations are a distinct concern with a distinct ledger.
- **No reservations; DEAL consumes immediately at agreement.** Rejected — consuming before the WO releases means a declined/rolled-back DEAL has already eaten stock, and `stock_qty` no longer reflects physical reality between DEAL and Release. ATP-with-reservation keeps physical and promisable distinct.
- **Two states only (`reserved`/`consumed`).** Rejected — drops `committed`, the WO-Release-vs-physical-pull seam the domain has. Cheap to model now; a migration later.
- **DB CHECK enforcing ATP >= 0.** Rejected per no-sql-specific — the ATP gate is the app-layer reservation check in the DEAL saga, not a schema constraint.

## Open questions (each names its trigger)

1. **Stale-reservation cleanup tooling.** Trigger: first operator complaint about ATP starved by abandoned reservations. Likely `tools/list-stale-reservations` + a release route.
2. **Reservation TTL / auto-expiry.** Trigger: stale reservations become chronic AND an auto-release policy is judged safer than manual cleanup. Deferred — auto-release mid-deal is a footgun.
3. **`StockReservationCancelled` audit kind.** Trigger: release volume makes the absence-of-consume signal too weak for auditors.
4. **ATP cache.** Trigger: products-list render cost from re-summing becomes real (high SKU count). Mirrors ADR-0061's `stock_qty` cache decision.
5. **Multi-bin reservations.** Trigger: ADR-0061 OQ#1 (multi-cell `bin_location`) lands — reservations then need a bin dimension.

## Invariants pinned

1. **A reservation never changes `products.stock_qty`.** Only `consumed` (via the `BomConsumption` movement) decrements physical stock. Pinned by `reserving_does_not_change_stock_qty`.
2. **ATP = `stock_qty` − SUM(open reservations), re-summed at write time.** Pinned by `atp_excludes_open_reservations` and `concurrent_reserve_sees_prior_reservation`.
3. **`reserved → committed → consumed` and `{reserved,committed} → cancelled` are the only transitions.** Pinned by `reservation_state_machine_rejects_illegal_transitions`.
4. **WO-Release with an open reservation for the wo_id consumes THAT reservation; with none, consumes blind (manual-WO fallback).** Pinned by `wo_release_consumes_reservation_when_present` and `wo_release_blind_consume_when_no_reservation`.
5. **One `mes.stock_reserved` per reserve, one `mes.stock_reservation_consumed` per consume.** Pinned by `reserve_and_consume_each_emit_one_audit`.
6. **`stock_reservations` qty is never UPDATEd in place (append/close discipline; only the `state` column flips per the documented state-machine exception).** Pinned by code review + a grep test mirroring ADR-0061's.
