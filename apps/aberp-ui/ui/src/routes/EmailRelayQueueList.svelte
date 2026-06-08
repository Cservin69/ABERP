<script lang="ts">
  // S281 / PR-266 — storefront email-relay queue inspector (ADR-0007).
  //
  // Read-only operator surface. The drain daemon is the only writer;
  // this page shows queue state + per-row attempt counter + last error
  // so the operator can triage failed mail.
  //
  // No edit/retry affordance in v1: the daemon does retry + termination
  // on its own (exponential backoff, 5 attempts then `Failed`). A v2
  // operator-retry button would mirror the Pricing Jobs pattern (S279)
  // — out of scope here per the brief's §E "conservative default: skip
  // in v1".
  //
  // Dark-theme tokens per [[spa-dark-theme-default]].

  import { onMount } from "svelte";

  import {
    listEmailRelayQueue,
    type EmailRelayQueueRow,
  } from "../lib/api";

  type LoadState = "idle" | "loading" | "ready" | "error";
  type StateFilter = "all" | "queued" | "sending" | "sent" | "failed";

  let loadState = $state<LoadState>("idle");
  let errorMessage = $state<string | null>(null);
  let rows = $state<EmailRelayQueueRow[]>([]);
  let stateFilter = $state<StateFilter>("all");

  onMount(() => {
    void refresh();
  });

  async function refresh(): Promise<void> {
    loadState = "loading";
    errorMessage = null;
    try {
      const opts = stateFilter === "all" ? {} : { state: stateFilter };
      const res = await listEmailRelayQueue(opts);
      rows = res.rows;
      loadState = "ready";
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
      loadState = "error";
    }
  }

  function setFilter(f: StateFilter): void {
    if (stateFilter === f) return;
    stateFilter = f;
    void refresh();
  }

  function stateChipClass(state: string): string {
    switch (state) {
      case "queued":
        return "erq-chip erq-chip--queued";
      case "sending":
        return "erq-chip erq-chip--sending";
      case "sent":
        return "erq-chip erq-chip--sent";
      case "failed":
        return "erq-chip erq-chip--failed";
      default:
        return "erq-chip";
    }
  }

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
    return `${(n / (1024 * 1024)).toFixed(2)} MiB`;
  }

  function shortHash(hash: string): string {
    if (hash.length <= 16) return hash;
    return `${hash.slice(0, 8)}…${hash.slice(-4)}`;
  }
</script>

