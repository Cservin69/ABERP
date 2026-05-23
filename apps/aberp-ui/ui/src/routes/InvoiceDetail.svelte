<script lang="ts">
  // PR-25 / session-29 — Invoice-detail modal.
  //
  // Renders one invoice's metadata plus its full audit-ledger trail in
  // a native <dialog>. Mounted once at the App level; opens / closes
  // by `invoiceId` prop toggling between a string and `null`. ADR-0021
  // §Part B's wire surface is reused via `getInvoice` — no new Tauri
  // command. Per ADR-0036 §7 the state chip reuses `labels.ts` so the
  // detail header and the list row carry identical affordances; per
  // ADR-0017 the audit-entries table uses the same dense pattern as
  // InvoiceList (monospace, tabular numbers, hairline dividers, no
  // chrome). No SvelteKit routing dependency added — modal posture
  // matches CLAUDE.md rule 3.
  //
  // Why a native <dialog>: the browser handles focus trap, ESC
  // dismiss, inert-on-backdrop, ARIA modal semantics, and stacking
  // context. A custom modal component would re-implement five things
  // that already exist. Per CLAUDE.md rule 2 — simplicity first.
  //
  // PR-26 / session-30 — chain-link clickable navigation. The Rust
  // side now emits `chain_base_invoice_id: Option<String>` on the
  // `AuditEntryView` shape (typed payload probe over
  // `InvoiceStornoIssued` / `InvoiceModificationIssued` entries).
  // The kind cell for a chain-link row renders `<kind> → <base_id>`
  // where the base id is a button that calls `onNavigate(baseId)`;
  // the parent rebinds the modal's `invoiceId` prop and the existing
  // `$effect` fetches the base invoice's data into the SAME dialog
  // (no breadcrumb stack — operator's browser-Back-equivalent is
  // their head per the session-29 handoff lean). No new audit event
  // fires on navigation — inspection is read-only per CLAUDE.md
  // rule 13.

  import {
    getInvoice,
    type InvoiceDetail,
  } from "../lib/api";
  import { labelMeta, type LabelSignal } from "../lib/labels";

  interface Props {
    invoiceId: string | null;
    onClose: () => void;
    /** PR-26 — chain-link navigation callback. Invoked when the
     * operator clicks the base invoice id rendered next to an
     * `InvoiceStornoIssued` / `InvoiceModificationIssued` audit row.
     * The parent rebinds its `selectedId` to the base id and the
     * `$effect` below re-fetches into the same modal. */
    onNavigate: (baseId: string) => void;
  }

  let { invoiceId, onClose, onNavigate }: Props = $props();

  let dialogEl: HTMLDialogElement | null = $state(null);
  let detail: InvoiceDetail | null = $state(null);
  let loadState: "idle" | "loading" | "loaded" | "error" = $state("idle");
  let errorMessage: string | null = $state(null);

  // Drive the dialog open/close lifecycle from the `invoiceId` prop.
  // Opening: invoke `showModal()` and kick off the fetch. Closing:
  // invoke `close()` if the dialog is still open. Guarded against the
  // double-open `InvalidStateError` from the platform.
  $effect(() => {
    if (!dialogEl) return;
    if (invoiceId !== null) {
      if (!dialogEl.open) dialogEl.showModal();
      void load(invoiceId);
    } else {
      if (dialogEl.open) dialogEl.close();
      detail = null;
      loadState = "idle";
      errorMessage = null;
    }
  });

  async function load(id: string) {
    loadState = "loading";
    errorMessage = null;
    detail = null;
    try {
      detail = await getInvoice(id);
      loadState = "loaded";
    } catch (err: unknown) {
      loadState = "error";
      errorMessage = err instanceof Error ? err.message : String(err);
    }
  }

  function signalClass(signal: LabelSignal): string {
    return `signal-${signal}`;
  }

  // ESC + backdrop dismiss both fire the native `close` event; we
  // mirror it back to the parent so the parent's `selectedId` resets.
  // Without this, a second click on the same row would not re-open
  // because `invoiceId` never transitioned through `null`.
  function handleDialogClose() {
    onClose();
  }

  // Clicking the dialog backdrop closes the dialog. The native
  // <dialog> only treats clicks on the dialog element itself (not
  // its children) as backdrop clicks; we forward those to `close()`.
  function handleDialogClick(e: MouseEvent) {
    if (e.target === dialogEl) {
      dialogEl?.close();
    }
  }

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
</script>

