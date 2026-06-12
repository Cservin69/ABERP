# S380 — NAV v3.0 storno XSD + business-rule audit (stop the whack-a-mole)

**Session:** S380 (2026-06-12, read-only research off `origin/main` 59b648a = PROD_v2.27.41)
**Scope:** every NAV Online Számla v3.0 requirement for an `invoiceOperation = STORNO`
data report, cross-referenced against what `render_storno_data` actually produces.
**Trigger:** operator confirmed storno has NEVER reached NAV SAVED (35 submissions last
month, zero with negative amounts). Fix chain so far: S184 `INVALID_LINE_OPERATION` →
S369 `INVOICE_LINE_ALREADY_EXISTS` → S373 `LINE_NUMBER_NOT_SEQUENTIAL`.

---

## 0. Executive summary

**The post-S373 storno body is, as far as NAV's published contract can be statically
evaluated, CLEAN.** The audit walked:

1. all four official v3.0 XSDs (`invoiceData.xsd`, `invoiceBase.xsd`, `invoiceApi.xsd`,
   `invoiceAnnulment.xsd` from `github.com/nav-gov-hu/Online-Invoice`,
   `src/schemas/nav/gov/hu/OSA/`) — **every element, order, cardinality, namespace and
   numeric facet of the rendered storno conforms** (§3);
2. **all 80 blocking business-validation rows** of the official interface spec
   (`EN_Online Invoice System 3.0 Interface Specification (2026.02.12.).pdf`,
   §3.3.2, spec pages 184–203) — **no row fires against the current storno body**
   given the chain state the operator describes (zero prior SAVED chain members) (§4);
3. the WARN catalogue (Annex I) — **two warnings will fire or are risked**, neither
   blocks SAVED (§5).

The three historical rejections were each real and each is now fixed; the spec's own
blocking table contains **no further storno-scoped rule our body violates**. The
whack-a-mole sequence is — per NAV's published contract — exhausted.

**Consequence: S381 must start with evidence, not code.** S373 shipped today
(PROD_v2.27.41) and no storno has been submitted against NAV since. If the next
submission still ABORTs, the rejection code can no longer be predicted from the spec
and MUST be read from the actual ack (`queryTransactionStatus`
`businessValidationMessages`, stored in the audit ledger `response_xml` payload — the
S184 decode discipline). One real finding is fatal-but-MODIFY-path (F1), and four
WARN/correctness gaps are worth fixing while the file is open (F2–F5).

---

## 1. Sources (all verbatim quotes carry their origin)

| Source | Pin |
|---|---|
| `invoiceData.xsd` v3.0 | `https://raw.githubusercontent.com/nav-gov-hu/Online-Invoice/master/src/schemas/nav/gov/hu/OSA/invoiceData.xsd` (123,691 bytes, fetched 2026-06-12) |
| `invoiceBase.xsd` v3.0 | same path, `invoiceBase.xsd` (15,765 bytes) — header pins `Version: v3.0 2020/07/31`, `elementFormDefault="qualified"` |
| `invoiceApi.xsd` v3.0 | same path, `invoiceApi.xsd` (113,591 bytes) |
| Interface spec EN | `docs/API docs/en/EN_Online Invoice System 3.0 Interface Specification (2026.02.12.).pdf` in the same repo (410 pages; page numbers below are the printed footer numbers) |
| Rendered storno (DEV, post-S373) | `~/.aberp/serve/test/issued/01KTYJFEXGR8MZ2CPK31P1PPWK.xml` (2026-06-12 20:46, `TEST-ABERP/2026/0047` cancelling `TEST-ABERP/2026/0046`) |
| Emitter | `apps/aberp/src/nav_xml.rs::render_storno_data_with_number` (nav_xml.rs:710), `write_invoice_reference` (:1074), `negate_line` (:1045), `write_lines` (:1444), `write_summary` (:1598), `write_invoice_detail` (:1259) |
| Storno issuance | `apps/aberp/src/issue_storno.rs::storno_from_inputs` (:220), date defaults (:332–340), `next_modification_index_in_tx` (:1102) |
| Envelope | `crates/nav-transport/src/soap/mod.rs` (`invoiceOperations` loop :226–241), `apps/aberp/src/submit_invoice.rs::detect_operation_from_xml` (:729) |

