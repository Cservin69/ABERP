<script lang="ts">
  // ADR-0110 D7 — the operator's durability-loss alarm.
  //
  // Presentational only. The parent (`App.svelte`) owns the /health poll; the
  // decision lives in `lib/durability-alert.ts`. The STATE lives on the
  // backend, which is what makes this survive a reload and stay up until it is
  // explicitly cleared.
  //
  // # Surfaced conflict, resolved deliberately (CLAUDE.md rule 7)
  //
  // ADR-0017 says the design language is ambient, not theatrical, and the whole
  // SPA obeys that — even `--color-signal-negative` is a muted #c66060 chosen
  // to sit quietly on a dark surface. This element deliberately BREAKS that
  // rule, on Ervin's explicit instruction (2026-08-12), and it is the only
  // element that does.
  //
  // The reason is the keep-serving decision it exists to support: when the
  // fence fires the backend does NOT hard-stop, so this banner is the entire
  // difference between "the operator stops and recovers" and "the operator
  // keeps invoicing into a database that is not persisting". A calm token here
  // would make it look like the other advisories, and the one thing it must not
  // be is dismissible-looking. So: literal high-contrast red, hard-coded rather
  // than tokenised, precisely so nobody later "harmonises" it back into the
  // palette without reading this comment.
  //
  // No dismiss control, by the same logic. There is nothing the operator can
  // click here that would be true.

  import type { DurabilityAlert } from "./api";
  import {
    DURABILITY_BANNER_HEADLINE,
    durabilityBannerDetail,
  } from "./durability-alert";

  interface Props {
    alert: DurabilityAlert;
  }
  let { alert }: Props = $props();
</script>

<!-- `role="alert"` + `aria-live="assertive"`: this must interrupt, not queue
     politely behind whatever else the screen reader is saying. -->
<div
  class="durability-alarm"
  role="alert"
  aria-live="assertive"
  data-testid="durability-alert-banner"
  data-breach={alert.breach}
>
  <span class="durability-alarm__headline">{DURABILITY_BANNER_HEADLINE}</span>
  <span class="durability-alarm__detail">{durabilityBannerDetail(alert)}</span>
</div>

<style>
  .durability-alarm {
    /* Full width, at the very top of the frame, above everything. Not
       `position: fixed` — it must PUSH the app down rather than float over it,
       so it cannot be scrolled behind or mistaken for a toast. */
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    padding: var(--space-4) var(--space-5);
    /* Hard-coded on purpose — see the header comment. */
    background: #b3120f;
    color: #ffffff;
    border-bottom: 3px solid #ff6b62;
    text-align: center;
    z-index: 100;
  }

  .durability-alarm__headline {
    font-family: var(--type-family-body);
    font-size: var(--type-size-xl);
    font-weight: 700;
    letter-spacing: 0.02em;
    color: #ffffff;
  }

  .durability-alarm__detail {
    font-family: var(--type-family-body);
    font-size: var(--type-size-md);
    color: #ffe3e1;
  }
</style>
