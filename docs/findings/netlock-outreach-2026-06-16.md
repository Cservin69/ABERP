# NETLOCK QTSP onboarding — sales-outreach pack

**Date:** 2026-06-16
**Prepared for:** Ervin Aben (Áben Consulting Kft.)
**Purpose:** First-contact sales email + first-call checklist for engaging NETLOCK
as our qualified timestamp authority (TSA) for the ABERP Defense audit-chain
(Path A — qualified timestamp anchoring per operator/service session).

---

## 0. Verified facts (read this before sending)

**Our company (from repo — `~/.aberp/prod/seller.toml`, `Cargo.toml`):**

| Field | Value | Source |
|---|---|---|
| Legal name | ÁBEN CONSULTING KFT | `seller.toml` |
| Adószám | 24904362-2-41 | `seller.toml` |
| EU VAT | HU24904362 | `seller.toml` |
| Székhely | 1037 Budapest, Visszatérő köz 6 | `seller.toml` |
| Contact | Ervin Aben — ervin@aben.ch | `Cargo.toml` authors |
| Product | ABERP — <https://github.com/Cservin69/ABERP> | `Cargo.toml` repository |

> ⚠️ The adószám above is the **real, production** seller number pulled from
> `~/.aberp/prod/seller.toml`. It is a normal B2B identifier (it appears on
> every invoice ABERP issues), so including it in a vendor inquiry is fine —
> but **confirm it's the entity you want on the NETLOCK contract** before
> sending. If the Defense line should be billed to a different legal entity,
> swap it.

**NETLOCK (from public web research — June 2026):**

| Field | Value | Source |
|---|---|---|
| Company | NETLOCK Kft. | netlock.hu/kapcsolat |
| Address | 1143 Budapest, Hungária krt. 17–19. | netlock.hu/kapcsolat |
| Sales / quote inbox | **ajanlat@netlock.hu** (Értékesítés / ajánlatkérés) | netlock.hu/kapcsolat |
| General / customer service | info@netlock.hu | netlock.hu/kapcsolat |
| Technical support | support@netlock.hu | netlock.hu/kapcsolat |
| Phone | +36 1 437 6655 (telefonos ügyfélszolgálat 08:30–13:00) | netlock.hu/kapcsolat |
| Service | Minősített időbélyeg-szolgáltatás (qualified TSA), eIDAS QTSP | netlock.hu/termekek/idobelyegzes |
| Standards | RFC 3161 (X.509 TSP); eIDAS 910/2014; Hungarian 2015. évi CCXXII (Eüsztv.) | netlock.hu/termekek/idobelyegzes |
| Algorithm | Migrated RSA2048 → **ECC** as of 2024-07-10 | netlock.hu/termekek/idobelyegzes |
| Integration | REST API, SOAP, file transfer / NFS; ETSI Baseline, XAdES, PAdES, CAdES, ASIC | NETLOCK CryptoServer / SignAssist pages |

**Published self-service pricing tiers (gross, annual prepaid buckets):**

| Package | Annual fee (gross) | Unit price | Overage |
|---|---|---|---|
| TS1000 — 1,000 stamps | 22,860 Ft | 23 Ft/stamp | 34 Ft/stamp |
| TS5000 — 5,000 stamps | 95,250 Ft | 19 Ft/stamp | 25 Ft/stamp |

> 🔑 **These published buckets are far below our volume and are annual, not
> monthly.** Our cadence (50–100 stamps / operator / workday) is ≈ **12,500–25,000
> stamps per operator per *year*** — a single operator already blows past TS5000.
> Add the daemon "service session" stamps at the same cadence and we are firmly
> in **high-volume / enterprise** territory. The whole point of the email below
> is to get routed off the webshop price list and onto an enterprise quote.
> **Do not anchor on 19 Ft/stamp** — bulk machine-to-machine rates should be
> materially lower; that's what the call is for.