NOTE: no `.xsd` file is vendored in the ABERP repo; the in-repo "XSD" is the
hand-rolled allowlist `crates/nav-xsd-validator` (ADR-0022). Validator drift findings
are scored separately (F6).

---

## 2. What we actually put on the wire (post-S373)

The full rendered body (one-line base, one-line storno), abridged to the
structurally-significant skeleton; every element name/order below is byte-real from
`01KTYJFEXGR8MZ2CPK31P1PPWK.xml`:

```xml
<InvoiceData xmlns="http://schemas.nav.gov.hu/OSA/3.0/data"
             xmlns:common="http://schemas.nav.gov.hu/OSA/3.0/base">
  <invoiceNumber>TEST-ABERP/2026/0047</invoiceNumber>
  <invoiceIssueDate>2026-06-12</invoiceIssueDate>
  <completenessIndicator>false</completenessIndicator>
  <invoiceMain><invoice>
    <invoiceReference>
      <originalInvoiceNumber>TEST-ABERP/2026/0046</originalInvoiceNumber>
      <modifyWithoutMaster>false</modifyWithoutMaster>
      <modificationIndex>1</modificationIndex>
    </invoiceReference>
    <invoiceHead>
      <supplierInfo>…structured taxNumber + name + simpleAddress…</supplierInfo>
      <customerInfo>DOMESTIC + customerVatData + name + address</customerInfo>
      <invoiceDetail>
        <invoiceCategory>NORMAL</invoiceCategory>
        <invoiceDeliveryDate>2026-06-12</invoiceDeliveryDate>   <!-- = storno issue date -->
        <currencyCode>HUF</currencyCode>
        <exchangeRate>1.000000</exchangeRate>
        <paymentMethod>TRANSFER</paymentMethod>
        <paymentDate>2026-06-12</paymentDate>                    <!-- = storno issue date -->
        <invoiceAppearance>ELECTRONIC</invoiceAppearance>
      </invoiceDetail>
    </invoiceHead>
    <invoiceLines>
      <mergedItemIndicator>false</mergedItemIndicator>
      <line>
        <lineNumber>1</lineNumber>                               <!-- document-local, S373 -->
        <lineModificationReference>
          <lineNumberReference>2</lineNumberReference>           <!-- base count + 1, S369 -->
          <lineOperation>CREATE</lineOperation>                  <!-- S184 -->
        </lineModificationReference>
        <lineExpressionIndicator>false</lineExpressionIndicator>
        <lineDescription>Unicredit BA</lineDescription>
        <quantity>15</quantity>                                  <!-- POSITIVE -->
        <unitOfMeasure>DAY</unitOfMeasure>
        <unitPrice>-135000</unitPrice>                           <!-- sign lives here -->
        <lineAmountsNormal>
          <lineNetAmountData>  -2025000 / -2025000 HUF </lineNetAmountData>
          <lineVatRate><vatPercentage>0.27</vatPercentage></lineVatRate>
          <lineVatData>        -546750 / -546750  HUF </lineVatData>
          <lineGrossAmountData>-2571750 / -2571750 HUF </lineGrossAmountData>
        </lineAmountsNormal>
      </line>
    </invoiceLines>
    <invoiceSummary>
      <summaryNormal>
        <summaryByVatRate>0.27 → net/vat/gross all negative, HUF twins equal</summaryByVatRate>
        <invoiceNetAmount>-2025000</invoiceNetAmount> (+HUF)
        <invoiceVatAmount>-546750</invoiceVatAmount> (+HUF)
      </summaryNormal>
      <summaryGrossData><invoiceGrossAmount>-2571750</invoiceGrossAmount> (+HUF)</summaryGrossData>
    </invoiceSummary>
  </invoice></invoiceMain>
</InvoiceData>
```

