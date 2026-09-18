# Seeding real lineage into CDGC (Custom Lineage)

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

## Why a catalog source and not an API call

CDGC has **no supported inline "create lineage" API.** Re-probed live against the
tenant on 2026-09-18: every plausible REST path on
`ccgf-catalog-source-management/api/v1` returns 404 — `/datasourcetypes`,
`/customtypes`, `/customCatalogSourceTypes`, `/models`, `/seeds`, `/connectors`,
`/datasources/{id}/files`, `/datasources/{id}/jobs` — and no OpenAPI document is
served. Only `/datasources` (list, GET, multipart POST) and
`/datasource/{id}/operations/sync` exist.

So **Phase A below is genuinely UI-only**, and the `GenericLinks.zip` upload is
part of it. Phase B (re-running the scan) *is* scriptable, and is what
`seed_lineage.py --sync` does.

> **Tested and rejected:** the catalog source's *"Provide a Local Path"* option
> would let the Secure Agent read the zip off its own filesystem, which would make
> the whole flow scriptable. On this tenant it does not work — a probe file staged
> on the demo SFTP share (`/upload/data-products/...`) never appeared to a catalog
> source scanning the corresponding agent path (`/data/csv/...`) within 8 minutes,
> so that share is **not** the agent's mount. Use **Upload**.

## Artifacts in this directory

| file | what |
|---|---|
| `links.csv.example` | the 3 lineage edges, in CDGC's `Source,Target,Association` format, with placeholder reference ids |
| `links.csv` | your real copy — **gitignored**, it embeds tenant asset ids |
| `GenericLinks.zip` | generated from `links.csv`; the only name the scanner accepts |
| `validate_links.py` | read-only pre-flight: resolves every reference id against CDGC |
| `seed_lineage.py` | `--zip` / `--validate` / `--sync` / `--verify` |

### links.csv format

Columns: **`Source,Target,Association`**.

- `Source` / `Target` are each the asset's **Reference ID** — which is exactly its
  CDGC `core.externalId`, of the form `<catalogSourceId>://<path>~<classType>`.
  In Data Governance and Catalog it is the **Reference ID** field on an asset's
  **System Attributes** tab.
- `Association` is the relationship type, and is **case-sensitive**:

  | Association | Links |
  |---|---|
  | `core.ResourceParentChild` | catalog source → schema |
  | `core.DataSourceParentChild` | schema → table |
  | `core.DataSetToDataElementParentship` | table → column |
  | `core.DataSetDataFlow` | **table → table (dataset lineage)** |
  | `core.DirectionalDataFlow` | **column → column (element lineage)** |

This kit uses the last two. The shape of a row (placeholders for the two catalog
source ids):

```
<productCatalogSourceId>://FileServer/data/csv/dim_product.csv~com.infa.odin.models.file.flat.FlatFile,<orderTransactionSourceId>://FileServer/data/csv/order-transactions/fact_order_line.csv~com.infa.odin.models.file.flat.FlatFile,core.DataSetDataFlow
```

Columns append `/<colName>` to the file path and use the `…FlatField` class type.

---

## Phase A — one-time setup in Metadata Command Center (UI)

**Before you start:** your role needs Create/Read/Update/Delete on the
**Custom Catalog Source Type** asset (Administrator → Roles → Asset permissions).

### A1. Create the Custom Catalog Source Type

1. In **Metadata Command Center**, click **New** in the left navigation panel.
2. In the **New** dialog, select **Customization** in the left pane, then click
   **Custom Catalog Source Type** in the right pane.
3. **Name:** `Custom Lineage` (any name; it just labels the source system).
4. Optionally add a description, then click **Save**.

It now appears on the **Customize** page.

> **No custom model is needed.** A custom model (Steps 1–3 of Informatica's
> Custom Metadata Integration workflow) only matters when you are introducing
> *new asset classes*. Here `links.csv` references assets that CDGC has already
> catalogued by their existing reference ids, so the stock custom source type is
> enough.

### A2. Build the zip

