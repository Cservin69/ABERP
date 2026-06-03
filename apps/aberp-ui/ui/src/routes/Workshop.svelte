<script lang="ts">
  // S235 / PR-231 — Workshop / Műhely operator dashboard.
  //
  // Wall-TV at-a-glance view of Stage 3 state: Work Orders by state,
  // low-stock product count, QA backlog, Dispatch panel, today's
  // invoice headline, recent audit-ledger activity, and MES adapter
  // env-config snapshot.
  //
  // One backend endpoint (`get_workshop_dashboard`) returns the whole
  // bundle in one shot; the SPA polls every ~10s. Per-tile refresh
  // re-fetches the bundle. No per-tile fetcher fan-out — cheaper than
  // six round-trips and the SPA stays simpler.
  //
  // Dark-theme tokens only per [[spa-dark-theme-default]]. Canonical
  // references: DispatchList.svelte (S234) + QaList.svelte (S233) +
  // StatisticsPage.svelte (S225 / PR-221).

  import { onMount, onDestroy } from "svelte";
  import {
    getWorkshopDashboard,
    type WorkshopDashboard,
    type WorkOrderStateCounts,
    type QaStateCounts,
  } from "../lib/api";
  import { navigateTo } from "../lib/router";
  import {
    adapterDotClass,
    fmtEventKind,
    fmtMinor,
    resolvePollInterval,
  } from "../lib/workshop-format";

  type LoadState = "idle" | "loading" | "ready" | "error";

  // Default poll cadence — 10s. Operator can override via the
  // `VITE_WORKSHOP_POLL_MS` env var read at build time. Bounded via
  // `resolvePollInterval` so a typo neither burns the backend nor
  // never refreshes.
  const POLL_INTERVAL_MS = resolvePollInterval(
    (import.meta as unknown as { env?: Record<string, string> }).env
      ?.VITE_WORKSHOP_POLL_MS,
    10_000,
  );

  // Debounce — Refresh button bursts get coalesced into one fetch.
  const REFRESH_DEBOUNCE_MS = 500;

  let loadState: LoadState = $state("idle");
  let errorMessage = $state<string | null>(null);
  let bundle: WorkshopDashboard | null = $state(null);
  // Locale flip — match the bilingual chrome of QaList / DispatchList.
  let lang: "hu" | "en" = $state("hu");

  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let lastRefreshAt = 0;
  let inFlight = $state(false);

  onMount(() => {
    void refresh();
    pollTimer = setInterval(() => {
      void refresh();
    }, POLL_INTERVAL_MS);
  });

  onDestroy(() => {
    if (pollTimer !== null) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  });

  async function refresh(): Promise<void> {
    // Refresh-storm protection per [[trust-code-not-operator]]: an
    // operator double-clicking the button does NOT issue two requests
    // back-to-back. The poll-driven tick also goes through this guard,
    // so a manual click immediately followed by the next tick coalesces.
    const now =
      typeof performance !== "undefined" && performance.now
        ? performance.now()
        : Date.now();
    if (inFlight) return;
    if (now - lastRefreshAt < REFRESH_DEBOUNCE_MS) return;
    lastRefreshAt = now;
    inFlight = true;

    if (loadState === "idle") loadState = "loading";
    try {
      const next = await getWorkshopDashboard();
      bundle = next;
      loadState = "ready";
      errorMessage = null;
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
      loadState = "error";
    } finally {
      inFlight = false;
    }
  }

  function toggleLang(): void {
    lang = lang === "hu" ? "en" : "hu";
  }

  // ── Click-through helpers ─────────────────────────────────────────
  //
  // For v1 we navigate to the module's list route; the operator selects
  // the desired state facet on the next screen. This keeps the
  // dashboard surgical per CLAUDE.md rule 3 — adding sessionStorage-
  // mediated cross-route filter init touches every list and is its
  // own PR if a future operator survey calls for it.

  function gotoWorkOrders(): void {
    navigateTo("work-orders");
  }
  function gotoProducts(): void {
    navigateTo("products");
  }
  function gotoQa(): void {
    navigateTo("qa");
  }
  function gotoDispatch(): void {
    navigateTo("dispatch");
  }
  function gotoStatistics(): void {
    navigateTo("statistics");
  }

  // ── Format helpers ────────────────────────────────────────────────

  function fmtRelativeTime(iso: string): string {
    if (iso === "" || iso === undefined) return "";
    const then = new Date(iso).getTime();
    if (Number.isNaN(then)) return iso;
    const diffSec = Math.round((then - Date.now()) / 1000);
    const abs = Math.abs(diffSec);
    const rtf = new Intl.RelativeTimeFormat(
      lang === "hu" ? "hu-HU" : "en-GB",
      { numeric: "auto" },
    );
    if (abs < 60) return rtf.format(diffSec, "second");
    if (abs < 3600) return rtf.format(Math.round(diffSec / 60), "minute");
    if (abs < 86_400) return rtf.format(Math.round(diffSec / 3600), "hour");
    return rtf.format(Math.round(diffSec / 86_400), "day");
  }

  // ── Localised labels ──────────────────────────────────────────────

  interface WoStateLabel {
    key: keyof WorkOrderStateCounts;
    hu: string;
    en: string;
  }
  const WO_STATE_LABELS: WoStateLabel[] = [
    { key: "created", hu: "Létrehozva", en: "Created" },
    { key: "released", hu: "Kiadva", en: "Released" },
    { key: "in_progress", hu: "Folyamatban", en: "In progress" },
    { key: "on_hold", hu: "Várakozik", en: "On hold" },
    { key: "completed", hu: "Kész", en: "Completed" },
    { key: "cancelled", hu: "Megszakítva", en: "Cancelled" },
  ];

  interface QaStateLabel {
    key: keyof QaStateCounts;
    hu: string;
    en: string;
  }
  // Pending + Reworking are the operator-actionable buckets per the
  // brief; the others are surfaced for completeness in a separate row
  // below so the tile reads as "what's blocking work" first.
  const QA_PRIMARY_LABELS: QaStateLabel[] = [
    { key: "pending", hu: "Függőben", en: "Pending" },
    { key: "reworking", hu: "Újramunkálás", en: "Reworking" },
  ];
  const QA_SECONDARY_LABELS: QaStateLabel[] = [
    { key: "passed", hu: "Sikeres", en: "Passed" },
    { key: "failed", hu: "Hibás", en: "Failed" },
    { key: "disposed", hu: "Selejt", en: "Disposed" },
  ];

  function fmtMinorWithLang(minor: number, currency: "HUF" | "EUR"): string {
    return fmtMinor(minor, currency, lang);
  }
