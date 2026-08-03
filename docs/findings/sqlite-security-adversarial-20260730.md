# Adversarial security review — adopting SQLite (WAL) as ABERP's transactional store

- **Date:** 2026-07-30
- **Scope:** the engine direction proposed by ADR-0107 / PR #47 (Option B).
- **Gate:** Ervin's — *proceed to a rollback-only DEV migration only if SQLite's
  security weaknesses are acceptable AND mitigable.*
- **Method:** read-only static review of `crates/`, `apps/`, `modules/` at `b7d5c61`.
  **No engine change, no migration, no schema change, nothing under `~/.aberp/**`
  touched.**

---

## VERDICT

> **SQLite's security posture is ACCEPTABLE-AND-MITIGABLE for the ABERP ledger.
> No unmitigable blocker was found across the eight surfaces. Against the DuckDB
> baseline the migration is a security *improvement* on five surfaces, an
> equivalence on two, and a *regression on one* — surface 6 (money typing), where
> SQLite's dynamic typing silently accepts a float where DuckDB's `DECIMAL`
> rejects it. That regression is real, it is triggered by a bind ABERP already
> performs today with **zero code change**, and it is fully closed by `STRICT`
> tables. It is a Phase-0 exit condition, not a blocker.**

Two findings are **newly introduced by the migration** and must be closed inside
Phase 0 before Phase 1 touches the invoice path:

- **F-6a** — money is bound to the DB as a *string* into a `DECIMAL` column. On
  SQLite that column takes NUMERIC affinity and the string is silently converted
  to an `f64`. Same code, same bind, different arithmetic. (§6)
- **F-1c** — 105 `ALTER TABLE … ADD COLUMN IF NOT EXISTS` sites have no SQLite
  equivalent and must become "inspect `pragma_table_info`, then build DDL". That
  converts 105 declarative idempotent statements into dynamically-constructed SQL
  with a fail-open shape. (§1)

Everything else is either already clean in ABERP or closed by a one-line pragma
plus a test.

### Required mitigation checklist

Every item is a **Phase-0 exit condition** and every one must be pinned by a
mutation-verified test — ADR-0107 §4.1's own rule: *a durability pragma that no
test can red is not configured*. The same rule is applied here to every security
pragma.

| # | Mitigation | Closes | Pin |
|---|---|---|---|
| **M1** | `STRICT` tables for every money, quantity, rate, and hash-chain column. Declared types restricted to `INTEGER` / `TEXT` / `BLOB`. No `DECIMAL(…)`, no `NUMERIC`, no `REAL` anywhere on a monetary path. | F-6a, F-6b, F-6c | A test that `INSERT`s a float into each money column and asserts `SQLITE_CONSTRAINT_DATATYPE`; plus `SELECT typeof(col)` = `'integer'`/`'text'` on every migrated row. |
| **M2** | Build with `SQLITE_OMIT_LOAD_EXTENSION`; never enable `rusqlite`'s `load_extension` feature; call `sqlite3_db_config(SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION, 0)` belt-and-braces at open. | §2 | A test asserting `SELECT load_extension('/tmp/x.dylib')` errors, **and** a cut-gate grep asserting the cargo feature is absent from every `Cargo.toml`. |
| **M3** | `sqlite3_limit(SQLITE_LIMIT_ATTACHED, 0)` at open. No `ATTACH` in the tree (0 today). | §3 | A test asserting `ATTACH DATABASE '/tmp/evil.db' AS e` errors; a cut-gate grep for the `ATTACH` token in `crates/ apps/ modules/`. |
| **M4** | `SQLITE_DBCONFIG_DEFENSIVE=1`, `SQLITE_DBCONFIG_ENABLE_TRIGGER=0`, `SQLITE_DBCONFIG_ENABLE_VIEW=0`, `PRAGMA trusted_schema=OFF`. ABERP has 0 triggers and 0 views today; turning them off makes that structural rather than incidental, and neutralises the malicious-schema class on a file the operator can replace. | §2, §3, §4 | A test asserting `CREATE TRIGGER`/`CREATE VIEW` are rejected on the live handle. |
| **M5** | **All read-modify-write transactions use `BEGIN IMMEDIATE`, never the default `BEGIN DEFERRED`.** Applies to the audit-chain append, the invoice-number allocator, and every `ON CONFLICT` upsert. | F-7a | A concurrency test with two connections interleaving read-head → append; must not produce two links off one `prev_hash`. |
| **M6** | **Keep the F-E whole-DB writer flock.** SQLite *permits* a second writing process where DuckDB corrupted under one; the flock stops being a corruption guard and becomes the guard for app-layer invariants that are not expressible in DDL. Do not retire it in Phase 3. | F-7b | The existing `db_writer_lock` refusal tests, re-pointed at the SQLite handle. |
| **M7** | `journal_mode=WAL`, `synchronous=FULL`, `fullfsync=1`, explicit `busy_timeout`, and **`shared_cache` explicitly OFF**. | §7 | Already required by ADR-0107 §4.1; add `shared_cache` to that pin list. |
| **M8** | The 105 `ADD COLUMN IF NOT EXISTS` rewrites take column name **and** declared type from a `const` table, never from a value, and **fail loud** when the expected column is absent after migration — no `unwrap_or_default()`, no silent skip. | F-1c | A test that seeds a pre-migration schema and asserts every expected column exists post-`ensure_schema`; the shape already exists at `modules/billing/tests/migration_pr73_old_schema.rs`. |
| **M9** | Chmod the tenant DB **and its `-wal` / `-shm` siblings** to `0600` and the tenant dir to `0700` on create, matching what `smtp_config.rs` / `tenant_registry.rs` already do for secrets. | §5 | A test asserting the mode of all three files after a fresh open. |
| **M10** | Pin `rusqlite` with **bundled** `libsqlite3-sys` (never the system library), floor the bundled SQLite at **≥ 3.51.3**, and add `libsqlite3-sys` to the `cargo-deny`/`cargo-audit` gate that already runs in CI. Do **not** add an ignore entry for it. | §8 | `cargo deny check` in CI (exists); a test asserting `sqlite3_libversion_number() >= 3051003`. |
| **M11** | Escape `%` / `_` / `\` in the two `LIKE` search patterns and add `ESCAPE '\'`; replace SQL `LOWER()` with Rust-side `to_lowercase()` on both sides of the comparison. SQLite's `LOWER()` is **ASCII-only** — Hungarian accented characters stop folding, which weakens the partner **duplicate-detection** guard, not just search. | §1, §4 | A test with `Árvíztűrő` / `ÁRVÍZTŰRŐ` asserting the dedup guard still matches, and one with a `%` needle asserting it does not over-match. |
| **M12** | Verify the bundled SQLite is **≥ 3.39** (`IS NOT DISTINCT FROM`, 8 sites) and audit the 21 `ON CONFLICT` sites for a matching `UNIQUE` index — SQLite's upsert requires an explicit conflict target that ADR-0019's "minimal DDL" posture may not have created. | §1 | Compile/run the query set against SQLite in Phase 0's leaf-port prototype. |