```bash
python seed_lineage.py --zip        # links.csv -> GenericLinks.zip
python seed_lineage.py --validate   # every reference id must resolve
```

`--validate` matters: a reference id that doesn't match a catalogued asset does
**not** fail the scan — it just silently writes no edge, so you get a green job
and no lineage.

The names are fixed by the scanner (KB 000192802): the archive must be
`GenericLinks.zip` and the file inside it `links.csv`. The zip name may not
contain extra dots.

### A3. Create the catalog source

1. Click **New** in the left navigation panel, then select **Catalog Source**
   in the left pane.
2. In the right pane, expand **Custom Catalog Source Type** and select the
   **Custom Lineage** type you created in A1. Click **Create**.
3. On the **Registration** page, enter a name, e.g. `Product Lineage Seed`.
4. In **Connection Information**:
   - **Metadata Source Type:** `CSV Files`
   - **Source Type:** `Upload`
   - **File Details:** Browse to (or drag in) `GenericLinks.zip`
   - **Runtime Environment:** the Secure Agent runtime — the same one the four
     File System catalog sources already use.
5. Click **Next** to **Configuration**. **Metadata Extraction** is enabled by
   default; leave it. Nothing else needs enabling for pure lineage.
6. Click **Next** through **Associations** (stakeholders — optional) and
   **Schedule** (leave unscheduled; this is a one-off).
7. Click **Save**.

### A4. Note the catalog source id

Open the saved catalog source; its id is the UUID in the browser address bar.

```bash
export CUSTOM_LINEAGE_DS_ID=<that-uuid>
```

### A5. Run it

Click **Run** on the wizard (or **Actions → Run** on the Explore page), then
**Run** again in the **Run Catalog Source Job** dialog. Watch it under
**Job Monitoring**.

---

## Phase B — re-running (scripted)

Once the source exists, re-running the scan needs no UI:

```bash
source ../demo/env.local.sh                 # CDGC creds (gitignored)
export IDMC_TOOLKIT=<dir containing the idmc/ package>
export CUSTOM_LINEAGE_DS_ID=<uuid from A4>

python seed_lineage.py --sync               # POSTs operations/sync, prints the jobId
python seed_lineage.py --verify             # did real lineage edges appear?
```

`--verify` exits 0 once a **non-structural** edge touches `dim_product.csv` or its
columns (it filters out the file/field, DQ, glossary and marketplace edges that
are always there).

**To change the links** after Phase A, edit `links.csv`, re-run
`--zip --validate`, then **re-upload `GenericLinks.zip` under File Details** on
the catalog source in MCC and `--sync` again. The upload is the one step that
cannot be scripted.

---

## Verify end to end

```bash
python seed_lineage.py --verify
```

Expect a `core.DataSetDataFlow` / `core.DirectionalDataFlow` edge now touching
`dim_product.csv` and its `sku` / `list_price` columns. The deployed Provenance
Stamping policy then stamps:

```
x-dp-lineage-downstream: fact_order_line.csv
x-dp-lineage-status: present
```

with **no policy rebuild** — it detects lineage by a denylist of structural
relationship types, so any real dataflow edge counts. Re-run `../demo/demo.sh` to
see it on the wire.

> The policy caches derived provenance for `refreshIntervalSeconds` (default 24h),
> and a redeploy does not clear it. To see the change immediately, lower that
> value via `api-mgr:policy:edit`, redeploy, make one warm-up call, then restore.

---

## Do not re-scan the Product Catalog source

The catalogued path for `dim_product.csv` is `/data/csv/dim_product.csv`, but the
file now also lives under a `product-catalog/` subfolder on the demo share. A full
re-scan of that catalog source could re-home the asset under a new path, which
changes its `core.externalId` — invalidating both `links.csv` **and** the
`schemaId` the deployed policy is configured with. Seed lineage with its own
catalog source (as above); leave the existing four alone.

## Security

CDGC credentials are read from the environment only — never write them to a file
in this repo. `../demo/env.local.sh` and `../demo/config.json` are gitignored;
only the `.example` variants are tracked. `links.csv` is gitignored too, because
reference ids embed tenant catalog source ids.
