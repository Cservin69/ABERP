# Always-on Thunderbolt link: MacBook (DB) ⇄ Mac Studio (client)

ADR-0111 (remote client) and ADR-0112 (issue idempotency key). The production DB, the NAV
credentials, the SMTP password and every other secret stay on the MacBook. The Studio holds no
database and only one secret: its client bearer token. If the MacBook isn't there, the Studio
fails closed with a clear message. There is no offline mode.

| | MacBook (user `aben`) | Mac Studio (user `abenm5`) |
|---|---|---|
| Thunderbolt Bridge IPv4 | `10.69.0.1/30` | `10.69.0.2/30` |
| ABERP role | `aberp serve` (LaunchAgent), DB at `~/.aberp/prod/` | `aberp-ui` in remote mode |
| Listens on | `127.0.0.1:18443` (own window), `10.69.0.1:8443` on `bridge0` | nothing |

## 1. Fixed bridge addresses (both Macs, once)

1. Open System Settings → Network → **Thunderbolt Bridge** → Details… → TCP/IP.
2. Set Configure IPv4 to **Manually**.
3. Enter the IP address and subnet mask (255.255.255.252). On the MacBook, 10.69.0.1; on the
   Studio, 10.69.0.2.
4. Leave Router **empty**. That way the bridge never becomes a default route.

`networksetup -setmanual` insists on a router argument, so use the GUI. Check on each Mac:

```bash
ifconfig bridge0 | grep "inet "
```

```bash
netstat -rn -f inet | grep default
```

The `inet` line must show the new address. The default route must not be `bridge0`.

## 2. MacBook stays awake (once)

On the charger it never sleeps. On battery it sleeps after 60 idle minutes. The Studio is on a
UPS, so a short power cut never drops the link.

```bash
sudo pmset -c sleep 0 displaysleep 10 disksleep 0 standby 0 powernap 0 tcpkeepalive 1 womp 1
```

```bash
sudo pmset -b sleep 60 displaysleep 5 disksleep 0 standby 0 powernap 0 tcpkeepalive 1
```

```bash
pmset -g custom
```

Keep the lid **open**. A closed lid forces sleep on battery, and on the charger without an
external display.

## 3. Bridge-only SSH on the MacBook (optional, once)

You only need this so the Studio can pull its token and fingerprint in step 5. Under launchd,
macOS's `sshd` runs per connection, so `ListenAddress` has no effect. Restrict by local address
instead. First check that the drop-in directory is included:

```bash
grep -n "^Include" /etc/ssh/sshd_config
```

```bash
printf 'AllowUsers aben\nMatch LocalAddress *,!10.69.0.1\n    DenyUsers *\n' | sudo tee /etc/ssh/sshd_config.d/10-aberp-bridge-only.conf && sudo sshd -t && echo OK
```

Then turn on System Settings → General → Sharing → **Remote Login** → *Only these users: aben*.
Over Wi-Fi or Ethernet the TCP port answers, but every login is refused.

## 4. First run on the MacBook

Do this after the branch is released (see §8). Quit the running prod app first with Ctrl-C in
its terminal. Then run:

```bash
cd ~/ABERP && ABERP_REMOTE_BIND=10.69.0.1:8443 ABERP_REMOTE_PEER=10.69.0.2 ./run/upgrade_prod.sh PROD_v2.36.0
```

`upgrade_prod.sh` does the following, then `exec`s `run_prod.sh` with your environment:
- takes the encrypted snapshot (it prompts twice for the snapshot password);
- switches to the release;
- runs `run_prod.sh`, which builds `--features production`.

No separate migration step is needed: schema changes apply at backend boot, and ADR-0111/0112
add no schema. On this first boot the backend mints `remote_session_token` in the MacBook
keychain. Expect two prompts:
- the keychain will ask for the new item, so click **Always Allow**;
- the macOS firewall may ask to allow incoming connections for `aberp`, so click **Allow**.

