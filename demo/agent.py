#!/usr/bin/env python3
"""
Agent simulation for the catalog-driven Provenance Stamping demo.

The policy is configured with only a CDGC schema-asset id. It derives the data
product's provenance from Informatica CDGC (the scanned dim_product.csv: catalog
origin, source-of-record path, scan freshness, lifecycle, certification, and any
data lineage) and stamps it onto the response as x-dp-provenance-* / x-dp-lineage-*
headers.

Usage:
    PROV_GW_URL="https://<host>/provenance-stamp-demo/mcp" python3 agent.py
"""
import json, os, ssl, sys, urllib.request

GW = (sys.argv[1] if len(sys.argv) > 1 else os.environ.get("PROV_GW_URL", "")).strip()
if not GW:
    sys.exit("Set PROV_GW_URL (governed endpoint). See demo/env.local.sh.example")
_CTX = ssl.create_default_context(); _CTX.check_hostname = False; _CTX.verify_mode = ssl.CERT_NONE


def call():
    body = {"jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": "get_products", "arguments": {"variant": "leak"}}}
    req = urllib.request.Request(GW, data=json.dumps(body).encode(), method="POST", headers={
        "Content-Type": "application/json", "Accept": "application/json, text/event-stream",
        "Accept-Encoding": "identity", "mcp-session-id": "provenance-stamp-demo"})
    try:
        resp = urllib.request.urlopen(req, timeout=25, context=_CTX)
        return resp.status, {k: v for k, v in resp.headers.items() if k.lower().startswith("x-dp-")}
    except urllib.error.HTTPError as e:
        return e.code, {k: v for k, v in e.headers.items() if k.lower().startswith("x-dp-")}


def main():
    print(f"🧬  catalog-driven provenance stamping  →  {GW}\n")
    st, dp = call()
    print(f"  get_products → HTTP {st}\n")
    prov = {k: v for k, v in dp.items() if k.lower().startswith("x-dp-provenance-")}
    lin = {k: v for k, v in dp.items() if k.lower().startswith("x-dp-lineage-")}
    print("  Provenance headers derived live from CDGC (dim_product.csv):")
    for k in sorted(prov):
        print(f"    {k}: {prov[k]}")
    print("\n  Lineage headers:")
    for k in sorted(lin):
        print(f"    {k}: {lin[k]}")
    status = (dp.get("x-dp-provenance-status") or dp.get("X-Dp-Provenance-Status") or "").lower()
    lstatus = (dp.get("x-dp-lineage-status") or dp.get("X-Dp-Lineage-Status") or "").lower()
    print()
    if status == "ok":
        print("✅ The response attests its origin: catalog source, source-of-record path,")
        print("   last-scan freshness, lifecycle and certification — all from Informatica")
        print("   CDGC, from a config of just a schema-asset id.")
        if lstatus == "present":
            print("   Lineage is seeded — the response also names its downstream consumers.")
        else:
            print("   (No lineage edges in CDGC yet — see seed-lineage/ to author them.)")
    else:
        print("⚠️  Provenance unavailable — check CDGC creds/ids/egress (fail-open: data still flowed).")


if __name__ == "__main__":
    main()