Nothing on this list is research. Every item is a pragma, a `const`, a chmod, or a
test.

---

## Baseline: what ABERP looks like today

Measured, not assumed:

| Probe | Count | Note |
|---|---:|---|
| `params!` call sites | 449 | all parameterized |
| `format!` producing a SQL string | **7** (+1 false positive) | every one interpolates a `const` column list or a `const` knob name |
| `push_str` SQL builders | 4 | static fragments only; values stay `?` |
| `ATTACH` | **0** | |
| `load_extension` / `INSTALL` / `sqlite_scanner` | **0** | |
| `CREATE TRIGGER` | **0** | |
| `CREATE VIEW` | **0** | |
| `GENERATED ALWAYS` | **0** | |
| `LIKE` | 2 | both parameterized; neither escapes metacharacters |
| `GLOB` | **0** | |
| `json_extract` / `json_each` / `$.` paths | **0** | |
| `ADD COLUMN IF NOT EXISTS` | **105** | ⚠ no SQLite equivalent |
| `ALTER COLUMN` | 5 | ⚠ no SQLite equivalent |
| `ON CONFLICT` | 21 | ⚠ needs an explicit unique target on SQLite |
| `IS NOT DISTINCT FROM` | 8 | needs SQLite ≥ 3.39 |

**The SQL-injection posture of this codebase is already good.** That is not luck —
it is `[[no-sql-specific]]` and ADR-0019's port traits. The findings below are
therefore almost entirely about *what the migration changes*, not about what is
broken today.

---

## 1. SQL injection

### Exposure today: **clean**

All 449 `params!` sites bind values. The seven `format!`-built statements
interpolate compile-time constants only:

- `apps/aberp/src/avl_vendors.rs:336,370`, `quoting_machines.rs:263`,
  `margin_profiles.rs:277`, `quality.rs:488,577` — a `const COLS: &str` column
  list into a `SELECT`; every value stays a `?`.
- `apps/aberp/src/quoting_tunables.rs:573` — `UPDATE quoting_parameters SET {col}
  = ? WHERE {col} IS NULL`, where `col` iterates a `&[(&str, f64)]` **literal
  array** declared four lines above. Not reachable from any request.
- `crates/audit-ledger/src/storage/mod.rs:411` — `ADD COLUMN {col}` over a const
  migration list.

