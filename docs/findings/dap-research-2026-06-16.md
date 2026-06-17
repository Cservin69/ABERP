# DÁP-backed operator login + court-admissible audit chain — research & design findings

**Date:** 2026-06-16
**Status:** RESEARCH ONLY — no code, no commits. Input to a forthcoming design session (ADRs).
**Author context:** ABERP Defense line deep-dive. Ervin has a Hungarian government ID + DÁP (Digitális Állampolgárság Program) MFA. Build target: HU operators in manufacturing, defense/aerospace certified.

> **Design principle being validated:** DÁP is hit **once per login** (and once at logout). Every interior audit event is signed by a **session key** endorsed at login. The chain reads:
> **DÁP-attested-login → N session-key-signed events → DÁP-attested-logout.**
> This avoids DÁP/network latency per audit append while preserving legal binding via the endorsement bracket.

---

## 0. TL;DR — does the design hold up?

**Yes, with one important correction and one structural choice.**

1. **The "once per login" principle is sound and matches industry append-only-log practice** (Certificate-Transparency-style signed checkpoints, §3.4). You do NOT need a network call per event.
2. **CORRECTION — DÁP cannot mint the binding for you.** A federated IdP (DÁP/KAÜ) will not sign an arbitrary custom claim containing your session-key bytes (§3.1). The realistic options are (a) carry `hash(session_pubkey‖tenant)` in the OIDC `nonce`/SAML request so the IdP's *own* signature transitively covers the key (cheap, partial), and/or (b) a **QTSP side-channel** — a QES/QSeal signs the endorsement payload right after login (robust, the real legal anchor).
3. **STRUCTURAL CHOICE — DÁP's two faces are different products.** `DÁP eAzonosítás` (identity, an OpenID4VP wallet flow) and `DÁP eAláírás` (a genuine personal QES) are *separate* services. eAláírás is **personal-use-only and signs PDFs**, so it does not cleanly sign an arbitrary endorsement blob (§1.4, §5). The design must decide whether the per-login legal anchor is DÁP eAláírás (free but PDF/ceremony-bound, personal-only) or a commercial QTSP remote-QES (Microsec/NETLOCK).
4. **Unattended daemon writes need an organizational QSeal, not a personal signature** (§3.2, §4.3). NETLOCK Sign Enterprise (documented REST + server-side QSCD keys) is the most automatable HU option.
5. **Court weight comes from the qualified timestamp/seal, not the hash chain alone** (§4). The hash chain gives tamper-*evidence*; binding the chain head with a **qualified timestamp** (Art. 41(2)) and/or **QSeal** (Art. 35(2)) is what creates the *statutory integrity presumption* and the HU `Pp. § 326` burden-shift.

---

## 1. DÁP developer surface (2025/2026 state)

