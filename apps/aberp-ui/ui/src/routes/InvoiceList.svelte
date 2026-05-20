<script lang="ts">
  // First dense-table screen — pins the table CSS pattern per
  // ADR-0017. Subsequent screens (invoice detail, audit drill-down,
  // billing summary) inherit this style without re-inventing tokens.
  //
  // Columns:
  //   Invoice id          monospace, primary text
  //   Series #            monospace, tabular numbers, right-aligned
  //   Fiscal year         monospace, tabular numbers, right-aligned
  //   State               signal-coloured pill (categorical signal)
  //   Total (gross, HUF)  monospace, tabular numbers, right-aligned
  //
  // Per ADR-0017 §3 every numeric column is monospace + tabular +
  // right-aligned. Per ADR-0017 §1-2 chrome is quiet, colour means
  // state. Per §4 a freshly-fetched table fades in over 200ms — no
  // spinners, no skeleton shimmers.

  import { onMount } from "svelte";
  import {
    listInvoices,
    type InvoiceListItem,
    type InvoiceState,
  } from "../lib/api";

  let rows: InvoiceListItem[] = $state([]);
  let loadState: "idle" | "loading" | "loaded" | "error" = $state("idle");
  let errorMessage: string | null = $state(null);

  onMount(() => {
    void refresh();
  });

  async function refresh() {
    loadState = "loading";
    errorMessage = null;
    try {
      rows = await listInvoices();
      loadState = "loaded";
    } catch (err: unknown) {
      loadState = "error";
      errorMessage = err instanceof Error ? err.message : String(err);
    }
  }

  // HUF amount formatter — tabular, no fractional digits because the
  // forint has no sub-unit. Locale `hu-HU` gives space-separated
  // thousands (1 234 567 Ft) which is the Hungarian convention.
  const hufFormatter = new Intl.NumberFormat("hu-HU", {
    style: "currency",
    currency: "HUF",
    minimumFractionDigits: 0,
    maximumFractionDigits: 0,
  });

  function formatHuf(value: number | null): string {
    if (value === null) return "—";
    return hufFormatter.format(value);
  }

  function stateSignalClass(s: InvoiceState | string): string {
    switch (s) {
      case "Finalized":
        return "signal-positive";
      case "Rejected":
        return "signal-negative";
      case "Submitted":
      case "Abandoned":
        return "signal-warning";
      case "Ready":
        return "signal-muted";
      case "Unknown":
      default:
        return "signal-muted";
    }
  }
</script>

<section class="screen">
  <div class="screen-head">
    <h2>Invoices</h2>
    <div class="actions">
      <button
        type="button"
        class="quiet-button"
        onclick={() => void refresh()}
        disabled={loadState === "loading"}
      >
        Refresh
      </button>
    </div>
  </div>

  {#if loadState === "error"}
    <p class="error" role="alert">{errorMessage}</p>
  {/if}

  <table class="dense">
    <thead>
      <tr>
        <th scope="col" class="col-id">Invoice id</th>
        <th scope="col" class="col-num">Series #</th>
        <th scope="col" class="col-num">Fiscal year</th>
        <th scope="col" class="col-state">State</th>
        <th scope="col" class="col-num">Total (gross)</th>
      </tr>
    </thead>
    <tbody>
      {#if loadState === "loaded" && rows.length === 0}
        <tr class="empty">
          <td colspan="5">
            No invoices on this tenant yet. Issue one with
            <code>aberp issue-invoice</code> and reload.
          </td>
        </tr>
      {/if}
      {#each rows as row (row.invoice_id)}
        <tr>
          <td class="col-id mono">{row.invoice_id}</td>
          <td class="col-num mono">{row.sequence_number}</td>
          <td class="col-num mono">{row.fiscal_year}</td>
          <td class="col-state">
            <span class="state-pill {stateSignalClass(row.state)}">{row.state}</span>
          </td>
          <td class="col-num mono">{formatHuf(row.total_gross)}</td>
        </tr>
      {/each}
    </tbody>
  </table>
</section>

<style>
  .screen {
    animation: aberp-fade-in var(--motion-fade-in) both;
  }

  .screen-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin-bottom: var(--space-3);
  }

  h2 {
    margin: 0;
    font-size: var(--type-size-xl);
    font-weight: 500;
    color: var(--color-text-strong);
    letter-spacing: 0.02em;
  }

  .actions {
    display: flex;
    gap: var(--space-2);
  }

  .quiet-button {
    background: var(--color-surface-raised);
    color: var(--color-text-secondary);
    border: 1px solid var(--color-surface-divider);
    padding: var(--space-1) var(--space-3);
    font-family: var(--type-family-body);
    font-size: var(--type-size-sm);
    cursor: pointer;
    transition: color var(--motion-fade-in);
  }

  .quiet-button:hover:not(:disabled) {
    color: var(--color-text-strong);
  }

  .quiet-button:disabled {
    opacity: 0.5;
    cursor: progress;
  }

  .error {
    color: var(--color-signal-negative);
    font-family: var(--type-family-mono);
    font-size: var(--type-size-sm);
    margin: var(--space-2) 0;
    white-space: pre-wrap;
    word-break: break-word;
  }

  /* Dense table — the load-bearing CSS of ADR-0017. */
  table.dense {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--type-size-md);
    background: var(--color-surface-sunken);
  }

  table.dense thead th {
    text-align: left;
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--color-surface-divider);
    color: var(--color-text-secondary);
    font-weight: 500;
    font-size: var(--type-size-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  table.dense tbody td {
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--color-surface-divider);
    vertical-align: top;
  }

  table.dense tbody tr:hover {
    background: var(--color-surface-raised);
  }

  /* Tabular figures for every numeric column — ADR-0017 §3. */
  td.mono,
  .mono {
    font-family: var(--type-family-mono);
    font-variant-numeric: tabular-nums;
  }

  .col-num {
    text-align: right;
  }

  .col-id {
    width: 30ch;
  }

  .col-state {
    width: 14ch;
  }

  .state-pill {
    display: inline-block;
    padding: 0 var(--space-2);
    font-family: var(--type-family-mono);
    font-size: var(--type-size-xs);
    line-height: 1.6;
    letter-spacing: 0.04em;
    border: 1px solid var(--color-surface-divider);
    border-radius: 2px;
    background: var(--color-surface-base);
    color: var(--color-text-secondary);
  }

  /* Categorical signal colours — only state cells carry colour. */
  .state-pill.signal-positive {
    color: var(--color-signal-positive);
    border-color: var(--color-signal-positive);
  }
  .state-pill.signal-negative {
    color: var(--color-signal-negative);
    border-color: var(--color-signal-negative);
  }
  .state-pill.signal-warning {
    color: var(--color-signal-warning);
    border-color: var(--color-signal-warning);
  }
  .state-pill.signal-muted {
    color: var(--color-text-muted);
    border-color: var(--color-surface-divider);
  }

  .empty td {
    color: var(--color-text-muted);
    font-style: italic;
    text-align: center;
    padding: var(--space-5) var(--space-3);
  }

  code {
    font-family: var(--type-family-mono);
    color: var(--color-text-strong);
  }
</style>