## 5. Hand the Studio its token and fingerprint (MacBook, once)

Run this in the MacBook's own Terminal, not over SSH, so the keychain can ask you:

```bash
mkdir -p -m 700 ~/.aberp/handoff && (umask 077; security find-generic-password -s aberp.nav.prod -a remote_session_token -w > ~/.aberp/handoff/remote_session_token && cp ~/.aberp/serve/prod/loopback.fingerprint.sha256 ~/.aberp/handoff/) && ls -l ~/.aberp/handoff
```

The Studio side (Claude) then pulls both files with
`scp aben@10.69.0.1:.aberp/handoff/* …`, stores the token in the Studio keychain, writes
`~/.aberp/remote/prod.env` (mode 600), and deletes its own copies. After that, remove the
MacBook copy:

```bash
rm -P ~/.aberp/handoff/remote_session_token ~/.aberp/handoff/loopback.fingerprint.sha256 && rmdir ~/.aberp/handoff
```

Nothing NAV-related is copied. The Studio shell's only keychain read is this one token.

## 6. Backend at login: LaunchAgent (MacBook, once)

Quit the app started in §4 (Ctrl-C). Then:

```bash
cp ~/ABERP/run/launchd/ch.aben.aberp.serve.plist ~/Library/LaunchAgents/ && launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/ch.aben.aberp.serve.plist && sleep 3 && tail -5 ~/Library/Logs/aberp-serve.log
```

Check that only the bridge address and loopback are listening:

```bash
lsof -nP -iTCP -sTCP:LISTEN | grep aberp
```

You should see exactly `127.0.0.1:18443` and `10.69.0.1:8443`.

## 7. Daily use

MacBook window. From `PROD_v2.36.1` on, `run_prod.sh` finds the LaunchAgent backend by itself and
attaches to it (ADR-0111 addendum 2). If the agent is loaded but unhealthy it refuses, rather than
starting a second backend.

```bash
cd ~/ABERP && ./run/run_prod.sh
```

Studio window. Clone the same release once, then run:

```bash
git clone --branch PROD_v2.36.0 https://github.com/Cservin69/ABERP ~/ABERP && cd ~/ABERP && ./run/run_remote_client.sh
```

On later days:

```bash
cd ~/ABERP && ./run/run_remote_client.sh
```

## 8. Releases and upgrades

Release as usual: `./run/release.sh PROD_vX.Y`.

**`origin/PROD_v2.35.3` carries 3 hotfixes that are not on `main`:**
- `d25e2d1` run_prod self-clear
- `8c5045e` ap-ui confirm
- `2180e18` products stock column

Merge them back into `main` before releasing from `main`, or the release drops them. The ADR-0111
and ADR-0112 change applies cleanly on top of `PROD_v2.35.3`.

Upgrading the MacBook. Close the prod window first. From `PROD_v2.36.1` on, `upgrade_prod.sh`
handles the rest:
1. it stops the LaunchAgent;
2. snapshots and switches;
3. `run_prod.sh` restarts the agent on the new build, waits for Ready, and opens the window
   attached to it.

```bash
cd ~/ABERP && ./run/upgrade_prod.sh PROD_vX.Y
```

**Only for 2.36.0 → 2.36.1:** that upgrade is performed by the older script, so stop the agent
by hand in the same line:

```bash
launchctl bootout gui/$(id -u)/ch.aben.aberp.serve; cd ~/ABERP && ./run/upgrade_prod.sh PROD_v2.36.1
```

Then upgrade the Studio to the same branch:

```bash
cd ~/ABERP && git fetch && git checkout -B PROD_vX.Y origin/PROD_vX.Y && ./run/run_remote_client.sh
```

## 9. When the link is down

The Studio's boot pane, or the failing action, says the backend is unreachable and that nothing
was issued on the Studio. Reconnect or wake the MacBook and press Retry.

A retry of an issue whose response was lost returns the invoice that was already issued. It does
not create a second one (ADR-0112).
