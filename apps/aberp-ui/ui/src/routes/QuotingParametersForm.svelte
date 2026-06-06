<script lang="ts">
  // S267 / PR-256 — Maintenance → Quoting → Global parameters page.
  // Singleton form: GET shows the seeded row, PUT updates it. No
  // list view (only one row ever).

  import { onMount } from "svelte";

  import {
    getQuotingParameters,
    updateQuotingParameters,
    type QuotingParameters,
    type QuotingParametersInput,
  } from "../lib/api";
  import { isDemoMode } from "../lib/workshop-demo-mode";

  type LoadState = "idle" | "loading" | "ready" | "error";

  let loadState = $state<LoadState>("idle");
  let errorMessage = $state<string | null>(null);
  let current = $state<QuotingParameters | null>(null);
  let draft = $state<QuotingParametersInput>({
    scrap_factor: 0.08,
    profit_margin_base: 0.35,
    overhead_factor: 0.20,
    setup_amortization_threshold: 5,
    min_margin: 0.10,
    exotic_material_tax: 0.05,
    notes: null,
  });

  let saving = $state(false);
  let saveError = $state<string | null>(null);
  let saved = $state<boolean>(false);

  const demo = isDemoMode();

  onMount(() => {
    void refresh();
  });

  async function refresh(): Promise<void> {
    loadState = "loading";
    errorMessage = null;
    saved = false;
    try {
      const p = await getQuotingParameters();
      current = p;
      draft = {
        scrap_factor: p.scrap_factor,
        profit_margin_base: p.profit_margin_base,
        overhead_factor: p.overhead_factor,
        setup_amortization_threshold: p.setup_amortization_threshold,
        min_margin: p.min_margin,
        exotic_material_tax: p.exotic_material_tax,
        notes: p.notes,
      };
      loadState = "ready";
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
      loadState = "error";
    }
  }

  async function save(): Promise<void> {
    saving = true;
    saveError = null;
    try {
      const updated = await updateQuotingParameters(draft);
      current = updated;
      saved = true;
    } catch (e) {
      saveError = e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
    }
  }

  function pct(n: number): string {
    return `${(n * 100).toFixed(1)}%`;
  }
</script>

