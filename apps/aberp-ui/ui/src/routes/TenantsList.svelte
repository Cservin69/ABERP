<script lang="ts">
  // S433 — Tenants admin (Settings area). List every tenant from
  // tenants.toml with its state + the running indicator, and drive the
  // full lifecycle: Add (provision a new tenant), Switch (restart-based),
  // Archive (soft-delete), Restore.
  //
  // The per-row button-enable rules come from the pure `tenants-list`
  // helper, which mirrors the backend registry invariants so a refused
  // action is disabled rather than shown as a dead-end ([[hulye-biztos]]);
  // the backend remains the source of truth ([[trust-code-not-operator]]).
  //
  // Switch is restart-based: the backend writes a one-shot hint + acks,
  // then the Tauri shell drains + re-spawns the backend so the next boot
  // comes up as the chosen tenant. We never swap DB/creds in-process.
  //
  // Dark-theme tokens per [[spa-dark-theme-default]].

  import { onMount } from "svelte";

  import {
    archiveTenant,
    createTenant,
    listTenants,
    restoreTenant,
    switchTenant,
    type TenantRow,
  } from "../lib/api";
  import { buttonStateFor, orderTenants } from "../lib/tenants-list";

  type LoadState = "loading" | "ready" | "error";

  let loadState = $state<LoadState>("loading");
  let errorMessage = $state<string | null>(null);
  let rows = $state<TenantRow[]>([]);
  let busySlug = $state<string | null>(null);
  let switching = $state<string | null>(null);

  // Add-tenant inline form.
  let showAdd = $state(false);
  let newSlug = $state("");
  let newDisplayName = $state("");
  let addError = $state<string | null>(null);

  let ordered = $derived(orderTenants(rows));
  let canSubmitAdd = $derived(
    newSlug.trim().length > 0 && newDisplayName.trim().length > 0 && busySlug === null,
  );

  async function load(): Promise<void> {
    loadState = "loading";
    errorMessage = null;
    try {
      const resp = await listTenants();
      rows = resp.tenants;
      loadState = "ready";
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
      loadState = "error";
    }
  }

  onMount(() => {
    void load();
  });

  function fmtDate(iso: string): string {
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
  }

  async function onAddSubmit(event: Event): Promise<void> {
    event.preventDefault();
    if (!canSubmitAdd) return;
    busySlug = "__add__";
    addError = null;
    try {
      const resp = await createTenant(newSlug.trim(), newDisplayName.trim());
      rows = resp.tenants;
      showAdd = false;
      newSlug = "";
      newDisplayName = "";
    } catch (e) {
      addError = e instanceof Error ? e.message : String(e);
    } finally {
      busySlug = null;
    }
  }

  async function onSwitch(row: TenantRow): Promise<void> {
    if (
      !confirm(
        `Switch to ${row.display_name} (${row.slug})?\n\n` +
          "ABERP will restart to load this tenant. In-flight work finishes first.",
      )
    ) {
      return;
    }
    busySlug = row.slug;
    errorMessage = null;
    try {
      await switchTenant(row.slug);
      // The backend acked + wrote the hint; the shell is now draining +
      // re-spawning. Show a terminal "switching" state — the app boot
      // poller takes over from here.
      switching = row.display_name;
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
      busySlug = null;
    }
  }

  async function onArchive(row: TenantRow): Promise<void> {
    if (!confirm(`Archive ${row.display_name} (${row.slug})? It can be restored later.`)) {
      return;
    }
    busySlug = row.slug;
    errorMessage = null;
    try {
      const resp = await archiveTenant(row.slug);
      rows = resp.tenants;
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
    } finally {
      busySlug = null;
    }
  }

  async function onRestore(row: TenantRow): Promise<void> {
    busySlug = row.slug;
    errorMessage = null;
    try {
      const resp = await restoreTenant(row.slug);
      rows = resp.tenants;
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
    } finally {
      busySlug = null;
    }
  }
</script>