Envelope (`crates/nav-transport/src/soap/mod.rs:226–241`): `<invoiceOperations>` →
`compressedContent=false` → per item `<index>` (1-based by slice position),
`<invoiceOperation>STORNO</invoiceOperation>` (detected from body shape by
`submit_invoice.rs:729 detect_operation_from_xml`), `<invoiceData>` = base64 of the
body. `electronicInvoiceHash` omitted — legal, it is `minOccurs="0"` in
`invoiceApi.xsd::InvoiceOperationType` and only required when
`completenessIndicator=true` (spec rule #26, page 194).

---

## 3. XSD structural conformance (invoiceData.xsd v3.0)

| # | NAV rule (verbatim XSD citation) | What we produce | Verdict |
|---|---|---|---|
| X1 | Root `InvoiceData` in `http://schemas.nav.gov.hu/OSA/3.0/data`; `InvoiceDataType` sequence = `invoiceNumber` (`common:SimpleText50NotBlankType`), `invoiceIssueDate` (`base:InvoiceDateType`), `completenessIndicator` (`xs:boolean`), `invoiceMain` | exactly that, in order (nav_xml.rs:733–757) | ✅ PASS |
| X2 | `InvoiceType` sequence: `invoiceReference` **minOccurs=0, FIRST child** → `invoiceHead` → `invoiceLines` (minOccurs=0) → `productFeeSummary` (0..2) → `invoiceSummary` | `invoiceReference` first, then head/lines/summary; no productFeeSummary | ✅ PASS |
| X3 | `InvoiceReferenceType` sequence = `originalInvoiceNumber` + `modifyWithoutMaster` + `modificationIndex` — **exactly three children, no `modificationIssueDate` exists in v3.0** | storno emits exactly the three (nav_xml.rs:1074–1092) | ✅ PASS (storno) / 🔴 **MODIFY path emits a 4th, schema-illegal element — F1** |
| X4 | `modificationIndex` type `base:InvoiceUnboundedIndexType` = `xs:int, minInclusive 1` | `u32` ≥ 1 from chain walker | ✅ PASS |
| X5 | `InvoiceHeadType` sequence: `supplierInfo`, `customerInfo` (0..1), `fiscalRepresentativeInfo` (0..1), `invoiceDetail` | supplier → customer → detail | ✅ PASS |
| X6 | `TaxNumberType` (invoiceBase.xsd): `taxpayerId` + `vatCode` (0..1) + `countyCode` (0..1); `elementFormDefault="qualified"` + `targetNamespace=…/OSA/3.0/base` ⇒ children are in the **base** namespace | emitter binds prefix `common:` to `http://schemas.nav.gov.hu/OSA/3.0/base` (nav_xml.rs:438,561) and writes `common:taxpayerId` etc. — the prefix NAME differs from the XSD's convention (`base:`), but the **URI is the namespace-correct one**; prefixes are arbitrary per XML-NS. Proven accepted: base CREATEs reach SAVED with the same writer | ✅ PASS |
| X7 | `InvoiceDetailType` sequence (page-order): `invoiceCategory`, `invoiceDeliveryDate`, [9 optionals], `currencyCode`, `exchangeRate`, [2 optionals], `paymentMethod` (0..1), `paymentDate` (0..1), [1 optional], `invoiceAppearance`, `conventionalInvoiceInfo` (0..1), `additionalInvoiceData` (0..∞) | category → deliveryDate → currencyCode → exchangeRate → paymentMethod → paymentDate → invoiceAppearance — a legal projection of the sequence | ✅ PASS |
| X8 | `ExchangeRateType`: `xs:decimal, totalDigits 14, fractionDigits 6, minExclusive 0` | `1.000000` (6 frac digits) | ✅ PASS |
| X9 | `LinesType`: `mergedItemIndicator` then `line` (1..∞) | matches | ✅ PASS |
| X10 | `LineType` sequence: `lineNumber`, `lineModificationReference` (0..1), …, `lineExpressionIndicator`, …, `lineDescription` (0..1), `quantity` (0..1), `unitOfMeasure` (0..1), `unitOfMeasureOwn` (0..1), `unitPrice` (0..1), …, choice(`lineAmountsNormal`/`lineAmountsSimplified`) | exact subsequence in order (nav_xml.rs:1444–1527) | ✅ PASS |
| X11 | `LineNumberType`: `xs:nonNegativeInteger, minInclusive 1`; `LineModificationReferenceType/lineNumberReference` also `base:LineNumberType`. XSD doc verbatim: *"In case of create operation the tag shall contain the new line number, as a sequential increment of the the existing lines set"* | `lineNumber` = document-local ordinal+1 (S373); `lineNumberReference` = base_line_count+ordinal+1 (S369) — exactly the XSD's own description | ✅ PASS |
| X12 | `LineOperationType` enum `{CREATE, MODIFY}` | `CREATE` (`CHAIN_LINE_OPERATION`, nav_xml.rs:1395) | ✅ PASS |
| X13 | `QuantityType` / `MonetaryType`: `common:GenericDecimalType` restricted by totalDigits/fractionDigits only — **no sign facet ⇒ negative amounts are schema-legal** | negative unitPrice/net/vat/gross | ✅ PASS |
| X14 | `RateType`: decimal 0..1, totalDigits 5, fractionDigits 4 | `0.27` | ✅ PASS |
| X15 | `LineAmountsNormalType`: `lineNetAmountData`, `lineVatRate`, `lineVatData` (0..1), `lineGrossAmountData` (0..1) | all four, in order | ✅ PASS |
| X16 | `SummaryType`: choice(`summaryNormal` \| `summarySimplified`+) then `summaryGrossData` (0..1); `SummaryNormalType`: `summaryByVatRate` (1..∞), `invoiceNetAmount`, `invoiceNetAmountHUF`, `invoiceVatAmount`, `invoiceVatAmountHUF`; `SummaryByVatRateType`: `vatRate`, `vatRateNetData`, `vatRateVatData`, `vatRateGrossData` (0..1) | matches exactly (nav_xml.rs:1629–1684) | ✅ PASS |
| X17 | `invoiceApi.xsd::InvoiceOperationType`: `index` (`base:InvoiceIndexType`), `invoiceOperation` (`ManageInvoiceOperationType`), `invoiceData` (`xs:base64Binary`), `electronicInvoiceHash` (0..1) | index from 1, `STORNO`, base64 body, hash omitted | ✅ PASS |

**XSD verdict: zero structural violations on the storno path.**

---

## 4. Blocking business rules (spec §3.3.2, all 80 rows walked)

Verbatim header (page 184): *"Blocking validation errors indicate content errors that
prevent successful data reporting. When such an error occurs, the invoice data report
cannot be considered successful."*

### 4.1 Rows that bit us historically — now closed

| Row | Code | Spec text (verbatim, abridged) | Our state | Verdict |
|---|---|---|---|---|
| 60 | `INVALID_LINE_OPERATION` | *"If the value of lineOperation element is 'MODIFY' in a data report of a modifying or cancelling invoice, it is an error … The value of lineOperation must always be 'CREATE'."* (p.199) | `CREATE` since S184 (nav_xml.rs:1395) | ✅ |
| 7 | `INVOICE_LINE_ALREADY_EXISTS` | *"includes a line number (lineNumberReference) designated as a line to be created (lineOperation = CREATE), which already exists in the data report regarding a previous invoice of the invoice chain"* (p.191–192) | reference = base_line_count + ordinal + 1 since S369 (nav_xml.rs:1462) | ✅ |
| 3 | `LINE_NUMBER_NOT_SEQUENTIAL` | *"The lineNumber element under the invoiceLines list element must be continuously ascending (no gaps in numbering)"* (p.191) | `lineNumber` decoupled from chain offset since S373 (nav_xml.rs:1461) | ✅ |

### 4.2 Storno-scoped rows that could still fire — scored against the current body

| Row | Code | Condition (spec verbatim, abridged) | Our state | Verdict |
|---|---|---|---|---|
| 1 | `SUPPLIER_TAX_NUMBER_MISMATCH` | seller tax number ≠ authenticated tax number | same supplier writer as SAVED CREATEs | ✅ |
| 2 | `INVOICE_NUMBER_NOT_UNIQUE` | *"The taxpayer has already performed data reporting on the invoice number"* | each storno burns a FRESH own number; ABORTed numbers are never registered by NAV (only saved reports count) | ✅ |
| 4 | `INVOICE_LINE_MISSING` | original invoice without items (CREATE, STORNO) | storno always carries the negated base lines | ✅ |
| 5 | `INVALID_INVOICE_REFERENCE` | *"refers to an invoice that cannot be found among the taxpayer's base invoices (where invoiceOperation = CREATE) … and modifyWithoutMaster tag is false"* | `originalInvoiceNumber` read byte-exact from the base's on-disk NAV XML since S184 (nav_xml.rs:1732) | ✅ (assuming base reached SAVED — true per operator) |
| 6 | `INVOICE_TYPE_MISMATCH` | *"the type of the invoice referenced … (invoiceCategory) does not match the invoice type specified in the modifying document"* | both hard-coded `NORMAL` (nav_xml.rs:1292) | ✅ today; latent if a future base is SIMPLIFIED/AGGREGATE — see F7 |
| 9 | `ANNULMENT_IN_PROGRESS` | *"a new modifying or cancelling invoice is received for a base invoice for which a technical annulment is already in progress and waiting for approval"* | not statically checkable — **operator must confirm no pending technical annulment sits on the base** (an unapproved ANNUL from past debugging would block every storno with this exact code) | ⚠️ VERIFY in ack |
| 11 | `REQUEST_VERSION_REFERENCE_ERROR` | chain version ≥ base version | base+storno both 3.0 | ✅ |
| 12 | `LINE_NUMBER_REFERENCE_NOT_UNIQUE` | *"lineNumberReference element must be unique at the invoice level"* | distinct per ordinal (offset+ordinal+1) | ✅ |
| 13 | `MANDATORY_LINE_CONTENT_MISSING` | lineExpressionIndicator=false ⇒ description required | description always written | ✅ |
| 14 | `INVALID_VAT_DATA` | vatPercentage ∈ {0.05, 0.07, 0.12, 0.18, 0.2*, 0.25*, 0.27, 0†} | `0.27` (and any base-inherited rate is one the base already passed with) | ✅ |
| 15 | `MULTIPLE_INVOICES_FOUND` | >1 valid instance of the base | single base | ✅ |
| 16 | `MODIFICATION_INDEX_NOT_UNIQUE` | *"Data has already been submitted using the modificationIndex provided"* | only SAVED reports occupy an index; all 35 prior attempts ABORTed ⇒ no occupied index. BUT our walker inflates the index anyway — see F4 | ✅ today |
| 18 | `INVOICE_REFERENCE_EXPECTED` | invoiceReference node mandatory for STORNO | present | ✅ |
| 20 | `MODIFY_WITHOUT_MASTER_MISMATCH` | modifyWithoutMaster=true but base exists | we always send `false` | ✅ |
| 21 | `LINE_MODIFICATION_EXPECTED` | lineModificationReference mandatory on every line | present on every line (nav_xml.rs:1467) | ✅ |
| 24/25 | `CUSTOMER_DATA_NOT_EXPECTED` / `_EXPECTED` | PRIVATE_PERSON forbids, non-PP requires name+address | conditional emit per ADR-0048 (nav_xml.rs:1175–1224) | ✅ |
| 26–28 | `ELECTRONIC_INVOICE_HASH_*` | hash required **only** *"If the completenessIndicator flag is true"* | completeness=false, hash omitted | ✅ |
| 29 | `INVOICE_APPEARANCE_MISMATCH` | completeness=true ⇒ appearance must be ELECTRONIC | completeness=false | ✅ (n/a) |
| 35 | `INCOMPLETE_ELECTRONIC_INVOICE_REFERENCE` | base completeness=false AND chain member completeness=true | both false | ✅ |
| 45 | `MISSING_CUSTOMER_DOMESTIC_TAXNUMBER` | DOMESTIC ⇒ customerVatData required | structured block emitted | ✅ |
| 51 | `INVALID_INVOICE_NUMBER` | no leading/trailing whitespace in invoiceNumber | template-validated + trimmed | ✅ |
| 52 | `MODIFICATION_SOURCE_MISMATCH` | OPG (online cash register) source conflicts | n/a, not OPG | ✅ |
| 54 | `INVOICE_DELIVERY_DATE_LATE` | delivery ≤ issue + 5 years | delivery = issue date | ✅ |
| 55 | `INVOICE_ISSUE_DATE_LATE` | issue ≤ submission + 1 year | server-clock issue date | ✅ |
| 56–59 | `LINE_SUMMARY_TYPE_MISMATCH_*` | normal/simplified cross-contamination | normal-only everywhere | ✅ |
| 61 | `INVALID_PREDECESSOR_OPERATION` | legal-predecessor additionalInvoiceData rules | we emit no additionalInvoiceData | ✅ |
| 66 | `INCORRECT_HEAD_DATA_MOD_REF_INVOICE_NUMBER` | *"The serial number of the amendment document is the same as the serial number of the original invoice"* | storno burns its own (different) number | ✅ |
| 76 | `…MODIFICATIONINDEX_UNREAL` | modificationIndex > 1000 | far below; F4 inflation is bounded by retry count | ✅ |
| 77 | `INCORRECT_HEAD_DATA_CURRENCY_CODE_HUF` | *"The currency of the invoice is HUF, but the exchange rate of the invoice is not 1"* | `1.000000` — numerically 1 (xs:decimal value-space comparison); empirically proven: every SAVED CREATE carries the same 6-decimal form | ✅ |
| 79 | `INVALID_ORIGINAL_INVOICE_NUMBER` | no leading/trailing whitespace | byte-exact read from base XML | ✅ |

Rows not listed (8, 10, 17, 19, 22, 23, 30–34, 36–50, 53, 62–65, 67–75, 78, 80) are
N/A by construction: ANNUL-only, CREATE-only, batch-only, OPG-only, product-fee,
vatExemption/vatOutOfScope/vatAmountMismatch/reverse-charge codes we never emit, or
aggregate/simplified categories we never emit.

**Blocking verdict: with a SAVED base, no pending technical annulment, and the chain
state as described (zero saved chain members), no blocking rule matches the current
storno body.**

---

## 5. WARN-level divergences (do not block SAVED, but NAV flags them)

| Code (Annex I) | Trigger (spec verbatim, abridged) | Our state | Will it fire? |
|---|---|---|---|
| `UNINTENDED_CANCELLATION_DELIVERY_DATE` (ID 11401, p.265) | *"The cancellation changes the delivery date of the original invoice … to the issue date of the cancelling invoice. This is a common error in invoicing programs."* Runs when the dates *"fall on different days."* STORNO-scoped. | `issue_storno.rs:332–340` stamps **today** as `delivery_date`; base delivery date is ignored | **YES**, whenever the storno is issued on a different day than the base's delivery date — i.e. essentially always. F2. |
| `INCONSISTENT_MODIFICATION_DATA_NETAMOUNT_NOT_ZERO_NORMAL` (ID 1200, p.361) + `_VATAMOUNT_NOT_ZERO` (1220) + `_VATAMOUNT_NOT_ZERO_HUF` (1230) | *"the sum of the amount of the invoice submitted with the STORNO operation and the aggregated net amount of the original invoice referenced and its previous modifications … is not zero. Tolerated deviation: 1 unit. … Only runs if … the chain of previous modifications (modificationIndex) is complete."* | storno negates the BASE's lines only (`render_storno_data` nav_xml.rs:792). Correct while chains are storno-only; WRONG once a prior MODIFY exists (must reverse base ⊕ all modifications). Also note the *"chain … complete"* gating — index gaps (F4) silently disable this safety net. | Not today (no MODIFY ever SAVED). Latent — F5. |
| `INCONSISTENT_MODIFICATION_DATA_STORNO_ALREADY_EXISTS` (p.360) | a storno already SAVED for the base | zero SAVED stornos | No (until the first success; subsequent retries would WARN). |
| `INCORRECT_LINE_REFERENCE` (p.299) | *"Only runs if in the given line item lineOperation='MODIFY'"* | always CREATE | No. |
| Quantity-sign convention (spec §2.5.1, p.163, not a coded WARN) | *"It should contain the data of the line items of the original invoice, with opposite signs for all quantities (consequently, the total quantities for all line items tends to be negative)."* | `negate_line` (nav_xml.rs:1045) keeps **quantity positive** and negates **unitPrice**. The arithmetic-consistency WARNs (`INCORRECT_LINE_CALCULATION_NET_AMOUNT`: quantity × unitPrice ≈ lineNetAmount, tolerance *"1% of the net price, but no less than 1 unit"*, p.204) hold either way: 15 × −135000 = −2025000 ✓ | No coded check fires; letter-of-spec divergence only. F3. |

---

## 6. Findings

### 🔴 F1 — MODIFY sibling path emits `<modificationIssueDate>`, an element that does not exist in NAV v3.0; the STORNO/MODIFY classifier is built on it

- **Evidence:** v3.0 `InvoiceReferenceType` (invoiceData.xsd, quoted §3 X3) has exactly
  three children. `write_modification_reference` (nav_xml.rs:1114–1137) emits a fourth,
  `<modificationIssueDate>`, between `originalInvoiceNumber` and `modifyWithoutMaster`.
  PR-11/ADR-0024 sourced it from the research doc
  (`docs/research/nav-and-billingo.md:220–222`), which describes the **v2.0** shape —
  v3.0 removed the element.
- **Blast radius:** every `MODIFY` submission is a guaranteed schema-fail
  (`technicalValidationMessages` row 1: *"not schema-valid XML"*, p.183) — this path
  has simply never been exercised against NAV. Worse, the operation classifier
  `detect_operation_from_xml` (submit_invoice.rs:729–740) uses the PRESENCE of this
  illegal element as the STORNO/MODIFY discriminator, and the nav-xsd-validator
  allowlists it (validate.rs:259–264). Removing the element naively re-classifies
  every modification as STORNO.
- **Fix:** drop the element from `write_modification_reference`; carry the operation
  as a typed parameter end-to-end (issuance already knows which chain kind it is — the
  audit-ledger payload kind names it) instead of sniffing the XML; prune the validator
  allowlist in the same PR (ADR-0022's "extend in the same PR" rule). MODIFY's
  legally-required "modification issue date" is simply `invoiceIssueDate` of the
  amending document (spec §2.5.2 item 3, p.163) — already emitted.

### 🟡 F2 — Storno stamps TODAY as `invoiceDeliveryDate`; NAV names this "a common error in invoicing programs"

- **Evidence:** `issue_storno.rs:332–340` (`delivery_date = payment_deadline =
  issue_date.date()`); WARN `UNINTENDED_CANCELLATION_DELIVERY_DATE` fires on
  day-granularity difference (§5). Beyond the WARN, the delivery date drives
  VAT-period assignment — a storno reversing a May invoice with a June delivery date
  asserts the reversal in the wrong VAT period on the regulatory record.
- **Fix:** read the base's `<invoiceDeliveryDate>` from the base's on-disk NAV XML
  (same S184/S369 canonical-record discipline; add a sibling helper next to
  `count_invoice_lines_from_xml`, nav_xml.rs:1818) and stamp it on the storno.
  `paymentDate` is `minOccurs=0` — for a storno either copy the base's or omit it
  (omission is the cleaner reading; nothing falls due on a cancellation).

### 🟡 F3 — Sign convention: spec says negate QUANTITIES; we negate unitPrice

- **Evidence:** §2.5.1 quote in §5 vs `negate_line` (nav_xml.rs:1045–1064: *"Quantities
  stay positive … the negation lives in unit_price"*).
- **Impact:** no coded ERROR or WARN fires (arithmetic stays consistent within NAV's
  1%/1-unit tolerance), but we diverge from the spec's letter and from what NAV's
  analysts/auditors expect to see in the warehouse. Cheap to conform while F2 is open:
  negate `quantity`, keep `unit_price` positive — line totals are unchanged.
- **Risk note:** the WARN engine evolves ("The list of data and contexts checked …
  remains open", p.204); conforming now removes a future tripwire.

### 🟡 F4 — `modificationIndex` is allocated from the LOCAL ledger, so NAV-ABORTed attempts inflate it; gaps disable NAV's chain-zeroing safety net

- **Evidence:** `next_modification_index_in_tx` (issue_storno.rs:1102) returns
  `max(local modification_index)+1`, counting storno issuances whose NAV submits
  ABORTed. After N failed attempts the next storno carries `modificationIndex=N+1`
  while NAV has saved zero chain members.
- **Impact:** NOT blocking — the spec enforces only uniqueness
  (`MODIFICATION_INDEX_NOT_UNIQUE`, p.193) and `>1000` (row 76). But all three
  `INCONSISTENT_MODIFICATION_DATA_*_NOT_ZERO*` WARNs *"only run if … the chain of
  previous modifications (modificationIndex) is complete"* — a gap from 1 silently
  switches off NAV's own zero-sum verification of our storno forever.
- **Fix:** allocate the index from SAVED-confirmed chain members only (the ledger
  already records ack status), falling back to 1 when no chain member ever reached
  SAVED. Cross-system correctness (Billingo-era bases) eventually wants
  `queryInvoiceChainDigest` (ADR-0023 §4's named deferral).

### 🟡 F5 — Storno reverses the BASE only, not "the original invoice and the result of all previous modifications"

- **Evidence:** §2.5.1 (p.163): summary amounts *"shall be displayed with the sign
  which is the opposite of that of the original invoice **and the result of all
  previous modifications**"*; `render_storno_data` negates `invoice.lines` = the
  base's lines (nav_xml.rs:792).
- **Impact:** correct today (no MODIFY has ever reached NAV — see F1), wrong the day
  the first modification lands in a chain. WARN-level (IDs 1200/1220/1230), not
  blocking. Park behind F1; fix when MODIFY goes live.

### 🟢 F6 — nav-xsd-validator drift (informational, fix alongside F1)

- `walk_invoice_reference` allowlists `modificationIssueDate`
  (validate.rs:254–264) — dead-wrong-positive surface per F1.
- The validator never checks the storno-specific business layer (sign coherence,
  reference offsets) — by design (ADR-0022 structural-only); no action.

### 🟢 F7 — Hard-coded `invoiceCategory=NORMAL` matches today's universe

- `INVOICE_TYPE_MISMATCH` (row 6) requires the storno's category to equal the base's.
  Both sides are the same constant (nav_xml.rs:1292). Becomes a real bug only if
  SIMPLIFIED/AGGREGATE issuance ever ships; note for that future session.

### 🟢 F8 — Confirmed-fine items worth not re-litigating

- `1.000000` HUF exchange rate (row 77 + X8); `common:`-prefix-bound-to-base-URI
  namespace mapping (X6); omitted `electronicInvoiceHash` under
  `completenessIndicator=false` (rows 26–28); `paymentMethod`/`paymentDate` presence
  (both `minOccurs=0`, no storno-scoped blocking rule); positive `quantity` passing the
  calculation WARNs; `<index>=1` envelope; operation token `STORNO`
  (soap/mod.rs:113–120).

---

## 7. Risk ordering — what would actually cause the NEXT rejection

1. **Unknown-unknown in the live ack (highest information value, zero code).** Nothing
   in the published contract blocks the current body. S381 step 0 must be: issue ONE
   storno against a fresh DEV base on the NAV test endpoint, poll
   `queryTransactionStatus`, and decode `businessValidationMessages`/
   `technicalValidationMessages` from the stored `response_xml` (S184 decode
   discipline). If it ABORTs, the code we read there IS the next fix; if it's
   DONE+WARN, expect exactly the two WARNs predicted in §5 and ship.
2. **`ANNULMENT_IN_PROGRESS` (row 9)** — if any past debugging left an unapproved
   technical annulment on a prod base, every storno against it blocks with this code.
   Check the NAV portal / `queryTransactionStatus` for pending annulments before
   blaming the body.
3. **F1** if the operator ever triggers the modification path (guaranteed
   schema-fail today).
4. **F2** — fires a WARN on virtually every storno and mis-states the VAT period;
   first body change to make.
5. **F4** — restores NAV's own zero-sum verification; small, contained.
6. **F3** — convention conformance; trivially co-ships with F2.
7. **F5/F7** — latent; park behind MODIFY/SIMPLIFIED enablement.

## 8. Proposed S381 scope (one paragraph)

S381 ("NAV storno XSD conformance") should (0) submit one storno from DEV against the
NAV test endpoint at PROD_v2.27.41 and decode the actual ack before changing any code
— if a blocking code appears it overrides this list; then land four contained changes:
(1) copy the base's `invoiceDeliveryDate` onto the storno from the base's on-disk NAV
XML and stop stamping `paymentDate`/`deliveryDate` with the storno's issue date
(F2, new `read_invoice_delivery_date_from_xml` helper beside
`count_invoice_lines_from_xml`); (2) flip `negate_line` to negate `quantity` instead
of `unit_price` per spec §2.5.1 (F3, line totals unchanged, update round-trip pins);
(3) allocate `modificationIndex` from SAVED-confirmed chain members instead of all
local issuances (F4, issue_storno.rs:1102 + issue_modification.rs sibling); and
(4) delete `<modificationIssueDate>` from `write_modification_reference`, replace the
XML-sniffing `detect_operation_from_xml` with a typed operation passed from the
issuance layer, and prune the validator allowlist in the same PR (F1/F6). Each change
carries a round-trip test pinning the new wire bytes; expected outcome of the next
real submission is SAVED (or DONE with only `UNINTENDED_CANCELLATION_DELIVERY_DATE`
until (1) deploys).