**Could NOT verify in public domain (flagged, ask on the call):**
- REST API base URLs (sandbox + production), auth method, rate limits — not published; behind their docs / sales.
- Whether a **free evaluation / sandbox before contract** exists — not stated publicly.
- Enterprise / high-volume per-stamp pricing — not published.
- Their stated SLA % and status-page URL for the TSA — not found publicly.
- Typical sales response time — not published; see "what to expect" note at the bottom.

---

## 1. Sales-outreach email — Hungarian (the one to send)

**Címzett:** ajanlat@netlock.hu
**Tárgy:** Érdeklődés minősített időbélyeg-szolgáltatás (REST API) iránt — ABERP / Áben Consulting Kft.

---

Tisztelt NETLOCK Értékesítési Csapat!

Áben Ervin vagyok, az **Áben Consulting Kft.** (adószám: 24904362-2-41)
ügyvezetője. Cégünk fejleszti az **ABERP**-et, egy magyar, asztali (desktop)
vállalatirányítási rendszert gyártó kis- és középvállalkozások számára, amely
árajánlat-készítést, NAV Online Számla integrációt, anyag-nyomonkövetést és egy
manipuláció-biztos, hash-láncolt auditnaplót egyesít egyetlen, helyben futó
alkalmazásban.

Egy konkrét igénnyel keresem Önöket. Az ABERP védelmi/aerospace termékvonalának
**bíróság előtt is felhasználható auditnaplót** kell biztosítania (Pp. 325. §
(1) f) pont, illetve az eIDAS-rendelet 41. cikke szerinti minősített elektronikus
időbélyeg). Ehhez szeretnénk a **minősített időbélyeg-szolgáltatásukat REST API-n
keresztül** igénybe venni: az auditnaplónk hash-lánc fejeit bélyegeznénk le
rendszeresen. A tervezett ütemezés operátoronként: 1 időbélyeg bejelentkezéskor,
1 minden 15 percben aktív munkamenet alatt (heartbeat), 1 kijelentkezéskor —
ugyanez vonatkozik a háttérben futó „szolgáltatási munkamenetekre" is. Ez
becsléseink szerint **napi 50–100 időbélyeg operátoronként**, azaz nagyságrendileg
**évi 12 500–25 000 bélyeg operátoronként**. Hangsúlyozom: nekünk **nincs**
szükségünk teljes minősített aláírásra (QES) — kizárólag minősített időbélyegekre,
szabványos **RFC 3161 / eIDAS TST** formátumban, SHA-256 lenyomat felett.

Az ajánlatkéréshez az alábbiakban kérnék tájékoztatást:

1. **REST API-hozzáférés** — sandbox (teszt) és éles végpont; van-e lehetőség
   **kiértékelésre / tesztelésre még szerződéskötés előtt?**
2. **Árazás a mi volumenünkre** — a publikus TS1000/TS5000 csomagok ezt a
   mennyiséget messze meghaladják; kérem a **nagy volumenű / vállalati
   díjszabásukat** (sávos, pl. 1–10 ezer / 10–50 ezer / 50 ezer+ bélyeg/hó),
   illetve hogy van-e **havi minimum**, és előre fizetett (prepaid) vagy utólag
   elszámolt (postpaid) konstrukció.
3. **Bevezetési idő** — szerződéskötéstől az első éles (vagy sandbox) bélyegig
   mennyi a tipikus átfutási idő, és milyen dokumentumokat kérnek (pl.
   cégbírósági kivonat, aláírási címpéldány)?
4. **Szolgáltatási szint** — rendelkezésre állási SLA, tervezett karbantartási
   ablakok, incidens-értesítés és státusz-oldal.

Megjegyzés a jövőre nézve: középtávon felmerülhet nálunk a **szervezeti
elektronikus bélyegző (QSeal)** igénye is — felügyelet nélküli auditbejegyzések
aláírására, jellemzően a NETLOCK Sign Enterprise irányába. Ez most még nem
sürgős, de hálás lennék, ha jeleznék, kihez fordulhatok ez ügyben, amikor
aktuálissá válik.

Szívesen egyeztetek telefonon vagy online híváson is, ahogyan Önöknek
kényelmesebb. Előre is köszönöm a segítségüket!

