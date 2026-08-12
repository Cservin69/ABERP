import { describe, expect, it } from "vitest";
// Vite's `?raw` query — the component sources as strings. See `src/app.d.ts`
// for the ambient declaration and why this beats `@types/node` + `readFileSync`.
import banner from "./DurabilityAlertBanner.svelte?raw";
import app from "../App.svelte?raw";

// ADR-0110 D7 — structural pins on the durability banner's markup contract.
//
// # Honest scope: these are SOURCE pins, not DOM renders
//
// This package has no jsdom and no @testing-library/svelte, and none of its 79
// test files mount a component — the convention here is pure logic in a `.ts`
// module plus a thin `.svelte` shell (CLAUDE.md rule 10). Adding a DOM stack
// for one banner would be a bigger change than the banner. The DECISION logic
// is genuinely unit-tested in `durability-alert.test.ts`; what is left is the
// shell, and these pin its contract by reading the source.
//
// That is weaker than a render, and the weakness is specific: they cannot prove
// the banner is VISIBLE or that the red is legible. Ervin eyeballs that on the
// next cut. What they DO catch is every regression that has a plausible motive:
//
//   * someone "harmonising" the deliberately off-palette red back into the
//     calm `--color-signal-*` tokens (the whole point is that it is not calm);
//   * someone dropping `role="alert"`, which is the only reason a screen-reader
//     operator hears it at all;
//   * someone adding a dismiss control, which would let the operator silence a
//     durability loss with a click;
//   * someone moving the mount inside `{#if viewMode === "ready"}`, which would
//     hide it in exactly the boot/setup states where a sick box gets poked at.

/** Just the `<style>` block — the header comment legitimately NAMES the token
 * it is overriding, and a check that could not tell the two apart would be a
 * check that punishes documenting the decision. */
const bannerStyle = banner.slice(banner.indexOf("<style>"));

describe("DurabilityAlertBanner.svelte", () => {
  it("is announced as an alert, assertively", () => {
    expect(banner).toContain('role="alert"');
    expect(banner).toContain('aria-live="assertive"');
  });

  it("is high-contrast red, NOT the calm signal token", () => {
    // ADR-0017's ambient palette is overridden here on purpose (Ervin,
    // 2026-08-12) — `--color-signal-negative` is a muted #c66060 designed to
    // sit quietly, and this element must not sit quietly.
    expect(bannerStyle).toMatch(/background:\s*#b3120f/i);
    expect(bannerStyle).toMatch(/color:\s*#ffffff/i);
    expect(bannerStyle).not.toContain("--color-signal-negative");
  });

  it("carries no dismiss control", () => {
    // There is nothing the operator could click here that would be true.
    expect(banner).not.toContain("<button");
    expect(banner).not.toContain("onDismiss");
  });

  it("renders the fixed headline and the backend's own detail line", () => {
    expect(banner).toContain("DURABILITY_BANNER_HEADLINE");
    expect(banner).toContain("durabilityBannerDetail(alert)");
  });

  it("exposes the breach code for support to read off the DOM", () => {
    expect(banner).toContain("data-breach={alert.breach}");
    expect(banner).toContain('data-testid="durability-alert-banner"');
  });
});

describe("App.svelte mounts the banner unconditionally", () => {
  it("renders it on `durabilityAlert` alone, with no viewMode gate", () => {
    const mount = app.match(
      /\{#if durabilityAlert\}[\s\S]{0,200}?<DurabilityAlertBanner[\s\S]{0,120}?\{\/if\}/,
    );
    expect(
      mount,
      "the banner must mount on `{#if durabilityAlert}` and nothing else",
    ).not.toBeNull();
    expect(mount?.[0]).not.toContain("viewMode");
  });

  it("sits above the topbar, inside the frame", () => {
    const frameStart = app.indexOf('<div class="frame">');
    const bannerAt = app.indexOf("<DurabilityAlertBanner");
    const topbarAt = app.indexOf('<header class="topbar">');
    expect(frameStart).toBeGreaterThan(-1);
    expect(bannerAt).toBeGreaterThan(frameStart);
    expect(
      bannerAt,
      "the alarm must be the first thing in the frame — above the topbar, not \
tucked under it",
    ).toBeLessThan(topbarAt);
  });

  it("derives its state from /health and holds none of its own", () => {
    // A `localStorage` flag or a local `$state` would be dismissible by
    // accident (a refresh, a second window), and an alarm the operator can
    // silence by accident is not an alarm.
    expect(app).toContain("durabilityAlertOf(healthInfo)");
  });
});
