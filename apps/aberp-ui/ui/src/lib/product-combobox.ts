// PR-100 — pure-module helper for the IssueInvoice line-item editor's
// product combobox. Mirrors the PR-74 buyer-combobox architecture:
// pure function over (needle, savedProducts) → (matches,
// shouldShowDropdown); the Svelte component owns the products-list
// fetch + keyboard nav + dropdown rendering; this module owns the
// "given a needle + the loaded list, what does the dropdown show"
// decision.
//
// The helper is intentionally pure (no Svelte runes, no DOM, no
// backend calls) so vitest can pin the pick-vs-type-through invariants
// without mounting a component or stubbing `invoke`.

import type { Product } from "./api";

/** PR-100 — derived view returned to the combobox renderer.
 *
 * `matches` is the saved-product subset whose `name` contains every
 * token of the (lowercased, whitespace-split) needle. Capped at
 * `maxMatches` so a wildcard prefix like "a" cannot blow the dropdown
 * up to the full catalog.
 *
 * `shouldShowDropdown` is `true` once the trimmed needle reaches
 * `minChars`. Distinct from `matches.length > 0` because we want to
 * surface a "no match — typed value will be used as a one-off line
 * description" hint when the operator types something that doesn't
 * match any saved product (rather than silently hiding the dropdown,
 * which would look broken — the same posture PR-74 pinned for the
 * buyer combobox). */
export interface ProductLineComboboxState {
  matches: Product[];
  shouldShowDropdown: boolean;
}

export interface ProductLineComboboxArgs {
  /** Current operator-typed text in the line's description input. */
  needle: string;
  /** Full saved-products list (loaded once on form mount). The combobox
   * filters client-side; no per-keystroke fetch. */
  savedProducts: Product[];
  /** Minimum trimmed needle length before the dropdown shows. Defaults
   * to 2 — product names tend to be shorter than partner display names
   * (single Hungarian words like `Gázolaj`, `Konzultáció`), so a more
   * responsive threshold than the buyer combobox's 3-char default. */
  minChars?: number;
  /** Maximum matches to surface in the dropdown. Defaults to 5 per the
   * PR-100 brief. */
  maxMatches?: number;
}

/** PR-100 — given the current input value + the loaded products list,
 * compute what the dropdown should show.
 *
 * **Matching rules** (exhaustively pinned in `product-combobox.test.ts`):
 *   - The needle is lowercased, trimmed, and split on whitespace into
 *     one or more tokens.
 *   - A product matches iff EVERY token is a (case-insensitive)
 *     substring of `product.name`. Multi-token AND (the operator
 *     typing "tan nap" matches `"Tanácsadói nap"` but not
 *     `"Tanácsadás (egyéb)"`). One-token-typeahead is the common case
 *     and just collapses to plain substring search.
 *   - Ranking: prefix matches beat internal-substring matches.
 *     `"Widget A"` outranks `"Mini-Widget B"` when the needle is
 *     `"wid"` because the name STARTS with the needle. Within the
 *     same tier, source order is preserved (stable sort).
 *   - Below `minChars`, the dropdown is hidden and `matches` is empty.
 *   - The match list is capped at `maxMatches`. */
export function productLineComboboxState(
  args: ProductLineComboboxArgs,
): ProductLineComboboxState {
  const minChars = args.minChars ?? 2;
  const maxMatches = args.maxMatches ?? 5;
  const trimmed = args.needle.trim();
  if (trimmed.length < minChars) {
    return { matches: [], shouldShowDropdown: false };
  }
  const tokens = trimmed.toLowerCase().split(/\s+/).filter((t) => t.length > 0);
  if (tokens.length === 0) {
    return { matches: [], shouldShowDropdown: false };
  }
  // Filter: every token must be a substring of the lowercased name.
  const filtered: Array<{ product: Product; tier: number; order: number }> = [];
  for (let i = 0; i < args.savedProducts.length; i += 1) {
    const product = args.savedProducts[i];
    const haystack = product.name.toLowerCase();
    let allHit = true;
    for (const token of tokens) {
      if (!haystack.includes(token)) {
        allHit = false;
        break;
      }
    }
    if (!allHit) continue;
    // Tier 0 = name starts with the first token (prefix); tier 1 =
    // internal substring. The single comparison is enough to capture
    // "prefix beats substring" — additional ranking refinements
    // (longest-prefix-wins, exact-match-bumps-to-top) are deliberately
    // not implemented: every extra rule needs a vitest pin and a
    // matching operator expectation. CLAUDE.md rule 2 — no speculative
    // sort heuristics.
    const tier = haystack.startsWith(tokens[0]) ? 0 : 1;
    filtered.push({ product, tier, order: i });
  }
  // Stable sort by tier ascending; ties preserve source order via the
  // captured index (Array.prototype.sort is stable in modern V8/JSC,
  // but the explicit `order` tie-breaker is defence-in-depth).
  filtered.sort((a, b) => (a.tier - b.tier) || (a.order - b.order));
  return {
    matches: filtered.slice(0, maxMatches).map((entry) => entry.product),
    shouldShowDropdown: true,
  };
}
