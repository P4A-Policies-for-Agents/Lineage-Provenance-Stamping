# Data Product Provenance Stamping — MuleSoft Omni/Flex Gateway Policy

An **inbound, headers-only, fail-open** custom policy for the MuleSoft Omni/Flex
Gateway that makes a data product's output port **self-attesting about where its
data came from**. Given only a **CDGC asset id** for the scanned schema (a flat
file, table, etc.), it derives the product's **provenance** — catalog origin,
source-of-record path, scan freshness, lifecycle, certification, marketplace
collection — and, when present, its **data lineage** (upstream/downstream
neighbours), and stamps them onto every response as `x-dp-provenance-*` and
`x-dp-lineage-*` headers. No per-field configuration — everything is read from the
catalog at runtime.

Built with the PDK, Rust → `wasm32-wasip1`, split-model. Applies to **MCP** and
**REST/HTTP APIs** (`assetTypes: mcp,rest,http`). Headers-only, so it is
transport-agnostic across both.

> Enrichment only — it adds headers and **fails open** (a CDGC outage degrades to
> `x-dp-provenance-status: unavailable`; it never blocks the response). It is the
> provenance sibling of the **Contract Metadata Injection** policy (which stamps
> the *contract* — fields/required/sensitive) and shares its catalog-driven CDGC
> client, IDMC auth, and DataStorage cache.

---

## How it works (catalog-driven)

On the **request leg** (await-safe under `enable_stop_iteration`), on a cache miss
it authenticates to IDMC (Login → JWT) and then, via the CDGC search API
**`POST cdgc-api…/ccgf-searchv2/api/v1/search`** (Elasticsearch DSL):

1. **Resolve the schema asset** (`core.identity = schemaId`) → reads its provenance
   attributes: `core.origin`, `core.externalId`, `core.resourceType`,
   `core.resourceName` (catalog source), `com.infa.odin.models.file.path` (source
   path), `core.sourceModifiedOn` (source-of-record mtime), `core.modifiedOn`
   (last scan, epoch ms), `core.assetLifecycle`, `core.supplement.certified`, and
   `core.aggregatedByObjects` → resolved marketplace **collection** name.
2. **(optional, `includeLineage`)** query lineage RELATIONSHIPs where the asset or
   any of its columns is the source (**downstream**) or target (**upstream**),
   skipping structural edges (a denylist — so it recognises *any* real dataflow
   type without a rebuild), and resolves neighbour names.

It caches the result (lazy refresh, single-flight, `distributed` opt-in) and stamps
the summary **synchronously on the response leg** — so the slow fetch never races
the streamed response-head commit.

Headers stamped (example, `dim_product.csv`):
```
x-dp-provenance-name: dim_product.csv
x-dp-provenance-origin: <catalogSourceId>          (catalog / scan origin id)
x-dp-provenance-external-id: <catalogSourceId>://FileServer/data/csv/dim_product.csv~…FlatFile
x-dp-provenance-source-type: FlatFile
x-dp-provenance-catalog-source: Product Catalog
x-dp-provenance-path: /FileServer/data/csv/dim_product.csv
x-dp-provenance-source-modified: 2026-08-…        (source-of-record mtime)
x-dp-provenance-catalog-modified-ms: 175…         (last CDGC scan, epoch ms)
x-dp-provenance-lifecycle: TECHNICAL
x-dp-provenance-certified: true
x-dp-provenance-collection: <marketplace data collection name>
x-dp-provenance-source: cdgc     x-dp-provenance-status: ok
# with lineage seeded (see seed-lineage/):
x-dp-lineage-downstream: fact_order_line.csv     x-dp-lineage-status: present
```

---

## Configuration reference

| Property | Type | Default | Description |
|---|---|---|---|
| `cdgcLoginUrl` | string (service) | required | IDMC login base URL. |
| `cdgcSearchUrl` | string (service) | required | CDGC search host (`ccgf-searchv2`), e.g. `https://cdgc-api.<pod>.informaticacloud.com`. |
| `cdgcOrgUsername` / `cdgcOrgPassword` | string (sensitive) | required | IDMC read-only service account. |
| `schemaId` | string | required | CDGC asset id of the scanned schema whose provenance is stamped. |
| `schemaIdHeader` | string | `x-dp-schema-id` | Per-request schema-asset id override. |
| `schemaIdClaim` | string | _unset_ | Optional JWT claim to read `schemaId` from; when set + present it wins over the header, binding the caller to a data product. Needs an upstream JWT Validation policy. |
| `includeLineage` | boolean | `true` | Also resolve lineage and stamp `x-dp-lineage-*`. Off = provenance only (one fewer CDGC call). |
| `headerOnMiss` | boolean | `true` | Stamp `x-dp-provenance-status: unavailable` when CDGC can't be resolved. |
| `refreshIntervalSeconds` | integer | `86400` | Provenance cache TTL (min 30). |
| `failOpenOnCdgcError` | boolean | `true` | Serve last-known-good on transient CDGC error. |
| `distributed` | boolean | `false` | Share cache + refresh lock across replicas. |
| `timeout` | integer (ms) | `5000` | Per-CDGC-call timeout (≤ ~15s chained budget). |

---

## Build, test & release

```bash
cd provenance-stamping-definition && make release          # publish definition to Exchange first
cd ../provenance-stamping-flex
make build-asset-files && cargo build --target wasm32-wasip1 --release
make release
```
Requires **PDK 1.10** with `enable_stop_iteration` (request-leg CDGC fetch).
`make build-asset-files` regenerates `src/generated/config.rs` from the published
`gcl.yaml`, so the config struct always matches the schema.

> **Sourcing the schema id from a JWT:** set `schemaIdClaim` to read `schemaId`
> from the caller's Bearer token instead of the `x-dp-schema-id` header — the
> configured claim wins when present. The token is only decoded; a **JWT
> Validation policy must run upstream** to verify it.

---

## Live demo

```bash
cp demo/config.json.example demo/config.json     # fill CDGC creds/urls + schemaId
# provision per demo/PROVISION.md, then:
cp demo/env.local.sh.example demo/env.local.sh    # set PROV_GW_URL
./demo/demo.sh
```
The agent calls the product mock through the gateway and prints the
`x-dp-provenance-*` / `x-dp-lineage-*` headers the gateway derived from CDGC
(`dim_product.csv`).

---

## Seeding lineage (optional)

Out of the box CDGC has provenance for the demo asset but **no lineage edges**. To
light up `x-dp-lineage-downstream`, author real lineage with the one-off seeding
kit in **[`seed-lineage/`](seed-lineage/SEED-LINEAGE.md)** — a `links.csv`
(`dim_product.csv → fact_order_line.csv`, plus sku/price column flows) packaged as
`GenericLinks.zip` and ingested via a CDGC **Custom Lineage** catalog source on a
Secure Agent. The policy detects lineage by a denylist of structural relationship
types, so it stamps the new edges with **no rebuild**.

---

## Skills used

- **PDK** (`omni-gateway-pdk-skills`): `pdk-create-policy`, `pdk-mcp`,
  `pdk-request-headers-bodies`, `pdk-data-storage`, `pdk-distributed-cache-gossip`,
  `pdk-schema-definition`.
- **P4A** (`p4a-skills`): `p4a-build-policy`, `p4a-verify-requirements`,
  `p4a-mcp-usage`, `p4a-test-mcp-policies-with-a2d`.
- **IDMC** (`governed-data-product-skills`, `IDMC - Data Governance Skills`):
  CDGC `ccgf-searchv2` provenance attributes, Custom Lineage ingestion.
