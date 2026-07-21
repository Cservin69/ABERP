# Phase 2 infra bring-up — stand up Keycloak (OIDC identity) from zero

**Audience:** first-time operator / self-hoster. No prior server-admin assumed.
**Time:** ~90 min the first time (most of it is waiting on DNS + Let's Encrypt).
**Cost:** ~€4–5/month (one Hetzner CX22) + your existing domain. Needs a credit/debit card once.
**Grounded in:** ADR-0100 §0 (decided stack) + §4 forks (Hetzner CX22 / sops+age / Keycloak+Postgres / TOTP+WebAuthn / Caddy+Cloudflare).

> **📌 Substitute your own domain.** This walkthrough uses `example.com` as a placeholder throughout — `auth.example.com` for Keycloak and `app.example.com` for the application host. Replace **every** occurrence of `example.com` with your real domain before running any command.

---

## 0. Read this box before you touch anything

### What this walkthrough IS

This stands up the **infrastructure prerequisite** for ADR-0100 **Phase 2** (identity + MFA delegated to a self-hosted Keycloak container speaking OIDC). When you finish, you will have:

- a hardened Hetzner server on the public internet,
- a self-hosted **sops + age** secrets store on it,
- a **Keycloak** container (with its own private Postgres) reachable over **real HTTPS** at `auth.example.com`,
- a Keycloak **realm** + an **ABERP OIDC client** ready for ABERP to authenticate against,
- **TOTP required** as the MFA baseline, and a note on where WebAuthn step-up gets configured later,
- scheduled encrypted backups + a restore drill.

### What this walkthrough is NOT (say it out loud so nobody is surprised)

- ❌ **It does not move ABERP to the cloud.** ABERP keeps running exactly as today (desktop Tauri + loopback). Relocating ABERP is ADR-0100 **Phases 4/6/7** — separate work, later.
- ❌ **It does not write a single line of ABERP↔Keycloak code.** No OIDC relying-party code, no callback handler, no token validation. That is the **Phase-2 code session**, done after this infra exists. This walkthrough *ends* by handing that session the four values it needs (§11).
- ❌ **It does not touch `main`, the desktop build, or the `run/` launch scripts.** Nothing here changes `run_prod.sh` / `upgrade_prod.sh`. The desktop binary stays the rollback target (ADR-0100 §6).

If you only remember one thing: **this session's deliverable is a working Keycloak login page at `https://auth.example.com`.** Nothing about ABERP itself changes yet.

### 🚩 Flagged assumptions (conservative defaults taken; correct me if wrong)

| # | Assumption | Why this default | How to change |
| --- | --- | --- | --- |
| A1 ✅ | **Subdomain = `auth.example.com`** for Keycloak. **CONFIRMED (2026-07-15, route assumptions approved).** | ADR-0100 never fixes an auth hostname; `auth.` is the conventional IdP subdomain and keeps the storefront (`example.com`) and the future ABERP app (`app.example.com`, per ADR-0059/0100) cleanly separated. | Pick any subdomain in Step 12; substitute it everywhere `auth.example.com` appears. |
| A2 ✅ | **ABERP will live at `app.example.com`** (the OIDC redirect target). **CONFIRMED (2026-07-15, route assumptions approved).** | That is the SaaS app hostname decided in ADR-0059 §4 and carried into ADR-0100. | If ABERP gets a different hostname, fix the redirect URI in Step 33. |
| A3 ✅ | **OIDC callback path = `/auth/callback`.** **CONFIRMED (2026-07-15, route assumptions approved).** | The Phase-2 code session owns the real path; `/auth/callback` is the conventional placeholder so the client isn't empty. | The code session updates the client redirect URI (Keycloak admin console, no infra change). |
| A4 | **Region = Falkenstein (`fsn1`), Ubuntu 24.04 LTS.** | ADR-0100 says EU-Falkenstein; 24.04 is the current Ubuntu LTS. | Choose another EU location / LTS in Step 2. |
| A5 | **Keycloak `26.x`, Postgres `16`** (pinned image tags). | Current stable Keycloak major + its supported Postgres. | Bump the tags in the compose file (Step 26). Never use `:latest`. |
| A6 | **Firewall opens 22, 80, 443** — not just 22+443. | Port **80** is mandatory for Let's Encrypt HTTP-01 challenge and the HTTP→HTTPS redirect. The ADR brief said "22+443"; 80 is a hard requirement for automatic TLS. No plaintext app traffic ever rides 80 — Caddy 301-redirects it. | If you switch Caddy to the DNS-01 ACME challenge you may close 80; that's more setup, not done here. |
| A7 | **Cloudflare left in "DNS only" (grey cloud)** during bring-up. | So Caddy can complete the HTTP-01 challenge directly. Turning on Cloudflare's proxy (orange cloud) before the cert exists breaks issuance. | Enable the orange-cloud proxy *after* Step 31 verifies a working cert, if you want Cloudflare's edge (ADR §4 edge fork). |

---

## PART 1 — The server (Steps 1–11)

## 1. [Prereqs — YOU do this by hand] Gather the three things you need

Before any command, confirm you have all three. Missing one blocks everything downstream.

1. **A payment card** — Hetzner requires a card (or PayPal) to create an account. Expect ~€4.50/mo for the CX22 plus a few cents for the assigned IPv4. This is real money leaving monthly; that's expected (ADR-0100 §6: "~€8–12/mo OpEx from €0").
2. **Control of the `example.com` DNS zone** — you must be able to add an **A record**. (Per project memory the zone is managed at your DNS provider / Cloudflare; you'll use it in Step 12.)
3. **An SSH keypair on your Mac** — created in Step 2 if you don't have one.

**✅ Success check:** you can log into your DNS provider's dashboard and see the `example.com` zone, and you have a card ready. Do not proceed otherwise.

---

## 2. [your Mac — Terminal] Make an SSH key (skip if you already have one)

One command. This is the key that will be the *only* way into the server.

```bash
ls -al ~/.ssh/id_ed25519.pub 2>/dev/null && echo "KEY ALREADY EXISTS — skip keygen" || ssh-keygen -t ed25519 -C "aberp-keycloak-$(whoami)" -f ~/.ssh/id_ed25519 -N ""
```

- If it prints `KEY ALREADY EXISTS`, do nothing more here.
- Otherwise it creates `~/.ssh/id_ed25519` (private — never leaves your Mac) and `~/.ssh/id_ed25519.pub` (public — goes to Hetzner).

Now print the **public** key so you can paste it into Hetzner in Step 3:

```bash
cat ~/.ssh/id_ed25519.pub
```

**✅ Success check:** the last command prints one line starting with `ssh-ed25519 AAAA…`. That whole line is what you paste into Hetzner. **Never** print or paste `id_ed25519` (the one without `.pub`).

---

## 3. [Hetzner Cloud Console — YOU do this by hand] Create the account and upload your SSH key

1. Go to **https://console.hetzner.cloud/** and **Register**. Confirm your email, add your card. (Prohibited-for-me action: creating accounts and entering card details is yours to do by hand — I never touch these.)
2. Once logged in, click **+ New Project**, name it `aberp`, open it.
3. Left sidebar → **Security** → **SSH Keys** → **Add SSH Key**. Paste the full `ssh-ed25519 AAAA…` line from Step 2. Name it `mac-<yourname>`. Save.

**✅ Success check:** the SSH key appears in the project's **Security → SSH Keys** list with the name you gave it.

---

## 4. [Hetzner Cloud Console — YOU do this by hand] Create the CX22 server

1. In the `aberp` project → **Servers** → **Add Server**.
2. **Location:** Falkenstein (`fsn1`) — EU. *(🚩A4)*
3. **Image:** Ubuntu **24.04**.
4. **Type:** **Shared vCPU → CX22** (2 vCPU / 4 GB / 40 GB). This is the ADR-0100 §4 pick. Do **not** upsize; CX22 has headroom for Keycloak + Postgres at 1 user.
5. **Networking:** leave **Public IPv4** enabled (you need it for DNS + SSH).
6. **SSH keys:** tick the key you uploaded in Step 3. **Do NOT set a root password** — key-only from the start.
7. **Firewalls / Volumes / Backups:** skip for now (we build the firewall by hand in Step 8 so you understand it; Hetzner's paid backups are optional on top of our pg_dump backups in Step 34).
8. **Name:** `aberp-auth`.
9. Click **Create & Buy now**.

**✅ Success check:** after ~30 s the server shows a green dot and a **public IPv4** address (e.g. `95.216.x.x`). **Write this IP down** — call it `<SERVER_IP>` from here on. You'll use it in Steps 5 and 12.

---

## 5. [your Mac — Terminal] First SSH login as root (one time only)

Hetzner's fresh Ubuntu lets `root` in via your key. We log in once, only to create a safer user, then we lock root out.

```bash
ssh -o StrictHostKeyChecking=accept-new root@<SERVER_IP>
```

Replace `<SERVER_IP>` with the address from Step 4.

**✅ Success check:** your prompt changes to `root@aberp-auth:~#`. You are on the server. If it asks for a password, something is wrong with the key — stop and re-check Step 3 (you likely pasted the wrong key).

> Every step from here labelled **[on the server — SSH]** runs inside this session.

---

## 6. [on the server — SSH] Create a non-root sudo user

We will stop using root for daily work. Create user `aberp` and copy your SSH key to it so you can log in directly as `aberp` later.

```bash
adduser --disabled-password --gecos "" aberp
usermod -aG sudo aberp
install -d -m 700 -o aberp -g aberp /home/aberp/.ssh
cp /root/.ssh/authorized_keys /home/aberp/.ssh/authorized_keys
chown aberp:aberp /home/aberp/.ssh/authorized_keys
chmod 600 /home/aberp/.ssh/authorized_keys
echo "aberp ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/aberp
chmod 440 /etc/sudoers.d/aberp
```

> 🚩 The `NOPASSWD` sudoers line is a deliberate convenience for a **key-only, single-operator** box: since there is no password login at all (Step 9), a sudo password would only protect against someone who already holds your SSH private key — at which point they own the box regardless. If you later add more operators, replace it with a real password.

**✅ Success check:** run `id aberp` — the output includes `groups=…(sudo)`. And `sudo -l -U aberp` lists `NOPASSWD: ALL`.

---

## 7. [your Mac — Terminal] Verify you can log in as `aberp` BEFORE locking root out

**Do not skip this.** This is the anti-lockout gate. Open a **second, new** Terminal window on your Mac (leave the root SSH session in Step 6 open and untouched) and run:

```bash
ssh -o StrictHostKeyChecking=accept-new aberp@<SERVER_IP> "echo LOGIN_OK && sudo whoami"
```

**✅ Success check:** it prints:
```
LOGIN_OK
root
```
`LOGIN_OK` proves key login as `aberp` works; `root` proves passwordless sudo works. **Only if you see both** do you continue. If either fails, fix it using your still-open root session before touching Step 9 — otherwise the next step can lock you out permanently.

---

## 8. [on the server — SSH] Firewall: allow SSH FIRST, then everything else, THEN enable

**Order is everything here.** The classic footgun is `ufw enable` with no rule allowing 22 → you are instantly locked out of a cloud box with no console. We add the allow-rules *before* enabling.

Back in your root SSH session (Step 5/6), run these **in this exact order**:

```bash
ufw default deny incoming          # deny everything inbound by default
ufw default allow outgoing         # let the server reach out (updates, ACME, etc.)
ufw allow 22/tcp   comment 'SSH'   # ← SSH FIRST, before enable
ufw allow 80/tcp   comment 'HTTP (ACME challenge + HTTPS redirect)'
ufw allow 443/tcp  comment 'HTTPS (Keycloak via Caddy)'
ufw status numbered                # eyeball the rules BEFORE enabling
```

Confirm the printed list shows **22, 80, 443** all `ALLOW IN`. Only then:

```bash
ufw --force enable
```

> 🚩A6 Port **80** is open on purpose — Let's Encrypt's HTTP-01 challenge and the HTTP→HTTPS redirect both need it. No unencrypted application traffic uses it; Caddy answers 80 only to redirect to 443.
> ⚠️ Postgres (5432) is deliberately **absent** from this ufw allow-list — but do **not** rely on ufw to protect a database container. **Docker inserts its own iptables rules ahead of ufw**, so any container port published to `0.0.0.0` is internet-reachable *regardless of what ufw says*. What actually keeps Postgres safe is that it has **no `ports:` mapping at all** (internal docker network only, Step 22). Keycloak is safe because it is bound to **`127.0.0.1:8080`** (Step 22), not `0.0.0.0`. Rule of thumb: **never publish a container to `0.0.0.0` and expect ufw to save you** — it won't.

**✅ Success check:** `ufw status verbose` shows `Status: active`, default `deny (incoming)`, and the three allow rules. Now confirm you are **not** locked out: from your Mac Terminal, `ssh aberp@<SERVER_IP> "echo STILL_IN"` must print `STILL_IN`. If it does, your root session has done its job.

---

## 9. [on the server — SSH] Harden SSH: key-only, no root login, no passwords

Now that `aberp` login is proven (Step 7) and the firewall is up (Step 8), lock SSH down. We write a drop-in config (leaves the vendor file intact) and validate before reloading.

```bash
cat > /etc/ssh/sshd_config.d/99-aberp-hardening.conf <<'EOF'
PermitRootLogin no
PasswordAuthentication no
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
PubkeyAuthentication yes
EOF
sshd -t && echo "SSHD CONFIG OK"
```

Only if it prints `SSHD CONFIG OK`:

```bash
systemctl reload ssh || systemctl reload sshd
```

**✅ Success check:** open **another new** Mac Terminal and run `ssh root@<SERVER_IP> "echo SHOULD_FAIL"` — it must be **rejected** (`Permission denied (publickey)`), proving root is locked out. Then `ssh aberp@<SERVER_IP> "echo GOOD"` must still print `GOOD`. Once both are confirmed, you can close the root session — **from here on log in as `aberp`.**

---

## 10. [on the server — SSH, as `aberp`] fail2ban + automatic security updates

Log in as `aberp` now (`ssh aberp@<SERVER_IP>`). Install the two low-effort hardening services.

```bash
sudo apt update
sudo NEEDRESTART_MODE=a apt upgrade -y      # NEEDRESTART_MODE=a auto-restarts services, no interactive prompt
sudo apt install -y fail2ban unattended-upgrades
sudo systemctl enable --now fail2ban
sudo dpkg-reconfigure -f noninteractive unattended-upgrades
```

> On 24.04 `apt upgrade` otherwise pops an interactive `needrestart` dialog that stalls the non-interactive run; `NEEDRESTART_MODE=a` makes it auto-restart affected services.

fail2ban ships with an SSH jail enabled by default on Ubuntu, which is all we need (it bans IPs after repeated failed logins — though with password auth off, the exposure is already small).

> 🚩 **Don't ban yourself.** Add your own IP to fail2ban's ignore list so a fat-fingered SSH attempt can't lock you out. Edit `/etc/fail2ban/jail.local` (create it if absent) with a `[DEFAULT]` block:
> ```
> [DEFAULT]
> ignoreip = 127.0.0.1/8 <YOUR_HOME_IP>
> ```
> Replace `<YOUR_HOME_IP>` with your current public IP (`curl -s ifconfig.me` from your Mac — the same value used in Step 30), then `sudo systemctl restart fail2ban`. If you *do* get banned mid-setup, un-ban from the server console: `sudo fail2ban-client set sshd unbanip <YOUR_IP>`.

**✅ Success check:**
```bash
sudo systemctl is-active fail2ban          # → active
sudo fail2ban-client status sshd           # → shows the sshd jail
cat /etc/apt/apt.conf.d/20auto-upgrades     # → both lines "1"
```

---

## 11. [on the server — SSH] Set the timezone and a swap file (small-box hygiene)

Keycloak's JVM + Postgres on 4 GB is comfortable, but a small swap file prevents an OOM-kill during Keycloak's build/start. And a correct clock matters for TLS + TOTP.

```bash
sudo timedatectl set-timezone Europe/Budapest
sudo fallocate -l 2G /swapfile && sudo chmod 600 /swapfile && sudo mkswap /swapfile && sudo swapon /swapfile
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

> If `fallocate` fails (some filesystems don't support it), use the portable `dd` form instead: `sudo dd if=/dev/zero of=/swapfile bs=1M count=2048` — then the same `chmod 600` / `mkswap` / `swapon` / fstab lines.

**✅ Success check:** `free -h` shows a `Swap:` line of ~2.0Gi; `timedatectl` shows your timezone and `System clock synchronized: yes`. (An accurate clock is a hard requirement for TOTP in Step 32 — a skewed clock breaks every 30-second code.)

---

## PART 2 — DNS + Docker (Steps 12–14)

## 12. [your DNS provider — YOU do this by hand] Point `auth.example.com` at the server

In your DNS dashboard for `example.com`, add **one A record**:

| Field | Value |
| --- | --- |
| Type | **A** |
| Name / Host | `auth` (some providers want `auth.example.com`) |
| Value / Points to | `<SERVER_IP>` (from Step 4) |
| TTL | Auto / 300 |
| Proxy status (Cloudflare) | **DNS only — grey cloud** 🚩A7 |

> 🚩A7 If your DNS is on Cloudflare, keep the proxy **OFF (grey cloud)** for now. Caddy needs a direct HTTP-01 challenge in Step 30. You can switch it to orange-cloud *after* the cert is confirmed (Step 31), if you want Cloudflare's edge/CDN (ADR-0100 §4 edge fork).

**✅ Success check (from your Mac):**
```bash
dig +short auth.example.com
```
must return exactly `<SERVER_IP>`. DNS can take a few minutes to an hour to propagate — re-run until it matches. **Do not proceed to TLS (Step 30) until this returns the right IP**, or Let's Encrypt will fail.

---

## 13. [on the server — SSH] Install Docker + the compose plugin

Use Docker's official repository (Ubuntu's bundled `docker.io` lags). This is the canonical install.

```bash
sudo install -m 0755 -d /etc/apt/keyrings
sudo curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
sudo chmod a+r /etc/apt/keyrings/docker.asc
echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu $(. /etc/os-release && echo $VERSION_CODENAME) stable" | sudo tee /etc/apt/sources.list.d/docker.list > /dev/null
sudo apt update
sudo apt install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
sudo usermod -aG docker aberp
```

The `usermod` lets `aberp` run docker without sudo. **Log out and back in** for the group to take effect:

```bash
exit
```
then from your Mac: `ssh aberp@<SERVER_IP>`

**✅ Success check:**
```bash
docker run --rm hello-world | grep -q "working correctly" && echo "DOCKER OK"
docker compose version
```
prints `DOCKER OK` (no `sudo` needed) and a compose version ≥ v2.

---

## 14. [on the server — SSH] Lay out the deployment directory

One place for everything, owned by `aberp`.

```bash
sudo install -d -m 750 -o aberp -g aberp /opt/aberp-auth
cd /opt/aberp-auth
mkdir -p secrets backups
```

**✅ Success check:** `ls -la /opt/aberp-auth` shows `secrets/` and `backups/`, owned by `aberp`.

---

## PART 3 — Secrets: sops + age (Steps 15–20)

> **The core rule of this part, read it twice:** the **age private key** decrypts *every* secret in this deployment. **If you lose it, every encrypted secret is permanently unrecoverable** (ADR-0100 §6 "Locked in"). **If someone steals it, they own all your secrets.** It is backed up **offline** (Step 18) and **never committed to git**. Treat it like the master key it is.

## 15. [on the server — SSH] Install age and sops

```bash
sudo apt install -y age
SOPS_VER=v3.9.4    # 🚩 pin; check github.com/getsops/sops/releases for the current stable
curl -fsSL -o /tmp/sops "https://github.com/getsops/sops/releases/download/${SOPS_VER}/sops-${SOPS_VER}.linux.amd64"
sudo install -m 0755 /tmp/sops /usr/local/bin/sops && rm /tmp/sops
```

**✅ Success check:** `age --version` and `sops --version` both print a version. (`sops --version` should match `SOPS_VER`.)

---

## 16. [on the server — SSH] Generate the age keypair

The private key lives at `/etc/aberp/age.key`, mode `0400`, root-owned — exactly the location ADR-0100 §4-B specifies.

```bash
sudo install -d -m 700 /etc/aberp
sudo sh -c 'age-keygen -o /etc/aberp/age.key 2>/tmp/agepub'
sudo chmod 400 /etc/aberp/age.key
cat /tmp/agepub   # prints: "Public key: age1........"
```

Copy the `age1…` string from the `Public key:` line — call it `<YOUR_AGE_PUBLIC_KEY>`. You'll paste it into `.sops.yaml` next.

Print it once more cleanly and clean up the temp file:
```bash
sudo grep -oE 'age1[0-9a-z]+' /etc/aberp/age.key | head -1   # this is <YOUR_AGE_PUBLIC_KEY>
rm -f /tmp/agepub
```

> The public key is embedded in the age.key file (as a comment) and is safe to share — it only *encrypts*. The secret line `AGE-SECRET-KEY-1…` in that file is what must never leave the box unencrypted.

**✅ Success check:** `sudo ls -l /etc/aberp/age.key` shows `-r-------- root root`. The public-key command prints one `age1…` string. Write `<YOUR_AGE_PUBLIC_KEY>` down for the next step.

---

## 17. [on the server — SSH] Write `.sops.yaml` so every secret auto-encrypts to your key

```bash
cd /opt/aberp-auth
cat > .sops.yaml <<EOF
creation_rules:
  - path_regex: secrets/.*\.env$
    age: "<YOUR_AGE_PUBLIC_KEY>"
EOF
```

Replace `<YOUR_AGE_PUBLIC_KEY>` with the real `age1…` from Step 16.

**✅ Success check:** `cat .sops.yaml` shows your real `age1…` key (not the placeholder). Any file matching `secrets/*.env` will now be encrypted to that key automatically.

---

## 18. [your Mac — Terminal — YOU do this by hand] Back up the age private key OFFLINE

**This is a by-hand step and the single most important backup in the whole system.** Copy the private key off the server to an **offline** location (a password manager entry, or an encrypted USB stick kept in a drawer — **not** a git repo, **not** a cloud drive synced folder, **not** the server itself).

From your Mac:
```bash
ssh aberp@<SERVER_IP> "sudo cat /etc/aberp/age.key" | pbcopy
```

The full key file (including the `AGE-SECRET-KEY-1…` line) is now on your clipboard. **Paste it into your password manager** as a secure note titled `ABERP age master key — DO NOT LOSE`. Then clear your clipboard (`echo -n | pbcopy`).

> 🚩 Why by hand: I will not move a secret key to an external service for you (that would be "sending user data to an external endpoint"). You place it, in a vault you control.

**✅ Success check:** you can see the key text in your password manager, and it starts with `# created:` / contains a line `AGE-SECRET-KEY-1…`. Confirm this before continuing — the encrypted secrets you create next are only as recoverable as this backup.

---

## 19. [on the server — SSH] Generate strong secret values

Generate the four secrets Keycloak needs. We generate them with the OS CSPRNG (`openssl rand`) — never type these by hand, never reuse a password.

```bash
cd /opt/aberp-auth
KC_DB_PASSWORD="$(openssl rand -base64 32 | tr -d '/+=' | head -c 40)"
KC_ADMIN_PASSWORD="$(openssl rand -base64 32 | tr -d '/+=' | head -c 40)"
ABERP_CLIENT_SECRET="$(openssl rand -base64 48 | tr -d '/+=' | head -c 60)"
echo "Generated. (Values are NOT printed. They go straight into the encrypted file next.)"
```

> 🚩 **The one password you DO set by hand:** the Keycloak **admin username** is `admin`; its password we just generated randomly (`KC_ADMIN_PASSWORD`) rather than asking you to invent one — a 40-char random string is stronger than any memorable password, and you'll retrieve it from sops when you need it (Step 32), never type it. If you would rather set a memorable admin password by hand, replace the `KC_ADMIN_PASSWORD=` line with `read -rs KC_ADMIN_PASSWORD` and type a strong one. The generated default is the conservative, harder-to-phish choice.

**✅ Success check:** `echo "${#KC_DB_PASSWORD} ${#KC_ADMIN_PASSWORD} ${#ABERP_CLIENT_SECRET}"` prints three numbers ~`40 40 60`. (We check lengths, not values.)

> ⚠️ **Run Steps 19 and 20 back-to-back in the same SSH session** — the three variables live only in this shell's memory. If you log out (or the session drops) between them, the variables vanish and Step 20 would write an empty-password secrets file. Step 20 opens with a guard that aborts loudly if that happened, so you can't silently ship blank secrets — but the clean path is one uninterrupted block.

---

## 20. [on the server — SSH] Write the secrets file and encrypt it with sops

Write the plaintext env, then encrypt it **in place** with sops (which uses `.sops.yaml` to pick your age key). The plaintext exists only for the seconds between these two commands, in a `0600` file, then is replaced by ciphertext.

```bash
cd /opt/aberp-auth
umask 077
# Fail loud if Step 19's variables didn't survive into this shell (logged out between steps, etc.)
: "${KC_DB_PASSWORD:?run Step 19 first — variable is empty}" "${KC_ADMIN_PASSWORD:?run Step 19 first}" "${ABERP_CLIENT_SECRET:?run Step 19 first}"
cat > secrets/keycloak.env <<EOF
POSTGRES_USER=keycloak
POSTGRES_PASSWORD=${KC_DB_PASSWORD}
POSTGRES_DB=keycloak
KC_DB_USERNAME=keycloak
KC_DB_PASSWORD=${KC_DB_PASSWORD}
KEYCLOAK_ADMIN=admin
KEYCLOAK_ADMIN_PASSWORD=${KC_ADMIN_PASSWORD}
KC_BOOTSTRAP_ADMIN_USERNAME=admin
KC_BOOTSTRAP_ADMIN_PASSWORD=${KC_ADMIN_PASSWORD}
ABERP_OIDC_CLIENT_SECRET=${ABERP_CLIENT_SECRET}
EOF

sops --encrypt --in-place secrets/keycloak.env
unset KC_DB_PASSWORD KC_ADMIN_PASSWORD ABERP_CLIENT_SECRET
```

> **Why these keys:**
> - `POSTGRES_PASSWORD` is **mandatory** — the `postgres:16` image *aborts on startup* if it's unset. It must equal `KC_DB_PASSWORD` (the password Keycloak uses to connect), so both reference the same generated value.
> - Both admin pairs are set on purpose. **Keycloak 26 renamed** `KEYCLOAK_ADMIN`/`KEYCLOAK_ADMIN_PASSWORD` → `KC_BOOTSTRAP_ADMIN_USERNAME`/`KC_BOOTSTRAP_ADMIN_PASSWORD`. Setting **both** (harmless overlap) makes this work on any 26.x minor regardless of which names that build honours. 🚩 **MUST verify against the docs for the exact image tag you pinned** (Step 22, `26.0`) — if that minor only reads the new names, the legacy pair is simply ignored.
> - ⚠️ The bootstrap admin is created **only on the very first boot into an empty Postgres volume.** Changing `KC_ADMIN_PASSWORD` in sops *later* will **not** rotate an already-created admin — you'd change it in the Keycloak admin console (or wipe the pg volume and re-bootstrap). Treat the first-boot value as the one you'll actually use.

**✅ Success check:**
```bash
head -3 secrets/keycloak.env
```
The file must now look like ciphertext — you'll see `POSTGRES_USER=ENC[AES256_GCM,data:...]` and a `sops_age__list` / `sops_version` block at the bottom. **If you see plaintext passwords, STOP** — the encryption didn't run; re-check `.sops.yaml` (Step 17). Verify you can decrypt:
```bash
sudo SOPS_AGE_KEY_FILE=/etc/aberp/age.key sops --decrypt secrets/keycloak.env | grep -c '='
```
prints `10` (ten `key=value` lines recovered: 3 postgres + 2 KC-DB + 2 legacy-admin + 2 bootstrap-admin + 1 client-secret). This encrypted file is now **safe to commit / back up**; the plaintext is gone.

> ⚠️ **Never** `git commit` the age.key, and **never** write decrypted secrets to disk outside `tmpfs` (Step 27 handles the runtime decrypt into RAM).
> 🔓 **Honest scope of this protection:** sops protects these secrets **at rest** (on-disk ciphertext) and tmpfs keeps the decrypted copy **in RAM only** (Steps 24/27). At *runtime* the same values live in the containers' environment, readable by anyone who is already local `root` or in the `docker` group. That's inherent to env-file config, not a leak — the threat model this defends is disk theft / backup exfiltration / accidental `git commit`, not a compromised root on the box.

---

## PART 4 — Keycloak + Postgres via docker-compose (Steps 21–29)

## 21. [on the server — SSH] Understand the shape before writing the compose file

Three pieces, one private network:

```
Internet ──443──► Caddy (host) ──► Keycloak container ──► Postgres container
                  (TLS term)        (:8080 internal)       (:5432 internal only)
                                    proxy=xforwarded        NEVER exposed to host/internet
```

- **Postgres** is on an internal docker network only — no `ports:` mapping, so it is unreachable from the host or the internet. Only Keycloak can reach it.
- **Keycloak** listens on `8080` **bound to `127.0.0.1`** on the host — only Caddy (running on the host) reaches it. It is told it sits behind a proxy (`KC_PROXY_HEADERS=xforwarded`) and that its public URL is `https://auth.example.com`.
- **Caddy** (Step 30) is the only thing the firewall exposes (443/80), and it terminates real TLS.

---

## 22. [on the server — SSH] Write the docker-compose file

```bash
cd /opt/aberp-auth
cat > docker-compose.yml <<'EOF'
services:
  postgres:
    image: postgres:16
    restart: unless-stopped
    env_file: [/run/aberp/keycloak.env]     # decrypted at deploy into tmpfs (Step 27)
    volumes:
      - pgdata:/var/lib/postgresql/data      # persistence
    networks: [internal]
    # NO ports: — Postgres is never exposed to host or internet
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U $$POSTGRES_USER -d $$POSTGRES_DB"]
      interval: 10s
      timeout: 5s
      retries: 10

  keycloak:
    image: quay.io/keycloak/keycloak:26.0    # 🚩A5 pin; never :latest
    restart: unless-stopped
    command: start --optimized
    env_file: [/run/aberp/keycloak.env]
    environment:
      KC_DB: postgres
      KC_DB_URL: jdbc:postgresql://postgres:5432/keycloak
      KC_HOSTNAME: https://auth.example.com   # 🚩A1
      KC_HTTP_ENABLED: "true"                  # plaintext INSIDE the box only; Caddy adds TLS
      KC_PROXY_HEADERS: xforwarded             # trust X-Forwarded-* from Caddy
      KC_HEALTH_ENABLED: "true"
    ports:
      - "127.0.0.1:8080:8080"                  # bound to loopback — only Caddy reaches it
    depends_on:
      postgres:
        condition: service_healthy
    networks: [internal]
    # NO container healthcheck on Keycloak: the obvious /dev/tcp probe needs bash,
    # which the minimal Keycloak 26 image likely does NOT ship — the check would sit
    # `unhealthy` forever even while Keycloak is perfectly up, stranding you at Step 26.
    # Nothing depends on Keycloak's health, so we drop it and prove liveness externally
    # with `curl 127.0.0.1:8080` (Step 26) and the public HTTPS probe (Step 31) instead.
    # 🚩 If you ever re-add a container healthcheck, MUST verify which shell the pinned
    # image actually provides (`docker run --rm --entrypoint sh quay.io/keycloak/keycloak:26.0 -c 'echo ok'`).

networks:
  internal:
    driver: bridge

volumes:
  pgdata:
EOF
```

> Notes: `start --optimized` runs Keycloak in production mode (not `start-dev`). `KC_HTTP_ENABLED=true` allows plaintext **only on the internal loopback** between Caddy and Keycloak — the wire to the internet is always Caddy's TLS. The admin/DB passwords and client secret are **never in this file** — they arrive via `env_file` from the tmpfs-decrypted `/run/aberp/keycloak.env` (Step 27). This compose file is safe to commit.

**✅ Success check:** `docker compose config` parses without error (it will warn that `/run/aberp/keycloak.env` doesn't exist yet — expected; we create it in Step 27).

---

## 23. [on the server — SSH] Build the optimized Keycloak image config

`start --optimized` expects the DB vendor baked in at build. The simplest robust path is to let Keycloak auto-build on first `start` by dropping `--optimized` for the very first run, OR pre-build. To keep it hülye-biztos, we use the auto-build form for first boot: temporarily we rely on Keycloak 26's ability to build on start when `--optimized` is absent.

Edit the command line for the first boot only:
```bash
cd /opt/aberp-auth
sed -i 's/command: start --optimized/command: start/' docker-compose.yml
```

> Why: `start` (without `--optimized`) performs the build step automatically using the `KC_DB` env, then starts. It's slightly slower to boot but removes a separate build step — correct for a first bring-up. You can switch back to `start --optimized` after a successful boot if you want faster restarts.

**✅ Success check:** `grep 'command:' docker-compose.yml` shows `command: start`.

---

## 24. [on the server — SSH] Create the tmpfs mount for decrypted secrets

Secrets get decrypted into `/run/aberp/` which is on **tmpfs** (RAM) — it never hits disk and is wiped on reboot. Create it and make sure it's tmpfs.

```bash
sudo install -d -m 700 -o aberp -g aberp /run/aberp
mount | grep -q '/run type tmpfs' && echo "/run is tmpfs — GOOD" || echo "WARNING: /run not tmpfs"
```

On Ubuntu, `/run` is already tmpfs by default, so `/run/aberp` inherits that. The `echo` confirms it.

**✅ Success check:** prints `/run is tmpfs — GOOD`. If it warns instead, stop and investigate — you do not want decrypted secrets on persistent disk.

> ⚠️ Because `/run` is wiped on reboot, `/run/aberp/keycloak.env` disappears after a restart. Step 29 installs a systemd unit that re-decrypts it on every boot **before** docker starts, so a reboot doesn't strand Keycloak.

---

## 25. [on the server — SSH] Write the deploy script (decrypt → up)

This is the one command you run to (re)deploy. It decrypts secrets into tmpfs, then brings the stack up.

```bash
cd /opt/aberp-auth
cat > deploy.sh <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
cd /opt/aberp-auth
# Decrypt secrets into tmpfs (RAM only). sops must read the age key as root, so we
# decrypt as root — then hand the file to aberp, because `docker compose` runs as
# aberp and reads env_file: as that user (NOT via the daemon). Without the chown,
# aberp gets EACCES on the root:0600 file and the first `up` fails.
sudo install -d -m 700 -o aberp -g aberp /run/aberp
sudo SOPS_AGE_KEY_FILE=/etc/aberp/age.key sops --decrypt secrets/keycloak.env | sudo tee /run/aberp/keycloak.env > /dev/null
sudo chown aberp:aberp /run/aberp/keycloak.env
sudo chmod 600 /run/aberp/keycloak.env
docker compose up -d
echo "Deployed. Watch: docker compose logs -f keycloak"
EOF
chmod +x deploy.sh
```

**✅ Success check:** `cat deploy.sh` shows the script; `ls -l deploy.sh` shows it executable (`-rwxr-xr-x`).

---

## 26. [on the server — SSH] First deploy

```bash
cd /opt/aberp-auth
./deploy.sh
```

This pulls the Postgres + Keycloak images (first time: ~1–2 min) and starts both. Keycloak's first boot builds + imports its schema into Postgres — give it up to ~90 s.

Watch it come up:
```bash
docker compose logs -f keycloak
```
Wait for a line like `Keycloak 26.x on JVM … started in …s. Listening on: http://0.0.0.0:8080`. Press `Ctrl-C` to stop tailing (the container keeps running).

**✅ Success check:**
```bash
docker compose ps
```
`postgres` should show `running` and (after its start period) `healthy`. `keycloak` has **no container healthcheck** (dropped in Step 22), so it will just show `running` (or briefly `health: starting` if you left one in) — that is expected, **not** a failure. The real liveness proof is the external probe: confirm Keycloak answers locally (still no TLS — that's Step 30):
```bash
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8080/
```
prints `200` or `302`. **That HTTP code — not the `ps` health column — is the authoritative "Keycloak is alive" signal.** If it returns `000`/connection-refused, tail `docker compose logs -f keycloak` and wait for the `started in …s` line.

---

## 27. [on the server — SSH] Confirm Postgres is NOT reachable from outside

Prove the isolation invariant before exposing anything.

```bash
# From the host, Postgres port must be closed (no ports: mapping):
curl -s -m 3 http://127.0.0.1:5432 ; echo "exit=$?"      # connection refused expected
sudo ss -tlnp | grep 5432 || echo "5432 not listening on host — GOOD"
```

**✅ Success check:** prints `5432 not listening on host — GOOD`. Postgres is safe because it has **no `ports:` mapping** — it exists only on the internal docker network, so there is nothing published for anyone (host or internet) to reach. Note this is **not** because of ufw: Docker's iptables rules sit *ahead* of ufw, so if Postgres *did* publish a port to `0.0.0.0`, ufw's default-deny would **not** stop it. The no-`ports:` design is the real guarantee.

---

## 28. [on the server — SSH] Confirm the admin console is NOT yet publicly reachable

Keycloak is bound to `127.0.0.1:8080` — not `0.0.0.0`. From the internet it's invisible until Caddy fronts it (and even then, only over TLS). Verify the binding:

```bash
sudo ss -tlnp | grep 8080
```

**✅ Success check:** the `Local Address` shows `127.0.0.1:8080`, **not** `0.0.0.0:8080` or `*:8080`. If it shows `0.0.0.0`, the `ports:` line in Step 22 lost its `127.0.0.1:` prefix — fix it and redeploy. (Exposing the Keycloak admin console directly to the internet is a top-tier footgun. What prevents it is the **`127.0.0.1:` loopback bind** — *not* ufw, which Docker's iptables rules would bypass if the bind were `0.0.0.0`. The loopback prefix is load-bearing; guard it.)

---

## 29. [on the server — SSH] Make secrets survive a reboot (systemd oneshot)

`/run/aberp/keycloak.env` is wiped on reboot. Install a tiny systemd unit that re-decrypts it **before** docker starts, so the stack comes back cleanly after any restart.

```bash
sudo tee /etc/systemd/system/aberp-secrets.service > /dev/null <<'EOF'
[Unit]
Description=Decrypt ABERP Keycloak secrets into tmpfs before Docker
Before=docker.service
After=local-fs.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/sh -c 'install -d -m 700 -o aberp -g aberp /run/aberp && SOPS_AGE_KEY_FILE=/etc/aberp/age.key /usr/local/bin/sops --decrypt /opt/aberp-auth/secrets/keycloak.env > /run/aberp/keycloak.env && chown aberp:aberp /run/aberp/keycloak.env && chmod 600 /run/aberp/keycloak.env'

[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now aberp-secrets.service
```

**✅ Success check:** `sudo systemctl status aberp-secrets.service` shows `active (exited)`. Optionally rehearse a reboot: `sudo reboot`, wait ~30 s, `ssh aberp@<SERVER_IP>`, then `docker compose -f /opt/aberp-auth/docker-compose.yml ps` shows both containers back up with no manual step (`postgres` `healthy`, `keycloak` `running` — it has no healthcheck, see Step 22). That proves reboot-safety.

---

## PART 5 — TLS reverse proxy (Steps 30–31)

## 30. [on the server — SSH] Install Caddy and front Keycloak with real HTTPS

Caddy gets a Let's Encrypt cert automatically and proxies to Keycloak. Install from Caddy's official repo:

```bash
sudo apt install -y debian-keyring debian-archive-keyring apt-transport-https curl
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt update
sudo apt install -y caddy
```

**First, get your own public IP** — the admin surface is locked to it. From **your Mac** (the machine you administer from):

```bash
curl -s ifconfig.me ; echo
```

Note that address — call it `<YOUR_HOME_IP>`. You'll paste it into the Caddyfile below (as `<YOUR_HOME_IP>/32`).

Write the Caddyfile. This is **not** a bare reverse-proxy: it fences off the admin console and the `master` realm so only your IP can reach them — everyone else on the internet gets a `403` *before* the request ever touches Keycloak. The public OIDC endpoints (the `aberp` realm) stay open, as they must:

```bash
sudo tee /etc/caddy/Caddyfile > /dev/null <<'EOF'
auth.example.com {
    encode gzip

    # Admin surface (Keycloak /admin + the master realm) — operator IP only.
    # Everyone else gets 403 before the request reaches Keycloak.
    @admin path /admin* /realms/master/*
    handle @admin {
        @blocked not remote_ip <YOUR_HOME_IP>/32
        respond @blocked 403
        reverse_proxy 127.0.0.1:8080 {
            header_up X-Forwarded-Proto https
        }
    }

    # Everything else (the aberp realm's public OIDC endpoints) — open to all.
    reverse_proxy 127.0.0.1:8080 {
        header_up X-Forwarded-Proto https
    }
}
EOF
sudo systemctl reload caddy
```

Replace `<YOUR_HOME_IP>` with the address from `ifconfig.me` above (keep the `/32`). Validate before trusting it: `sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile` should print `Valid configuration`.

> 🚩 **This closes a real hole:** a bare `reverse_proxy` exposes `/admin` and the brute-forceable `master` realm login to the entire internet. The IP fence shuts that down now, not "later."
> 🚩 **If your home IP is dynamic** (most consumer ISPs): the `/32` allow-rule will start 403-ing *you* when your IP rotates — you'd re-run `ifconfig.me` and update the Caddyfile. If that's too fragile, the fallback is to **also enable Keycloak brute-force detection on the `master` realm** (Step 34 only turns it on for `aberp`): in the admin console pick the **master** realm → **Realm settings → Security defenses → Brute force detection → ON**. Do that *in addition to* (not instead of) the IP fence if you can hold a static IP; do it as the primary defense if you genuinely can't.

Caddy now: answers `:80` and 301-redirects to `:443`, obtains a Let's Encrypt cert for `auth.example.com` over HTTP-01 (this is why port 80 + the DNS A-record had to be right first), terminates TLS, and forwards to Keycloak with the `X-Forwarded-*` headers Keycloak trusts (`KC_PROXY_HEADERS=xforwarded`).

**✅ Success check:** watch the cert get issued:
```bash
sudo journalctl -u caddy -f | grep -i 'certificate obtained\|serving initial configuration'
```
Within ~30 s you should see `certificate obtained successfully` for `auth.example.com`. `Ctrl-C` to stop. If it errors about the challenge, re-verify Step 12 (`dig +short auth.example.com` = `<SERVER_IP>`) and Step 8 (port 80 open) and Cloudflare grey-cloud (🚩A7).

---

## 31. [your Mac — Terminal] Verify HTTPS from the outside world

```bash
curl -sI https://auth.example.com/ | head -5
curl -s https://auth.example.com/realms/master/.well-known/openid-configuration | head -c 200 ; echo
```

**✅ Success check:** the first prints an HTTP `200`/`302` with a valid TLS handshake (no cert warning). The second prints JSON starting `{"issuer":"https://auth.example.com/realms/master",...`. If you see the OIDC discovery JSON over HTTPS, **the infrastructure is up** — this is the milestone from the §0 box.

> Also verify plaintext is refused-upgraded: `curl -sI http://auth.example.com/ | grep -i location` shows a `Location: https://…` 301. No app traffic rides HTTP.

---

## PART 6 — Keycloak realm + ABERP client + MFA (Steps 32–33)

## 32. [Keycloak admin console — browser + YOU by hand] First login, create the realm

1. Retrieve the admin password from sops (on the server):
   ```bash
   sudo SOPS_AGE_KEY_FILE=/etc/aberp/age.key sops --decrypt /opt/aberp-auth/secrets/keycloak.env | grep KEYCLOAK_ADMIN_PASSWORD
   ```
   Copy the value (everything after `=`).
2. In your **browser**, go to **https://auth.example.com/admin/**. Log in with username `admin` and that password. *(Entering the admin password into the login form is yours to do by hand — I never type credentials into fields.)*
3. Top-left realm dropdown (says **Keycloak** / **master**) → **Create realm**. Name it **`aberp`**. Create.

> 🚩 Do your realm work in the **`aberp`** realm, never `master`. `master` is only for the admin account. The admin console (`/admin*`) and the `master` realm are **already IP-fenced at Caddy to your operator IP** (Step 30) — that restriction is live, not deferred. If your login here 403s, it's because your public IP changed; re-run `curl -s ifconfig.me` and update the `<YOUR_HOME_IP>/32` line in the Caddyfile (see the dynamic-IP note in Step 30).

**✅ Success check:** the realm dropdown now shows **`aberp`** selected, and the left nav shows Clients / Users / Authentication for that realm.

---

## 33. [Keycloak admin console — browser + YOU by hand] Create the ABERP OIDC client

Still in the **`aberp`** realm:

1. **Clients → Create client.**
   - Client type: **OpenID Connect**
   - Client ID: **`aberp-backend`**  ← this is the `client_id` ABERP will use
   - Next.
2. **Capability config:**
   - **Client authentication: ON** (this makes it a *confidential* client — required, since ABERP is a server-side relying party holding a secret).
   - **Authentication flow:** tick **Standard flow** (authorization code). Leave Direct access grants **off**.
   - Next.
3. **Login settings:**
   - **Valid redirect URIs:** `https://app.example.com/auth/callback` 🚩A2 🚩A3
     - Add a second for local dev: `http://localhost:8080/*` — note Keycloak only honours a `*` wildcard **at the end** of the URI, so a mid-path form like `http://localhost:*/auth/callback` may silently fail to match. Use a trailing-`*` form (e.g. `http://localhost:8080/*`, dev only) or pin the exact dev callback URL.
   - **Valid post-logout redirect URIs:** `https://app.example.com/*`
   - **Web origins:** `https://app.example.com`
   - Save.
4. **Set the client secret to the value we generated (so it matches sops):** go to the client → **Credentials** tab. You can either (a) copy the Keycloak-shown secret and re-encrypt it into sops, or (b) paste our pre-generated `ABERP_OIDC_CLIENT_SECRET`. The clean path is **(a)** — let Keycloak be the source of truth for its own client secret:
   - On the **Credentials** tab, copy the **Client secret** Keycloak shows.
   - On the server, update sops so ABERP later reads the same value:
     ```bash
     cd /opt/aberp-auth
     sudo EDITOR=nano SOPS_AGE_KEY_FILE=/etc/aberp/age.key sops secrets/keycloak.env
     # in the editor (nano; ^O to save, ^X to exit), set ABERP_OIDC_CLIENT_SECRET=<the secret you copied>, save & quit
     ```
     (`sops secrets/keycloak.env` opens the decrypted file in an editor and re-encrypts on save.)

> 🚩 The `ABERP_OIDC_CLIENT_SECRET` we generated in Step 19 was a placeholder so the file was complete; **Keycloak mints the authoritative client secret** when the client is created. Step 4a reconciles them. The Phase-2 code session reads this value from sops (§11), never from the compose file.

**✅ Success check:** the client `aberp-backend` exists, shows **Client authentication: On**, Standard flow enabled, and the redirect URI you set. The Credentials tab shows a secret, and `sops --decrypt … | grep ABERP_OIDC_CLIENT_SECRET` on the server matches it.

---

## 34. [Keycloak admin console — browser + YOU by hand] Set TOTP as the MFA baseline

Per ADR-0100 §0 decision 3: **TOTP required** is the cheap-but-safe baseline every login must clear.

Still in the **`aberp`** realm:

1. **Authentication → Required actions.**
2. Find **Configure OTP** → toggle **Set as default action: ON**. (Now every new user is forced to enrol a TOTP authenticator at first login.)
3. Turn on brute-force protection: **Realm settings → Security defenses → Brute force detection → ON** (leave the defaults; this is the ADR's "brute-force detection on" gate item).

> **WebAuthn step-up is NOT configured here** — that's a Phase-2 *code+config* task. ADR-0100 §3 Phase 2 maps the NAV-irreversible routes (invoice submit, storno, restore/recover-from-nav) to a higher Keycloak **ACR/LoA** that ABERP requests via `acr_values`. Configuring that requires the ABERP relying-party code to exist first (to know which routes request step-up). Where it will live: **Authentication → Flows** (a step-up flow with a WebAuthn authenticator) + an **ACR-to-LoA** mapping in the realm. Left as a documented Phase-2-code handoff, not done now. 🚩

**✅ Success check:** **Configure OTP** shows **Default action: On** under Required actions. Brute force detection shows **Enabled** in Security defenses. (You can leave user creation to the Phase-2 code session, or create your own user now under **Users** — at first login it will force TOTP enrolment, which confirms the baseline works.)

---

## PART 7 — Backups + handoff (Steps 35–37)

## 35. [on the server — SSH] Schedule an encrypted Postgres backup

Keycloak's entire state (realm, the `aberp-backend` client, users, TOTP enrolments) lives in Postgres. Back it up nightly, **encrypted with age**, so a dump is useless to anyone without the master key.

```bash
cd /opt/aberp-auth
AGE_PUB="$(sudo grep -oE 'age1[0-9a-z]+' /etc/aberp/age.key | head -1)"
cat > backup.sh <<EOF
#!/usr/bin/env bash
set -euo pipefail
cd /opt/aberp-auth
STAMP="\$(date -u +%Y%m%dT%H%M%SZ)"
OUT="backups/keycloak-\${STAMP}.sql.age"
docker compose exec -T postgres pg_dump -U keycloak keycloak \
  | age -r "${AGE_PUB}" -o "\${OUT}"
# keep 14 days
find backups -name 'keycloak-*.sql.age' -mtime +14 -delete
echo "Backup written: \${OUT}"
EOF
chmod +x backup.sh
```

Schedule it nightly at 03:15 via cron:
```bash
( crontab -l 2>/dev/null; echo "15 3 * * * /opt/aberp-auth/backup.sh >> /opt/aberp-auth/backups/backup.log 2>&1" ) | crontab -
```

Run it once now to prove it works:
```bash
./backup.sh
```

> **Where the backups live and the offline rule:** the encrypted dumps sit in `/opt/aberp-auth/backups/` on the server. Because they're age-encrypted, they're safe to copy anywhere — but a backup on the same box it protects is not a real backup. 🚩 **Pushing these dumps + a copy of the age public key off-machine** (to an object store) is ADR-0100 **Phase 4** work (off-machine encrypted backup) and is **not automated here** — for now, periodically `scp` the newest `backups/*.sql.age` to your Mac / offline drive by hand. The **age private key** backup (Step 18) is the *other* half — a dump is unrecoverable without it. Keep both, in different places.

**✅ Success check:** `ls -lh backups/` shows a `keycloak-<stamp>.sql.age` file of non-trivial size (tens of KB+). `crontab -l` shows the nightly line.

---

## 36. [reference] Restore drill (one paragraph — read it before you need it)

**To restore Keycloak from a dump:** on a server that has the **age private key** at `/etc/aberp/age.key` and the stack deployed (Steps 13–29), decrypt and load the dump into a *fresh* Postgres. Bring the stack down and wipe the volume first so you import into a clean DB: `cd /opt/aberp-auth && docker compose down && docker volume rm aberp-auth_pgdata && docker compose up -d postgres` (wait for `healthy`), then `sudo age -d -i /etc/aberp/age.key backups/keycloak-<stamp>.sql.age | docker compose exec -T postgres psql -U keycloak -d keycloak`, then `docker compose up -d keycloak`. Verify by logging into `https://auth.example.com/admin/` and confirming the `aberp` realm + `aberp-backend` client are present. **Note the `sudo` on `age -d`:** the private key is `0400 root` (Step 16), so decrypting as `aberp` without `sudo` fails with permission-denied — exactly when you can least afford it. **The volume name** `aberp-auth_pgdata` is `<compose-project>_<volume>`; the project defaults to the directory name (`aberp-auth`), and `postgres` is the compose *service* name used by `docker compose exec` — both hold as long as you run these from `/opt/aberp-auth` with the Step 22 compose file unchanged (confirm with `docker volume ls | grep pgdata`). **The drill's whole point:** if you cannot decrypt the dump, you've lost the age key — which is why Step 18 (offline age-key backup) is the most important backup in this system. Rehearse this restore **once** on a throwaway Hetzner box before you rely on it in anger.

---

## 37. [ABERP repo — handoff] Exactly what the Phase-2 code session needs

The later ABERP↔Keycloak OIDC code session (a **separate** session — no ABERP code was written here) needs these four values and nothing more:

| Value | What it is | Where to get it |
| --- | --- | --- |
| **Issuer URL** | `https://auth.example.com/realms/aberp` | Fixed by 🚩A1 + the realm name (Step 32). Confirm via `curl -s https://auth.example.com/realms/aberp/.well-known/openid-configuration` |
| **Client ID** | `aberp-backend` | Set in Step 33. |
| **Client secret** | the confidential-client secret | In sops on the server: `sudo SOPS_AGE_KEY_FILE=/etc/aberp/age.key sops --decrypt /opt/aberp-auth/secrets/keycloak.env \| grep ABERP_OIDC_CLIENT_SECRET`. Behind the ADR-0100 Phase-1 `SecretStore` seam when ABERP consumes it — never in the compose file, never in `seller.toml`. |
| **Realm name** | `aberp` | Step 32. |

Plus two facts the code session must honor (from ADR-0100 §3 Phase 2):
- The **redirect URI** it registers must match the client (🚩A3 `https://app.example.com/auth/callback`) — update the Keycloak client (Step 33) if the code chooses a different path.
- **TOTP is enforced at the IdP** (Step 34); **WebAuthn step-up** for NAV-irreversible routes is requested by ABERP via `acr_values` and enforced by checking the returned `acr` claim — that Keycloak flow is configured *together with* the code that requests it (Step 34's flagged deferral).

**✅ Success check (the whole walkthrough):** from any browser, `https://auth.example.com/realms/aberp/.well-known/openid-configuration` returns OIDC discovery JSON with `"issuer":"https://auth.example.com/realms/aberp"`, and the `aberp-backend` client + TOTP-required action exist in the realm. That is a working Keycloak, ready for the Phase-2 code session. **Nothing about the ABERP desktop deployment changed.**

---

## Footgun checklist (read once, top to bottom)

- ✅ **ufw:** allowed 22 **before** `ufw enable` (Step 8). Never enable-then-allow.
- ✅ **Proved `aberp` login + sudo** (Step 7) **before** disabling root/passwords (Step 9).
- ✅ **age private key backed up OFFLINE** (Step 18). Losing it = losing every secret, permanently.
- ✅ **age.key never committed**, decrypted secrets only ever on **tmpfs** (`/run/aberp`, Steps 24/27).
- ✅ **Postgres has no `ports:`** and is on an internal network (Steps 22/27) — never internet-reachable. This is the *real* guarantee; **not** ufw (Docker's iptables sit ahead of ufw — never publish a container to `0.0.0.0` expecting ufw to save you).
- ✅ **Keycloak bound to `127.0.0.1:8080`** (Step 28) — the loopback bind (not ufw) is what keeps it off the internet.
- ✅ **Admin surface IP-fenced at Caddy** (Step 30): `/admin*` + `/realms/master/*` 403 for any IP but yours. If dynamic, also enable master-realm brute-force detection.
- ✅ **Real Let's Encrypt TLS** (Step 30); HTTP 301-redirects to HTTPS; no plaintext app traffic.
- ✅ **Strong generated passwords** (Step 19), retrieved from sops, never typed/reused.
- ✅ **Cloudflare grey-cloud** during bring-up (🚩A7) so ACME HTTP-01 works.
