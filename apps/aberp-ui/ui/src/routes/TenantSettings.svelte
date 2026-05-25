<script lang="ts">
  // PR-53 / session-73 — Tenant Settings page. Reads the persisted
  // seller.toml via GET /api/seller-info, lets the operator edit any
  // field, POSTs the updated body back via the existing
  // POST /api/setup-seller-info route (the wizard's write surface
  // already handles overwrite semantics).
  //
  // Mirrors `SellerConfigWizard.svelte`'s field shape exactly — same
  // composer + validator from `seller-config.ts`. The difference is
  // operator UX: the wizard is one-shot first-run; this page is
  // view-then-edit with the saved values pre-filled and a brief
  // "Saved" indicator on success (no navigation away).

  import { onMount } from "svelte";
  import { getSellerInfo, setupSellerInfo } from "../lib/api";
  import {
    composeSellerConfigBody,
    DEFAULT_SELLER_CONFIG_FORM,
    parseSetupSellerInfoErrorBody,
    validateSellerConfig,
    type SellerConfigForm,
  } from "../lib/seller-config";
  import { formFromSellerInfo } from "../lib/tenant-settings";

  let form: SellerConfigForm = $state({ ...DEFAULT_SELLER_CONFIG_FORM });
  let loading = $state(true);
  let loadError: string | null = $state(null);
  let submitting = $state(false);
  let submitError: string | null = $state(null);
  let saved = $state(false);
  let fieldErrors: Record<string, string> = $state({});

  let validation = $derived(validateSellerConfig(form));

  onMount(() => {
    void loadSellerInfo();
  });

  async function loadSellerInfo() {
    loading = true;
    loadError = null;
    try {
      const response = await getSellerInfo();
      form = formFromSellerInfo(response);
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : String(err);
      loadError = message;
    } finally {
      loading = false;
    }
  }

  async function onSubmit(event: Event) {
    event.preventDefault();
    submitError = null;
    fieldErrors = {};
    saved = false;
    if (!validation.ok) {
      return;
    }
    submitting = true;
    try {
      const body = composeSellerConfigBody(form);
      await setupSellerInfo(body);
      saved = true;
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : String(err);
      const typed = parseSetupSellerInfoErrorBody(message);
      if (typed !== null) {
        const next: Record<string, string> = {};
        for (const f of typed.fields) {
          next[f.field] = f.message;
        }
        fieldErrors = next;
        submitError = "Some fields need attention — see the inline messages.";
      } else {
        submitError = message;
      }
    } finally {
      submitting = false;
    }
  }

  function fieldError(name: string, clientSide: string | null): string | null {
    if (fieldErrors[name] !== undefined) {
      return fieldErrors[name];
    }
    return clientSide;
  }
</script>