The four `push_str` builders (`aberp-dispatch`, `aberp-qa` ×2,
`aberp-work-orders`) append static `" AND state = ?"` / `" ORDER BY … LIMIT ?
OFFSET ?"` fragments. No user string reaches a fragment.

No dynamic table names. No user-controlled `ORDER BY`. No JSON1 path
construction. `LOWER(name) LIKE ?` is the only pattern surface and it is bound.

### F-1a — `LIKE` metacharacters are unescaped — **LOW, pre-existing, equivalent**

`products.rs:402` and `partners.rs:1049` build `format!("{}%", needle.to_lowercase())`
and bind it. A needle containing `%` or `_` over-matches. This is wildcard
injection, not SQL injection: the operator can only over-match their **own**
tenant's rows, and the `tenant_id = ?` predicate is unaffected. Identical on
DuckDB. → **M11**.

### F-1b — `LOWER()` is ASCII-only on SQLite — **MEDIUM, new, mitigable**

Not a search-quality nit. `partners.rs:1001–1005` uses `LOWER(legal_name) =
LOWER(?)` plus four `LOWER(…) IS NOT DISTINCT FROM LOWER(?)` address predicates as
the **duplicate-partner detection guard**. DuckDB's `LOWER()` is Unicode-aware;
SQLite's built-in `LOWER()` folds ASCII only, unless the ICU extension is compiled
in — which **M2/M4 explicitly forbid**. On a Hungarian partner book (`Árvíztűrő
Kft.` vs `ÁRVÍZTŰRŐ KFT.`) the guard stops matching and duplicate legal entities
can be created, each accumulating its own invoice history. This is an integrity
regression with a clean fix (fold in Rust, compare the folded values). → **M11**.

### F-1c — 105 sites become dynamically-constructed DDL — **MEDIUM, new, mitigable**

This is the direct answer to *"does the migration risk introducing string-built SQL
where DuckDB's API differed?"* — **yes, in exactly one place, and it is large.**

SQLite has no `ALTER TABLE … ADD COLUMN IF NOT EXISTS` and no `ALTER COLUMN`. All
110 sites must become: query `pragma_table_info`, then conditionally issue
`ALTER TABLE {t} ADD COLUMN {c} {type}`. Three consequences:

1. **A new string-built-SQL surface of 110 sites**, replacing 110 declarative ones.
   Injection risk is nil *if* the identifiers come from `const` tables — they do
   today, and **M8** freezes that.
2. **A new fail-open surface, and it is D2a's exact shape.** A wrong
   `table_info` predicate means the column is silently not added; the next read
   `.unwrap_or_default()`s and a guard passes vacuously — the mechanism that let an
   exempt ÁFA base be re-filed to NAV at 0% (ADR-0107 §1.1 #3). The declarative
   form cannot fail this way. **M8's fail-loud post-condition is not optional.**
3. **It runs on every boot.** `ensure_schema` is on the startup path of every
   family, so a defect here is a first-boot-after-upgrade defect on the operator's
   machine.

Mitigable, and `modules/billing/tests/migration_pr73_old_schema.rs` already
demonstrates the right test shape. But this is the single largest *code-churn*
item in the migration after the decimal work, and ADR-0107 does not name it.
It belongs in §4.1's Phase-0 scope alongside the `DECIMAL` audit.

**Verdict: acceptable-with-mitigation (M8, M11, M12).**

---

## 2. Extension loading (`load_extension` → arbitrary dylib → RCE)

**Weakness:** `load_extension()` and `.load` map a caller-supplied path to
`dlopen`. Any path into that function is arbitrary code execution in the `serve`
process — which holds the NAV credentials, the CAD blob key, and the tenant DB.

**Exposure:** **zero today, and structurally closable.** No `load_extension`
anywhere in the tree. `rusqlite` gates the binding behind an opt-in `load_extension`
cargo feature, and the C API's `sqlite3_enable_load_extension` defaults off, so
loading requires *two* deliberate acts. The right posture is a third: compile the
capability out entirely.

**Note the direction of travel.** ADR-0107 §4 rec. 6 offers, as a future option,
"DuckDB can read a SQLite file directly through its `sqlite_scanner` extension."
That is an *extension-loading* design, on the analytics side. It does not
contradict M2 (it would run in a separate read-only process against a copy), but
it must never be implemented by enabling extensions inside `serve`. Recording it
so the ratchet is not quietly unwound in Phase 4.

**Mitigation: M2 + M4.** Compile-time off, runtime off, test-pinned, gate-grepped.

**vs DuckDB:** an **improvement**. DuckDB's extension mechanism (`INSTALL`/`LOAD`,
autoinstall/autoload of signed extensions from a remote repository) is on by
default in the bundled build and is a *network-reaching* code-load path. ABERP
does not use it, but it has never been switched off or pinned by a test either.
SQLite's can be compiled out; DuckDB's cannot.

