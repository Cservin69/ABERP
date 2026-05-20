# ID Prefix Registry

Authoritative list of entity ID prefixes. Adding or changing a prefix requires
a PR that updates this file and the corresponding module.

| Prefix | Entity                              | Owning module     | Notes |
|--------|-------------------------------------|-------------------|-------|
| `tnt`  | Tenant                              | bootstrap         | Lives in the tenant registry, not in tenant DBs |
| `usr`  | User                                | auth              | |
| `ses`  | Session                             | auth              | Short-lived |
| `inv`  | Invoice                             | billing           | Internal ID; NAV-facing number is separate |
| `inl`  | Invoice line                        | billing           | |
| `pay`  | Payment                             | billing           | |
| `cus`  | Customer                            | billing / contacts | |
| `sup`  | Supplier                            | contacts          | |
| `prd`  | Product / SKU                       | inventory         | |
| `var`  | Product variant                     | inventory         | |
| `lot`  | Lot / batch                         | inventory         | |
| `srn`  | Serial number record                | inventory         | |
| `loc`  | Stocking location                   | inventory         | |
| `mvt`  | Inventory movement                  | inventory         | |
| `ord`  | Sales / purchase order              | orders            | |
| `oln`  | Order line                          | orders            | |
| `shp`  | Shipment                            | logistics         | |
| `pkg`  | Package / parcel                    | logistics         | |
| `lbl`  | Printed label / QR vignette         | labels            | |
| `rbt`  | Robotics task                       | robotics          | |
| `cad`  | CAD artifact                        | cad               | |
| `cam`  | CAM artifact / toolpath             | cad               | |
| `aud`  | Audit ledger entry                  | audit             | Hash-chained |
| `evt`  | Domain event envelope               | platform          | |
| `idem` | Idempotency key (Layer-1 retries)   | billing           | Per ADR-0009 §5; same ULID for retries of the same command. Canonical string is the on-disk format stored in `audit_ledger.idempotency_key` |

**Reserved, not yet used:** `prj` (project), `wrk` (work order), `mat` (material spec), `bom` (bill of materials).