<dialog
  bind:this={dialogEl}
  class="detail"
  onclose={handleDialogClose}
  onclick={handleDialogClick}
  aria-label="Invoice detail"
>
  <div class="detail-frame">
    <header class="detail-head">
      <div class="detail-title">
        <span class="detail-label">Invoice</span>
        <h2 class="detail-id mono">{invoiceId ?? ""}</h2>
      </div>
      <button
        type="button"
        class="quiet-button"
        onclick={() => dialogEl?.close()}
        aria-label="Close invoice detail"
      >
        Close
      </button>
    </header>

    {#if loadState === "loading"}
      <p class="muted">Loading…</p>
    {:else if loadState === "error"}
      <p class="error" role="alert">{errorMessage}</p>
    {:else if loadState === "loaded" && detail}
      {@const meta = labelMeta(detail.state)}
      <dl class="meta-grid">
        <dt>Series #</dt>
        <dd class="mono">{detail.sequence_number}</dd>
        <dt>Fiscal year</dt>
        <dd class="mono">{detail.fiscal_year}</dd>
        <dt>State</dt>
        <dd>
          <span
            class="state-pill {signalClass(meta.signal)}"
            title={meta.tooltip}
          >
            <span class="state-icon" aria-hidden="true">{meta.icon}</span>
            <span class="state-text">{detail.state}</span>
          </span>
        </dd>
        <dt>Total (gross)</dt>
        <dd class="mono">{formatHuf(detail.total_gross)}</dd>
      </dl>

      <h3 class="section-head">Audit trail</h3>
      {#if detail.audit_entries.length === 0}
        <p class="muted">
          No audit-ledger entries reference this invoice id directly.
          Chain-link entries (storno / modification) reference this
          invoice via their <code>base_invoice_id</code> payload field
          and do not appear in this list per <code>serve.rs</code>'s
          per-id walker.
        </p>
      {:else}
        <table class="dense">
          <thead>
            <tr>
              <th scope="col" class="col-num">Seq</th>
              <th scope="col" class="col-kind">Kind</th>
              <th scope="col" class="col-actor">Actor</th>
              <th scope="col" class="col-time">Occurred at</th>
            </tr>
          </thead>
          <tbody>
            {#each detail.audit_entries as entry (entry.seq)}
              <tr>
                <td class="col-num mono">{entry.seq}</td>
                <td class="col-kind mono">
                  {entry.kind}
                  {#if entry.chain_base_invoice_id}
                    <span class="chain-arrow" aria-hidden="true">→</span>
                    <button
                      type="button"
                      class="id-link"
                      onclick={() => onNavigate(entry.chain_base_invoice_id!)}
                      aria-label={`Navigate to base invoice ${entry.chain_base_invoice_id}`}
                    >
                      {entry.chain_base_invoice_id}
                    </button>
                  {/if}
                </td>
                <td class="col-actor mono">{entry.actor}</td>
                <td class="col-time mono">{entry.occurred_at}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    {/if}
  </div>
</dialog>

<style>
  /* Native <dialog> reset — the platform default carries chrome
   * (border, padding, background) that fights ADR-0017's quiet
   * surfaces. */
  dialog.detail {
    border: 1px solid var(--color-surface-divider);
    background: var(--color-surface-base);
    color: var(--color-text-primary);
    padding: 0;
    max-width: 90vw;
    max-height: 90vh;
    width: 720px;
    overflow: hidden;
  }

  dialog.detail::backdrop {
    background: rgba(0, 0, 0, 0.5);
  }

  .detail-frame {
    display: flex;
    flex-direction: column;
    max-height: 90vh;
    overflow: auto;
    padding: var(--space-4) var(--space-5);
    animation: aberp-fade-in var(--motion-fade-in) both;
  }

  .detail-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
    margin-bottom: var(--space-4);
  }

  .detail-title {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .detail-label {
    text-transform: uppercase;
    font-size: var(--type-size-xs);
    letter-spacing: 0.06em;
    color: var(--color-text-secondary);
  }

  .detail-id {
    margin: 0;
    font-size: var(--type-size-lg);
    font-weight: 500;
    color: var(--color-text-strong);
    word-break: break-all;
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

  .quiet-button:hover {
    color: var(--color-text-strong);
  }

  /* Two-column dt/dd grid for the invoice metadata. */
  .meta-grid {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--space-2) var(--space-4);
    margin: 0 0 var(--space-5) 0;
    font-size: var(--type-size-sm);
  }

  .meta-grid dt {
    text-transform: uppercase;
    font-size: var(--type-size-xs);
    letter-spacing: 0.06em;
    color: var(--color-text-secondary);
    align-self: center;
  }

  .meta-grid dd {
    margin: 0;
    color: var(--color-text-strong);
  }

  .section-head {
    margin: 0 0 var(--space-2) 0;
    font-size: var(--type-size-sm);
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--color-text-secondary);
  }

  .muted {
    color: var(--color-text-muted);
    font-size: var(--type-size-sm);
    margin: 0 0 var(--space-3) 0;
  }

  .error {
    color: var(--color-signal-negative);
    font-family: var(--type-family-mono);
    font-size: var(--type-size-sm);
    margin: var(--space-2) 0;
    white-space: pre-wrap;
    word-break: break-word;
  }

  /* Dense table — same pattern as InvoiceList per ADR-0017 §3. */
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

  .mono {
    font-family: var(--type-family-mono);
    font-variant-numeric: tabular-nums;
  }

  .col-num {
    text-align: right;
    width: 6ch;
  }

  .col-kind {
    /* Pre-PR-26 the column was a fixed 22ch (longest kind name).
     * PR-26 lets chain-link rows append `→ <base_id>` (~30 chars),
     * so the column grows to fit while keeping the 22ch floor so
     * non-chain rows still align with the rest of the dense table.
     * `word-break: break-all` lets long ULID-style base ids wrap if
     * the modal is narrowed. */
    min-width: 22ch;
    word-break: break-all;
  }

  /* PR-26 — chain-link affordance inside the kind cell. Same quiet-
   * link aesthetic as InvoiceList's id-link (per ADR-0017 §1-2 —
   * chrome stays quiet; underline-on-hover is the signal). */
  .id-link {
    background: none;
    border: none;
    padding: 0;
    margin: 0;
    font: inherit;
    color: var(--color-text-primary);
    text-align: left;
    cursor: pointer;
  }

  .id-link:hover,
  .id-link:focus-visible {
    color: var(--color-text-strong);
    text-decoration: underline;
    text-decoration-color: var(--color-text-muted);
    text-underline-offset: 2px;
  }

  .id-link:focus-visible {
    outline: 1px solid var(--color-text-muted);
    outline-offset: 2px;
  }

  .chain-arrow {
    color: var(--color-text-muted);
    margin: 0 var(--space-1);
  }

  .col-actor {
    width: 16ch;
  }

  .col-time {
    /* RFC3339 strings are ~25 chars; let the column take the rest. */
    width: auto;
  }

  .state-pill {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    padding: 0 var(--space-2);
    font-family: var(--type-family-mono);
    font-size: var(--type-size-xs);
    line-height: 1.6;
    letter-spacing: 0.04em;
    border: 1px solid var(--color-surface-divider);
    border-radius: 2px;
    background: var(--color-surface-base);
    color: var(--color-text-secondary);
    cursor: help;
  }

  .state-icon {
    font-family: var(--type-family-body);
    font-size: var(--type-size-sm);
    line-height: 1;
  }

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
  .state-pill.signal-divergence {
    color: var(--color-signal-divergence);
    border-color: var(--color-signal-divergence);
  }
  .state-pill.signal-muted {
    color: var(--color-text-muted);
    border-color: var(--color-surface-divider);
  }

  code {
    font-family: var(--type-family-mono);
    color: var(--color-text-strong);
  }
</style>