**Verdict: acceptable-with-mitigation, and better than today.**

---

## 3. `ATTACH DATABASE`

**Weakness:** `ATTACH` opens or **creates** a file at an arbitrary path with the
process's privileges, and cross-database `INSERT … SELECT` then exfiltrates or
plants data. It also multiplies the transaction surface.

**Exposure:** **zero.** No `ATTACH` in `crates/`, `apps/`, `modules/`, or `tools/`.
No untrusted value reaches any SQL identifier or file path (§1), so there is no
route to construct one. The tenant DB path itself is already defended by
`apps/aberp/src/db_path_guard.rs` (whose `..`-before-`strip_prefix` fail-open was
closed at `59d6076`).

**Mitigation: M3.** `SQLITE_LIMIT_ATTACHED = 0` makes it structural rather than
merely absent — the same "make the rule shape-based, not name-based" lesson PR #43
paid for. Add the token to a cut-gate grep so a future `ATTACH` cannot land
unnoticed.

**vs DuckDB:** an **improvement** — DuckDB also has `ATTACH` (including
`ATTACH 'https://…'` and `ATTACH … (TYPE sqlite)`), and there is no
`SQLITE_LIMIT_ATTACHED`-equivalent to clamp it to zero.

**Verdict: acceptable-with-mitigation.**

---

## 4. Untrusted-input parsing

Three untrusted sources reach the DB. Traced individually.

**CAD uploads** (`apps/aberp/src/cad_blob.rs`, `quote_pricing_pipeline`) — bytes
land on the **filesystem** under `artifact_dir/<quote_id>/<filename>`, AES-256-GCM
encrypted with a per-tenant keychain key (S430 / ADR-0083). The DB stores metadata
and a path, never the geometry. The Python extractor's *derived* numbers
(`total_price_eur`) reach the DB as bound values. No CAD-derived string reaches an
identifier, a `format!`-built statement, a trigger (there are none), or a
generated column (there are none).

**Storefront payloads** (`crates/aberp-quote-intake/src/log_table.rs`) — the raw
JSON is stored as a bound value. Every one of the 25 functions in that module is
`params!`-bound. Values only.

**NAV response XML** (`apps/aberp/src/restore_from_nav_extract.rs`,
`restore_from_nav_outgoing.rs`, `ap_sync.rs`) — the base64 `invoiceData` payload
is extracted and **restored into invoice rows**. This is the most sensitive
untrusted→DB path in the product, and it is fully parameterized. `ap_sync.rs:808`
already truncates the response preview boundary-safely before storing it.

### F-4a — the NAV XML path is the *carrier* for the money-coercion defect — **HIGH when combined with §6**

Individually harmless; combined with F-6a it is the sharp end. NAV-supplied
numeric strings (`exchangeRate`, totals) are extracted from remote XML and bound
as **strings** into columns SQLite gives NUMERIC affinity. A remote party's text
therefore selects the storage class of a monetary value in the local ledger. With
**M1** in place the value is rejected or stored as declared; without it, it is
silently an `f64`. This is why M1 is a *security* mitigation and not a formatting
one.

### DoS surface

| Vector | Exposure |
|---|---|
| Recursive triggers | **None** — 0 triggers; **M4** makes it structural. |
| Generated columns | **None** — 0 in tree. |
| `randomblob` / `zeroblob` amplification | Requires attacker-controlled SQL; §1 shows there is none. |
| Deep/hostile JSON | JSON is stored as an opaque value and parsed in Rust (`serde`), never by SQL — 0 `json_extract`/`json_each`. This is *safer* than the SQL-side JSON parsing SQLite would otherwise offer. |
| Huge blobs | Capped app-side: `MAX_ATTACHMENT_BYTES` 20 MB, `MAX_ATTACHMENTS_PER_REQUEST` 5. SQLite's `SQLITE_MAX_LENGTH` default (1 GB) is a second, higher backstop. |
| Recursive CTE amplification | 0 `WITH RECURSIVE` (ADR-0107 §1.4) and none can be introduced by input. |

A single-operator desktop app where the "attacker" reaching SQL is a storefront
form or a NAV response — not an interactive SQL surface — makes query-level DoS
a low-value target. The real DoS is disk exhaustion, which is engine-independent.