Üdvözlettel,

**Áben Ervin**
ügyvezető — Áben Consulting Kft.
ABERP — <https://github.com/Cservin69/ABERP>
E-mail: `[Ervin's email]`
Telefon: `[Ervin's phone]`

---

## 2. Sales-outreach email — English (record copy / EN-preferring account manager)

**To:** ajanlat@netlock.hu
**Subject:** Inquiry — qualified timestamp service (REST API) for ABERP / Áben Consulting Kft.

---

Dear NETLOCK Sales Team,

My name is Ervin Aben, managing director of **Áben Consulting Kft.** (Hungarian
tax number 24904362-2-41). We develop **ABERP**, a Hungarian desktop ERP for
small and mid-sized manufacturing companies — quoting, NAV Online Számla
e-invoicing, material traceability, and a tamper-evident, hash-chained audit
ledger, all in one locally-run application.

I'm reaching out with a specific need. ABERP's Defense / aerospace product line
must provide a **court-admissible audit log** (Hungarian Civil Procedure Code
§325(1)f and eIDAS Article 41 — qualified electronic timestamp). To achieve this
we want to consume your **qualified timestamp service over a REST API**, stamping
the hash-chain heads of our audit ledger on a fixed cadence: per operator, one
stamp at login, one every 15 minutes during an active session (heartbeat), and
one at logout — with the same cadence for background "service sessions." We
estimate **50–100 stamps per operator per workday**, i.e. roughly **12,500–25,000
stamps per operator per year**. To be clear, we do **not** need full qualified
signatures (QES) — only qualified timestamps in the standard **RFC 3161 / eIDAS
TST** format over a SHA-256 digest.

For a quote, I'd appreciate information on:

1. **REST API access** — sandbox and production endpoints; is **evaluation / test
   access available *before* a contract is signed?**
2. **Pricing for our volume** — your published TS1000/TS5000 packages are far
   below this, so please share **high-volume / enterprise tiers** (e.g.
   1–10k / 10–50k / 50k+ stamps per month), any **monthly minimum**, and whether
   billing is prepaid or postpaid.
3. **Onboarding lead time** — from contract signature to the first sandbox /
   production stamp, and what documents you require (company extract, signature
   specimen, etc.).
4. **Service level** — availability SLA, planned maintenance windows, incident
   notification, and status page.

For the future: we may eventually want an **organizational electronic seal
(QSeal)** for unattended audit writes — likely via NETLOCK Sign Enterprise. It's
not urgent now, but I'd be grateful to know whom to contact for that when the
time comes.

I'm happy to talk by phone or video call, whichever suits you. Thank you in
advance for your help!

Best regards,

**Ervin Aben**
Managing Director — Áben Consulting Kft.
ABERP — <https://github.com/Cservin69/ABERP>
Email: `[Ervin's email]`
Phone: `[Ervin's phone]`

---

## 3. First-call checklist (~15 questions, grouped)

Run top-to-bottom on the first call. Capture answers inline.

### REST API & integration
1. **Documentation URL** for the timestamping REST API — can we get it before contract?
2. **Authentication method** — mTLS client cert, API key/secret, or OAuth2 client-credentials?
3. **Sandbox endpoint URL** (test TSA) — and is its TST issued under a *test* chain vs the qualified production chain?
4. **Production endpoint URL** for the qualified TSA.
5. **Rate limits** — max requests per second / per minute per account? (We burst ~1 stamp/operator at login+logout and steady 15-min heartbeats; need headroom for many concurrent operators + daemons.)

### Pricing
6. **High-volume tier breakdown** beyond the published TS1000/TS5000 — e.g. 1–10k, 10–50k, 50k+ stamps/month; what's the per-stamp rate at our band?
7. **Monthly minimum** (or annual commitment minimum), if any.
8. **Prepaid vs postpaid**, and **contract length** + **cancellation terms** (notice period, early-termination cost).

