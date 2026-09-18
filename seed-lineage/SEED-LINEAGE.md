# Seeding real lineage into CDGC

This kit authors the lineage edges the Provenance Stamping policy needs before it
can stamp `x-dp-lineage-*` headers:

```
dim_product.csv  ──(dataset flow)──▶  fact_order_line.csv
  dim_product.sku        ─────▶  fact_order_line.sku
  dim_product.list_price ─────▶  fact_order_line.unit_price
```

`dim_product` (the Product Catalog scan) is the master; `fact_order_line` (the
Order Transaction scan) consumes its sku / price. That is a genuine, defensible
dataflow to model.

**There is a direct lineage API.** Earlier versions of this kit claimed CDGC had
no inline "create lineage" endpoint and that lineage could only be ingested by a
Custom Metadata Integration catalog source scanned from a `GenericLinks.zip`. That
is wrong: the content service's **`relationship` segment** authors edges directly.
`apply_lineage.py` uses it, needs no Secure Agent, no MCC UI step and no scan, and
is idempotent. The catalog-source route is kept below as a fallback.

---

## Primary route — `apply_lineage.py`

```bash
export IDMC_TOOLKIT=<dir containing the idmc/ package>
source ../demo/env.local.sh          # CDGC creds (gitignored)

python apply_lineage.py --dry-run    # resolve reference ids, print the plan
python apply_lineage.py              # author the edges
python seed_lineage.py --verify      # confirm real lineage now exists
```

`links.csv` stays the source of truth, so the same file drives either route.

### The API

```
PATCH {cdgc-api-host}/data360/content/v1/assets/{assetId}?scheme=internal
[
  { "operation": "add",
    "segment": "relationship",
    "items": [ { "fromIdentity": "<uuid>",
                 "toIdentity":   "<uuid>",
                 "association":  "core.DataSetDataFlow" } ] }
]
```

Headers: `Authorization: Bearer <jwt>`, `X-INFA-ORG-ID`, `X-INFA-PRODUCT-ID: CDGC`,
plus the login session cookie (`USER_SESSION` / `IDS_TOKEN`) — these writes are
PEP-gated and the JWT alone is not enough.

### Deleting *any* catalog source can silently wipe your edges

`DELETE /datasources/{id}` submits an async **"Bulk Purge Group"** job, and that
purge removed all three edges authored here — even though they were created
through the content API and their `core.sourceOrigin` was the *Product Catalog*
source, not the one being deleted. Observed live: delete the unrelated
`Custom Lineage` source, and `seed_lineage.py --verify` goes from three edges to
none.

Two things make this easy to miss:

- **The gateway keeps serving `present`** from its 24h cache long after the edges
  are gone, so the demo looks fine while the catalog underneath is empty.
- **The purge is async and slow.** The delete returns `200` immediately, the
  source stays in the listing for 10+ minutes, and the edges disappear on the
  purge's own schedule.

Re-run `apply_lineage.py` after deleting any catalog source, and confirm with
`--verify` rather than trusting the response headers. Rows coming back `200`
instead of `409 SAME` is itself the tell that the edges had been purged.

### Three things that will cost you an afternoon

1. **The body is a bare JSON list**, not a single object. Sending the object form
   documented in most examples fails with
   `Cannot deserialize value of type ArrayList<UpdateAssetRequest>` — reported as
   an HTTP **500**, which reads like a server fault rather than a bad request.
2. **`fromIdentity` / `toIdentity` are the internal `core.identity` UUID**, not the
   `core.externalId` reference id that `links.csv` carries. `apply_lineage.py`
   resolves each row through `ccgf-searchv2` first.
3. **409 `Relationship already exists` is success**, and is what makes re-runs
   idempotent. The script reports those as `SAME`.

`operation: "remove"` is exposed as `--remove` for symmetry; it has not been
exercised against a live edge here.

### links.csv format

Columns: **`Source,Target,Association`**.

- `Source` / `Target` are each the asset's **Reference ID** — exactly its CDGC
  `core.externalId`, of the form `<catalogSourceId>://<path>~<classType>`. In Data
  Governance and Catalog it is the **Reference ID** field on an asset's
  **System Attributes** tab. Copy it verbatim; a hand-built id silently matches
  nothing.
- `Association` is the relationship type, and is **case-sensitive**:

  | Association | Links |
  |---|---|
  | `core.ResourceParentChild` | catalog source → schema |
  | `core.DataSourceParentChild` | schema → table |
  | `core.DataSetToDataElementParentship` | table → column |
  | `core.DataSetDataFlow` | **table → table (dataset lineage)** |
  | `core.DirectionalDataFlow` | **column → column (element lineage)** |

