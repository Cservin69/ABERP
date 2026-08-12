<script lang="ts">
  // ADR-0110 D7 — the operator's durability-loss alarm.
  //
  // Presentational plus one action. The parent (`App.svelte`) owns the /health
  // poll and the re-probe; the decision lives in `lib/durability-alert.ts`. The
  // STATE lives on the backend, which is what makes this survive a reload —
  // and, since B2, a full restart.
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
  // # Acknowledge is NOT dismiss (B2)
  //
  // The button does not hide anything locally. It POSTs, the backend writes a
  // permanent hash-chained `DbDurabilityAlertAcknowledged` row and only then
  // clears its sticky flag, and the banner goes down on the next /health probe
  // because the BACKEND stopped reporting it. Nothing here can silence the
  // alarm on its own, and the acknowledgement is durable — it is what stops the
  // next boot re-raising the banner from the audit mirror.
  //
  // Before B2 there was no button, on the reasoning that "there is nothing the
  // operator could click here that would be true". The adversarial found the
  // hole in that: the restart the banner ASKS FOR was clearing the alert
  // silently, so the alarm was already dismissible — just untraceably, by the
  // one action it recommends.

  import type { DurabilityAlert } from "./api";
  import {
    DURABILITY_BANNER_HEADLINE,
    durabilityBannerDetail,
  } from "./durability-alert";

  interface Props {
    alert: DurabilityAlert;
    /** POSTs the acknowledgement and re-probes /health. Owned by the parent. */
    onAcknowledge: () => Promise<void>;
  }
  let { alert, onAcknowledge }: Props = $props();

  let acknowledging = $state(false);
  let ackError: string | null = $state(null);

  async function acknowledge() {
    if (acknowledging) return;
    acknowledging = true;
    ackError = null;
    try {
      await onAcknowledge();
    } catch (err: unknown) {
      // A failed acknowledgement leaves the banner UP (the backend audits
      // before it clears), which is the safe direction. Say why, in place —
      // the operator must not be left thinking they cleared it.
      ackError = err instanceof Error ? err.message : String(err);
    } finally {
      acknowledging = false;
    }
  }
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
  {#if ackError}
    <span class="durability-alarm__detail" data-testid="durability-alert-ack-error">
      Acknowledgement FAILED — the alert stands: {ackError}
    </span>
  {/if}
  <button
    type="button"
    class="durability-alarm__ack"
    onclick={acknowledge}
    disabled={acknowledging}
    data-testid="durability-alert-acknowledge"
  >
    {acknowledging ? "Acknowledging…" : "Acknowledge (records who and when)"}
  </button>
</div>

<style>
  .durability-alarm {
    /* Full width, at the very top of the frame, above everything.
       `position: sticky` rather than `static` or `fixed`, deliberately:
         * `static` (what this was) makes `z-index` INERT — an unpositioned
           element does not participate in stacking, so the
           `FirstProdLaunchModal`'s `position: fixed; inset: 0; z-index: 1000`
           covered the alarm completely;
         * `fixed` would float it OVER the app, where it reads as a toast and
           can be mistaken for dismissible chrome.
       Sticky keeps it in flow — it PUSHES the app down — while creating a
       stacking context that honours the z-index below, and keeps the alarm on
       screen when the operator scrolls. */
    position: sticky;
    top: 0;
    /* Above `FirstProdLaunchModal` (1000). An alarm the operator cannot see
       because a modal is up is not an alarm, and the first-prod-launch modal is
       precisely the moment someone is about to start invoicing. */
    z-index: 1100;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-1);
    padding: var(--space-4) var(--space-5);
    /* Hard-coded on purpose — see the header comment. */
    background: #b3120f;
    color: #ffffff;
    border-bottom: 3px solid #ff6b62;
    text-align: center;
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

  .durability-alarm__ack {
    margin-top: var(--space-2);
    padding: var(--space-2) var(--space-4);
    font-family: var(--type-family-body);
    font-size: var(--type-size-sm);
    /* Understated against the alarm field on purpose: acknowledging is a
       deliberate, recorded act, not the obvious way out of the banner. */
    color: #ffffff;
    background: transparent;
    border: 1px solid #ffb3ae;
    border-radius: 3px;
    cursor: pointer;
  }

  .durability-alarm__ack:hover:not(:disabled) {
    background: #8e0d0b;
  }

  .durability-alarm__ack:disabled {
    cursor: progress;
    opacity: 0.7;
  }
</style>
