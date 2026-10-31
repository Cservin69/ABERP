# ADR-0111 — Second-workstation remote client over the Thunderbolt Bridge

- **Status:** Proposed — implemented on branch; builds, clippy-clean and unit-tested on the M5 (2026-10-31). Not yet run end to end over the bridge. Retry safety over the link depends on ADR-0112.
- **Date:** 2026-10-31
- **Deciders:** Ervin
- **Amends:** ADR-0100 Phase 1 invariant "nothing binds beyond loopback" —
  for the desktop build only, behind an explicit opt-in. `transport_bind_host()`
  stays `127.0.0.1`; this ADR adds a *second* listener, it does not widen the
  first. ADR-0007 threat model — adds one adjacent-network surface (one cable,
  one peer).
- **Related:** ADR-0002 (DB-per-tenant), ADR-0004 (UI never touches storage; one
  wire protocol for every client), ADR-0007 (security baseline), ADR-0008 /
  ADR-0030 (audit chain + mirror), ADR-0009 / ADR-0045 (gap-free numbering),
  ADR-0016 (one side authoritative, the other a projection), ADR-0021 Part B
  (HTTPS+JSON), ADR-0099 H3 (single writer per tenant DB), ADR-0100 (SaaS).

## Context

Ervin has a second machine (an M5 Mac Studio) wired to the MacBook by a
Thunderbolt cable (macOS "Thunderbolt Bridge", `bridge0`). He wants to issue
invoices from either machine. The production DB stays on the MacBook
(decision 2026-10-31).

Copying or syncing the DB is out: two writers fork the gap-free per-series
numbering (Áfa tv. §169, ADR-0009 §3) and the hash-chained audit ledger
(ADR-0008), and the single-writer lock (ADR-0099 H3) only works on a local
filesystem. A per-machine invoice series is legal but splits one taxpayer
across two DBs (partners, products, the invoice list, storno-at-origin).

ADR-0004 already says what to do: the UI is a client of the backend's wire
protocol, never of storage. A remote shell is just one more client.

## Decision

**One backend, two shells.** The MacBook runs `aberp serve` as today. With
`ABERP_REMOTE_BIND` set it opens a second HTTPS listener on the bridge. The M5
runs the same Tauri shell with `ABERP_REMOTE_URL` set. In that mode it spawns
nothing and holds no DB.

### Backend (`apps/aberp/src/remote_listener.rs`)

| Env | Meaning |
|---|---|
| `ABERP_REMOTE_BIND=<ipv4>:<port>` | Opt-in. Unset → no remote listener (the default). |
| `ABERP_REMOTE_PEER=<ipv4>` | Required. The ONE client address allowed. |
| `ABERP_REMOTE_IFACE=<name>` | Default `bridge0`. Never `lo*`. |

The layers, all fail-closed (misconfiguration refuses boot, CLAUDE.md rule 12):

1. **Interface pin.** The socket is bound with `IP_BOUND_IF` to the interface,
   and boot checks via `getifaddrs` that the address is on it. macOS is a
   weak-host stack: an address-only bind would accept a packet for
   `10.69.0.1` that arrived on Wi-Fi.
2. **Address shape.** Bind and peer must be RFC 1918 or link-local IPv4. The
   bind port must be fixed. `0.0.0.0`, loopback and public addresses are refused.
3. **Peer allowlist.** A connection from any other source gets a 403 and a WARN log.
4. **Separate bearer.** Keychain `aberp.nav.<tenant>` / `remote_session_token`
   is minted like `session_token` (32 B CSPRNG). It is required on **every**
   remote route, `/health` included. The remote router runs on a state clone
   whose `session_token` is the remote token. So every per-handler
   `check_bearer_rejection` validates the remote token, and the loopback token is worthless over the
   cable. To revoke: delete the item on the MacBook and restart.
5. **Deny-list.** The deny-list is matched on the axum route template
   (`MatchedPath`), so encoded paths cannot slip past it. These routes stay
   at the DB machine:
   - NAV credential setup and rotation
   - the seller wizard
   - snapshot restore and restore-from-NAV
   - restore-lock abandon
   - tenant create, switch, archive, restore, toggle-nav and hide-demo
   - the first-prod-launch consent
   - the mock DÁP login
