# Pruned Defense refs — ABERP.git origin

**Date:** 2026-07-11
**Repo pruned:** `origin` = https://github.com/Cservin69/ABERP.git (this repo only)
**Reason:** The live Defense line moved to a separate repo, **ABERP-Editions.git**
(dev checkout `~/Documents/Claude/Projects/ABERP-Editions`, runs from `~/ABERP-Defense`,
live tip `PROD_Defense_v0.2.11`). The `PROD_Defense_v0.1.x`–`v0.2.1` refs listed below
are the **abandoned pre-saw-off** Defense cuts left behind on ABERP.git. They are dead
and confusing — a prior session aimed durability work at this wrong base. They are pruned
here so that cannot recur.

## Safety verification (performed before deletion)

- `~/ABERP-Defense` origin = **ABERP-Editions.git** (confirmed) — the live Defense is NOT
  on ABERP.git. Nothing live is touched here.
- None of the pruned refs is `main`, `PROD_v2.27.76`, `PROD_v2.28.0`, or any `PROD_v*`
  prod ref. They are distinctly `PROD_Defense_*`-named.
- **Every** pruned commit SHA is an **ancestor of `origin/main`** (the Defense cuts were
  made on main's trunk and merely tagged/branched with Defense names). Deleting these
  aliases orphans **no commits** — all remain reachable from `main`. This prune is
  therefore zero-data-loss and fully reversible.

## Recovery

Each ref is recorded below with its full commit SHA. To restore any branch:

```
git push origin <full-sha>:refs/heads/<branch-name>
```

To restore any lightweight-equivalent tag at the same commit:

```
git tag <tag-name> <full-sha> && git push origin <tag-name>
```

(The original tags were annotated objects; the annotated-tag object SHAs are also
recorded below for completeness, though the commit they point to is what matters.)

## Pruned branches (5)

| Branch | Commit SHA | Note |
|---|---|---|
| `PROD_Defense_v0.1.0` | `071b7ed746a8ccb31f34cc8bad2116b9f348a38b` | 1st abandoned Defense cut; ancestor of main |
| `PROD_Defense_v0.1.1` | `11f050c555b8325bf5c6c826bc59962555ff14fa` | ancestor of main |
| `PROD_Defense_v0.1.2` | `2f9227a7bf13f056ba3a011f2d48d57ddbc07686` | ancestor of main |
| `PROD_Defense_v0.2.0` | `f9ab44e3167c3ef29066205ec5c763dea8f54103` | ancestor of main |
| `PROD_Defense_v0.2.1` | `ba69566485258e440438dd08c2846db89a70ba0a` | abandoned Defense tip; ancestor of main |

## Pruned tags (5)

| Tag | Commit SHA (target) | Annotated-tag object SHA |
|---|---|---|
| `PROD_Defense_v0.1.0` | `071b7ed746a8ccb31f34cc8bad2116b9f348a38b` | `60a96247bf2372c4d26575261ff5f5bce92e93ef` |
| `PROD_Defense_v0.1.1` | `11f050c555b8325bf5c6c826bc59962555ff14fa` | `3dfdf562ccafd5711e3f832a89d9bd58fd64a621` |
| `PROD_Defense_v0.1.2` | `2f9227a7bf13f056ba3a011f2d48d57ddbc07686` | `8d8132fa91ca124ce67a3c9fc94ef07cfb1e0e6e` |
| `PROD_Defense_v0.2.0` | `f9ab44e3167c3ef29066205ec5c763dea8f54103` | `6eafea261c6781ddc388c8e830e436eafd3d9e1b` |
| `PROD_Defense_v0.2.1` | `ba69566485258e440438dd08c2846db89a70ba0a` | `a60a6cd9d99c8483693665bb2d740bb0668f9909` |

## NOT pruned (deliberately preserved)

- **All prod refs**: `main`, and `PROD_v1.4.1` … `PROD_v2.28.0` branches + tags. Untouched.
- **Portable line** (`PROD_Portable_v0.1.0/0.1.1/0.1.2`, branches + tags): **VERIFIED LIVE on
  ABERP.git — NOT pruned, and must not be.** `~/ABERP-Portable`'s origin is ABERP.git (not
  Editions), it is checked out at `PROD_Portable_v0.1.2` (`6a51d4f`), and ABERP-Editions.git
  carries **no** Portable refs. So the Portable upgrade/relaunch path depends on these refs.
  Unlike Defense (which moved to Editions), the Portable line still lives on ABERP.git — these
  are permanent refs, not dead aliases. (Their commits are also ancestors of `main`, but the
  live-dependency is the reason they stay.)
- **Local Defense tags**: the 5 `PROD_Defense_v0.1.0`–`v0.2.1` tags still exist in the local
  clone (scope was origin refs only). They can be dropped with
  `git tag -d PROD_Defense_v0.1.0 …` in a follow-up if desired.
