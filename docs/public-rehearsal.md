# The public rehearsal

The one exercise nothing local can stand in for: a real VM, a real domain, a
real public IP, and a certificate a stranger's browser trusts.

Everything else has been rehearsed on containers — the fresh-host recovery
(deployment §7.6), the upgrade and its rollback (§5.4), and a full deployment
from the release tarball with the firewall applied and the proxy serving both
hostnames (§11, "The throwaway deployment"). Those runs found and fixed a
`/metrics` endpoint answering the internet, a `caddy validate` that broke the
first `systemctl start`, an `nft -c` that had never been run, and a bundle check
that could never pass. What they could not touch is anything downstream of a
publicly resolvable name:

| Still unproven | Why a container could not do it |
|---|---|
| Let's Encrypt issuance over ACME HTTP-01 | needs a public name and inbound :80 |
| Certificate **renewal** | needs an account, a real clock, and the issuer |
| `certbot`, its two lineages, and its renewal timer | never run at all (§11) |
| nginx's `/.well-known/acme-challenge/` location | never served a challenge |
| The firewall against unsolicited internet traffic | it was exercised as a ruleset, not against scanners |
| **x86_64** | every binary so far has been `aarch64` |
| HSTS and the chain as a browser sees them | no publicly trusted chain existed |

This document is the sequence. It is written to be run top to bottom in one
sitting by one person at a terminal, with the gates called out — a gate is a
place where the next step is wrong if the previous one did not do what it says.

> **Use a throwaway name, not the launch domain.** A subdomain you are willing
> to burn, e.g. `try.example.com` and `hub.try.example.com`. Let's Encrypt
> rate-limits failed validations per account and per hostname, and the first
> real ACME run on a config nobody has issued with is exactly where failures
> happen. Burning the limit on a name you are about to launch on is a bad
> afternoon; burning it on a throwaway is a Tuesday.

> **Treat the box and its keys as disposable.** The hub mints an operator key,
> a custody key and an escrow secret on first boot, and they are real custody
> for whatever chain this box runs. Generate fresh ones for launch — do not
> carry this box's keys forward because they happen to work.

---

## 0. What you need in hand

