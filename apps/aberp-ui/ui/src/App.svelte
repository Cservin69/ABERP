<script lang="ts">
  // Root component — owns the health probe + the invoice-list mount.
  //
  // ADR-0017 puts the first dense-table screen at the centre; everything
  // around it is chrome. The header carries one signal token (the
  // backend liveness dot) and one text label (the ABERP wordmark). No
  // search, no settings, no nav — those land in subsequent PRs as
  // their underlying routes ship.

  import { onMount } from "svelte";
  import { health, type HealthResponse } from "./lib/api";
  import InvoiceList from "./routes/InvoiceList.svelte";

  let healthState: "pending" | "ok" | "error" = $state("pending");
  let healthInfo: HealthResponse | null = $state(null);
  let healthError: string | null = $state(null);

  // Initial probe + a slow refresh so the dot stays honest if the
  // backend dies. 10s matches the cold-start ceiling in
  // `backend::HANDSHAKE_TIMEOUT`; faster polling would be theatre on a
  // single-operator workstation (ADR-0017 §"ambient, never theatrical").
  onMount(() => {
    void probe();
    const id = setInterval(() => void probe(), 10_000);
    return () => clearInterval(id);
  });

  async function probe() {
    try {
      healthInfo = await health();
      healthState = "ok";
      healthError = null;
    } catch (err: unknown) {
      healthState = "error";
      healthError = err instanceof Error ? err.message : String(err);
    }
  }
</script>

<div class="frame">
  <header class="topbar">
    <h1 class="wordmark">ABERP</h1>
    <div class="status" data-state={healthState} title={healthInfo ? `binary ${healthInfo.binary_hash.slice(0, 12)}… · NAV XSD ${healthInfo.nav_xsd_version}` : (healthError ?? "")}>
      <span class="dot" aria-hidden="true"></span>
      <span class="label">
        {#if healthState === "ok" && healthInfo}
          backend ok · NAV XSD {healthInfo.nav_xsd_version}
        {:else if healthState === "pending"}
          probing backend…
        {:else}
          backend unreachable
        {/if}
      </span>
    </div>
  </header>

  <main>
    {#if healthState === "error"}
      <section class="banner" role="alert">
        <strong>Backend is not responding.</strong>
        <p class="banner-detail">{healthError}</p>
        <p class="banner-hint">
          Run <code>aberp serve --tenant default</code> in a terminal at least
          once so the session token is minted in the OS keychain, then relaunch
          this shell.
        </p>
      </section>
    {/if}

    <InvoiceList />
  </main>
</div>

<style>
  .frame {
    display: flex;
    flex-direction: column;
    min-height: 100vh;
  }

  .topbar {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    padding: var(--space-3) var(--space-5);
    background: var(--color-surface-raised);
    border-bottom: 1px solid var(--color-surface-divider);
  }

  .wordmark {
    margin: 0;
    font-family: var(--type-family-mono);
    font-size: var(--type-size-lg);
    font-weight: 600;
    letter-spacing: 0.06em;
    color: var(--color-text-strong);
  }

  .status {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--type-size-sm);
    color: var(--color-text-secondary);
  }

  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--color-signal-muted);
  }

  .status[data-state="ok"] .dot {
    background: var(--color-signal-positive);
    animation: aberp-fade-in var(--motion-fade-in) both;
  }

  .status[data-state="error"] .dot {
    background: var(--color-signal-negative);
  }

  main {
    flex: 1;
    padding: var(--space-5);
    overflow: auto;
  }

  .banner {
    margin-bottom: var(--space-5);
    padding: var(--space-3) var(--space-4);
    border-left: 3px solid var(--color-signal-negative);
    background: var(--color-surface-raised);
    color: var(--color-text-primary);
    font-size: var(--type-size-sm);
  }

  .banner-detail {
    margin: var(--space-2) 0 0 0;
    font-family: var(--type-family-mono);
    font-size: var(--type-size-xs);
    color: var(--color-text-secondary);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .banner-hint {
    margin: var(--space-2) 0 0 0;
    color: var(--color-text-muted);
  }

  code {
    font-family: var(--type-family-mono);
    color: var(--color-text-strong);
  }
</style>
