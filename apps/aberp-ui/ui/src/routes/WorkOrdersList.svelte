<script lang="ts">
  // S232 / PR-228 / ADR-0062 — Stage 3 Phase γ Work Orders v1 SPA
  // surface.
  //
  // ONE component does double duty: a list view with state-facet
  // chips + a per-row drill-down to a detail panel showing the WO,
  // its routing operations, the active BOM snapshot, and the
  // state-aware action buttons. Keeps the route surface narrow
  // (single `#/work-orders` slug) for v1 per the ADR-0062 §"SPA
  // surface" defaults; a future deep-linkable detail-by-id can split
  // when the operator needs it.
  //
  // Per CLAUDE.md rule 2 (simplicity first) + ADR-0062's "v1 is the
  // list + the action buttons" cap, this screen deliberately does
  // NOT have:
  //   - sort/filter columns beyond the state-facet chips
  //   - persistence (the inventory v1 didn't have it either; lands
  //     when a real operator survey demands it)
  //   - a Gantt / shop-floor board
  //   - per-routing-op start/complete buttons (per-op cascade is
  //     auto per ADR-0062 §2; v1 surfaces the state)

  import { onMount } from "svelte";
  import {
    createWorkOrder,
    getWorkOrder,
    listWorkOrders,
    transitionWorkOrder,
    listProducts,
    type Product,
    type WorkOrder,
    type WorkOrderDetailResponse,
    type WorkOrderState,
    type WoAction,
  } from "../lib/api";

  const STATE_FACETS: { state: WorkOrderState | null; hu: string; en: string }[] =
    [
      { state: null, hu: "Mind", en: "All" },
      { state: "created", hu: "Létrehozva", en: "Created" },
      { state: "released", hu: "Kiadva", en: "Released" },
      { state: "in_progress", hu: "Folyamatban", en: "In progress" },
      { state: "on_hold", hu: "Várakozik", en: "On hold" },
      { state: "completed", hu: "Kész", en: "Completed" },
      { state: "cancelled", hu: "Megszakítva", en: "Cancelled" },
    ];

  let rows: WorkOrder[] = $state([]);
  let products: Product[] = $state([]);
  let loadState: "loading" | "loaded" | "error" = $state("loading");
  let loadError: string | null = $state(null);
  let selectedState: WorkOrderState | null = $state(null);

  let detail: WorkOrderDetailResponse | null = $state(null);
  let detailError: string | null = $state(null);
  let detailLoading = $state(false);
  let actionError: string | null = $state(null);
  let warningsToShow: string[] = $state([]);

  // Create-WO modal state.
  let showCreateForm = $state(false);
  let formWoNumber = $state("");
  let formProductId = $state("");
  let formQtyTarget = $state("1");
  let formNotes = $state("");
  let formOps: { op_name: string; est_time_min: string; est_cost_huf: string }[] =
    $state([{ op_name: "", est_time_min: "", est_cost_huf: "" }]);
  let createError: string | null = $state(null);
  let creating = $state(false);

  async function refresh(): Promise<void> {
    loadState = "loading";
    try {
      rows = await listWorkOrders(selectedState);
      if (products.length === 0) {
        products = await listProducts();
      }
      loadState = "loaded";
      loadError = null;
    } catch (e) {
      loadState = "error";
      loadError = String(e);
    }
  }

  async function openDetail(woId: string): Promise<void> {
    detailLoading = true;
    detailError = null;
    actionError = null;
    warningsToShow = [];
    try {
      detail = await getWorkOrder(woId);
    } catch (e) {
      detailError = String(e);
      detail = null;
    } finally {
      detailLoading = false;
    }
  }

  function closeDetail(): void {
    detail = null;
    detailError = null;
    actionError = null;
    warningsToShow = [];
  }

  function mintIdempotencyKey(prefix: string): string {
    if (
      typeof globalThis !== "undefined" &&
      globalThis.crypto?.randomUUID
    ) {
      return `${prefix}-${globalThis.crypto.randomUUID()}`;
    }
    return `${prefix}-${Date.now().toString(36)}-${Math.random()
      .toString(36)
      .slice(2, 10)}`;
  }

  async function submitTransition(action: WoAction): Promise<void> {
    if (detail === null) return;
    actionError = null;
    warningsToShow = [];
    const reason =
      action === "cancel" || action === "hold"
        ? window.prompt(
            action === "hold"
              ? "Okot adj meg (kötelező OnHold-hoz) / Reason (required for Hold)"
              : "Megszakítás oka? / Cancellation reason?",
          )
        : null;
    if (action === "hold" && (reason === null || reason.trim() === "")) {
      actionError = "Hold requires a reason";
      return;
    }
    if (reason === null && action === "cancel") {
      // Operator pressed Cancel on the prompt — treat as abort.
      return;
    }
    try {
      const resp = await transitionWorkOrder(detail.work_order.wo_id, {
        action,
        reason: reason,
        idempotency_key: mintIdempotencyKey(`${action}-${detail.work_order.wo_id}`),
      });
      if (resp.warnings && resp.warnings.length > 0) {
        warningsToShow = resp.warnings;
      }
      // Refresh the detail + the list to reflect the new state.
      await openDetail(detail.work_order.wo_id);
      await refresh();
    } catch (e) {
      actionError = String(e);
    }
  }

  function addOpRow(): void {
    formOps = [...formOps, { op_name: "", est_time_min: "", est_cost_huf: "" }];
  }

  function removeOpRow(idx: number): void {
    formOps = formOps.filter((_, i) => i !== idx);
    if (formOps.length === 0) addOpRow();
  }

  async function submitCreate(): Promise<void> {
    createError = null;
    if (formWoNumber.trim() === "") {
      createError = "WO number required";
      return;
    }
    if (formProductId === "") {
      createError = "Product required";
      return;
    }
    if (formOps.some((o) => o.op_name.trim() === "")) {
      createError = "Every routing op needs a name";
      return;
    }
    creating = true;
    try {
      const body = {
        wo_number: formWoNumber.trim(),
        product_id: formProductId,
        qty_target: formQtyTarget.trim(),
        notes: formNotes.trim() === "" ? null : formNotes.trim(),
        routing_ops: formOps.map((o) => ({
          op_name: o.op_name.trim(),
          est_time_min:
            o.est_time_min.trim() === "" ? null : Number(o.est_time_min.trim()),
          est_cost_huf:
            o.est_cost_huf.trim() === "" ? null : o.est_cost_huf.trim(),
        })),
        idempotency_key: mintIdempotencyKey("create"),
      };
      await createWorkOrder(body);
      // Reset form state + close.
      showCreateForm = false;
      formWoNumber = "";
      formProductId = "";
      formQtyTarget = "1";
      formNotes = "";
      formOps = [{ op_name: "", est_time_min: "", est_cost_huf: "" }];
      await refresh();
    } catch (e) {
      createError = String(e);
    } finally {
      creating = false;
    }
  }

  function setStateFilter(s: WorkOrderState | null): void {
    selectedState = s;
    refresh();
  }

  function actionsForState(s: WorkOrderState): WoAction[] {
    // Mirror of ADR-0062 §2 transition table. The buttons render only
    // for actions whose `from` state is the current state.
    switch (s) {
      case "created":
        return ["release", "cancel"];
      case "released":
        return ["start", "hold", "cancel"];
      case "in_progress":
        return ["complete", "hold", "cancel"];
      case "on_hold":
        return ["resume", "cancel"];
      case "completed":
      case "cancelled":
        return [];
    }
  }

  function productName(id: string): string {
    const p = products.find((p) => p.id === id);
    return p?.name ?? id;
  }

  onMount(refresh);