- A **VM**: x86_64, Debian 12 or Ubuntu 24.04 (the two this documentation is
  written against — see §4's `listen … http2` note for why the version matters),
  1 vCPU and 1 GB is enough, with a public IPv4 **and** a public IPv6 address.
  Take the IPv6; half of what this rehearsal is for is the second family.
- **DNS you can edit**, for the throwaway name.
- The **release tarball**, `itx-<version>-x86_64-linux.tar.gz`, from a CI run —
  not a local build. This is the first time the x86_64 binaries will be run, and
  a tarball you built yourself proves less.
- An **age keypair for backups**, generated on your laptop, whose private half
  never touches the VM (§7.1):
  ```bash
  age-keygen -o ~/itx-throwaway-restore-key.txt   # keep this OFF the box
  grep 'public key' ~/itx-throwaway-restore-key.txt
  ```
- A **second machine** to check from. Your laptop is fine as long as it is not
  the box and has nothing on port 9100 (§5.1).

Fill these in once and paste the block into every shell you open on the box:

```bash
export ITX_SITE=try.example.com          # the board
export ITX_API=hub.try.example.com       # the API
export ITX_EMAIL=ops@example.com         # a real inbox: expiry notices land here
export ITX_AGE_RECIPIENT=age1...         # the PUBLIC half
export ITX_REL=itx-v0.1.0-x86_64-linux   # the unpacked release directory
```

---

## 1. DNS first, and wait for it

Both names, both families, both to this box:

```
try.example.com.       A     203.0.113.10
try.example.com.       AAAA  2001:db8::10
hub.try.example.com.   A     203.0.113.10
hub.try.example.com.   AAAA  2001:db8::10
```

**Gate.** From your laptop, not the box:

```bash
dig +short A    "$ITX_SITE" "$ITX_API"
dig +short AAAA "$ITX_SITE" "$ITX_API"
```

All four must answer with the box's addresses before you go on. Caddy retries a
name that does not resolve yet, but certbot does not — and an ACME failure
against a name that was not ready counts against the rate limit exactly as much
as a real one.

---

## 2. The firewall, before anything listens

This is the step whose order is not negotiable. The node binds `0.0.0.0` with an
**unauthenticated wire protocol** and has no `--bind` flag; the ruleset is the
only thing between it and the internet (§1). Apply it while the only thing
listening is `sshd`.

```bash
sudo apt-get update && sudo apt-get install -y nftables netbase
sudo cp "$ITX_REL/deploy/nftables.conf" /etc/nftables.conf
sudo nft -c -f /etc/nftables.conf        # gate: must pass before applying
sudo systemctl enable --now nftables
sudo nft -a list table inet itx | head -20
```

`netbase` is not decoration: `nft` resolves `ipv6-icmp` through `/etc/protocols`,
and without it the check fails naming the rule rather than the missing file
(§3). Most cloud images have it; install it anyway.

**Gate.** From your laptop, before anything else is installed:

```bash
ssh "$ITX_SITE" true && echo "ssh survived the ruleset"
```

If that fails you are locked out of a box you have not yet put anything on,
which is the cheapest possible time to find out.

---

## 3. Install the release, per §5

```bash
tar xzf itx-*-x86_64-linux.tar.gz && cd "$ITX_REL"
sha256sum -c SHA256SUMS                  # gate: the transfer, not the build

sudo useradd --system --home /var/lib/itx --shell /usr/sbin/nologin itx
sudo mkdir -p /var/lib/itx/secrets
sudo chown -R itx:itx /var/lib/itx
sudo chmod 700 /var/lib/itx/secrets

for b in node hub miner console wallet; do sudo install -m 0755 "itx-$b" "/usr/local/bin/itx-$b"; done
sudo cp deploy/itx-*.service deploy/itx-*.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemd-analyze verify /etc/systemd/system/itx-*.service /etc/systemd/system/itx-*.timer   # gate: must be silent
```

The miner needs a **public** key to pay coinbase to, and the release tarball
does not contain a key generator — deliberately, since §5.1's argument against
putting a build toolchain beside the treasury applies to `cargo` as much as to
`node`. So generate the pair **on your laptop**, from a checkout, and carry only
what the box needs (§6.5):

```bash
# on your laptop, in a checkout
cargo run --release -p btclib --bin key_gen -- operator
#  -> operator.pub.pem     public: the address block rewards are paid to
#  -> operator.priv.cbor   spends every reward, and is the treasury
scp operator.pub.pem operator.priv.cbor "$ITX_SITE:"
```

On a throwaway, pointing the miner at the operator's own address is what gives
the hub coin to fund faucet grants and bounties with. A real deployment keeps
them separate (§6.5) and funds the operator by a transfer instead — which is
worth knowing now, because **the tooling for that transfer is not in the release
either** (see §9.10). For a throwaway, mine to the operator:

```bash
sudo install -o itx -g itx -m 600 operator.priv.cbor /var/lib/itx/secrets/hub_operator.priv.cbor
sudo install -o itx -g itx -m 644 operator.pub.pem   /var/lib/itx/miner.pub.pem
shred -u operator.priv.cbor     # off the box you carried it through
```

Pre-placing the operator key means the hub's `--generate-keys` creates only the
custody key and the escrow secret on first boot, which is the documented
first-boot path one step earlier (§6).

---

## 4. Start the stack, node first

`After=itx-node.service` orders the starts and does **not** wait for the node's
socket, and the node replays the whole chain before it binds. Starting all three
together leaves `hub_operator_fan_out_failures_total` non-zero on a box that is
otherwise fine (§7.6), so do it in sequence:

```bash
sudo systemctl enable --now itx-node
sudo journalctl -u itx-node -b -o cat -f     # wait for "Listening on", then ^C
sudo systemctl enable --now itx-hub itx-miner
```

**Gate.** Read the banner rather than tailing past it, and **write the three
addresses down** — you will compare against them after every restore:

```bash
sudo journalctl -o cat -u itx-hub -b | head -40
curl -s localhost:9100/health
curl -s localhost:9100/metrics | grep -E 'fan_out_failures_total|ready_outputs'
```

`fan_out_failures_total` should read `0`. If it does not, you started them
together.

---

## 5. The site, per §5.1

```bash
sudo mkdir -p /var/www/itx
sudo cp -r "$ITX_REL/site/." /var/www/itx/
sudo chown -R root:root /var/www/itx
sudo sed -i "s|content=\"https://hub.itx.example.com\"|content=\"https://$ITX_API\"|" \
    /var/www/itx/index.html
grep -o 'itx-hub-url" content="[^"]*"' /var/www/itx/index.html
```

That one line is the whole configuration of the site. The shipped placeholder is
deliberately treated as *unset*, so skipping it gives a page that loads
perfectly and can never reach a hub.

---

## 6. The proxy, and the part that has never been done

Install the config and substitute the names:

```bash
sudo cp "$ITX_REL/deploy/Caddyfile" /etc/caddy/Caddyfile
sudo sed -i "s/hub\.itx\.example\.com/$ITX_API/g; s/itx\.example\.com/$ITX_SITE/g; s/ops@example\.com/$ITX_EMAIL/" \
    /etc/caddy/Caddyfile
sudo install -Dm644 "$ITX_REL/deploy/security.txt" /etc/caddy/well-known/security.txt
grep -nE "^($ITX_SITE|$ITX_API) \{" /etc/caddy/Caddyfile   # gate: exactly two, in that order
```

The API hostname is substituted **first** and the apex second, because the apex
pattern is a substring of the API one.

### 6a. Staging ACME first

```bash
sudo sed -i "s|^\temail |\tacme_ca https://acme-staging-v02.api.letsencrypt.org/directory\n\temail |" \
    /etc/caddy/Caddyfile
grep -n 'acme_ca' /etc/caddy/Caddyfile        # gate: exactly one line, inside the global block
sudo -u caddy caddy validate --config /etc/caddy/Caddyfile
sudo systemctl restart caddy
sudo journalctl -u caddy -f -o cat            # watch the issuance, then ^C
```

(The `grep` is there because an insertion that silently matched nothing is a
config that goes straight to the production issuer — which is the one thing this
step exists to avoid. The tab in `^\temail ` is a real tab; the Caddyfile is
tab-indented, and there is exactly one such line. This is GNU `sed`, as on the
target: BSD `sed` on a Mac does not read `\n` in the replacement as a newline
and will do nothing at all.)

**`-u caddy` on the validate.** Validate provisions the config, which opens the
access log; run as root it creates `/var/log/caddy/itx-access.log` owned by root
and the service — which runs as `caddy` — then cannot start. It says "Valid
configuration" and the failure is a permission error in the journal (§4.9).

**Gate.** The journal must show a certificate obtained for each name. Staging's
root is not in any trust store, so the client needs `-k` here and only here:

```bash
curl -sk https://$ITX_SITE/health                     # the hub's body
curl -sk -o /dev/null -w '%{http_version}\n' https://$ITX_API/tasks
```

If HTTP-01 failed, this is the moment to read why — a missing AAAA, a firewall
that does not have :80 open, a name that resolves elsewhere. Fix it here, where
failures are free.

### 6b. Then production, once

```bash
sudo sed -i "/acme_ca https:\/\/acme-staging/d" /etc/caddy/Caddyfile
sudo -u caddy caddy validate --config /etc/caddy/Caddyfile
sudo systemctl restart caddy
sudo journalctl -u caddy -f -o cat          # watch it again
```

**Gate, from your laptop, with no `-k` anywhere:**

```bash
curl -sS -w '\n%{http_code} %{http_version} verify=%{ssl_verify_result}\n' https://$ITX_SITE/health
echo | openssl s_client -connect "$ITX_SITE:443" -servername "$ITX_SITE" 2>/dev/null \
  | openssl x509 -noout -issuer -subject -dates -ext subjectAltName
```

`verify=0`, an issuer that says Let's Encrypt, and a `notAfter` about ninety days
out. **This is the line no rehearsal so far has been able to produce.**

### If you are using nginx instead

Two certificates, two lineages — one `certbot` invocation per name, never one
with two `-d` flags, because the two `server` blocks name two separate paths
under `/etc/letsencrypt/live/` (see the header of `deploy/nginx.conf`). Issue
them **before** the config is installed and nginx is running, with certbot's
own listener on :80, because nginx will not start on a missing certificate and
`certonly --nginx` would have nothing to drive:

```bash
sudo certbot certonly --standalone -d "$ITX_SITE" \
     --pre-hook 'systemctl stop nginx' --post-hook 'systemctl start nginx'
sudo certbot certonly --standalone -d "$ITX_API" \
     --pre-hook 'systemctl stop nginx' --post-hook 'systemctl start nginx'
# now install deploy/nginx.conf, `sudo nginx -t`, `sudo systemctl enable --now nginx`
sudo certbot renew --dry-run          # gate: the renewal path, hooks included -- nginx blips
systemctl list-timers | grep certbot  # gate: the timer exists and is scheduled
```

The hooks are the part that matters in sixty days: `certonly` records no
installer, so without them a renewal succeeds and nginx keeps serving the
expired certificate.

`certbot renew --dry-run` is the single most valuable command in this document
for an nginx deployment, because renewal is the part that fails silently three
months after everyone has stopped looking.

---

## 7. The off-box battery

All of this from the second machine. These are the checks that were run against
the container deployment (§11) and are worth repeating where the addresses are
real.

```bash
# The public surface
curl -sS -w '  %{http_code} %{http_version}\n' https://$ITX_SITE/health
curl -sS -o /dev/null -w '  apex   %{http_code}\n' https://$ITX_SITE/
curl -sS -o /dev/null -w '  api    %{http_code}\n' https://$ITX_API/tasks
curl -sS -o /dev/null -w '  status %{http_code}\n' https://$ITX_SITE/status
curl -sS -o /dev/null -w '  llms   %{http_code}\n' https://$ITX_SITE/llms.txt
curl -sSI https://$ITX_SITE/ | grep -i strict-transport-security

# Over IPv6 specifically -- the family nothing has yet reached this stack on
curl -6 -sS -o /dev/null -w '  v6 apex %{http_code}\n' https://$ITX_SITE/
curl -6 -sS -o /dev/null -w '  v6 api  %{http_code}\n' https://$ITX_API/tasks

# The two internal ports, both families. Both must fail.
for p in 9100 9000; do
  nc -zv -w4 "$ITX_SITE" $p 2>&1 | sed 's/^/  v4 /'
  nc -6 -zv -w4 "$ITX_SITE" $p 2>&1 | sed 's/^/  v6 /'
done

# /metrics is the treasury's operational picture. It must not answer.
curl -sS -o /dev/null -w '  metrics %{http_code}  (must be 404)\n' https://$ITX_API/metrics

# security.txt, on both names
curl -sS -o /dev/null -w '  %{http_code} %{content_type}\n' https://$ITX_SITE/.well-known/security.txt
curl -sS -o /dev/null -w '  %{http_code} %{content_type}\n' https://$ITX_API/.well-known/security.txt
```

Then **on the box**, confirm the firewall did the refusing and the node never
saw it — the pair of claims §3 and §8.1 make together:

```bash
sudo nft -a list table inet itx | grep -E 'dport (9100|9000)'    # counters non-zero
sudo journalctl -u itx-node -b | grep -ci banning                # must be 0
```

A non-zero ban count means packets reached the node, which means the ruleset is
not doing its job.

**And the spoofing check**, which is the one line in the proxy config that is a
security control rather than tuning (§4.2):

```bash
for i in $(seq 1 130); do
  curl -s -o /dev/null -w '%{http_code}\n' \
    -H "X-Forwarded-For: 203.0.113.$((i % 250 + 1))" -H "X-Real-IP: 198.51.100.9" \
    https://$ITX_API/tasks
done | sort | uniq -c
```

A mix of `200` and `429` is correct: one shared bucket, and the spoofing bought
nothing. All `200` means the header is reaching the hub and the rate limiter is
charging whatever the client claimed.

---

## 8. A real agent, over the real internet

The end-to-end one. From the second machine, against the public name — this is
what an arriving agent does, and it exercises TLS, the path passthrough that the
signed envelope depends on (§4.6), the faucet's proof of work, and settlement:

```bash
pip install ./agent-sdk-py                 # from a checkout; not on PyPI yet
export ITX_HUB_URL="https://$ITX_API"      # or pass --hub-url to every command

itx-agent whoami        # generates a keypair on first run and prints the pubkey
itx-agent health        # gate: reaches the hub over the public name and TLS
itx-agent llms          # the manual, served by the hub itself
itx-agent faucet        # solves the proof of work and funds this identity
itx-agent find          # open tasks this identity can claim
itx-agent claim <task_id>
itx-agent submit <task_id> <answer>
itx-agent status        # reputation and what this identity has done
```

You need a task on the board to claim. Post one from the box with the operator
key, or use `hub/examples/smoke_agent` — which drives the whole loop itself and
was what the container rehearsal used.

**Gate.** Every signed write returns `200` and none returns `401`. A single
`401` means the proxy is rewriting the path, and the rate would be 100% rather
than occasional. Then watch the bounty settle:

```bash
curl -s "https://$ITX_API/tasks?status=all" | jq '[.[] | {id, status, bounty_confirmed}]'
```

`Paid` with a confirmed bounty closes it. `Submitted` for more than a couple of
minutes is §9.10.

---

## 9. Backups, and a restore that leaves the box

```bash
sudo install -m 0755 "$ITX_REL/deploy/itx-backup.sh"       /usr/local/bin/
sudo install -m 0755 "$ITX_REL/deploy/itx-restore-drill.sh" /usr/local/bin/
sudo /usr/local/bin/itx-backup.sh --recipient "$ITX_AGE_RECIPIENT"
```

**Gate.** The hub is down for the length of the copy and no longer: the
container run measured ~0.2 s (§11). If it is minutes, something has regressed
to stopping the hub for the whole archive.

Pull the archive to your laptop — an archive that only exists on the box it
backs up is not a backup — and run the drill there, where the identity file
lives:

```bash
scp "$ITX_SITE:/var/backups/itx/itx-*.tar.gz.age" .
itx-restore-drill.sh --archive itx-*.tar.gz.age \
  --identity ~/itx-throwaway-restore-key.txt \
  --expect-operator "<the address you wrote down in step 4>" \
  --unit-file deploy/itx-hub.service
```

---

## 10. Recovery on a real host

This is the step that closes the last caveat on §7.6: the recovery has been
rehearsed four times and never on a machine with real disks and a real network.

Provision a **second** VM, do nothing to it, and run the rehearsed script:

```bash
sudo ./itx-recover.sh \
    --archive  itx-<stamp>.tar.gz.age \
    --identity ~/itx-throwaway-restore-key.txt \
    --release-dir ./$ITX_REL \
    --expect-operator "<the address from step 4>"
```

**Record the elapsed time it prints.** That number, on real hardware with a real
chain, is the RTO — the container runs said two seconds and said plainly not to
believe it.

---

## 11. Renewal, which is the part you cannot finish today

Caddy renews automatically at about thirty days out; certbot's timer does the
same. Neither can be observed in an afternoon, so do what can be done now:

```bash
# Caddy: force one, and watch it go through the real issuer
sudo caddy reload --config /etc/caddy/Caddyfile --force
sudo journalctl -u caddy -b | grep -iE 'certificate|renew|obtain'

# nginx/certbot: the dry run IS the renewal path
sudo certbot renew --dry-run
```

Then **leave the box up for a week** if you can, and check once:

```bash
echo | openssl s_client -connect "$ITX_SITE:443" 2>/dev/null | openssl x509 -noout -dates
```

And confirm the address in `$ITX_EMAIL` actually receives mail. That inbox is
how you find out a renewal has been failing for two weeks, and an address nobody
reads is the same as no address.

---

## 12. What to write down

The point of a rehearsal is the record. Capture these and put them in the
checklist row rather than in a memory:

- The **issuer, subject and validity window** of both certificates.
- `verify=0` from a clean client, and the HSTS header as served.
- The **firewall counters** for 9100 and 9000, and the node's ban count (`0`).
- The **spoofing result** — the `200`/`429` split.
- The **agent journey**: task id, status, confirmed bounty, and that no signed
  write returned `401`.
- The **hub downtime** during the backup.
- The **RTO** from step 10, on real hardware.
- Anything that did not match this document. Four things did not, last time, and
  all four are now fixed in it.

---

## 13. Tear it down

```bash
sudo systemctl disable --now itx-hub itx-miner itx-node caddy
```

Then destroy both VMs and **remove the DNS records**. A dangling A record
pointing at a recycled cloud address is somebody else's box answering for your
name, and the certificate you just issued is in the public CT logs either way.

Do not carry this box's keys to launch. Generate fresh ones there; the whole
value of a throwaway is that throwing it away costs nothing.

---

## What this still will not tell you

- **How it behaves under load from strangers.** One agent driven by hand is not
  the thousand-agent profile in the harness, and neither is a scanner.
- **Whether the renewal actually renews**, until roughly sixty days from now.
- **Anything about a second hub.** redb is single-process; there is one box by
  construction (§10.2).
- **Whether the chain survives at size.** The node replays its whole store
  before binding, and every timing in this document was taken on a chain small
  enough for that to be free.