<section class="page" aria-labelledby="page-title">
  <header class="page__head">
    <h2 id="page-title" class="page__title">Tenant settings</h2>
    <p class="page__lede">
      Seller identity persisted to <code>~/.aberp/&lt;tenant&gt;/seller.toml</code>.
      Edits land via the same atomic write the first-run wizard uses; the
      printed-invoice PDF + the NAV XML rebuild against the new values
      on the next invoice issued.
    </p>
  </header>

  {#if loading}
    <p class="page__muted">Loading current values…</p>
  {:else if loadError !== null}
    <div class="page__error" role="alert">
      <strong>Could not load seller info.</strong>
      <p class="page__error-detail">{loadError}</p>
    </div>
  {:else}
    <form onsubmit={onSubmit} class="page__form">
      <fieldset disabled={submitting} class="page__fieldset">
        <div class="page__columns">
          <section class="page__column">
            <h3 class="page__section">Identity</h3>

            <label class="field">
              <span class="field__label">Legal name</span>
              <input
                class="field__input"
                type="text"
                autocomplete="organization"
                bind:value={form.legalName}
                aria-invalid={fieldError("legalName", validation.legalName) !== null}
              />
              {#if fieldError("legalName", validation.legalName) !== null}
                <span class="field__error">{fieldError("legalName", validation.legalName)}</span>
              {/if}
            </label>

            <label class="field">
              <span class="field__label">
                Tax number (ADÓSZÁM)
                <span class="field__hint">format: <code>xxxxxxxx-y-zz</code></span>
              </span>
              <input
                class="field__input"
                type="text"
                autocomplete="off"
                spellcheck="false"
                bind:value={form.taxNumber}
                aria-invalid={fieldError("taxNumber", validation.taxNumber) !== null}
              />
              {#if fieldError("taxNumber", validation.taxNumber) !== null}
                <span class="field__error">{fieldError("taxNumber", validation.taxNumber)}</span>
              {/if}
            </label>

            <label class="field">
              <span class="field__label">
                EU VAT number
                <span class="field__hint">optional</span>
              </span>
              <input
                class="field__input"
                type="text"
                autocomplete="off"
                spellcheck="false"
                bind:value={form.euVatNumber}
              />
            </label>

            <h3 class="page__section">Address</h3>

            <label class="field">
              <span class="field__label">Country code</span>
              <input
                class="field__input"
                type="text"
                autocomplete="country"
                bind:value={form.addressCountryCode}
                aria-invalid={fieldError("addressCountryCode", validation.addressCountryCode) !== null}
              />
              {#if fieldError("addressCountryCode", validation.addressCountryCode) !== null}
                <span class="field__error">{fieldError("addressCountryCode", validation.addressCountryCode)}</span>
              {/if}
            </label>

            <label class="field">
              <span class="field__label">Postal code</span>
              <input
                class="field__input"
                type="text"
                autocomplete="postal-code"
                bind:value={form.addressPostalCode}
                aria-invalid={fieldError("addressPostalCode", validation.addressPostalCode) !== null}
              />
              {#if fieldError("addressPostalCode", validation.addressPostalCode) !== null}
                <span class="field__error">{fieldError("addressPostalCode", validation.addressPostalCode)}</span>
              {/if}
            </label>

            <label class="field">
              <span class="field__label">City</span>
              <input
                class="field__input"
                type="text"
                autocomplete="address-level2"
                bind:value={form.addressCity}
                aria-invalid={fieldError("addressCity", validation.addressCity) !== null}
              />
              {#if fieldError("addressCity", validation.addressCity) !== null}
                <span class="field__error">{fieldError("addressCity", validation.addressCity)}</span>
              {/if}
            </label>

            <label class="field">
              <span class="field__label">Street</span>
              <input
                class="field__input"
                type="text"
                autocomplete="street-address"
                bind:value={form.addressStreet}
                aria-invalid={fieldError("addressStreet", validation.addressStreet) !== null}
              />
              {#if fieldError("addressStreet", validation.addressStreet) !== null}
                <span class="field__error">{fieldError("addressStreet", validation.addressStreet)}</span>
              {/if}
            </label>
          </section>

          <section class="page__column">
            <h3 class="page__section">
              Bank info
              <span class="page__section-hint">printed-invoice footer</span>
            </h3>

            <label class="field">
              <span class="field__label">Bank account number</span>
              <input
                class="field__input"
                type="text"
                autocomplete="off"
                spellcheck="false"
                bind:value={form.bankAccountNumber}
              />
            </label>

            <label class="field">
              <span class="field__label">IBAN</span>
              <input
                class="field__input"
                type="text"
                autocomplete="off"
                spellcheck="false"
                bind:value={form.iban}
              />
            </label>

            <label class="field">
              <span class="field__label">Bank name</span>
              <input
                class="field__input"
                type="text"
                autocomplete="off"
                bind:value={form.bankName}
              />
            </label>

            <label class="field">
              <span class="field__label">SWIFT / BIC</span>
              <input
                class="field__input"
                type="text"
                autocomplete="off"
                spellcheck="false"
                bind:value={form.swiftBic}
              />
            </label>
          </section>
        </div>

        {#if submitError !== null}
          <div class="page__error" role="alert">
            <strong>Could not save seller info.</strong>
            <p class="page__error-detail">{submitError}</p>
          </div>
        {/if}

        {#if saved}
          <div class="page__saved" role="status">Saved.</div>
        {/if}

        <div class="page__actions">
          <button
            type="submit"
            class="page__submit"
            disabled={submitting || !validation.ok}
          >
            {submitting ? "Saving…" : "Save"}
          </button>
        </div>
      </fieldset>
    </form>
  {/if}
</section>

<style>
  .page {
    max-width: 960px;
    margin: 0 auto;
  }

  .page__head {
    margin-bottom: var(--space-4);
  }

  .page__title {
    margin: 0 0 var(--space-2) 0;
    font-size: var(--type-size-lg);
    font-weight: 600;
    color: var(--color-text-strong);
  }

  .page__lede {
    margin: 0;
    color: var(--color-text-secondary);
    font-size: var(--type-size-sm);
    line-height: 1.5;
  }

  .page__muted {
    color: var(--color-text-muted);
    font-size: var(--type-size-sm);
  }

  .page__form {
    display: contents;
  }

  .page__fieldset {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    border: 0;
    padding: 0;
    margin: 0;
  }

  .page__columns {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-5);
  }

  @media (max-width: 720px) {
    .page__columns {
      grid-template-columns: 1fr;
    }
  }

  .page__column {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .page__section {
    margin: var(--space-3) 0 0 0;
    font-size: var(--type-size-sm);
    font-weight: 600;
    color: var(--color-text-strong);
    border-bottom: 1px solid var(--color-surface-divider);
    padding-bottom: var(--space-1);
  }

  .page__section-hint {
    font-weight: 400;
    color: var(--color-text-muted);
    font-size: var(--type-size-xs);
    margin-left: var(--space-2);
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .field__label {
    font-size: var(--type-size-sm);
    color: var(--color-text-primary);
    font-weight: 500;
  }

  .field__hint {
    margin-left: var(--space-2);
    font-size: var(--type-size-xs);
    color: var(--color-text-muted);
    font-weight: 400;
  }

  .field__input {
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-surface-divider);
    border-radius: 4px;
    background: var(--color-surface-base, var(--color-surface-raised));
    color: var(--color-text-strong);
    font-family: var(--type-family-mono);
    font-size: var(--type-size-sm);
  }

  .field__input[aria-invalid="true"] {
    border-color: var(--color-signal-negative);
  }

  .field__error {
    font-size: var(--type-size-xs);
    color: var(--color-signal-negative);
  }

  code {
    font-family: var(--type-family-mono);
    color: var(--color-text-strong);
  }

  .page__error {
    padding: var(--space-2) var(--space-3);
    border-left: 3px solid var(--color-signal-negative);
    background: var(--color-surface-raised);
    font-size: var(--type-size-sm);
  }

  .page__error-detail {
    margin: var(--space-1) 0 0 0;
    font-family: var(--type-family-mono);
    font-size: var(--type-size-xs);
    color: var(--color-text-secondary);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .page__saved {
    padding: var(--space-2) var(--space-3);
    border-left: 3px solid var(--color-signal-positive);
    background: var(--color-surface-raised);
    color: var(--color-text-primary);
    font-size: var(--type-size-sm);
  }

  .page__actions {
    display: flex;
    justify-content: flex-end;
  }

  .page__submit {
    padding: var(--space-2) var(--space-5);
    background: var(--color-signal-positive, var(--color-text-strong));
    color: var(--color-surface-base, white);
    border: 0;
    border-radius: 4px;
    font-size: var(--type-size-sm);
    font-weight: 500;
    cursor: pointer;
  }

  .page__submit:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
</style>