<section class="tn-page">
  <header class="tn-page__head">
    <h1 class="tn-page__title">
      <span>Bérlők / Tenants</span>
      <span class="tn-page__hint">
        Minden bérlő a tenants.toml-ból — hozzáadás, váltás (újraindítással),
        archiválás, visszaállítás.
      </span>
      <span class="tn-page__hint">
        Every tenant from tenants.toml — add, switch (via restart), archive,
        restore.
      </span>
    </h1>
    <button
      class="tn-btn tn-btn--primary"
      type="button"
      disabled={switching !== null}
      onclick={() => {
        showAdd = !showAdd;
        addError = null;
      }}
    >
      {showAdd ? "Cancel" : "+ Add tenant"}
    </button>
  </header>

  {#if switching}
    <p class="tn-banner tn-banner--info">
      Switching to <strong>{switching}</strong> — ABERP is restarting. This
      window will reconnect to the new tenant momentarily.
    </p>
  {/if}

  {#if showAdd}
    <form class="tn-add" onsubmit={onAddSubmit}>
      <div class="tn-add__field">
        <label class="tn-add__label" for="tn-slug">Slug</label>
        <input
          id="tn-slug"
          class="tn-add__input tn-add__input--mono"
          type="text"
          bind:value={newSlug}
          placeholder="acme"
          autocomplete="off"
          spellcheck="false"
        />
        <span class="tn-add__hint">Letters, digits, <code>_</code> and <code>-</code> only.</span>
      </div>
      <div class="tn-add__field">
        <label class="tn-add__label" for="tn-name">Display name</label>
        <input
          id="tn-name"
          class="tn-add__input"
          type="text"
          bind:value={newDisplayName}
          placeholder="ACME Manufacturing Kft."
          autocomplete="off"
        />
        <span class="tn-add__hint">
          NAV credentials + seller identity are set up on first switch into the
          new tenant (the existing setup wizard).
        </span>
      </div>
      <button class="tn-btn tn-btn--primary" type="submit" disabled={!canSubmitAdd}>
        {busySlug === "__add__" ? "Creating…" : "Create tenant"}
      </button>
      {#if addError}
        <p class="tn-add__error">{addError}</p>
      {/if}
    </form>
  {/if}

  {#if loadState === "loading"}
    <p class="tn-muted">Loading tenants…</p>
  {:else if loadState === "error"}
    <div class="tn-banner tn-banner--error">
      <strong>Could not load tenants.</strong>
      {errorMessage}
    </div>
  {:else if rows.length === 0}
    <p class="tn-muted">No tenants registered yet.</p>
  {:else}
    {#if errorMessage}
      <div class="tn-banner tn-banner--error">
        <strong>Action failed.</strong>
        {errorMessage}
      </div>
    {/if}
    <table class="tn-table">
      <thead>
        <tr>
          <th class="tn-th">Tenant</th>
          <th class="tn-th">Slug</th>
          <th class="tn-th">State</th>
          <th class="tn-th">Created</th>
          <th class="tn-th tn-th--actions">Actions</th>
        </tr>
      </thead>
      <tbody>
        {#each ordered as row (row.slug)}
          {@const btn = buttonStateFor(row, rows)}
          <tr class="tn-row" class:tn-row--running={row.running}>
            <td class="tn-td">
              {row.display_name}
              {#if row.running}
                <span class="tn-chip tn-chip--running" title="Currently running">
                  running
                </span>
              {/if}
            </td>
            <td class="tn-td tn-td--mono">{row.slug}</td>
            <td class="tn-td">
              <span
                class="tn-chip"
                class:tn-chip--active={row.state === "active"}
                class:tn-chip--archived={row.state === "archived"}
                class:tn-chip--demo={row.state === "demo"}
              >
                {row.state === "demo" ? "DEMO" : row.state}
              </span>
            </td>
            <td class="tn-td">{fmtDate(row.created_at)}</td>
            <td class="tn-td tn-td--actions">
              <button
                class="tn-btn"
                type="button"
                disabled={!btn.canSwitch || busySlug !== null || switching !== null}
                onclick={() => void onSwitch(row)}
              >
                {busySlug === row.slug ? "…" : "Switch"}
              </button>
              {#if row.state === "active"}
                <button
                  class="tn-btn tn-btn--danger"
                  type="button"
                  disabled={!btn.canArchive || busySlug !== null || switching !== null}
                  onclick={() => void onArchive(row)}
                >
                  Archive
                </button>
              {:else if row.state === "archived"}
                <button
                  class="tn-btn"
                  type="button"
                  disabled={!btn.canRestore || busySlug !== null || switching !== null}
                  onclick={() => void onRestore(row)}
                >
                  Restore
                </button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</section>

<style>
  .tn-page {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4) 0;
  }
  .tn-page__head {
    display: flex;
    justify-content: space-between;
    align-items: flex-end;
    gap: var(--space-3);
    flex-wrap: wrap;
  }
  .tn-page__title {
    font-size: var(--type-size-lg);
    font-weight: 600;
    margin: 0;
    color: var(--color-text-strong);
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .tn-page__hint {
    font-size: var(--type-size-sm);
    font-weight: 400;
    color: var(--color-text-muted);
  }
  .tn-muted {
    color: var(--color-text-muted);
    font-size: var(--type-size-sm);
  }
  .tn-banner {
    padding: var(--space-3);
    border-radius: 3px;
    font-size: var(--type-size-sm);
    color: var(--color-text-primary);
  }
  .tn-banner--error {
    background: var(--color-surface-sunken);
    border: 1px solid var(--color-signal-negative);
  }
  .tn-banner--error strong {
    color: var(--color-signal-negative);
  }
  .tn-banner--info {
    background: var(--color-surface-raised);
    border: 1px solid var(--color-surface-divider);
  }
  .tn-add {
    display: flex;
    align-items: flex-end;
    gap: var(--space-3);
    flex-wrap: wrap;
    padding: var(--space-3);
    border: 1px solid var(--color-surface-divider);
    background: var(--color-surface-raised);
    border-radius: 3px;
  }
  .tn-add__field {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    flex: 1 1 220px;
  }
  .tn-add__label {
    font-size: var(--type-size-sm);
    color: var(--color-text-secondary);
  }
  .tn-add__input {
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-surface-divider);
    border-radius: 3px;
    background: var(--color-surface-base);
    color: var(--color-text-strong);
    font-family: var(--type-family-body);
    font-size: var(--type-size-sm);
  }
  .tn-add__input--mono {
    font-family: var(--type-family-mono);
  }
  .tn-add__hint {
    font-size: var(--type-size-sm);
    color: var(--color-text-muted);
  }
  .tn-add__error {
    flex-basis: 100%;
    margin: 0;
    color: var(--color-signal-negative);
    font-size: var(--type-size-sm);
  }
  .tn-table {
    width: 100%;
    border-collapse: collapse;
    border: 1px solid var(--color-surface-divider);
    border-radius: 3px;
    background: var(--color-surface-base);
    font-variant-numeric: tabular-nums;
  }
  .tn-th {
    text-align: left;
    padding: var(--space-2) var(--space-3);
    background: var(--color-surface-raised);
    color: var(--color-text-secondary);
    font-weight: 600;
    font-size: var(--type-size-sm);
    border-bottom: 1px solid var(--color-surface-divider);
  }
  .tn-th--actions {
    text-align: right;
  }
  .tn-row {
    border-bottom: 1px solid var(--color-surface-divider);
  }
  .tn-row:last-child {
    border-bottom: none;
  }
  .tn-row--running {
    background: var(--color-surface-raised);
  }
  .tn-td {
    padding: var(--space-2) var(--space-3);
    color: var(--color-text-primary);
    font-size: var(--type-size-sm);
  }
  .tn-td--mono {
    font-family: var(--type-family-mono);
  }
  .tn-td--actions {
    text-align: right;
    white-space: nowrap;
  }
  .tn-chip {
    display: inline-block;
    padding: 0 var(--space-2);
    border-radius: 999px;
    font-size: var(--type-size-sm);
    border: 1px solid var(--color-surface-divider);
    color: var(--color-text-secondary);
  }
  .tn-chip--running {
    margin-left: var(--space-2);
    color: var(--color-signal-positive, var(--color-text-strong));
    border-color: var(--color-signal-positive, var(--color-text-strong));
  }
  .tn-chip--active {
    color: var(--color-signal-positive, var(--color-text-strong));
    border-color: var(--color-signal-positive, var(--color-text-strong));
  }
  .tn-chip--archived {
    color: var(--color-text-muted);
  }
  .tn-chip--demo {
    color: var(--color-signal-warning, var(--color-text-strong));
    border-color: var(--color-signal-warning, var(--color-text-strong));
    letter-spacing: 0.06em;
  }
  .tn-btn {
    padding: var(--space-1) var(--space-3);
    margin-left: var(--space-2);
    border: 1px solid var(--color-surface-divider);
    background: var(--color-surface-base);
    color: var(--color-text-secondary);
    border-radius: 3px;
    cursor: pointer;
    font-family: var(--type-family-body);
    font-size: var(--type-size-sm);
  }
  .tn-btn:first-child {
    margin-left: 0;
  }
  .tn-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .tn-btn--primary {
    color: var(--color-text-strong);
    border-color: var(--color-text-secondary);
  }
  .tn-btn--danger {
    color: var(--color-signal-negative);
    border-color: var(--color-signal-negative);
  }
</style>
