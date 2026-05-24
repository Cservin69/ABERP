# ADR-0013 — Robotics handoff (label print + place, pick + move)

- **Status:** Proposed (stub — scope only, no decisions yet)
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Depends on:** ADR-0006, ADR-0007, ADR-0011, ADR-0012

## Scope

A robotics module dispatches physical-world tasks: print a vignette and place
it on a part, pick a part from a location, move a tote to a packing station,
load a CNC fixture. The system records intent, dispatch, execution, and
outcome. Physical-world side effects must be reconcilable with the inventory
state.

Decisions to be made:

- **Task lifecycle**: created → queued → dispatched → executing → done | failed
  | aborted. Every transition is a ledger entry.
- **Command protocol** to robots — signed messages, ack required, no
  fire-and-forget for physical actions.
- **Failure semantics**: a task that ends `failed` does not mutate inventory;
  a task that ends `done` after a partial physical action is the worst case —
  we need explicit reconciliation flows.
- **Operator override**: any robotic task can be cancelled by an authorized
  operator; the cancellation is audit-logged.
- **Safety interlocks**: ABERP does not act as a safety system. Physical
  safety is the robot's responsibility; ABERP refuses to dispatch into an
  unsafe state but does not enforce safety itself.

## Open questions

- Which robotics platforms first — informs adapter priority.
- Whether ABERP holds the task queue or talks to a fleet manager.
- Simulation / dry-run mode for the inevitable "did I just tell the robot to
  scrap the wrong part" moment.

## Not in scope

- Robot programming, calibration, low-level control.
- Industrial safety certifications.