</script>

<section class="wo-page" aria-labelledby="wo-title">
  <header class="wo-head">
    <h2 id="wo-title">Gyártási rendelések / Work orders</h2>
    <div class="wo-head-actions">
      <button type="button" onclick={refresh}>Frissítés / Refresh</button>
      <button type="button" onclick={() => (showCreateForm = true)}>
        + Új gyártási rendelés / New work order
      </button>
    </div>
  </header>

  <div class="wo-facets" role="tablist" aria-label="State filter">
    {#each STATE_FACETS as f}
      <button
        type="button"
        class="wo-facet"
        class:wo-facet--active={selectedState === f.state}
        onclick={() => setStateFilter(f.state)}
      >
        <span class="wo-facet__hu">{f.hu}</span>
        <span class="wo-facet__en">{f.en}</span>
      </button>
    {/each}
  </div>

  {#if loadState === "loading"}
    <p>Loading…</p>
  {:else if loadState === "error"}
    <p class="wo-error">Error: {loadError}</p>
  {:else if rows.length === 0}
    <p class="wo-empty">No work orders yet.</p>
  {:else}
    <table class="wo-table">
      <thead>
        <tr>
          <th>WO #</th>
          <th>Termék / Product</th>
          <th>Mennyiség / Qty</th>
          <th>Állapot / State</th>
          <th>Létrehozva / Created</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each rows as row}
          <tr>
            <td>{row.wo_number}</td>
            <td>{productName(row.product_id)}</td>
            <td>{row.qty_target}</td>
            <td>{row.state}</td>
            <td>{row.created_at}</td>
            <td>
              <button type="button" onclick={() => openDetail(row.wo_id)}>
                Részletek / Open
              </button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}

  {#if detail !== null}
    <aside class="wo-detail" aria-labelledby="wo-detail-title">
      <header>
        <h3 id="wo-detail-title">
          {detail.work_order.wo_number} — {detail.work_order.state}
        </h3>
        <button type="button" onclick={closeDetail}>Bezár / Close</button>
      </header>
      <dl>
        <dt>Termék / Product</dt>
        <dd>{productName(detail.work_order.product_id)}</dd>
        <dt>Mennyiség / Qty</dt>
        <dd>{detail.work_order.qty_target}</dd>
        <dt>Megjegyzés / Notes</dt>
        <dd>{detail.work_order.notes ?? "—"}</dd>
        {#if detail.work_order.hold_reason !== null}
          <dt>Hold ok / reason</dt>
          <dd>{detail.work_order.hold_reason}</dd>
        {/if}
      </dl>

      {#if actionError !== null}
        <p class="wo-error">Action failed: {actionError}</p>
      {/if}
      {#if warningsToShow.length > 0}
        <ul class="wo-warnings">
          {#each warningsToShow as w}
            <li>{w}</li>
          {/each}
        </ul>
      {/if}

      <div class="wo-actions">
        {#each actionsForState(detail.work_order.state) as a}
          <button type="button" onclick={() => submitTransition(a)}>
            {a}
          </button>
        {/each}
      </div>

      <section class="wo-routing">
        <h4>Műveletek / Routing operations</h4>
        <table>
          <thead>
            <tr><th>#</th><th>Név / Name</th><th>Idő (perc)</th><th>Költség (HUF)</th><th>Állapot / State</th></tr>
          </thead>
          <tbody>
            {#each detail.routing_ops as op}
              <tr>
                <td>{op.sequence}</td>
                <td>{op.op_name}</td>
                <td>{op.est_time_min ?? "—"}</td>
                <td>{op.est_cost_huf ?? "—"}</td>
                <td>{op.state}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </section>

      <section class="wo-bom">
        <h4>BOM (aktív / active)</h4>
        {#if detail.bom.length === 0}
          <p>No active BOM for this product.</p>
        {:else}
          <table>
            <thead><tr><th>Component</th><th>Qty / unit</th></tr></thead>
            <tbody>
              {#each detail.bom as line}
                <tr>
                  <td>{productName(line.component_id)}</td>
                  <td>{line.qty_per_unit}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>
    </aside>
  {:else if detailLoading}
    <p>Loading detail…</p>
  {:else if detailError !== null}
    <p class="wo-error">Detail load failed: {detailError}</p>
  {/if}

  {#if showCreateForm}
    <div class="wo-modal" role="dialog" aria-labelledby="wo-create-title">
      <div class="wo-modal__body">
        <h3 id="wo-create-title">Új gyártási rendelés / New work order</h3>
        <label>
          WO szám / number
          <input type="text" bind:value={formWoNumber} />
        </label>
        <label>
          Termék / Product
          <select bind:value={formProductId}>
            <option value="">— select —</option>
            {#each products as p}
              <option value={p.id}>{p.name}</option>
            {/each}
          </select>
        </label>
        <label>
          Mennyiség / Qty
          <input type="text" bind:value={formQtyTarget} />
        </label>
        <label>
          Megjegyzés / Notes (optional)
          <textarea bind:value={formNotes}></textarea>
        </label>
        <h4>Műveletek / Routing ops</h4>
        {#each formOps as op, i}
          <div class="wo-op-row">
            <input
              type="text"
              placeholder="Op name"
              bind:value={op.op_name}
            />
            <input
              type="text"
              placeholder="Time (min)"
              bind:value={op.est_time_min}
            />
            <input
              type="text"
              placeholder="Cost (HUF)"
              bind:value={op.est_cost_huf}
            />
            <button type="button" onclick={() => removeOpRow(i)}>×</button>
          </div>
        {/each}
        <button type="button" onclick={addOpRow}>+ add op</button>
        {#if createError !== null}
          <p class="wo-error">{createError}</p>
        {/if}
        <div class="wo-modal__actions">
          <button
            type="button"
            onclick={() => (showCreateForm = false)}
            disabled={creating}
          >
            Mégse / Cancel
          </button>
          <button type="button" onclick={submitCreate} disabled={creating}>
            {creating ? "Saving…" : "Mentés / Save"}
          </button>
        </div>
      </div>
    </div>
  {/if}
</section>

<style>
  .wo-page {
    padding: 1rem;
  }
  .wo-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.5rem;
  }
  .wo-head-actions button + button {
    margin-left: 0.5rem;
  }
  .wo-facets {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
    margin-bottom: 0.75rem;
  }
  .wo-facet {
    padding: 0.25rem 0.5rem;
    border: 1px solid #ccc;
    background: #fafafa;
    cursor: pointer;
  }
  .wo-facet--active {
    background: #e8f4ff;
    border-color: #2b6cb0;
  }
  .wo-facet__hu {
    display: block;
    font-weight: 600;
  }
  .wo-facet__en {
    display: block;
    font-size: 0.75rem;
    color: #666;
  }
  .wo-table {
    width: 100%;
    border-collapse: collapse;
  }
  .wo-table th,
  .wo-table td {
    text-align: left;
    padding: 0.25rem 0.5rem;
    border-bottom: 1px solid #eee;
  }
  .wo-error {
    color: #c00;
  }
  .wo-empty {
    color: #666;
  }
  .wo-detail {
    margin-top: 1rem;
    padding: 1rem;
    border: 1px solid #ddd;
    background: #fcfcfc;
  }
  .wo-detail header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }
  .wo-detail dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25rem 1rem;
  }
  .wo-detail dt {
    color: #666;
  }
  .wo-actions {
    margin: 0.5rem 0;
    display: flex;
    gap: 0.5rem;
  }
  .wo-warnings {
    background: #fff4e5;
    border: 1px solid #f0c14b;
    padding: 0.5rem;
    margin: 0.5rem 0;
  }
  .wo-routing,
  .wo-bom {
    margin-top: 1rem;
  }
  .wo-routing table,
  .wo-bom table {
    width: 100%;
    border-collapse: collapse;
  }
  .wo-routing th,
  .wo-routing td,
  .wo-bom th,
  .wo-bom td {
    padding: 0.25rem 0.5rem;
    border-bottom: 1px solid #eee;
    text-align: left;
  }
  .wo-modal {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .wo-modal__body {
    background: #fff;
    padding: 1.5rem;
    border-radius: 4px;
    max-width: 600px;
    width: 90%;
    max-height: 80vh;
    overflow-y: auto;
  }
  .wo-modal__body label {
    display: block;
    margin-bottom: 0.5rem;
  }
  .wo-modal__body label input,
  .wo-modal__body label select,
  .wo-modal__body label textarea {
    display: block;
    width: 100%;
  }
  .wo-op-row {
    display: flex;
    gap: 0.25rem;
    margin-bottom: 0.25rem;
  }
  .wo-op-row input {
    flex: 1;
  }
  .wo-modal__actions {
    margin-top: 1rem;
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
  }
</style>