6. **TLS pin.** The remote listener uses the same rcgen cert as loopback. The
   remote shell pins its SHA-256 fingerprint from configuration, which takes
   the place of the stdout handshake line.

`/remote/hello` exists only on the remote router. It returns
`{tenant, state, is_production_build}`.

### Shell (`apps/aberp-ui/src/remote_client.rs`)

| Env / keychain | Meaning |
|---|---|
| `ABERP_REMOTE_URL=https://<lan-ipv4>:<port>` | Selects remote mode. Must be an IP literal: a `.local` name can resolve to Wi-Fi. |
| `ABERP_REMOTE_FINGERPRINT=<64 hex>` | Required. Copied once from `~/.aberp/serve/<tenant>/loopback.fingerprint.sha256`. |
| keychain `aberp.nav.<tenant>` / `remote_session_token` | Copied once from the MacBook keychain. |

The pinned client sends the bearer as a default header. Before the SPA mounts,
`/remote/hello` must match the tenant and the build profile, and report
`ready`. A dev shell can never drive a prod backend, nor the reverse. First-run
wizards are never offered remotely. Closing the remote shell signals nothing:
the backend is not its child. `run/run_remote_client.sh` reads
`~/.aberp/remote/<tenant>.env` (mode 600; parsed as data, never sourced). It
builds the SPA and the shell; `tenant=prod` builds with `--features production`.

## Consequences

- Both machines see one DB, one allocator and one audit chain. Concurrency is
  the same as two windows on one backend; the backend already serialises
  writes through the shared Handle.
- The M5 can issue only while the MacBook is awake and ABERP is running there.
  NAV polling, the email relay, snapshots and the audit mirror all stay on the
  MacBook.
- Two new direct deps, `socket2` (feature `all`) and `nix` (feature `net`). Both
  were already in `Cargo.lock` transitively, so no new crates. No `unsafe`.
- The NAV technical-user secrets never leave the MacBook keychain. Only the
  remote bearer and the cert fingerprint are copied, once.

## Adversarial review

- *"Why not an SSH tunnel to loopback and no new listener?"* — It is viable and
  needs no code. But it puts Remote Login (a full shell) on the bridge to get
  one HTTPS port. The tunnel also ends at the MacBook's loopback, so the
  backend cannot distinguish the remote client from the local one: no separate
  token, no deny-list. The dedicated listener is a smaller surface than sshd.
- *"Someone on the Wi-Fi spoofs 10.69.0.2."* — `IP_BOUND_IF` drops packets that
  did not arrive on `bridge0`. Spoofing over the cable means physical access.
  Even then the attacker still needs the bearer, and has to complete TLS
  against the pinned cert.
- *"The M5 is compromised."* — Then the attacker can do whatever the M5's
  operator can: issue invoices. They cannot read NAV secrets, restore over the
  DB, switch tenants or provision credentials. The fix is revocation (delete
  `remote_session_token` on the MacBook and restart).
- *"Link-local addresses change after reboot."* — Supported, but the runbook
  sets manual addresses (`10.69.0.1/30` and `.2/30`) so both env files stay
  valid.

## Addendum (2026-10-31): always-on link

Ervin's steer: treat the Studio ⇄ Thunderbolt ⇄ MacBook link as an always-on network, with rare
unplugs. That gives the following:

- **No offline mode on the Studio.** It has no DB and no fallback. When the backend is
  unreachable it fails closed: the boot pane or the failing action says so, and that nothing
  was issued.
- **Backend under a LaunchAgent on the MacBook**
  (`run/launchd/ch.aben.aberp.serve.plist`). It starts at login and binds the loopback port to
  `127.0.0.1:18443` and the bridge to `10.69.0.1:8443`.
- **Attach mode for the MacBook's own shell.** The LaunchAgent backend holds the ADR-0099 H3
  single-writer lock, so the MacBook's window must not spawn a second backend.
  - `ABERP_REMOTE_URL=https://127.0.0.1:<port>` attaches to it.
  - It uses the same pinned fingerprint and the same `/remote/hello` handshake.
  - The bearer is the loopback `session_token`.
  - `/remote/hello` therefore lives on the shared router, so both listeners serve it; on the
    bridge it stays behind the peer/bearer guard.