<section class="erq-page">
  <header class="erq-page__head">
    <h1 class="erq-page__title">
      <span>Kimenő levélsor / Outbound email queue</span>
      <span class="erq-page__hint">
        Storefront → ABERP továbbított levelek (ADR-0007). A daemon
        ürítője küld; ez a nézet csak forenzika.
      </span>
      <span class="erq-page__hint">
        Storefront-relayed mail (ADR-0007). The drain daemon is the
        sender; this view is read-only operator forensics.
      </span>
    </h1>
    <div class="erq-page__actions">
      <button
        type="button"
        class="erq-page__refresh"
        onclick={() => void refresh()}
        disabled={loadState === "loading"}
      >
        {loadState === "loading" ? "Frissítés…" : "Frissítés / Refresh"}
      </button>
    </div>
  </header>

  <div class="erq-filter" role="tablist" aria-label="State filter">
    {#each ["all", "queued", "sending", "sent", "failed"] as f}
      <button
        type="button"
        role="tab"
        class="erq-filter__btn"
        class:erq-filter__btn--active={stateFilter === f}
        aria-selected={stateFilter === f}
        onclick={() => setFilter(f as StateFilter)}
      >
        {f}
      </button>
    {/each}
  </div>

  <aside class="erq-page__notice">
    <strong>v1:</strong> a daemon 2s-enként ürít, soronként 5 próbálkozás
    után <code>Failed</code> állapotra vált — operátor retry-kapcsoló
    még nincs (S282+).
    <br />
    <strong>v1:</strong> the daemon drains every 2s; 5 retries then
    <code>Failed</code>. No operator-retry button yet (S282+).
  </aside>

  {#if loadState === "error"}
    <div class="erq-page__error" role="alert">
      <strong>Lista betöltése sikertelen. / Load failed.</strong>
      <p>{errorMessage}</p>
    </div>
  {:else if loadState === "ready" && rows.length === 0}
    <div class="erq-page__empty" role="status">
      Nincs sor a kiválasztott szűrőhöz.
      <br />
      No rows match the selected filter.
    </div>
  {:else if loadState === "ready"}
    <div class="erq-page__table-wrap">
      <table class="erq-table">
        <thead>
          <tr>
            <th class="erq-table__th erq-table__th--text">Created</th>
            <th class="erq-table__th erq-table__th--text">Submitter</th>
            <th class="erq-table__th erq-table__th--text">Subject</th>
            <th class="erq-table__th erq-table__th--text">State</th>
            <th class="erq-table__th erq-table__th--num">To</th>
            <th class="erq-table__th erq-table__th--num">Attempts</th>
            <th class="erq-table__th erq-table__th--num">Size</th>
            <th
              class="erq-table__th erq-table__th--text"
              title="SHA-256 of the canonicalised recipient set"
            >
              Recipient hash
            </th>
            <th class="erq-table__th erq-table__th--text">Last error</th>
          </tr>
        </thead>
        <tbody>
          {#each rows as r (r.id)}
            <tr
              class={r.state === "failed"
                ? "erq-table__row erq-table__row--failed"
                : "erq-table__row"}
            >
              <td class="erq-table__td erq-table__td--text erq-table__td--muted">
                {r.created_at}
              </td>
              <td class="erq-table__td erq-table__td--text">{r.submitter}</td>
              <td class="erq-table__td erq-table__td--text">
                {r.subject}
              </td>
              <td class="erq-table__td erq-table__td--text">
                <span class={stateChipClass(r.state)}>{r.state}</span>
              </td>
              <td class="erq-table__td erq-table__td--num">{r.to_count}</td>
              <td class="erq-table__td erq-table__td--num">{r.attempt_n}</td>
              <td class="erq-table__td erq-table__td--num">
                {fmtBytes(r.byte_size)}
              </td>
              <td
                class="erq-table__td erq-table__td--text erq-table__td--mono"
                title={r.recipient_hash}
              >
                {shortHash(r.recipient_hash)}
              </td>
              <td
                class="erq-table__td erq-table__td--text erq-table__td--muted"
                title={r.last_error ?? ""}
              >
                {r.last_error ?? "—"}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</section>

<style>
  .erq-page {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4) 0;
  }
  .erq-page__head {
    display: flex;
    justify-content: space-between;
    align-items: flex-end;
    gap: var(--space-3);
    flex-wrap: wrap;
  }
  .erq-page__title {
    font-size: var(--type-size-lg);
    font-weight: 600;
    margin: 0;
    color: var(--color-text-strong);
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .erq-page__hint {
    font-size: var(--type-size-sm);
    font-weight: 400;
    color: var(--color-text-muted);
  }
  .erq-page__actions {
    display: flex;
    gap: var(--space-2);
  }
  .erq-page__refresh {
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-surface-divider);
    background: var(--color-surface-raised);
    color: var(--color-text-secondary);
    border-radius: 3px;
    cursor: pointer;
    font-family: var(--type-family-body);
    font-size: var(--type-size-sm);
  }
  .erq-filter {
    display: flex;
    gap: var(--space-1);
  }
  .erq-filter__btn {
    padding: var(--space-1) var(--space-3);
    border: 1px solid var(--color-surface-divider);
    background: var(--color-surface-base);
    color: var(--color-text-muted);
    border-radius: 3px;
    cursor: pointer;
    font-family: var(--type-family-body);
    font-size: var(--type-size-sm);
    text-transform: capitalize;
  }
  .erq-filter__btn--active {
    background: var(--color-surface-raised);
    color: var(--color-text-strong);
    border-color: var(--color-text-secondary);
  }
  .erq-page__notice {
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-surface-divider);
    background: var(--color-surface-raised);
    color: var(--color-text-muted);
    border-radius: 3px;
    font-size: var(--type-size-sm);
    line-height: 1.5;
  }
  .erq-page__notice strong {
    color: var(--color-text-secondary);
  }
  .erq-page__error {
    padding: var(--space-3);
    background: var(--color-surface-sunken);
    border: 1px solid var(--color-signal-negative);
    border-radius: 3px;
    color: var(--color-text-primary);
  }
  .erq-page__error strong {
    color: var(--color-signal-negative);
  }
  .erq-page__empty {
    padding: var(--space-3);
    background: var(--color-surface-raised);
    border: 1px solid var(--color-surface-divider);
    border-radius: 3px;
    color: var(--color-text-muted);
    text-align: center;
    line-height: 1.6;
  }
  .erq-page__table-wrap {
    overflow-x: auto;
    border: 1px solid var(--color-surface-divider);
    border-radius: 3px;
    background: var(--color-surface-base);
  }
  .erq-table {
    width: 100%;
    border-collapse: collapse;
    font-variant-numeric: tabular-nums;
  }
  .erq-table__th {
    text-align: left;
    padding: var(--space-2) var(--space-3);
    background: var(--color-surface-raised);
    color: var(--color-text-secondary);
    font-weight: 600;
    font-size: var(--type-size-sm);
    border-bottom: 1px solid var(--color-surface-divider);
  }
  .erq-table__th--num {
    text-align: right;
  }
  .erq-table__row {
    border-bottom: 1px solid var(--color-surface-divider);
  }
  .erq-table__row:last-child {
    border-bottom: none;
  }
  .erq-table__row--failed {
    background: color-mix(in srgb, var(--color-signal-negative) 6%, transparent);
  }
  .erq-table__td {
    padding: var(--space-2) var(--space-3);
    color: var(--color-text-primary);
    font-size: var(--type-size-sm);
  }
  .erq-table__td--num {
    text-align: right;
  }
  .erq-table__td--text {
    text-align: left;
  }
  .erq-table__td--muted {
    color: var(--color-text-muted);
  }
  .erq-table__td--mono {
    font-family: var(--type-family-mono, monospace);
    font-size: 0.85em;
  }
  .erq-chip {
    display: inline-block;
    padding: 0 var(--space-2);
    border-radius: 2px;
    font-size: var(--type-size-xs);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    font-weight: 600;
    border: 1px solid var(--color-surface-divider);
  }
  .erq-chip--queued {
    background: color-mix(in srgb, var(--color-signal-info, #3a8) 14%, transparent);
    color: var(--color-text-strong);
  }
  .erq-chip--sending {
    background: color-mix(in srgb, var(--color-signal-warning, #d80) 14%, transparent);
    color: var(--color-text-strong);
  }
  .erq-chip--sent {
    background: color-mix(in srgb, var(--color-signal-positive, #6a4) 14%, transparent);
    color: var(--color-text-strong);
  }
  .erq-chip--failed {
    background: color-mix(in srgb, var(--color-signal-negative) 18%, transparent);
    color: var(--color-signal-negative);
  }
</style>
