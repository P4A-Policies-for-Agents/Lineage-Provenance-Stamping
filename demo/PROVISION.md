# Demo provisioning runbook

Stands up the live Data Product Provenance Stamping demo with `anypoint-cli-v4`
(already authenticated) + the A2D MCP tools — **no bearer token needed** for the
gateway side. The **CDGC side needs a real IDMC tenant** (a read-only service
account + a governed asset id) supplied in `config.json`. Replace `<...>`
placeholders (identifiers, not secrets).

| Placeholder | What it is |
|---|---|
| `<orgId>` | Business-group / org id |
| `<mockServerId>` | A2D data-product mock MCP server id |
| `<gatewayId>` | Managed Flex Gateway resource id **with a public ingress** |
| `<gatewayPublicHost>` | Gateway public ingress URL |
| `<apiInstanceId>` | API Manager instance id |

Governed endpoint: `https://<gatewayPublicHost>/provenance-stamp-demo/mcp`

## 1. Publish the data-product mock to Exchange (type=mcp)

```bash
anypoint-cli-v4 exchange:asset:upload --name "Product Catalog Data Product" \
  --type mcp --status published --properties='{"platform":"a2d"}' \
  --files='{"mcp-metadata.json":"./mcp-metadata.json"}' product-catalog-provenance/1.0.0
```
(You can reuse the existing Product Catalog mock from the Metadata Injection /
Entitlement demos — same `dim_product.csv`-shaped `get_products` tool.)

## 2. Create + deploy the MCP Flex instance

```bash
anypoint-cli-v4 api-mgr:api:manage product-catalog-provenance 1.0.0 <orgId> \
  --environment Sandbox --isFlex --type mcp \
  --uri "https://www.a2d-ai.com/api/platform/<mockServerId>/" --apiInstanceLabel provenance-stamp-demo
anypoint-cli-v4 api-mgr:api:edit <apiInstanceId> --environment Sandbox --isFlex --type mcp \
  --withProxy --scheme http --port 8081 --path "/provenance-stamp-demo/" \
  --uri "https://www.a2d-ai.com/api/platform/<mockServerId>/"
anypoint-cli-v4 api-mgr:api:deploy <apiInstanceId> --environment Sandbox \
  --target <gatewayId> --gatewayVersion 1.0.0 --overwrite
```

## 3. Wire IDMC egress + apply the policy

The policy's `cdgcLoginUrl` / `cdgcSearchUrl` are `format: service` — the gateway
must be able to **reach those IDMC hosts as an outbound service**. On a managed
gateway confirm egress to `*.informaticacloud.com` is permitted; the policy's
`service_create` registers the cluster from the config URL.

```bash
cp config.json.example config.json    # fill cdgcLoginUrl/SearchUrl/Username/Password/schemaId
anypoint-cli-v4 api-mgr:policy:apply <apiInstanceId> provenance-stamping \
  --environment Sandbox --groupId <orgId> --policyVersion 1.0.0 --configFile ./config.json
anypoint-cli-v4 api-mgr:api:redeploy <apiInstanceId> --environment Sandbox
```

## 4. Run the agent

```bash
cp env.local.sh.example env.local.sh   # set PROV_GW_URL to the governed endpoint
./demo.sh
```

Expected: `get_products` returns 200; the response carries
`x-dp-provenance-status: ok` plus the derived `x-dp-provenance-*` headers (name,
origin, catalog source, source path, scan freshness, lifecycle, certification,
collection). If lineage has been seeded (see `../seed-lineage/`), it also carries
`x-dp-lineage-downstream: fact_order_line.csv` and `x-dp-lineage-status: present`;
otherwise `x-dp-lineage-status: none`. If CDGC isn't reachable, the policy fails
open and stamps `x-dp-provenance-status: unavailable` (the data still flows).

## Notes

- **Two distinct upstreams:** the *route* target is the data-product mock; the
  *policy's* CDGC calls go to the IDMC hosts via the `format: service` egress —
  they are separate.
- **Provenance is derived, not configured.** The only tenant-specific input is
  `schemaId`; every provenance attribute is read live from the CDGC asset.
- `api:manage` alone leaves `deployment: null`; you need `api:edit --withProxy`
  then `api:deploy`. Use a gateway whose `configuration.ingress.publicUrl` is
  non-empty (else 404).
- **Refreshing the cache:** the derived provenance is cached for
  `refreshIntervalSeconds` (default 24h) and `api:redeploy` does NOT clear it. To
  force a re-derive after a CDGC change, temporarily lower `refreshIntervalSeconds`
  via `api-mgr:policy:edit`, redeploy, make one warm-up call, then restore.