- **Secrets: the Studio needs exactly one**, `remote_session_token`, plus the non-secret
  fingerprint.
  - The shell's only keychain read is its bearer (`aberp-ui/src/lib.rs::load_keychain_token`).
  - Everything else is a Tauri command forwarded to the backend: the SMTP commands, NAV
    credential status, and NAV signing and submission.
  - The NAV technical-user login, password, `xmlSignKey` and `xmlChangeKey` (ADR-0020), and the
    SMTP password, never leave the MacBook keychain.
- **Retry safety across sleep and unplug** is ADR-0112.
- **Runbook:** `docs/runbooks/thunderbolt-always-on.md`. It covers:
  - fixed IPs and `pmset`;
  - bridge-only SSH;
  - the first-run one-liners;
  - the token and fingerprint handoff;
  - LaunchAgent install;
  - upgrades.

## Addendum 2 (2026-10-31): the launchers decide, not the operator

Ervin's steer: plain `./run/run_prod.sh`, and the window `upgrade_prod.sh` opens at the end,
must find the LaunchAgent backend by themselves. No environment variables for the operator to
remember ([[trust-code-not-operator]]).

**One decision function.** `run/lib/aberp_launch_agent.sh::aberp_agent_decide`, sourced by both
launchers, returns:

| State | Meaning | What `run_prod.sh` does |
|---|---|---|
| `spawn` | Agent not installed and not loaded | Exactly the pre-0111 behaviour |
| `start` | Plist in `~/Library/LaunchAgents` but the job is not loaded (e.g. `upgrade_prod.sh` stopped it for the swap) | Builds `aberp`, starts the agent, waits up to 3 minutes for Ready (a rebuilt binary's keychain prompts wait for the operator), then decides again; anything but `attach` is a failure |
| `attach` | Agent loaded, loopback `/health` ok, and its `binary_hash` equals the SHA-256 of this checkout's `target/release/aberp` | Exports `ABERP_REMOTE_URL=https://127.0.0.1:18443` and the pinned fingerprint from `~/.aberp/serve/prod/loopback.fingerprint.sha256`; skips relinking `aberp`, because the agent is executing it |
| `fail:unhealthy` / `fail:version-skew` / `fail:no-fingerprint` / `fail:no-binary` | Agent loaded but unusable | Exit 3 with an EN/HU message; never spawns a second backend |

**`upgrade_prod.sh`** (from 2.36.1 on):
- If the agent is loaded, it refuses while a prod window is open. Otherwise it stops the agent
  itself (graceful drain) before the snapshot and swap.
- An `EXIT` trap restarts the agent on the binary still on disk if anything fails before
  hand-over, so the Studio is not left without a backend.
- After hand-over, `run_prod.sh` sees `start` and brings the agent up on the new build.

**Bridge bind and peer live in the plist.** `ABERP_REMOTE_BIND`, `ABERP_REMOTE_PEER` and
`ABERP_REMOTE_IFACE` stay the ADR-0111 opt-in mechanism. The plist (installed from the checkout
on every agent start) is their single source, so the operator never passes them. No new
`seller.toml` slot (ADR-0055) is added.

**Known limit.** `attach` compares the running agent with the binary on disk. It cannot see
source changes that were never built; the Frankenstein guard (HEAD must equal a released
`PROD_v*` branch, clean tree) plus `upgrade_prod.sh` as the only way to change the checkout
covers that.

**Transition 2.36.0 → 2.36.1.** The 2.36.0 `upgrade_prod.sh` that performs that upgrade predates
this addendum, so for that one upgrade the agent is stopped by hand first:
```bash
launchctl bootout gui/$(id -u)/ch.aben.aberp.serve; cd ~/ABERP && ./run/upgrade_prod.sh PROD_v2.36.1
```

**Tests:** `run/tests/run_prod_launch_agent_test.sh` covers every state with stubbed
`launchctl`/`curl`, pins the plist against the helper (label, port, binary path, bridge
bind/peer), and runs the real `run_prod.sh` for the unhealthy (exit 3, no build), attach,
installed-but-stopped and no-agent cases.

## Not decided here

- Multi-user identity (who on which machine issued what). Today both shells
  act as the same operator; the audit actor is the NAV login, as before.
  ADR-0100 Phase 2 (Keycloak) is where per-person identity lands.
- More than one remote peer.