Validate before applying — `validate_links.py` resolves every reference id and
checks the association names:

```bash
python validate_links.py
```

This matters on the fallback route especially: a reference id that doesn't match a
catalogued asset does **not** fail the scan, it just silently writes no edge, so
you get a green job and no lineage.

---

## Fallback route — Custom Lineage catalog source

Use this if the content API is unavailable on your tenant. It ingests the same
`links.csv`, packaged as `GenericLinks.zip`, via a Custom Metadata Integration
catalog source. Steps 4–7 of Informatica's
*Custom Metadata Integration Reference* plus KB 000192802.

**Phase A is UI-only.** Re-probed against this tenant: every REST path for creating
a custom catalog source *type* returns 404, and there is no file-upload endpoint.
Only catalog source CRUD and run are public
(`/data360/catalog-source-management/v1/catalogsources`, confirmed 200 here).

**Prerequisite:** your role needs Create/Read/Update/Delete on the **Custom Catalog
Source Type** asset (Administrator → Roles → Asset permissions). Since the
November 2025 release you also need permissions on the *connections* behind any
catalog source you create, edit, schedule or run.

1. **New → Customization → Custom Catalog Source Type.** Name it e.g.
   `Custom Lineage`, Save. There is no template or source-type picker here — the
   type is just a label.
2. **Build the zip:** `python seed_lineage.py --zip --validate`. The archive must be
   `GenericLinks.zip` containing `links.csv`, exactly those names and casing, and
   the zip name may not contain extra dots. The CSV must be **UTF-8 without BOM**,
   and the three headers must be three separate comma-delimited columns.
3. **New → Catalog Source**, expand **Custom Catalog Source Type**, select the type
   from step 1, **Create**.
4. **Registration** page: name it, then under **Connection Information** set
   **Metadata Source Type = CSV Files**, **Source Type = Upload**, and attach
   `GenericLinks.zip` under **File Details**.
5. **Runtime Environment:** `MultiTenantServerless` works and is the simplest
   choice — **no Secure Agent is required** for an Upload-based custom lineage
   source. (Verified on this tenant: a working `Custom Lineage` source runs with
   `Runtime Environment = MultiTenantServerless`.) Earlier notes in this project
   claimed lineage seeding was blocked on a Secure Agent; it never was.
6. **Next** → **Configuration**: **Metadata Extraction** is on by default and is all
   a links-only job needs. **Next** through **Associations** and **Schedule**, then
   **Save**.
7. Note the catalog source UUID from the browser address bar, then **Run** (or
   Actions → Run on the Explore page). Watch it under **Job Monitoring**.

Re-running afterwards is scriptable:

```bash
export CUSTOM_LINEAGE_DS_ID=<uuid from step 6>
python seed_lineage.py --sync
python seed_lineage.py --verify
```

> **No custom model is required.** A custom model only matters when introducing new
> asset *classes*. `links.csv` references assets CDGC has already catalogued, so a
> stock custom catalog source type is enough.

> **Scanned lineage is additive.** Deleting a row from `links.csv` and re-scanning
> does **not** remove the old edge — you have to purge the custom lineage catalog
> source and re-run. (Purging that source does not touch the real underlying
> catalog sources.) The content API route does not have this problem.

---

## Verify

```bash
python seed_lineage.py --verify      # exits 0 once real lineage edges exist
./../demo/demo.sh                    # end-to-end through the gateway
```

`--verify` reports only **non-structural** edges, filtering out the file/field, DQ,
glossary and marketplace relationships that are always present — the same denylist
the policy uses. The deployed policy then stamps:

```
x-dp-lineage-downstream: fact_order_line.csv
x-dp-lineage-status: present
```

with **no policy rebuild**.

> The policy caches derived provenance for `refreshIntervalSeconds` (default 24h)
> and a redeploy does not clear it. If a freshly-seeded edge doesn't show, lower
> that value via `api-mgr:policy:edit`, redeploy, make one warm-up call, then
> restore.

---

## Do not re-scan the Product Catalog source

`dim_product.csv` is catalogued at `/data/csv/dim_product.csv`, but the file now
also sits under a `product-catalog/` subfolder on the demo share. A full re-scan
could re-home the asset under a new path, changing its `core.externalId` — which
would invalidate both `links.csv` **and** the `schemaId` the deployed policy is
configured with.

## Security

CDGC credentials are read from the environment only — never write them to a file in
this repo. `../demo/env.local.sh` and `../demo/config.json` are gitignored; only the
`.example` variants are tracked. `links.csv` is gitignored too, because reference
ids embed tenant catalog source ids.