**vs DuckDB:** **equivalent**, with a small SQLite edge (extensions/triggers/views
can be compiled or configured out; DuckDB's cannot).

**Verdict: acceptable-with-mitigation (M1, M4, M11).**

---

## 5. File-at-rest and side files

**The facts.**

- Today: one plaintext `aberp.duckdb` per tenant under `~/.aberp/<tenant>/`, plus
  the fsync'd audit mirror and the `.bak` preservation files.
- After: one plaintext `aberp.sqlite` **plus `-wal` and `-shm` siblings**. The
  `-shm` file must be readable and writable by every process that opens the DB —
  that is not optional in WAL mode.
- **No code in the tree chmods the tenant DB.** It is created at the process
  umask, typically `0644`. Meanwhile `smtp_config.rs`, `tenant_registry.rs`,
  `setup_seller_info.rs`, `quote_intake_config.rs`, `mes_adapters_config.rs` and
  `serve.rs:3225` all deliberately chmod their files `0600` and their dirs `0700`.
- **The DB is the only sensitive store in the product that is not encrypted at
  rest.** Customer CAD geometry *is* — AES-256-GCM, per-tenant keychain key, S430
  / ADR-0067 "blob AES-GCM at rest, audit on read". The DB holds partner bank
  accounts, tax numbers, contact PII, and every invoice.

### F-5a — file mode, not encryption, is the actual gap — **MEDIUM, pre-existing, cheap**

The migration widens the file set from one to three, and all three inherit the
umask. → **M9**. This is a five-line fix and should land regardless of the engine
decision.

### The SQLCipher call — **NO. Do not encrypt the ledger at rest.**

Not a hedge; here is the reasoning.

**For:** financial + PII data; the project has already accepted keychain-backed
encryption-at-rest once (CAD blobs); a laptop is a theft target; and if it is ever
going to happen, doing it during a migration that rewrites the file is by far the
cheapest moment.

**Against, and decisive:**

1. **It creates a total-loss failure mode on a legally-mandated record.** ADR-0009
   requires 8-year retention. A SQLCipher key lives in the OS keychain. Keychain
   loss — machine swap, OS reinstall, corrupted keychain, the Defense line's
   existing keychain-probe fragility — turns a recoverable 20 MB file into
   unrecoverable ciphertext. Compare the CAD-blob case: losing that key costs one
   customer's geometry, which is re-requestable. Losing the ledger key is a
   statutory-compliance event. **The asymmetry between the two decisions is
   correct, not an inconsistency.**
2. **It destroys the recovery posture the July record depended on.** The
   2026-07-19 boot refusal was survivable because a human could read a plaintext
   file and a plaintext mirror with a hand-written incident script. Every
   forensic tool in `crates/aberp-snapshot` and the mirror's torn-tail classifier
   assume readable bytes.
3. **It is a supply-chain and packaging regression.** SQLCipher is not part of the
   bundled `libsqlite3-sys` amalgamation; adopting it means linking a *system* or
   separately-vendored crypto library into a notarized Tauri bundle — directly
   against **M10**, and into the macOS codesigning instability memory already
   records (`[[feedback_cargo_codesign_destabilizes]]`).
4. **The threat it defends is already covered.** The realistic threat is a stolen
   or discarded machine. FileVault (full-disk, key in the Secure Enclave, operator
   already authenticates to it) covers that completely, with no key-loss cliff and
   no code. An attacker with live local user privileges defeats SQLCipher anyway —
   the key is in the keychain the same process just unlocked.

**Call: plaintext file + `0600` + FileVault as the documented control.** If
encryption-at-rest is later mandated (a customer contract, a certification), it
gets its own ADR with a key-escrow design — it does not ride an engine migration.

**vs DuckDB:** **equivalent** — both plaintext. The two extra side files are a
marginal widening, fully closed by M9. **Not a regression.**

**Verdict: acceptable equivalence, with M9 required.**

---

## 6. Money integrity as a security property — **the one regression**

### The weakness

SQLite has no decimal type and no static typing. Column affinity is derived by
**substring match on the declared type name**: a name containing `INT` →
INTEGER affinity; `CHAR`/`CLOB`/`TEXT` → TEXT; `BLOB` or empty → BLOB;
`REAL`/`FLOA`/`DOUB` → REAL; **anything else, including `DECIMAL(18,6)` →
NUMERIC.** NUMERIC affinity converts a bound TEXT value to INTEGER or REAL
whenever the conversion is "lossless and reversible" — which `'0.145'` → `REAL
0.145` satisfies.

And in a non-`STRICT` table, affinity is a *preference*, not a constraint: an
`INTEGER`-affinity column will happily store a `REAL` when the value is not
integral. **SQLite has no way to reject a float except `STRICT` or a `CHECK`.**

### F-6a — the coercion happens with zero code change — **HIGH, new, mitigable**

`modules/billing/src/adapters/duckdb_store.rs` declares:

```
exchange_rate        DECIMAL(18, 6)
huf_equivalent_total DECIMAL(18, 0)
quantity             DECIMAL(18, 6)
```

and binds them **as strings** (`:777`, `:877` — "Decimal-as-string bind"), reading
them back with `CAST(… AS VARCHAR)`. On DuckDB the string is parsed into an exact
fixed-point `DECIMAL`. On SQLite the *identical bind* against a NUMERIC-affinity
column yields an `f64`, and the `CAST(… AS VARCHAR)` read-back then returns the
float's decimal rendering. `exchange_rate = 0.145` becomes
`0.14499999999999999`; a `huf_equivalent_total` carried through a float loses
exactness at scale.

This is a security/integrity defect by the ADR's own Phase-0 standard — a silent
float coercion of a monetary value — and it is **the worst possible shape**: no
compile error, no runtime error, no test failure unless a test is written to look
for it, and it lands on the ÁFA/HUF-conversion path that feeds NAV. It is a
*direct* instance of CLAUDE.md rule 11.

**Mitigation: M1.** `STRICT` tables make it a hard `SQLITE_CONSTRAINT_DATATYPE`
error at write. Note that `STRICT` *forbids* `DECIMAL(…)` as a declared type — so
adopting it forces the representation decision rather than allowing it to be
deferred, which is precisely what is wanted.

### F-6b — the invoice path is already integer, the quoting path already is not — **MEDIUM, pre-existing, must be scoped honestly**

ADR-0107 §3 Option B cost 1 says "money is already minor-unit integers
(`read_invoice_total_gross_minor`)". **True for the invoice path, false for the
quoting path.** Measured:

- `unit_price_minor BIGINT` — integer minor units ✓
- `quote_pricing_jobs.total_price_eur DOUBLE`, Rust type `f64`
  (`quote_pricing_jobs.rs:232,264,660,1495`; `serve.rs:22233,22382`)
- `quote_intake/log_table.rs:146` — `total_price_eur DOUBLE`
- `material_inventory` — `on_hand_qty` / `reserved_qty` / `committed_qty` all
  `DOUBLE`

So **a monetary value already lives on a float today, on DuckDB.** SQLite does not
cause this and does not worsen it — it is an equivalence — but the ADR's
Phase-0 rule "money must never touch a float" is stated as if it holds today, and
it does not. Phase 0's `DECIMAL` audit must be a **`DECIMAL` *and* `DOUBLE`
audit**, or the migration will faithfully carry a float-money defect across and
`STRICT` will bless it (`total_price_eur REAL` is a perfectly valid STRICT column).

This does not change the verdict. It changes the Phase-0 scope, and per ADR-0107
§4 that scope is the thing that could double Option B's cost — so it should be
measured before the DEV migration is authorised.

### F-6c — the hash chain has a storage-class hazard — **MEDIUM, new, mitigable**

`audit_ledger` declares `prev_hash BLOB`, `binary_hash BLOB`, `entry_hash BLOB`,
`payload BLOB`. In SQLite, BLOB and TEXT are **different storage classes that never
compare equal** — `x'ab…' = 'ab…'` is always false, and BLOB sorts after TEXT.
`rusqlite` binds `&[u8]`/`Vec<u8>` as BLOB and `String`/`&str` as TEXT, so
correctness depends entirely on the Rust type at each of ~30 bind and compare
sites being right. One `&str` where a `&[u8]` belongs and a chain-link lookup
returns "not found" — which, on the read path that already produced
"NAV-acked invoice not found in audit ledger" (§1.1 #2), is a familiar and
expensive symptom. `STRICT` + `typeof()` assertions (**M1**) close it; without
them nothing does.

**vs DuckDB:** the **only surface where SQLite is genuinely worse.** DuckDB's
static typing rejects these classes at write. That is a real loss and it is worth
saying plainly. It is bought back by `STRICT`, which is a 2021 feature, not a
workaround.

**Verdict: acceptable-with-mitigation (M1) — and M1 is non-negotiable. Without
`STRICT`, F-6a alone would be a ship-blocker.**

---

## 7. Locking and the single-writer guarantee

### F-7a — SQLite's *safer* concurrency is a new footgun — **MEDIUM, new, mitigable**

The counter-intuitive result: SQLite weakens nothing about durability but is
**more permissive** about concurrency than DuckDB-plus-flock, and ABERP has
app-layer invariants that were being protected by that restrictiveness without
anyone having to name them.

The audit-chain append is a read-modify-write: read the head's `entry_hash`,
compute the new link, insert. So is the invoice-number allocator. SQLite's default
transaction is `BEGIN DEFERRED`, which takes a read lock and only upgrades to a
write lock at the first write. In WAL mode, if another writer committed in the
interval, the upgrade returns `SQLITE_BUSY_SNAPSHOT` and the transaction must be
rolled back — so a *correctly-structured* single transaction is safe. But
**`busy_timeout` makes the failure mode a silent retry rather than a visible
error**, and any read-then-write split across two transactions (which is exactly
the shape of the 33 catalogued read-forks) has no protection at all. The result
would be two chain links computed off one `prev_hash`: a ledger fork, from the
engine that was supposed to make forks impossible.

**Mitigation: M5.** `BEGIN IMMEDIATE` for every read-modify-write takes the write
lock up front. Cheap, standard, test-pinnable. It must be in Phase 1 with the
ledger, not in Phase 3 cleanup.

### F-7b — do not retire the F-E flock — **MEDIUM, new, mitigable**

`apps/aberp/src/db_writer_lock.rs` refuses a second `serve` on a tenant DB
(`fs2::try_lock_exclusive`, non-blocking, held for process lifetime). Under DuckDB
it prevents corruption. Under SQLite, two processes writing concurrently is
*correct and supported* — so the natural Phase-3 reading is "F-E is obsolete,
delete it."

**That reading is wrong.** F-E is also what guarantees a single monotonic invoice
allocator and a single chain head across processes, including the DB-mutating CLI
one-shots. SQLite serializes *transactions*; it does not serialize the
*application-level sequence* that ADR-0107 §4 rec. 2 identifies as the legally
binding invariant. Retiring F-E converts a whole class of app invariant from
"impossible" to "depends on M5 being right at every site."

**Mitigation: M6.** Keep it, and re-scope its doc comment from "corruption guard"
to "app-invariant guard" so Phase 3 does not delete it on the strength of the
old comment. ADR-0107 §2's retirement table should be amended: `db_writer_lock`
is **not** in the ~8 000 retired lines.

### Other footguns

- **`shared_cache` must be OFF** (SQLite's own docs deprecate it). It is off by
  default; **M7** pins it, because a shared cache reintroduces exactly the
  cross-connection interference class that consumed July.
- **`busy_timeout` must be explicit and finite.** Default 0 means immediate
  `SQLITE_BUSY`; an unbounded value hides a deadlock as a hang.
- **WAL requires a local filesystem** for the `-shm` mapping. Fine for a desktop
  app; worth a loud startup refusal if the tenant dir is on a network mount,
  since silent WAL misbehaviour on NFS/SMB is a classic data-loss report.

**vs DuckDB:** **improvement on durability, small regression on
permissiveness.** Net improvement, conditional on M5 + M6.

**Verdict: acceptable-with-mitigation (M5, M6, M7).**

---

## 8. Supply chain

**The mechanism already exists.** `deny.toml` + `audit.toml` run `cargo-deny` and
`cargo-audit` against RustSec on every push and PR (ADR-0007). Every current
ignore entry is an *unmaintained* warning reached through Tauri's GTK3 backend —
**zero CVE-class ignores.** That discipline is exactly what this surface needs.

**Bundled vs system: bundled, unambiguously.** `libsqlite3-sys`'s `bundled`
feature compiles a pinned amalgamation into the binary. A system library means the
security posture of a legally-binding ledger depends on whatever
`/usr/lib/libsqlite3.dylib` macOS shipped — unpinnable, unauditable, and different
on every operator machine. Bundling also keeps the notarized Tauri bundle
self-contained. This matches today's `duckdb = { features = ["bundled"] }` posture
exactly. → **M10**.

**RustSec does track the bundled C library.** `CVE-2022-35737` (array-bounds
overflow via a multi-gigabyte string argument) was published against
`libsqlite3-sys < 0.25.1`, not just against upstream SQLite. So the *existing* CI
gate covers the new dependency with no new machinery — a genuinely favourable
property, and one that does **not** hold today: `libduckdb-sys` has no comparable
advisory history to gate on.

### Known CVE classes and ABERP's exposure

| CVE | Class | ABERP exposure |
|---|---|---|
| **CVE-2025-6965** (fixed 3.50.2) | Integer truncation → memory corruption; requires the attacker to **inject arbitrary SQL**. Flagged as exploited in the wild in browser contexts. | **None** — §1 establishes there is no SQL-injection route. This is the general shape of SQLite's serious CVEs: they presuppose the injection primitive ABERP does not offer. |
| **CVE-2025-7709** (fixed 3.51.3) | Integer overflow in the **FTS5** extension | **None** — no FTS. Compiled out under M2/M10. |
| **CVE-2025-70873** (fixed 3.51.3) | **zipfile** extension discloses uninitialised heap | **None** — extension compiled out. |
| **CVE-2022-35737** (`libsqlite3-sys` < 0.25.1) | Array-bounds overflow on billions-of-bytes string arguments | **None** — inputs capped at 20 MB app-side. |
| **RUSTSEC-2021-0128** (rusqlite) | Closure-lifetime unsoundness in `create_scalar_function`, `create_aggregate_function`, `create_window_function`, `commit_hook`, `rollback_hook`, `update_hook` | **None if those APIs are never used.** They are all attractive during a migration (a custom collation to replace `LOWER()`, an `update_hook` to feed the audit ledger). **Do not use them** — M11 already routes case-folding through Rust instead. |
| **CVE-2020-35866** (rusqlite < 0.23) | `VTab`/`VTabCursor` memory safety | **None** — no virtual tables. |

**The honest comparison, which cuts against the intuitive reading.** SQLite has a
longer CVE list than DuckDB. That is a measure of *scrutiny*, not of weakness:
TH3's 100 % MC/DC coverage, `dbsqlfuzz`, and two decades of being the fuzzing
target in every browser and phone. DuckDB's near-empty CVE list reflects that
nobody is looking — and ADR-0107 §1.2 documents an upstream ART assertion crash on
ABERP's highest-frequency write path (~17 k rows/day) that shipped **without a
fix** and without a CVE. A crash-on-write in the storage engine of a financial
ledger is CVE-class behaviour in any project that assigns CVEs. **Counting CVEs
scores SQLite worse and the reality is the reverse.**

**vs DuckDB:** **improvement** — a smaller native surface (extensions compiled
out vs an autoloading extension repository), a real advisory history to gate on,
and a stable on-disk format instead of one-way storage upgrades on an 8-year
statutory record.

**Verdict: acceptable-with-mitigation (M10, plus "never use the hook/vtab APIs").**

---

## Summary against the DuckDB baseline

| # | Surface | Today (DuckDB) | After (SQLite + mitigations) | Δ |
|---|---|---|---|---|
| 1 | SQL injection | Clean | Clean; +110 const-driven DDL sites | ⚠ **equivalent, more code** |
| 2 | Extension loading | Present, on by default, unpinned, network-reaching | Compiled out, test-pinned | ✅ **improvement** |
| 3 | `ATTACH` | Present, unclampable | Limit 0, test-pinned | ✅ **improvement** |
| 4 | Untrusted input | Parameterized; 0 triggers/views | Same; triggers/views structurally off | ✅ **slight improvement** |
| 5 | File at rest | Plaintext, umask mode | Plaintext + 2 side files, `0600` | ➡️ **equivalent** (better with M9) |
| 6 | Money typing | Static `DECIMAL`, engine-rejected | Dynamic; `STRICT` required | ❌ **regression, closed by M1** |
| 7 | Locking | flock + fragile single-instance | flock + real MVCC + `BEGIN IMMEDIATE` | ✅ **improvement** |
| 8 | Supply chain | Bundled, no advisory history | Bundled, RustSec-gated, extensions off | ✅ **improvement** |

**Five improvements, two equivalences, one regression with a standard, complete,
test-pinnable fix.** ADR-0107's framing that the migration is a security
equivalence-or-improvement is **correct** — with the single honest caveat that
surface 6 is a real regression until `STRICT` lands, and that it is the surface
touching the NAV-filed money path.

---

## Deferral ledger (CLAUDE.md rule 3)

Found in scope, not fixed here — this review is analysis-only.

| Item | Closed by |
|---|---|
| Tenant DB file has no explicit `0600` — **true today, not migration-dependent** | M9, or a standalone 5-line PR now |
| `LIKE` metacharacters unescaped (2 sites) — true today | M11 |
| `total_price_eur` is `f64`/`DOUBLE` — money on a float **today** | Phase-0 audit, re-scoped to `DECIMAL` **and** `DOUBLE` |
| ADR-0107 §2 lists `db_writer_lock` machinery as retirable | Amend §2 / §4.1 Phase 3 per F-7b |
| ADR-0107 §3 Option B cost 1 states money is already integer | Amend per F-6b |
| ADR-0107 §4.1 Phase 0 does not scope the 110 `ADD COLUMN IF NOT EXISTS` / `ALTER COLUMN` rewrites | Amend per F-1c |
| ADR-0107 §4 rec. 6 proposes `sqlite_scanner` for future analytics | Must never be implemented in-`serve`; record against M2 |

---

## Gate answer

**Security does not block the rollback-only DEV migration.** No surface is
unmitigable. Proceeding is conditional on M1–M12 landing as Phase-0 exit
conditions — with **M1 (`STRICT`), M5 (`BEGIN IMMEDIATE`), and M6 (keep F-E)**
as the three that must not be deferred to a later phase, because each one guards
the invoice/NAV/audit-chain path that Phase 1 migrates first.

The remaining open question is **cost, not security**: F-6b and F-1c together
suggest Phase 0's audit is larger than ADR-0107 §4.1 scopes it. ADR-0107 §4 names
that scope as the thing that could change the recommendation, so it should be
measured before the DEV migration is authorised — which is exactly what Phase 0
is for, and it remains cheaply abandonable.
