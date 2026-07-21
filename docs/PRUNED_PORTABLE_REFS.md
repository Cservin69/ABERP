# Pruned Portable refs — ABERP.git origin

**Date:** 2026-07-21
**Repo pruned:** `origin` = https://github.com/Cservin69/ABERP.git (this repo only)
**Authority:** [ADR-0093](../adr/0093-product-line-sawoff-isolation.md) (the saw-off
decision) as sequenced by **ABERP-Editions ADR-0100 §7 stage S3**.

---

## Read this first — what this document is and is not

**This is a record of an action taken. It is not a description of current state,
and nothing in it should be read as a decision made here.**

The decision that the Portable product line does not live in this repository was
taken in **ADR-0093 (Accepted 2026-06-23)**. This document does not make that
decision, restate it as an inference, or derive it from anything observable in
this repository. It records only that the refs listed below were deleted from
`origin` on 2026-07-21 in execution of that decision, and how to get them back.

**Portable lives in [`Cservin69/ABERP-Editions`](https://github.com/Cservin69/ABERP-Editions).**
That is true **by decision of ADR-0093**, not by observation of where files
happen to sit. If a future session finds Portable artefacts in this repository —
a launcher, a test, a ref, a version regex — that is **residue to be removed**,
not evidence that the line came back. Do not infer intent from state in this
repository; ADR-0093 is in `adr/` precisely so you never have to.

> **Why this warning exists.** Its sibling document,
> [`PRUNED_DEFENSE_REFS.md`](PRUNED_DEFENSE_REFS.md), recorded on 2026-07-11 an
> accurate observation — that a `~/ABERP-Portable` checkout was live on this
> repo's origin — in language ("VERIFIED LIVE … must not be [pruned]") that later
> sessions read as a standing decision to keep Portable here. It was never that.
> The misreading cost real work and real trust. The lesson is not "write more
> carefully"; it is **state the decision and its source, or say nothing about
> intent at all.**

---

## What was pruned

Six refs — three branches and three annotated tags — all deleted from `origin`
on 2026-07-21.

### Branches (3)

| Branch | Commit SHA |
|---|---|
| `PROD_Portable_v0.1.0` | `7b849f761cee9f90a0de03ec6e667517c31819f3` |
| `PROD_Portable_v0.1.1` | `9dbecb735162317cf0ca73d2cbf2f8568959d17a` |
| `PROD_Portable_v0.1.2` | `6a51d4ffafba03b123f7693f8b7fc27f8e9fce4a` |

### Tags (3, annotated)

| Tag | Annotated-tag object SHA | Target commit SHA |
|---|---|---|
| `PROD_Portable_v0.1.0` | `07d31599cfdf3265c5b191c96c77e40eecfb00dd` | `7b849f761cee9f90a0de03ec6e667517c31819f3` |
| `PROD_Portable_v0.1.1` | `059b498c8a66d641715112f8551a492a77540ef9` | `9dbecb735162317cf0ca73d2cbf2f8568959d17a` |
| `PROD_Portable_v0.1.2` | `e4de7dca1777b386099d10191da0632b56892bea` | `6a51d4ffafba03b123f7693f8b7fc27f8e9fce4a` |

The three same-named local tags in the shared dev checkout were deleted in the
same operation, so a stray `git push --tags` cannot resurrect them.

## Why this loses nothing

All three branch tips are **ancestors of `origin/main`** — the Portable cuts were
points on main's linear trunk, merely branched and tagged with Portable names.
Re-verified immediately before the deletion:

```
7b849f761cee9f90a0de03ec6e667517c31819f3 ancestor-of-origin/main: YES
9dbecb735162317cf0ca73d2cbf2f8568959d17a ancestor-of-origin/main: YES
6a51d4ffafba03b123f7693f8b7fc27f8e9fce4a ancestor-of-origin/main: YES
```

**Deleting these six refs orphans no commit and no tree.** Every line of Portable
history remains reachable from `main`; `git show 6a51d4f:run/run_portable.sh`
works in this repository today and will keep working.

The only genuinely GC-eligible objects were the three **annotated tag objects**
(`07d3159`, `059b498`, `e4de7dc`) — tagger metadata and a message, no content.
Those are what the archive preserves.

## Where the archive lives

The three annotated tag objects are mirrored, byte-identically, into
**`ABERP-Editions.git`** under:

```
refs/tags/archive/aberp-git/PROD_Portable_v0.1.0  -> 07d31599cfdf3265c5b191c96c77e40eecfb00dd
refs/tags/archive/aberp-git/PROD_Portable_v0.1.1  -> 059b498c8a66d641715112f8551a492a77540ef9
refs/tags/archive/aberp-git/PROD_Portable_v0.1.2  -> e4de7dca1777b386099d10191da0632b56892bea
```

The `refs/tags/archive/aberp-git/` namespace is deliberate and is reasoned out in
full in **ABERP-Editions ADR-0100 §3**. In short: `refs/tags/*` is the only
hierarchy besides `refs/heads/*` that git's default clone refspec transfers, so
the archive survives into every clone; and the `archive/aberp-git/` prefix fails
`upgrade_portable.sh`'s `VERSION_RE` and its `ls-remote --heads` existence check,
so the archived code can never be installed by an operator typing a release name.

**No branches were mirrored,** on purpose: `upgrade_portable.sh` resolves a
release from `origin/<version>` as a *branch*, so mirroring these tips as
branches would have made this repo's prod-line Portable code — including
`PORTABLE_HOME="${HOME}/.aberp/…"`, pointed at the **live HU prod data root** —
installable out of the Editions repo. That is the exact coupling the saw-off
exists to sever.

## How to retrieve

Read the archived code without restoring anything (from an `ABERP-Editions`
clone):

```sh
git show archive/aberp-git/PROD_Portable_v0.1.2:run/run_portable.sh
```

Or from this repository, where the commits are still ancestors of `main`:

```sh
git show 6a51d4ffafba03b123f7693f8b7fc27f8e9fce4a:run/run_portable.sh
```

Full restore of the pruned refs to this origin, if it is ever wanted (it should
not be — see the warning above):

```sh
git push origin \
  7b849f761cee9f90a0de03ec6e667517c31819f3:refs/heads/PROD_Portable_v0.1.0 \
  9dbecb735162317cf0ca73d2cbf2f8568959d17a:refs/heads/PROD_Portable_v0.1.1 \
  6a51d4ffafba03b123f7693f8b7fc27f8e9fce4a:refs/heads/PROD_Portable_v0.1.2
```

The **annotated tags** must be restored from the archive to preserve their
original object SHAs (re-tagging locally would mint new objects with new SHAs):

```sh
# in an ABERP-Editions clone, with a remote 'aberp' pointing at ABERP.git
git push aberp \
  archive/aberp-git/PROD_Portable_v0.1.0:refs/tags/PROD_Portable_v0.1.0 \
  archive/aberp-git/PROD_Portable_v0.1.1:refs/tags/PROD_Portable_v0.1.1 \
  archive/aberp-git/PROD_Portable_v0.1.2:refs/tags/PROD_Portable_v0.1.2
```

## Verification that authorised the prune

`tools/verify_ref_mirror.sh` (in `ABERP-Editions`) was run against a **fresh
`git clone`** — default refspec, no `--tags`, no `--mirror` — immediately before
the deletion. Pushing is not proof; surviving a clone is.

- **A. archive refs present in the fresh clone** — all three, `objecttype` = `tag`. PASS
- **B. per-tag assertions** — every tag-object SHA and every `^{commit}`
  identical to the ABERP.git original, for all three. PASS
- **C. not installable** — `ls-remote --exit-code --heads origin` returns **2**
  for the bare names and for the archive path. PASS
- **D. `VERSION_RE` gate** — rejects `archive/aberp-git/PROD_Portable_v0.1.2`. PASS
- **F. tagger metadata** — preserved verbatim in all three tag objects. PASS

**Check E reported FAIL, and that failure is the second precondition being met.**
Check E asserts "no Portable release branch anywhere on origin [Editions]". It
now matches `refs/heads/PROD_Portable_v1.0.0` — the first Editions Portable
release, cut at `234b598fa1e2` and install-proven (ABERP-Editions ADR-0100 §12).
The assertion was written during stage S2, when no Portable release was supposed
to exist in Editions yet; S5 deliberately made it false. Check E and the
precondition "Portable has a home to go to" **cannot both be green** — E going
red is what "cut" looks like. The script's aggregate `RESULT: FAILURES PRESENT`
was therefore not treated as a veto, and no assertion bearing on the archive
itself (A, B, C, D, F) failed. Re-baselining check E is Editions' work, not this
repository's; it is flagged in the residuals below.

## Residuals — Portable artefacts still in this repository

Recorded so no future session has to infer them from state:

- **`apps/aberp/tests/portable_demo_boot_e2e.rs` is still tracked here.** It is
  a Portable-only file (ABERP-Editions ADR-0100 §5, "delete") that stage S4a
  missed — the two launchers were deleted on 2026-07-21, this test was not.
  It should be deleted as S4a residue. Not done here: this stage is refs and
  documentation only.
- **`tools/verify_ref_mirror.sh` check E in `ABERP-Editions` is stale by
  design** (see above) and needs re-baselining against the existence of
  `PROD_Portable_v1.0.0`.

## Not pruned

`main`, and every `PROD_v*` branch and tag — the HU production line, which
**does** live in this repository and continues to release here on its own
cadence (ABERP-Editions ADR-0100 §8 corrects ADR-0093 §6, which wrongly called
prod frozen at `v2.27.76`). Nothing in this operation touched a prod ref.
