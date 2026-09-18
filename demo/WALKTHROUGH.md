# Demo walkthrough — Data Product Provenance Stamping

**Scenario.** An AI agent calls a **Product Catalog** data product (an MCP
`get_products` tool) through the Omni/Flex Gateway. The payload is ordinary
product data. Without touching the body, the gateway stamps onto every response
the data's **verifiable provenance**, derived live from Informatica CDGC — proving
*where the data came from, how fresh it is, and whether it's certified* — so the
agent (or a downstream audit/policy) can trust and attribute what it received.

**One input.** The policy is configured with just a CDGC `schemaId`
(`<schemaId>` = the scanned `dim_product.csv`). Everything else is read from the
catalog at request time.

## Run it

```bash
cp demo/env.local.sh.example demo/env.local.sh   # set PROV_GW_URL
./demo/demo.sh
```

## What you see (live)

`get_products` returns **HTTP 200** with the product records, and the response
carries these gateway-stamped headers (all derived from CDGC):

| Header | Live value | Meaning |
|---|---|---|
| `x-dp-provenance-name` | `dim_product.csv` | the governed asset |
| `x-dp-provenance-origin` | `<catalogSourceId>` | catalog / scan origin id |
| `x-dp-provenance-external-id` | `<catalogSourceId>://FileServer/data/csv/dim_product.csv~…FlatFile` | CDGC external id |
| `x-dp-provenance-source-type` | `File System` | connector class |
| `x-dp-provenance-catalog-source` | `Product Catalog dim_product` | the catalog source that scanned it |
| `x-dp-provenance-path` | `FileServer/data/csv/dim_product.csv` | source-of-record path |
| `x-dp-provenance-source-modified` | `2026-08-31` | source-of-record mtime |
| `x-dp-provenance-catalog-modified-ms` | `17883477…` | last CDGC scan (epoch ms) |
| `x-dp-provenance-lifecycle` | `Published` | governance lifecycle state |
| `x-dp-provenance-certified` | `true` | steward-certified |
| `x-dp-provenance-collection` | `Product Catalog` | marketplace data collection |
| `x-dp-provenance-source` / `-status` | `cdgc` / `ok` | provenance resolved from CDGC |
| `x-dp-lineage-status` | `none` | no lineage edges seeded **yet** |

## The lineage act (optional second beat)

Out of the box CDGC has provenance but **no lineage** for this asset, so
`x-dp-lineage-status: none`. Seed real lineage with `seed-lineage/` (authors
`dim_product.csv → fact_order_line.csv` via a CDGC Custom Lineage catalog source),
re-run, and the same response now also carries:

```
x-dp-lineage-downstream: fact_order_line.csv
x-dp-lineage-status: present
```

with **no policy rebuild** — the policy recognises any real dataflow edge by a
denylist of structural relationship types.

## Fail-open

If CDGC is unreachable, the policy stamps `x-dp-provenance-status: unavailable`
and the product data still flows (enrichment never blocks the response).