<section class="qt-page" data-testid="parameters-section">
  <header class="qt-page__head">
    <div>
      <h2 class="qt-page__title">
        Globális paraméterek / Global parameters
        <span class="qt-page__hint">
          Az automatikus árajánlat-motor alap-szorzói (selejt, fedezet,
          általános költség) / Auto-quoting engine baselines (scrap,
          margin, overhead)
        </span>
      </h2>
    </div>
    <div class="qt-page__actions">
      <button
        type="button"
        class="qt-page__refresh"
        disabled={loadState === "loading"}
        onclick={() => void refresh()}
      >
        {loadState === "loading" ? "Frissítés…" : "Frissítés / Refresh"}
      </button>
    </div>
  </header>

  {#if demo}
    <div class="qt-page__demo" role="status">
      Demo mód — módosítás letiltva. / Demo mode — changes disabled.
    </div>
  {/if}

  {#if loadState === "loading" && current === null}
    <p class="qt-page__muted">Betöltés… / Loading…</p>
  {:else if loadState === "error"}
    <div class="qt-page__error" role="alert">
      <strong>Sikertelen lekérdezés / Failed to load.</strong>
      <p>{errorMessage}</p>
    </div>
  {:else if current !== null}
    {#if current.updated_by_actor === "boot"}
      <div class="qt-page__notice" role="status">
        Az alapértékek vannak érvényben — még nem tuningoltad. /
        Default values active — not yet tuned by the operator.
      </div>
    {/if}

    <div class="qt-grid">
      <label>
        <span>Selejt-tényező / Scrap factor ({pct(draft.scrap_factor)})</span>
        <input
          type="number"
          step="0.01"
          min="0"
          max="1"
          disabled={demo}
          bind:value={draft.scrap_factor}
        />
        <small>Az anyagra szorozzuk, fémforgács-veszteség / Multiplied on stock for chip waste.</small>
      </label>
      <label>
        <span>Alap fedezet / Profit margin base ({pct(draft.profit_margin_base)})</span>
        <input
          type="number"
          step="0.01"
          min="0"
          max="1"
          disabled={demo}
          bind:value={draft.profit_margin_base}
        />
        <small>A költség fölé rakott fedezet / Margin above cost.</small>
      </label>
      <label>
        <span>Általános ktg. / Overhead factor ({pct(draft.overhead_factor)})</span>
        <input
          type="number"
          step="0.01"
          min="0"
          max="1"
          disabled={demo}
          bind:value={draft.overhead_factor}
        />
        <small>(anyag+munkadíj) × ez / Applied to (material + labour).</small>
      </label>
      <label>
        <span>Setup amortizáció küszöb / Setup amortization threshold</span>
        <input
          type="number"
          min="1"
          step="1"
          disabled={demo}
          bind:value={draft.setup_amortization_threshold}
        />
        <small>Ennyi darab fölött a setup amortizál / Above this qty setup amortizes.</small>
      </label>
      <label>
        <span>Minimum fedezet / Min margin ({pct(draft.min_margin)})</span>
        <input
          type="number"
          step="0.01"
          min="0"
          max="1"
          disabled={demo}
          bind:value={draft.min_margin}
        />
        <small>E küszöb alatt az ajánlatot a motor visszautasítja / Quotes below this are rejected.</small>
      </label>
      <label>
        <span>Egzotikus anyag-adó / Exotic material tax ({pct(draft.exotic_material_tax)})</span>
        <input
          type="number"
          step="0.01"
          min="0"
          max="1"
          disabled={demo}
          bind:value={draft.exotic_material_tax}
        />
        <small>Inconel/Monel-típusú anyagokra rakott pótdíj / Surcharge on exotic-class materials.</small>
      </label>
      <label class="qt-grid__notes">
        <span>Jegyzet / Notes</span>
        <input
          type="text"
          value={draft.notes ?? ""}
          disabled={demo}
          oninput={(e) => {
            const v = (e.target as HTMLInputElement).value;
            draft.notes = v.trim() === "" ? null : v;
          }}
        />
      </label>
    </div>

    {#if saveError !== null}
      <div class="qt-page__error" role="alert">
        <strong>Mentés sikertelen / Save failed.</strong>
        <p>{saveError}</p>
      </div>
    {/if}

    {#if saved}
      <div class="qt-page__success" role="status">
        Mentve / Saved at {current.updated_at}
      </div>
    {/if}

    <div class="qt-form__actions">
      <button
        type="button"
        class="qt-form__save"
        disabled={saving || demo}
        onclick={() => void save()}
      >
        {saving ? "Mentés…" : "Mentés / Save"}
      </button>
    </div>
  {/if}
</section>

<style>
  .qt-page {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4) 0;
  }
  .qt-page__head {
    display: flex;
    justify-content: space-between;
    align-items: flex-end;
    gap: var(--space-3);
    flex-wrap: wrap;
  }
  .qt-page__title {
    font-size: var(--type-size-lg);
    font-weight: 600;
    margin: 0;
    color: var(--color-text-strong);
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .qt-page__hint {
    font-size: var(--type-size-sm);
    font-weight: 400;
    color: var(--color-text-muted);
  }
  .qt-page__actions {
    display: flex;
    gap: var(--space-2);
  }
  .qt-page__refresh,
  .qt-form__save {
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-surface-divider);
    background: var(--color-surface-raised);
    color: var(--color-text-secondary);
    border-radius: 3px;
    cursor: pointer;
    font-family: var(--type-family-body);
    font-size: var(--type-size-sm);
  }
  .qt-form__save {
    color: var(--color-text-strong);
    border-color: var(--color-signal-positive);
  }
  .qt-page__demo {
    padding: var(--space-2) var(--space-3);
    border: 1px dashed var(--color-signal-warning);
    color: var(--color-signal-warning);
    border-radius: 3px;
    font-size: var(--type-size-sm);
  }
  .qt-page__notice {
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-surface-divider);
    background: var(--color-surface-raised);
    color: var(--color-text-muted);
    border-radius: 3px;
    font-size: var(--type-size-sm);
  }
  .qt-page__muted {
    color: var(--color-text-muted);
    font-style: italic;
  }
  .qt-page__error {
    padding: var(--space-3);
    background: var(--color-surface-sunken);
    border: 1px solid var(--color-signal-negative);
    border-radius: 3px;
    color: var(--color-text-primary);
  }
  .qt-page__error strong {
    color: var(--color-signal-negative);
  }
  .qt-page__success {
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-signal-positive);
    color: var(--color-signal-positive);
    background: var(--color-surface-raised);
    border-radius: 3px;
    font-size: var(--type-size-sm);
  }
  .qt-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
    gap: var(--space-3);
    background: var(--color-surface-sunken);
    padding: var(--space-3);
    border-radius: 4px;
    border: 1px solid var(--color-surface-divider);
  }
  .qt-grid label {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    font-size: var(--type-size-sm);
    color: var(--color-text-secondary);
  }
  .qt-grid__notes {
    grid-column: 1 / -1;
  }
  .qt-grid input {
    padding: var(--space-2);
    background: var(--color-surface-raised);
    border: 1px solid var(--color-surface-divider);
    border-radius: 3px;
    color: var(--color-text-strong);
    font-family: var(--type-family-mono);
    font-size: var(--type-size-sm);
  }
  .qt-grid small {
    color: var(--color-text-muted);
    font-size: var(--type-size-xs);
  }
  .qt-form__actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