### 1.1 Authoritative documentation
- **Platform / onboarding hub:** [platform.dap.gov.hu](https://platform.dap.gov.hu/) — lists three framework services (*keretszolgáltatások*): **Adattárca** (wallet/eID), **eAláírás** (e-signature), **Adatszolgáltatás** (data service). Onboarding email **services@dap.gov.hu**.
- **Service catalogue:** [services.gov.hu/dap-keretszolgaltatasok](https://services.gov.hu/dap-keretszolgaltatasok) (+ `/adattarca`, `/ealairas`, `/adatszolgaltatas`, `/design-system`).
- **THE technical connection portal:** [szeusz.gov.hu/szeusz/EAZON](https://szeusz.gov.hu/szeusz/EAZON) and [/EAZONHAASZ](https://szeusz.gov.hu/szeusz/EAZONHAASZ) (SZEÜSZ). Operated by **IdomSoft** under NISZ; support **szeusz@idomsoft.hu**.
- **eAláírás (QES) service description:** [hiteles.gov.hu/cikk/165](https://hiteles.gov.hu/cikk/165/dap_ealairas_szolgaltatas).
- **CRITICAL:** the detailed ~80-page technical spec is **gated behind KAÜ login**: *"KAÜ bejelentkezést követően elérhetővé válik a szolgáltatás műszaki leírása"* ("the technical service description becomes available after KAÜ login") — [szeusz.gov.hu/szeusz/EAZONHAASZ](https://szeusz.gov.hu/szeusz/EAZONHAASZ). **Exact endpoint paths, scope names, and claim identifiers are not public.**

### 1.2 Authentication / integration flows — TWO distinct things
**(a) KAÜ (Központi Azonosítási Ügynök)** — the existing government SSO federation. KAÜ historically uses **SAML** (developer materials reference `kau-saml-client-lib`, SP→KAÜ `AuthnRequest`) — [szeusz.gov.hu/szeusz/kau](https://szeusz.gov.hu/szeusz/kau). The DÁP app became a KAÜ identification method; from **2026, DÁP is the only login** for HU citizens' e-transactions (Ügyfélkapu+ EOL 31 Dec 2025) — [NAK](https://www.nak.hu/kamara/kamarai-hirek/orszagos-hirek/108109-tajekoztatas-az-ugyfelkapu-es-a-dap-mobilalkalmazas-hasznalatarol).

**(b) DÁP eAzonosítás** — the new wallet eID, a **Verifiable-Credentials flow over OpenID4VP** (NOT classic OIDC authorization-code/token/userinfo): *"a digitális adattárca a Verifiable Credentials … az OpenID4VP szabvány mentén valósítja meg"* — [fintechzone pt.4](https://fintechzone.hu/digitalis-adattarca-es-eidas-2-hogyan-segiti-a-dap-sandbox-a-piaci-integraciot-4-resz/). Flow: RP creates a request object → delivered as **QR code or deep link** → citizen approves in DÁP app → RP validates the signed presentation. **Early 2026: standard moved to OpenID4VP 1.0 + DCQL, non-backward-compatible** — [technokrata 2026-02-09](https://www.technokrata.hu/egazdasag/2026/02/09/szabvanyvaltas-dap-eazonositas-trustid-solutions).

**PID claim set (confirmed human-readable):** surname, given name, place of birth, date of birth, issuing authority/country, expiry, **citizenship**. Mother's name is NOT in PID (needs the consent module HAASZ) — [fintechzone eAzonosítás](https://fintechzone.hu/dap-eazonositas-biztonsagos-azonositas-a-digitalis-terben-2-resz/). Machine claim names / credential format (mdoc vs SD-JWT-VC) **not public**.

### 1.3 MFA mechanics
RP initiates via **QR / deep link**; the DÁP app itself is gated by **PIN and/or biometric**; HAASZ consent is approved via an in-app **pop-up (push-style) interface** — [fintechzone 30 Q&A](https://fintechzone.hu/dap-a-gyakorlatban-30-kerdes-es-valasz-amelyek-nem-fertek-bele-a-webinarba/), [fintechzone HAASZ](https://fintechzone.hu/dap-hozzajarulas-alapu-adatszolgaltatas-haasz-a-digitalis-allampolgarsag-uj-korszaka/). The "second factor" is fundamentally **app-bound possession (registered phone) + local PIN/biometric**, with QR/deep-link as the cross-channel.

### 1.4 Does DÁP perform QES? — **YES, but as a separate service**
- *"A létrehozott aláírás **minősített elektronikus aláírás**"* ("The created signature is a **qualified electronic signature**") — [hiteles.gov.hu/cikk/165](https://hiteles.gov.hu/cikk/165/dap_ealairas_szolgaltatas). eIDAS-compliant, certificate issued by **DAP-CA** (operated by NISZ), valid 3 years, liability cap 50,000,000 HUF — [csabamolnar.hu](https://csabamolnar.hu/2025/02/07/digitalis-alairas-a-dap-applikacioban/).
- **Important constraints for our design:** eAláírás is **personal-use-only** (no company/business signing) and the signing ceremony is **PDF-document-oriented** (upload PDF, max 30 MB/doc, 5/batch, enter signing password) — [csabamolnar.hu](https://csabamolnar.hu/2025/02/07/digitalis-alairas-a-dap-applikacioban/). It is **not** a general "sign these arbitrary bytes via API" primitive. Verified via the government **KEAESZ** validator at [mo.hu/keaesz](https://mo.hu/keaesz).
- **AVDH** (the predecessor, *Azonosításra Visszavezetett Dokumentumhitelesítés*) was **NOT a QES** (advanced/seal level at most) and the standalone service **ended 31 Dec 2024** — [domain.hu](https://www.domain.hu/hogyan-lehet-digitalisan-alairni-okiratot-az-avdh-megszunese-miatt/). Note: HU `Pp. § 325(1) g)` still lists AVDH-style identity-based authentication as a route to full-probative-force documents (§4.6) — DÁP is the successor channel.

### 1.5 Sandbox & registration
- **Sandbox:** yes for eAzonosítás/wallet (web-interface model with positive/negative cases); for **integrated eSignature only UAT + Prod** — [fintechzone pt.4](https://fintechzone.hu/digitalis-adattarca-es-eidas-2-hogyan-segiti-a-dap-sandbox-a-piaci-integraciot-4-resz/), [30 Q&A](https://fintechzone.hu/dap-a-gyakorlatban-30-kerdes-es-valasz-amelyek-nem-fertek-bele-a-webinarba/). **IdomSoft provides an RP-client sample solution** + service-provider/integrator/developer forums.
- **Registration:** via **szeusz.gov.hu** — organizational registration → cybersecurity audit → certificates per the (gated) technical doc → testing → demo of the connecting system → ÁSZF acceptance. Contact **szeusz@idomsoft.hu**.
- **Cost:** eAláírás is **free** for individuals; eAzonosítás/HAASZ data queries are governed by a fee decree (*díjrendelet*) — **exact amounts not public**.

### 1.6 Status
Open to third parties and live in 2026 (~39 orgs live, 250+ integrations in progress, 1.25M+ PID certs) — [fintechzone 2026](https://fintechzone.hu/dap-2026-a-digitalis-irattarca-mar-nem-csak-bejelentkezesre-jo/). Positioned as HU's forerunner of the EUDI Wallet under eIDAS 2.

---

## 2. Hungarian QTSPs for QES / QSeal / timestamps

**Active on the HU Trusted List** (NMHH supervisor; parsed from signed [nmhh.hu/tl/pub/HU_TL.pdf](https://nmhh.hu/tl/pub/HU_TL.pdf), supervisor [english.nmhh.hu/article/187339](https://english.nmhh.hu/article/187339/Trust_services_and_electronic_signature)): **Microsec (e-Szignó), NETLOCK, NISZ, Magyar Telekom** (latter two: state/legacy). **eVitel and MOL eSign are NOT on the HU list.**

| Provider | Qualified for | API style | Remote vs card | Pricing (sourced) | Citizenship req. | Programmatic / unattended? |
|---|---|---|---|---|---|---|
| **Microsec (e-Szignó / MicroSigner)** | QCert sig, QCert seal, qualified TSA, validation, preservation, **remote QSCD** | **e-Szignó SDK** + Microsec proxy (HTTPS, user/pass session); no public self-serve REST/Swagger | Both: card/USB, software, remote HSM | Seal cert ≈ **50,000 HUF/yr**; remote sig ≈ 41,000 HUF/yr ([árlista 2024](https://e-szigno.hu/hirek/arlista-valtozas-2024)) | **None** — "available worldwide" ([CPS v3.17](https://static.e-szigno.hu/docs/szsz--all--all--EN--v3.17.pdf)) | MicroSigner = **human ceremony** each sign; remote-QSCD seal = server-side but **contract-led, no public sandbox** |
| **NETLOCK** | QCert sig, QCert seal, qualified TSA, qualified preservation (QSCD), QWAC | **Documented REST API** (NETLOCK Sign Enterprise: user/cert/sign/ID, OAuth2) | Both; **server-side key storage** for automation | TSA: **18,000 HUF/yr** (1k stamps) / **75,000 HUF/yr** (5k); dev support 150k–360k HUF/day ([Sign Enterprise](https://netlock.hu/netlock-sign-enterprise/?lang=en), [timestamp](https://netlock.hu/timestamp/?lang=en)) | Non-EU passport; EU other ID; **video ID accepted** ([FAQ](https://netlock.hu/en/faq/)) | **Yes — REST + server-side seal**; most automation-friendly; still enterprise onboarding |
| **NISZ** | Government-oriented qualified services (incl. DAP-CA) | Not documented for 3rd-party commercial use | n/a (state PKI) | n/a | citizen/state eID | **Not a commercial API target** |
| **Magyar Telekom** | Legacy "Minősített CA" TL entries (many withdrawn) | none public | n/a | n/a | n/a | **Legacy — do not target** |

**Key points for our design:**
- **Personal QES at login/logout** → Microsec MicroSigner or NETLOCK (both inherently a per-signature human ceremony — exactly what a session boundary wants).
- **Interior events** → our own session key, not a QTSP product.
- **Unattended organizational QSeal** → needs a **remote/server-side QSCD e-seal** activated via a Signature Activation Module (SAM). **NETLOCK Sign Enterprise** (REST + server-side keys) is the most directly automatable; **Microsec remote QSCD** is equivalent but with no public sandbox. Both require a commercial contract.
- **Qualified timestamps (RFC 3161)** are the cheapest, easiest piece (NETLOCK ≈ **15–18 HUF/stamp**) and the load-bearing legal upgrade for the audit chain (§4.4).
- **Signer identity in a Microsec cert:** subject DN `CN`/`givenName`/`surname` + `serialNumber` as the HU natural-person identifier (`PNOHU-…` per ETSI EN 319 412-1) — [requirements PDF](https://www.microsec.hu/api/?func=cms.media&file=/microsec/Blog/requirements-for-qualified-signature-verification-according-to-eidas.pdf).

---

## 3. Per-login chain architecture details

### 3.1 Can the DÁP/IdP login attestation carry the session-key binding?  **Realistically NO — use a side-channel.**
- OIDC lets a client request *which* claims appear but **never lets the client dictate a claim's value** (the `value` member is a filter/assertion, not an injection) — [OIDC Core §5.5.1](https://openid.net/specs/openid-connect-core-1_0.html). A national IdP exposes a **fixed claim set**.
- The one standards-based key-binding OIDC offers is **proof-of-possession via `cnf`**: RFC 7800 (`cnf.jwk`), **DPoP / RFC 9449** (`cnf.jkt` = JWK SHA-256 thumbprint), **mTLS / RFC 8705** (`cnf.x5t#S256`). These bind the **client's** PoP key — *if and only if the IdP implements DPoP/mTLS*. Whether DÁP/KAÜ does is **unverified** and, given KAÜ's SAML lineage, **unlikely** by default.
- **The cheap partial binding that always works:** set the OIDC `nonce` (client-chosen, passed through unmodified and signed into the `id_token` — [OIDC Core](https://openid.net/specs/openid-connect-core-1_0.html)) — or the SAML request/`RelayState` — to `base64url(SHA-256(session_pubkey ‖ tenant_slug))`. The IdP's own signature then *transitively* covers the session key. A verifier recomputes the hash from the out-of-band pubkey. This proves "this key existed at this login," not a rich endorsement.
- **The robust binding (recommended legal anchor):** immediately after login, have a **QTSP QES (personal) or QSeal (org)** sign the endorsement payload `{ hash(IdP-attestation) ‖ session_pubkey ‖ tenant_slug ‖ timestamp }`. That signature, not DÁP, is the eIDAS-grade anchor.

### 3.2 Unattended audit writes (snapshot daemon, AP-sync, NAV-poll, calibration hook) — no operator session
- Industry/eIDAS pattern: an **organizational Qualified Electronic Seal (QSeal)** key in an **HSM/QSCD**, periodically re-authorized via a **Signature Activation Module (SAM)**, operated by/with a QTSP — [Alfatec QSCD/SAM](https://www.alfatec.ai/academy/resource-library/qualified-signature-creation-devices-qscd-under-eidas-and-signature-activation-module-sam), [CEN/TS 419221-6](https://signius.eu/2025/09/18/operating-your-own-hsm-for-qualified-electronic-seals-cen-ts-419221-6/).
- **Design:** a per-tenant **"service identity"** signs daemon events with a QSeal (legal-person seal), distinct from the operator's personal QES. The seal is endorsed periodically (e.g. a responsible operator authorizes a sealing window). Art. 35(2) gives the QSeal an integrity + correct-origin presumption (§4.3) — appropriate for machine-emitted records where no human "signs."

### 3.3 Key storage on macOS
- **Secure Enclave (`Token::SecureEnclave` in the `security-framework` Rust crate v3.7.0):** generates a **non-exportable P-256** key, `create_signature` + `public_key()` reachable from Rust — [docs.rs Token](https://docs.rs/security-framework/latest/security_framework/key/enum.Token.html), [docs.rs SecKey](https://docs.rs/security-framework/latest/security_framework/key/struct.SecKey.html). Constraint: **256-bit ECC P-256 only**, non-exportable — [Apple Forums 8030](https://developer.apple.com/forums/thread/8030).
- **Recommendation:** because the session key's trust comes from the **login-endorsement bracket** (not hardware provenance), a **software Ed25519 key generated in memory at login and zeroized at logout** is the cleanest, fastest fit. Use Secure-Enclave-P256 only if the threat model includes "audit signatures forged by malware running as the operator" — then the non-exportable HW key earns its complexity (at the cost of P-256-only + a Keychain-resident sealed blob).
- This mirrors ABERP's existing `aberp-digital-id` keychain-mirror pattern (session-token keychain read is already unconditional; cf. the S435 Portable launcher note in memory).

### 3.4 Logout / session-close guarantees (must fire even on crash)
- Borrow the **Certificate-Transparency Signed-Tree-Head pattern**: emit a **session-key-signed heartbeat/checkpoint every N minutes** that chains the running event hash — [CT](https://en.wikipedia.org/wiki/Certificate_Transparency), [SoK: Log-Based Transparency](https://arxiv.org/pdf/2305.01378). An abrupt termination then leaves a recent signed checkpoint.
- **On next boot:** detect any session whose last record is a heartbeat (not a clean close) and write a synthetic **"unclean-close recovered"** record chaining to it (ABERP already has the synthetic-state precedent — cf. the NAV-off `"nav-disabled"` synthetic Ready state). The graceful logout attestation is the *ideal* terminator; the heartbeat is the *guaranteed* one.
- Triggers: operator-clicks-Logout (ideal, fires closing QES if Candidate B) **and** an idle timeout **and** the periodic heartbeat. Never depend on a clean shutdown alone.

---

## 4. eIDAS Article 25 + HU law + admissibility

> Verbatim text below was sourced via [legislation.gov.uk retained-EU text](https://www.legislation.gov.uk/eur/2014/910) (word-identical for these paras) + the [eIDAS-2 amendment site](https://www.european-digital-identity-regulation.com/Article_25_(Regulation_EU_2024_1183).html). **Re-pull from EUR-Lex CELEX 02014R0910-20241018 before citing in a legal doc** (EUR-Lex is JS-rendered; direct fetch failed — see Gaps).

### 4.1 Article 25 — verbatim
- **25(1):** *"An electronic signature shall not be denied legal effect and admissibility as evidence in legal proceedings solely on the grounds that it is in an electronic form or that it does not meet the requirements for qualified electronic signatures."*
- **25(2):** *"A qualified electronic signature shall have the equivalent legal effect of a handwritten signature."*
- **25(3)** (cross-border recognition) was **deleted by eIDAS 2.0 (Reg. (EU) 2024/1183)** as redundant — **25(1)/(2) are untouched**.

### 4.2 What makes a signature "qualified" (Art. 3 + 26)
**QES = AES + QSCD + qualified certificate.** Art. 3(12): *"an advanced electronic signature that is created by a qualified electronic signature creation device, and which is based on a qualified certificate for electronic signatures."* Art. 26 AES requirements: uniquely linked to + capable of identifying the signatory; created under sole control; **linked to the data such that any subsequent change is detectable.**

### 4.3 Qualified electronic SEAL — Art. 35 (org / unattended)
- **35(2):** *"A qualified electronic seal shall enjoy the presumption of integrity of the data and of correctness of the origin of that data to which the qualified electronic seal is linked."* → the right primitive for unattended daemon writes (§3.2).

### 4.4 Qualified electronic TIMESTAMP — Art. 41 (chain anchoring) — **load-bearing**
- **41(2):** *"A qualified electronic time stamp shall enjoy the presumption of the accuracy of the date and the time it indicates and the integrity of the data to which the date and time are bound."* → binding the **chain head hash** with a qualified timestamp gives every anchored segment a statutory integrity + time presumption. This is the cheapest, strongest legal upgrade.

### 4.5 Is QES alone enough, or is a notary needed?
**General rule:** QES = handwritten signature; HU cannot deny it. **Exceptions are form-of-the-act rules, not signature-validity rules:** HU real-estate transfers fit for the land registry still require **lawyer countersignature (*ügyvédi ellenjegyzés*) or notarial deed**; certain enforceable/corporate acts require **közjegyzői okirat** (public deed). For ordinary contracts, invoices, declarations and **internal audit records, QES alone is fully sufficient and court-admissible** — [Jogászvilág](https://jogaszvilag.hu/napi/erdemes-kozjegyzoi-okiratba-foglalni-az-ingatlan-adasveteli-szerzodeset/).

### 4.6 Hungarian national law
- **Eüsztv. — 2015. évi CCXXII. törvény** defines QES *by direct reference to eIDAS Art. 3(12)* (§ 38) — [njt.jog.gov.hu/jogszabaly/2015-222-00-00](https://njt.jog.gov.hu/jogszabaly/2015-222-00-00). The handwritten-equivalence itself lives in the directly-applicable eIDAS Art. 25(2).
- **Pp. — 2016. évi CXXX. törvény, § 325(1) f) + § 326** is the evidentiary engine: a QES/qualified-cert-AES-signed-or-sealed electronic document is a ***teljes bizonyító erejű magánokirat*** ("private document with full probative force"), and § 326 deems the signed content ***"az ellenkező bizonyításáig meg nem hamisítottnak kell tekinteni"*** ("**deemed un-falsified until the contrary is proven**") — a **rebuttable presumption that shifts the burden to the challenger** — [Pp. § 325](https://mkogy.jogtar.hu/jogszabaly?docid=A1600130.TV&pagenum=5). § 325(1) g) covers AVDH/DÁP identity-based authentication as a separate route.
- **"Act CLXXII of 2024" (from the brief): UNVERIFIED / likely erroneous.** No relevant 2024 HU trust-services act was found. Controlling statutes remain the **Eüsztv. (2015. CCXXII.)** and **Pp. (2016. CXXX.)**, as amended. Confirm before citing.

### 4.7 Admissibility precedent for hash-chained + QES/timestamp logs
- **No direct EU or HU case law on hash-chained / tamper-evident audit-log admissibility was found.** The position is **doctrinal, not precedential**: (1) Art. 25(1)/35(1)/41(1) floor (cannot deny admissibility for being electronic); (2) qualified timestamp/seal over the chain head → Art. 41(2)/35(2) integrity presumption; (3) HU uplift → `Pp. § 325(1) f)` full probative force + § 326 burden-shift.
- **Caveat from the literature:** crypto integrity proves *the record wasn't altered after entry* — not that the entered content is true (GIGO); and timestamp reliability depends on the TSA's *qualified* status — [Frontiers in Blockchain 2024](https://www.frontiersin.org/journals/blockchain/articles/10.3389/fbloc.2024.1306058/full). A **qualified** (Art. 42) timestamp is materially stronger than self-asserted/blockchain time.

---

## 5. Implementation paths — two candidates for the design session

Both build on ABERP's existing `aberp-digital-id` trait (`DigitalIdProvider`: Mock + US-DoD-CAC stubs) and the SHA-256 hash-chained `audit-ledger` with its `Signed<T>{payload, signer: Option<DigitalIdRef>}` wrapper. Both add a real HU backend behind the trait + a session-key signer + a qualified-anchor.

### Candidate A — "Identity bracket + qualified-timestamp anchor" (lighter, cheaper)
**Login flow:** operator authenticates via DÁP eAzonosítás/KAÜ (identity only — verified HU citizen). Generate a **software Ed25519 session keypair** in memory. Bind it by putting `hash(session_pubkey‖tenant)` in the OIDC `nonce` / SAML request so the gov IdP signature transitively covers the key. Record a `DÁP-attested-login` audit event carrying the IdP attestation + pubkey.
**Audit signing:** interior events signed by the session key. A **qualified timestamp (NETLOCK RFC 3161, ≈15–18 HUF/stamp)** anchors the chain head on a schedule (e.g. every heartbeat, ~N min) → Art. 41(2) presumption.
**Logout:** session-key-signed close; heartbeat + next-boot reconcile guarantee a terminator.
**Unattended:** organizational QSeal (NETLOCK Sign Enterprise) for daemon writes — optional in a first cut, can fall back to timestamp-only.
- **Legal weight:** strong *integrity* presumption (timestamp) + gov-IdP identity. **No per-session personal QES** ⇒ weaker on personal non-repudiation, but ample for an *internal* audit trail.
- **Cost:** ~tens of thousands HUF/yr (timestamps + optional seal). **No per-login ceremony friction.**
- **Code estimate:** ~600–900 LOC. Crates: `security-framework` or `ed25519-dalek`, an RFC 3161 client (`rasn`/custom or `cmpv2`-style), a SAML or OpenID4VP client for DÁP. New EventKinds: `auth.session_opened` / `auth.heartbeat` / `auth.session_closed` / `audit.timestamp_anchored`.
- **Known unknowns:** does KAÜ expose OIDC `nonce` or only SAML? OpenID4VP 1.0/DCQL migration churn.

### Candidate B — "Full QES bracket" (heavier, maximum court weight)
**Login flow:** DÁP eAzonosítás/KAÜ identity **+** a **QTSP remote-QES ceremony at login** signing the endorsement payload `{hash(IdP-attestation) ‖ session_pubkey ‖ tenant_slug ‖ ts}`. That QES is the eIDAS Art. 25(2) handwritten-equivalent anchor for the whole session.
**Audit signing:** interior events session-key-signed (as A). Qualified timestamps anchor checkpoints (as A).
**Logout:** **closing QES ceremony** over the session's final chain head; heartbeat + reconcile as fallback.
**Unattended:** organizational QSeal on a remote QSCD (NETLOCK Sign Enterprise / Microsec remote QSCD) for daemon writes.
- **Legal weight:** maximum — personal QES per session ⇒ `Pp. § 326` burden-shift on the *operator's* attestation, not just integrity.
- **Cost:** QTSP contract + per-signature/subscription (Microsec seal ≈ 50k HUF/yr; remote sig ≈ 41k HUF/yr) + onboarding/audit. **Per-login + per-logout ceremony friction** (card PIN or mobile-app push).
- **Code estimate:** ~1200–1800 LOC. Crates as A **+** a QTSP integration (NETLOCK REST or Microsec e-Szignó SDK). New EventKinds add `auth.qes_endorsement_applied` / `auth.qes_close_applied` / `audit.qseal_applied`.
- **Known unknowns:** **DÁP eAláírás is personal-only + PDF-bound** → likely cannot sign the raw endorsement blob, so the per-login QES probably means a **commercial QTSP**, not DÁP itself; QTSP onboarding lead time; whether unattended QSeal needs per-write human authorization.

### Scoring (1 = poor, 5 = excellent)
| Axis | Candidate A (identity + timestamp) | Candidate B (full QES bracket) |
|---|---|---|
| Operator onboarding ease | **5** (login only, no ceremony) | 2 (per-login/logout QES ceremony) |
| Code complexity | **4** (~600–900 LOC) | 2 (~1200–1800 LOC + QTSP SDK) |
| Legal weight | 3 (integrity presumption, no personal QES) | **5** (personal QES + § 326 burden-shift) |
| Ongoing operational cost | **4** (timestamps cheap) | 2 (QTSP contract + per-sig) |
| Vendor lock-in risk | **4** (timestamp is commodity/swappable) | 3 (bound to one QTSP's SDK/API) |

**Recommendation for the design session:** start from **Candidate A** as the shippable foundation (it delivers the "DÁP-once-per-login + session-key interior + qualified-timestamp anchor" principle with low friction), and treat **Candidate B's QES bracket as an opt-in tier** for tenants/contracts that demand personal non-repudiation — the `DigitalIdProvider` trait already abstracts this cleanly. A hybrid is natural: A for everyone, B layered where a defense contract requires it.

---

## 6. Open questions for the design session (ADRs must answer)

1. **Does KAÜ/DÁP expose OIDC at all (with a client-controllable `nonce`), or is it SAML-only?** This decides whether the cheap transitive key-binding (§3.1) works or whether a QTSP side-channel is mandatory. *Resolve via the gated szeusz.gov.hu technical spec / IdomSoft developer relations.*
2. **Is the per-login legal anchor DÁP eAláírás or a commercial QTSP?** DÁP eAláírás is free but **personal-only + PDF-bound** — can it sign our endorsement payload at all, or must we wrap it in a generated PDF? If not, Candidate B requires Microsec/NETLOCK.
3. **For unattended daemon writes (snapshot/AP-sync/NAV-poll/calibration), is a qualified QSeal required, or is a qualified timestamp over the chain head sufficient** for the court-admissibility bar we're targeting? (Cost/complexity hinges on this.)
4. **What is the heartbeat/checkpoint interval N**, and what exactly does the synthetic "unclean-close recovered" record assert? (Crash-safety vs noise trade-off.)
5. **Session key: software Ed25519 (ephemeral, in-memory) or Secure-Enclave P-256 (non-exportable)?** Decided by the threat model — do we defend against malware forging audit signatures as the operator?
6. **Multi-tenant binding:** the endorsement payload includes a tenant slug — how does this interact with ABERP's existing per-tenant model and the NAV-off Portable build? (A defense tenant is presumably NAV-on, HU-jurisdiction.)
7. **What's the operator-facing "Sign in with DÁP" UX, and the fallback if DÁP is down?** (Offline grace period with session-key-only + later qualified re-anchor? Hard block?)
8. **Do we need qualified long-term preservation (Art. 34 / NETLOCK qualified preservation)** so signatures/timestamps remain verifiable past certificate expiry (3-yr DAP-CA cert, defense retention is decades)?
9. **OpenID4VP 1.0 / DCQL migration:** the 2026 standard change is non-backward-compatible — does our integration target the new profile from day one?
10. **EventKind budget:** the audit chain is currently at 148 kinds; how many new `auth.*` / `audit.*` kinds does the chosen candidate add, and does the count-drift assert need bumping?

---

## 7. What needs Ervin's direct input before design can proceed
- **Do you already have a Microsec or NETLOCK QTSP account / contract?** Onboarding (cybersecurity audit + contracting) is the long-lead item; if not, start it in parallel with design.
- **Target legal bar:** is an *internal court-admissible audit trail* (Candidate A: integrity presumption via qualified timestamp) enough, or do specific defense/aerospace contracts contractually require **personal QES non-repudiation per session** (Candidate B)?
- **Are you willing to do a per-login (and per-logout) signing ceremony** (card PIN / DÁP-app push), or is login friction a hard "no"? (This is the single biggest fork between A and B.)
- **Who/what is the "tenant service identity"** for unattended writes — is there a named responsible operator who periodically authorizes a sealing window?
- **DÁP integration access:** are you (as a HU citizen + business) able to register the org on szeusz.gov.hu and pull the gated technical spec, so we can close the protocol gaps below?

---

## 8. Gaps where authoritative info could not be found
1. **DÁP/KAÜ exact protocol surface** (OIDC vs SAML, endpoint paths, scope/claim identifiers, redirect-URI rules, DPoP/mTLS support) — **gated behind KAÜ login on szeusz.gov.hu**. Fill via org registration + **szeusz@idomsoft.hu** / **services@dap.gov.hu** + IdomSoft's RP-client reference + developer forums.
2. **DÁP eAláírás API for non-PDF payloads** — sources show only a PDF-upload ceremony; whether arbitrary bytes can be signed is unconfirmed. Likely answer: no → use a commercial QTSP for the endorsement signature.
3. **Microsec public REST/sandbox** — the e-Szignó SDK is real but endpoints/auth/test-env are not publicly documented; onboarding appears sales-led (sales@microsec.hu). **NETLOCK** REST details came from marketing copy, not a developer portal read directly — confirm before committing.
4. **eIDAS verbatim text** — quoted via legislation.gov.uk (Brexit-retained, word-identical for these paras); **re-pull from EUR-Lex CELEX 02014R0910-20241018** for citation safety. Art. 25(3)/35(3)/41(3) EU-original verbatim not independently captured.
5. **"Act CLXXII of 2024"** — not found; treat as erroneous. Controlling law: Eüsztv. 2015. CCXXII. + Pp. 2016. CXXX.
6. **No case law** on hash-chained/QES-anchored audit-log admissibility — position is doctrinal.
7. **Exact DÁP/HAASZ fee amounts** and **2025/2026 full QTSP price tables** — not public (pricing decree / JS-rendered price pages).

---

### Source index (primary)
DÁP: platform.dap.gov.hu · services.gov.hu/dap-keretszolgaltatasok · szeusz.gov.hu/szeusz/EAZON(HAASZ) · hiteles.gov.hu/cikk/165 · fintechzone.hu DÁP series · technokrata.hu 2026-02-09 · csabamolnar.hu.
QTSP: nmhh.hu/tl/pub/HU_TL.pdf · netlock.hu/netlock-sign-enterprise · netlock.hu/timestamp · e-szigno.hu/hirek/arlista-valtozas-2024 · static.e-szigno.hu CPS v3.17 · microsec.hu PKI blog.
Crypto/macOS: openid.net OIDC Core · datatracker RFC 7800 · rfc-editor RFC 9449 / RFC 8705 · docs.rs/security-framework · developer.apple.com forums/thread/8030 · arxiv 2305.01378 (transparency logs).
Legal: legislation.gov.uk/eur/2014/910 (Art. 25/26/35/41 + Art. 3 defs) · european-digital-identity-regulation.com (eIDAS 2 amendments) · njt.jog.gov.hu/jogszabaly/2015-222-00-00 (Eüsztv.) · mkogy.jogtar.hu Pp. § 325/326 · frontiersin.org Blockchain 2024.