### Onboarding
9. **Lead time** from contract signature to first **sandbox** stamp, and to first **production** stamp.
10. **Required documents** — cégbírósági kivonat (company extract), aláírási címpéldány (signature specimen), adószám, anything else? Remote (e-sign) or in-person?

### Technical / cryptographic
11. **TST format & profile** — RFC 3161 confirmed? Do they also offer PAdES/CAdES/ASiC timestamp tokens, or is it a bare RFC 3161 token we embed ourselves? (We want the bare token over our ledger hash.)
12. **Hash algorithms** — SHA-256 supported as default? (Confirm; also SHA-384/512 availability for future-proofing.) **Max payload / digest size.**
13. **Certificate chain & long-term verifiability** — do they provide the full TSU cert chain + the qualified CA roots? **When their TSA cert rotates, do our previously-archived TSTs still verify** (i.e. is the old TSU cert + its validity proof retained / available via their Trusted-List entry)? This is the single most important question for *court-admissible, years-later* verification.

### Operational
14. **Availability SLA** for the stamping endpoint (we expect ≥99.9% for an eIDAS QTSP), **incident notification process**, **planned maintenance windows**, and **status page URL**.

### Legal / compliance
15. **Governing legal acts & conformance** — confirm the service is provided under **eIDAS 910/2014 (Art. 41/42)** and **2015. évi CCXXII. tv. (Eüsztv.)**; which specific §-references they assert it satisfies; and **which ETSI audit certificate** they hold (**ETSI EN 319 421** for the TSA policy, **EN 319 422** for the RFC 3161 profile). Ask for their **conformity assessment report / Trusted-List listing** reference.

### Future (plant the seed — don't dwell)
16. **Sign Enterprise / QSeal** — we may later add an organizational electronic seal for unattended audit writes. **Who is the account manager** for that product line, so we can re-engage directly when it's time?

---

## 4. Parallel-track checklist — what Ervin can do today/tomorrow to accelerate

- [ ] **Pull a fresh cégbírósági kivonat** (company extract) and have the
      **aláírási címpéldány** (signature specimen) scanned — QTSPs always
      require these for a new business customer; having them ready cuts days off
      onboarding.
- [ ] **Decide the contracting entity & signatory role.** Default: Áben
      Consulting Kft., signed by Ervin as ügyvezető. If the Defense line should
      sit under a different legal entity or a named technical responsible
      (typically CTO / Engineering Lead on the contract), settle that before the
      call so the quote is addressed correctly.
- [ ] **Estimate real annual volume now** — operators × workdays × (50–100/day)
      **plus** the daemon service-session stamps. A single number per month gives
      NETLOCK what they need to put us in the right pricing band on the first
      call (and stops us being quoted the TS5000 webshop tier).
- [ ] **Confirm the audit-chain anchoring design is "hash-only"** — we send only
      the SHA-256 hash-chain head, never document contents. Worth stating up
      front: it simplifies their data-handling / GDPR position and reassures on
      the Defense confidentiality angle.
- [ ] **Decide how a TSA outage should behave in ABERP** — queue-and-retry the
      stamp vs block the audit write. Have a position before the SLA discussion,
      since their availability number drives this design choice. (No `.gov.hu`
      domain registration is needed for this engagement — we're a TSA *client*,
      not declaring relying-party/SAML URLs, so skip that step.)

---

## 5. Sources

- NETLOCK timestamping product page — <https://netlock.hu/termekek/idobelyegzes/>
- NETLOCK contact page — <https://netlock.hu/kapcsolat/>
- NETLOCK CryptoServer / SignAssist (REST/SOAP integration) — <https://netlock.hu/en/cryptoserver-signassist/>
- NETLOCK qualified timestamp service policy (Szolgáltatási Rend) — <https://netlock.hu/download/sp-qt-hu/>
- eIDAS Regulation 910/2014 (Art. 41 — qualified electronic time stamp)
- ETSI EN 319 421 (TSA policy) / EN 319 422 (RFC 3161 profile) — <https://www.etsi.org/standards>
- Internal: `~/.aberp/prod/seller.toml`, `Cargo.toml`, `README.md` (org details)