</script>

<section
  class="ws-page"
  aria-labelledby="ws-page-title"
  data-testid="workshop-page"
>
  <header class="ws-head">
    <div class="ws-head__titles">
      <h2 id="ws-page-title">
        {lang === "hu" ? "Műhely" : "Workshop"}
      </h2>
      <p class="ws-head__sub">
        {lang === "hu"
          ? "Gyártás állapota, élőben"
          : "Production at a glance"}
      </p>
    </div>
    <div class="ws-head__actions">
      <button
        type="button"
        class="ws-head__btn"
        onclick={toggleLang}
        aria-label={lang === "hu" ? "Switch to English" : "Magyar nyelvre"}
      >
        {lang === "hu" ? "EN" : "HU"}
      </button>
      <button
        type="button"
        class="ws-head__btn"
        onclick={() => void refresh()}
        disabled={inFlight}
        data-testid="workshop-refresh-all"
      >
        {lang === "hu" ? "Frissítés" : "Refresh"}
      </button>
      {#if bundle !== null}
        <span class="ws-head__stamp" title={bundle.snapshot_at_iso8601}>
          {fmtRelativeTime(bundle.snapshot_at_iso8601)}
        </span>
      {/if}
    </div>
  </header>

  {#if loadState === "loading" && bundle === null}
    <p class="ws-status">
      {lang === "hu" ? "Betöltés…" : "Loading…"}
    </p>
  {:else if loadState === "error" && bundle === null}
    <p class="ws-status ws-status--error" role="alert">
      {lang === "hu" ? "Hiba" : "Error"}: {errorMessage ?? "?"}
      <button
        type="button"
        class="ws-head__btn"
        onclick={() => void refresh()}>{lang === "hu" ? "Újra" : "Retry"}</button
      >
    </p>
  {/if}

  {#if bundle !== null}
    {@const b = bundle}
    <div class="ws-grid" role="region" aria-label="dashboard tiles">
      <!-- Work Orders by state -->
      <article
        class="ws-tile ws-tile--wide"
        aria-labelledby="tile-wo-title"
        data-testid="tile-work-orders"
      >
        <header class="ws-tile__head">
          <h3 id="tile-wo-title">
            {lang === "hu" ? "Munkalapok" : "Work orders"}
          </h3>
          <button
            type="button"
            class="ws-tile__link"
            onclick={gotoWorkOrders}>{lang === "hu" ? "Lista →" : "Open →"}</button
          >
        </header>
        <ul class="ws-grid-inner">
          {#each WO_STATE_LABELS as label}
            <li class="ws-stat">
              <button
                type="button"
                class="ws-stat__btn"
                onclick={gotoWorkOrders}
                data-testid={`wo-stat-${label.key}`}
              >
                <span class="ws-stat__value">{b.work_orders[label.key]}</span>
                <span class="ws-stat__label">
                  {lang === "hu" ? label.hu : label.en}
                </span>
              </button>
            </li>
          {/each}
        </ul>
      </article>

      <!-- Low stock -->
      <article
        class="ws-tile"
        aria-labelledby="tile-low-stock-title"
        data-testid="tile-low-stock"
      >
        <header class="ws-tile__head">
          <h3 id="tile-low-stock-title">
            {lang === "hu" ? "Készlethiány" : "Low stock"}
          </h3>
          <button
            type="button"
            class="ws-tile__link"
            onclick={gotoProducts}>{lang === "hu" ? "Termékek →" : "Open →"}</button
          >
        </header>
        <button
          type="button"
          class={`ws-bignum ${b.low_stock_products.count > 0 ? "ws-bignum--warn" : ""}`}
          onclick={gotoProducts}
          data-testid="low-stock-count"
        >
          <span class="ws-bignum__value">{b.low_stock_products.count}</span>
          <span class="ws-bignum__label">
            {lang === "hu"
              ? "minimum alatti termék"
              : "products below minimum"}
          </span>
        </button>
      </article>

      <!-- QA backlog -->
      <article
        class="ws-tile"
        aria-labelledby="tile-qa-title"
        data-testid="tile-qa"
      >
        <header class="ws-tile__head">
          <h3 id="tile-qa-title">
            {lang === "hu" ? "Minőségellenőrzés" : "QA queue"}
          </h3>
          <button
            type="button"
            class="ws-tile__link"
            onclick={gotoQa}>{lang === "hu" ? "Sor →" : "Open →"}</button
          >
        </header>
        <ul class="ws-qa-primary">
          {#each QA_PRIMARY_LABELS as label}
            <li class="ws-stat">
              <button
                type="button"
                class="ws-stat__btn"
                onclick={gotoQa}
                data-testid={`qa-stat-${label.key}`}
              >
                <span
                  class={`ws-stat__value ${b.qa[label.key] > 0 ? "ws-stat__value--warn" : ""}`}
                >
                  {b.qa[label.key]}
                </span>
                <span class="ws-stat__label">
                  {lang === "hu" ? label.hu : label.en}
                </span>
              </button>
            </li>
          {/each}
        </ul>
        <p class="ws-qa-secondary">
          {#each QA_SECONDARY_LABELS as label, i}
            <span class="ws-qa-pair">
              {lang === "hu" ? label.hu : label.en}: {b.qa[label.key]}
            </span>
            {#if i < QA_SECONDARY_LABELS.length - 1}<span
                class="ws-qa-sep"
                aria-hidden="true">·</span
              >{/if}
          {/each}
        </p>
      </article>

      <!-- Dispatch board -->
      <article
        class="ws-tile"
        aria-labelledby="tile-dispatch-title"
        data-testid="tile-dispatch"
      >
        <header class="ws-tile__head">
          <h3 id="tile-dispatch-title">
            {lang === "hu" ? "Kiszállítás" : "Dispatch"}
          </h3>
          <button
            type="button"
            class="ws-tile__link"
            onclick={gotoDispatch}>{lang === "hu" ? "Tábla →" : "Open →"}</button
          >
        </header>
        <ul class="ws-grid-inner ws-grid-inner--narrow">
          <li class="ws-stat">
            <button
              type="button"
              class="ws-stat__btn"
              onclick={gotoDispatch}
              data-testid="dispatch-eligible"
            >
              <span class="ws-stat__value">
                {b.dispatch.eligible_work_orders}
              </span>
              <span class="ws-stat__label">
                {lang === "hu" ? "Indítható WO" : "Eligible WOs"}
              </span>
            </button>
          </li>
          <li class="ws-stat">
            <button
              type="button"
              class="ws-stat__btn"
              onclick={gotoDispatch}
              data-testid="dispatch-drafted"
            >
              <span class="ws-stat__value">
                {b.dispatch.by_state.drafted}
              </span>
              <span class="ws-stat__label">
                {lang === "hu" ? "Tervezet" : "Drafted"}
              </span>
            </button>
          </li>
          <li class="ws-stat">
            <button
              type="button"
              class="ws-stat__btn"
              onclick={gotoDispatch}
              data-testid="dispatch-shipped-today"
            >
              <span class="ws-stat__value">{b.dispatch.shipped_today}</span>
              <span class="ws-stat__label">
                {lang === "hu" ? "Ma kiszállítva" : "Shipped today"}
              </span>
            </button>
          </li>
        </ul>
      </article>

      <!-- Today snapshot -->
      <article
        class="ws-tile"
        aria-labelledby="tile-today-title"
        data-testid="tile-today"
      >
        <header class="ws-tile__head">
          <h3 id="tile-today-title">
            {lang === "hu" ? "Ma" : "Today"}
            <span class="ws-tile__hint">({b.today.date})</span>
          </h3>
          <button
            type="button"
            class="ws-tile__link"
            onclick={gotoStatistics}
            >{lang === "hu" ? "Statisztika →" : "Open →"}</button
          >
        </header>
        <ul class="ws-grid-inner ws-grid-inner--narrow">
          <li class="ws-stat">
            <span class="ws-stat__value">
              {b.today.issued_count_huf + b.today.issued_count_eur}
            </span>
            <span class="ws-stat__label">
              {lang === "hu" ? "Kiállított számla" : "Issued invoices"}
            </span>
          </li>
          <li class="ws-stat">
            <span class="ws-stat__value ws-stat__value--money">
              {fmtMinorWithLang(b.today.gross_revenue_huf_minor, "HUF")}
            </span>
            <span class="ws-stat__label">
              {lang === "hu" ? "Bruttó HUF" : "Gross HUF"}
            </span>
          </li>
          {#if b.today.gross_revenue_eur_minor !== 0 || b.today.issued_count_eur > 0}
            <li class="ws-stat">
              <span class="ws-stat__value ws-stat__value--money">
                {fmtMinorWithLang(b.today.gross_revenue_eur_minor, "EUR")}
              </span>
              <span class="ws-stat__label">
                {lang === "hu" ? "Bruttó EUR" : "Gross EUR"}
              </span>
            </li>
          {/if}
        </ul>
      </article>

      <!-- Adapter status -->
      <article
        class="ws-tile"
        aria-labelledby="tile-adapters-title"
        data-testid="tile-adapters"
      >
        <header class="ws-tile__head">
          <h3 id="tile-adapters-title">
            {lang === "hu" ? "Adapterek" : "Adapters"}
          </h3>
        </header>
        {#if b.adapters.length === 0}
          <p class="ws-empty">
            {lang === "hu" ? "Nincs konfigurálva" : "None configured"}
          </p>
        {:else}
          <ul class="ws-adapter-list">
            {#each b.adapters as adapter}
              <li
                class="ws-adapter"
                data-testid={`adapter-${adapter.name}`}
              >
                <span
                  class={`ws-dot ${adapterDotClass(adapter.status)}`}
                  aria-hidden="true"
                ></span>
                <div class="ws-adapter__body">
                  <span class="ws-adapter__name">{adapter.name}</span>
                  <span class="ws-adapter__meta">
                    {adapter.kind} · {adapter.host}:{adapter.port}
                  </span>
                </div>
                <span class={`ws-pill ws-pill--${adapter.status}`}>
                  {adapter.status === "enabled"
                    ? lang === "hu"
                      ? "Aktív"
                      : "Enabled"
                    : lang === "hu"
                      ? "Kikapcsolva"
                      : "Disabled"}
                </span>
              </li>
            {/each}
          </ul>
        {/if}
      </article>

      <!-- Recent activity -->
      <article
        class="ws-tile ws-tile--tall"
        aria-labelledby="tile-recent-title"
        data-testid="tile-recent-activity"
      >
        <header class="ws-tile__head">
          <h3 id="tile-recent-title">
            {lang === "hu" ? "Friss események" : "Recent activity"}
          </h3>
        </header>
        {#if b.recent_activity.length === 0}
          <p class="ws-empty">
            {lang === "hu" ? "Még nincs esemény" : "Nothing yet"}
          </p>
        {:else}
          <ol class="ws-activity">
            {#each b.recent_activity as entry (entry.id)}
              <li class="ws-activity__row">
                <span class="ws-activity__kind">{fmtEventKind(entry.kind)}</span>
                <time
                  class="ws-activity__time"
                  datetime={entry.at_iso8601}
                  title={entry.at_iso8601}
                >
                  {fmtRelativeTime(entry.at_iso8601)}
                </time>
              </li>
            {/each}
          </ol>
        {/if}
      </article>
    </div>
  {/if}
</section>

<style>
  /* S235 / PR-231 — Workshop dashboard dark-theme styles. Tokens only;
     no hardcoded hex. Canonical references per
     [[spa-dark-theme-default]]: DispatchList.svelte (S234) + QaList.svelte
     (S233) + StatisticsPage.svelte (S225). */

  .ws-page {
    padding: var(--space-4);
    color: var(--color-text-primary);
    background: var(--color-surface-base);
    min-height: 100vh;
  }

  .ws-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
    margin-bottom: var(--space-4);
  }

  .ws-head__titles h2 {
    margin: 0;
    font-size: var(--type-size-xxl);
    font-weight: 500;
    color: var(--color-text-strong);
  }

  .ws-head__sub {
    margin: var(--space-1) 0 0 0;
    color: var(--color-text-secondary);
    font-size: var(--type-size-sm);
  }

  .ws-head__actions {
    display: flex;
    gap: var(--space-2);
    align-items: center;
  }

  .ws-head__btn {
    background: var(--color-surface-raised);
    color: var(--color-text-strong);
    border: 1px solid var(--color-surface-divider);
    padding: var(--space-1) var(--space-3);
    font-family: var(--type-family-body);
    font-size: var(--type-size-sm);
    border-radius: 4px;
    cursor: pointer;
  }

  .ws-head__btn:hover:not(:disabled) {
    border-color: var(--color-text-muted);
  }

  .ws-head__btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .ws-head__stamp {
    color: var(--color-text-muted);
    font-size: var(--type-size-xs);
    font-family: var(--type-family-mono);
  }

  .ws-status {
    padding: var(--space-3);
    color: var(--color-text-secondary);
    font-size: var(--type-size-sm);
  }

  .ws-status--error {
    color: var(--color-signal-negative);
  }

  /* Auto-fit grid: 320px min tile keeps the 13" laptop honest;
     the wide WO tile spans 2 cols on a wider screen. On a 1920x1080
     wall TV with the sidebar present this yields ~4 columns. */
  .ws-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
    gap: var(--space-3);
  }

  .ws-tile {
    background: var(--color-surface-sunken);
    border: 1px solid var(--color-surface-divider);
    border-radius: 6px;
    padding: var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .ws-tile--wide {
    grid-column: span 2;
  }

  .ws-tile--tall {
    grid-row: span 2;
  }

  .ws-tile__head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .ws-tile__head h3 {
    margin: 0;
    font-size: var(--type-size-md);
    font-weight: 500;
    color: var(--color-text-strong);
  }

  .ws-tile__hint {
    color: var(--color-text-muted);
    font-size: var(--type-size-xs);
    font-family: var(--type-family-mono);
    margin-left: var(--space-1);
  }

  .ws-tile__link {
    background: transparent;
    color: var(--color-text-secondary);
    border: 0;
    padding: 0;
    font-family: var(--type-family-body);
    font-size: var(--type-size-xs);
    cursor: pointer;
  }

  .ws-tile__link:hover {
    color: var(--color-text-strong);
  }

  .ws-grid-inner {
    list-style: none;
    padding: 0;
    margin: 0;
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: var(--space-2);
  }

  .ws-grid-inner--narrow {
    grid-template-columns: repeat(3, 1fr);
  }

  .ws-qa-primary {
    list-style: none;
    padding: 0;
    margin: 0;
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: var(--space-2);
  }

  .ws-qa-secondary {
    margin: var(--space-2) 0 0 0;
    color: var(--color-text-muted);
    font-size: var(--type-size-xs);
  }

  .ws-qa-pair {
    margin-right: var(--space-1);
  }

  .ws-qa-sep {
    color: var(--color-text-muted);
    margin: 0 var(--space-1);
  }

  .ws-stat {
    background: var(--color-surface-raised);
    border: 1px solid var(--color-surface-divider);
    border-radius: 4px;
    padding: var(--space-2);
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    min-width: 0;
  }

  /* Stat buttons keep text alignment + colours of the static stat
     when they are not "clickable" — operator should not see a button
     chrome on a passive number. */
  .ws-stat__btn {
    background: transparent;
    border: 0;
    padding: 0;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .ws-stat__btn:hover .ws-stat__value {
    color: var(--color-text-strong);
  }

  .ws-stat__value {
    font-size: var(--type-size-xl);
    font-weight: 500;
    color: var(--color-text-strong);
    font-family: var(--type-family-mono);
  }

  .ws-stat__value--warn {
    color: var(--color-signal-warning);
  }

  .ws-stat__value--money {
    font-size: var(--type-size-lg);
  }

  .ws-stat__label {
    color: var(--color-text-secondary);
    font-size: var(--type-size-xs);
  }

  .ws-bignum {
    background: var(--color-surface-raised);
    border: 1px solid var(--color-surface-divider);
    border-radius: 4px;
    padding: var(--space-3);
    color: inherit;
    text-align: left;
    cursor: pointer;
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    font: inherit;
  }

  .ws-bignum:hover {
    border-color: var(--color-text-muted);
  }

  .ws-bignum__value {
    font-size: var(--type-size-xxl);
    font-weight: 500;
    color: var(--color-text-strong);
    font-family: var(--type-family-mono);
  }

  .ws-bignum--warn .ws-bignum__value {
    color: var(--color-signal-warning);
  }

  .ws-bignum__label {
    color: var(--color-text-secondary);
    font-size: var(--type-size-sm);
  }

  .ws-empty {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--type-size-sm);
    font-style: italic;
  }

  .ws-adapter-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .ws-adapter {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    background: var(--color-surface-raised);
    border: 1px solid var(--color-surface-divider);
    border-radius: 4px;
    padding: var(--space-2);
  }

  .ws-adapter__body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .ws-adapter__name {
    color: var(--color-text-strong);
    font-family: var(--type-family-mono);
    font-size: var(--type-size-sm);
  }

  .ws-adapter__meta {
    color: var(--color-text-muted);
    font-size: var(--type-size-xs);
  }

  .ws-dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    flex: 0 0 auto;
    background: var(--color-text-muted);
  }

  .ws-dot--positive {
    background: var(--color-signal-positive);
  }

  .ws-dot--muted {
    background: var(--color-text-muted);
  }

  .ws-pill {
    font-size: var(--type-size-xs);
    padding: 2px var(--space-2);
    border-radius: 999px;
    border: 1px solid var(--color-surface-divider);
  }

  .ws-pill--enabled {
    background: var(--color-surface-base);
    color: var(--color-signal-positive);
  }

  .ws-pill--disabled {
    background: var(--color-surface-base);
    color: var(--color-text-muted);
  }

  .ws-activity {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    max-height: 320px;
    overflow-y: auto;
  }

  .ws-activity__row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-2);
    border-bottom: 1px solid var(--color-surface-divider);
    font-size: var(--type-size-xs);
  }

  .ws-activity__kind {
    color: var(--color-text-strong);
    font-family: var(--type-family-mono);
  }

  .ws-activity__time {
    color: var(--color-text-muted);
  }

  /* Wall-TV bias: collapse the WO span on narrower screens so the
     tile reads as one column rather than spilling. */
  @media (max-width: 960px) {
    .ws-tile--wide {
      grid-column: span 1;
    }
    .ws-grid-inner {
      grid-template-columns: repeat(2, 1fr);
    }
  }
</style>
