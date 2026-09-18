"""Author CDGC lineage edges directly, straight from links.csv. No UI, no scan.

Uses the content service's `relationship` segment:

    PATCH {cdgc}/data360/content/v1/assets/{assetId}?scheme=internal
    [ {"operation":"add","segment":"relationship",
       "items":[{"fromIdentity":…,"toIdentity":…,"association":…}]} ]

Two things the docs get wrong / leave out, both found the hard way:
  * the body is a bare JSON **list** of update requests. Sending a single object
    fails with "Cannot deserialize value of type ArrayList<UpdateAssetRequest>".
  * fromIdentity/toIdentity are the asset's internal `core.identity` UUID, not the
    `core.externalId` reference id that links.csv carries — so each row is resolved
    through ccgf-searchv2 first.

links.csv stays the source of truth, so the same file drives either this or the
Custom Lineage catalog source fallback (see SEED-LINEAGE.md).

Usage:
  export IDMC_TOOLKIT=<dir containing the idmc/ package>
  source ../demo/env.local.sh
  python apply_lineage.py --dry-run     # resolve + print, change nothing
  python apply_lineage.py               # apply every row
"""
import argparse
import csv
import os
import sys

import requests

_TOOLKIT = os.environ.get("IDMC_TOOLKIT")
if _TOOLKIT and _TOOLKIT not in sys.path:
    sys.path.insert(0, _TOOLKIT)

from idmc.config import load_config
from idmc.client import login

requests.packages.urllib3.disable_warnings()

HERE = os.path.dirname(os.path.abspath(__file__))
LINKS = os.path.join(HERE, "links.csv")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--links", default=LINKS)
    ap.add_argument("--dry-run", action="store_true",
                    help="resolve reference ids and print the plan; write nothing")
    ap.add_argument("--remove", action="store_true",
                    help="remove the edges instead of adding them")
    args = ap.parse_args()

    cfg = load_config()
    sess = login(cfg, dry_run=False)
    cdgc = cfg.cdgc_api_url.replace("idmc-api", "cdgc-api")

    write_hdr = {
        "Authorization": f"Bearer {sess.bearer}",
        "X-INFA-ORG-ID": sess.org_id,
        "X-INFA-PRODUCT-ID": "CDGC",
        "IDS-SESSION-ID": sess.session_id,
        "Cookie": f"USER_SESSION={sess.session_id}; IDS_TOKEN={sess.bearer}",
        "Accept": "application/json",
        "Content-Type": "application/json",
    }
    search_hdr = {
        "Authorization": f"Bearer {sess.bearer}",
        "X-INFA-ORG-ID": sess.org_id,
        "X-INFA-SEARCH-LANGUAGE": "elasticsearch",
        "Content-Type": "application/json",
        "Accept": "application/json",
    }
    search_url = f"{cdgc}/ccgf-searchv2/api/v1/search"

    cache = {}

    def identity(external_id):
        """core.externalId (what links.csv holds) -> core.identity (what the API wants)."""
        if external_id in cache:
            return cache[external_id]
        body = {"from": 0, "size": 2, "query": {"bool": {"must": [
            {"terms": {"elementType": ["OBJECT"]}},
            {"terms": {"core.externalId": [external_id]}}]}}}
        r = requests.post(search_url, json=body, headers=search_hdr, verify=False, timeout=120)
        r.raise_for_status()
        hits = r.json().get("hits", {}).get("hits", [])
        hit = hits[0].get("sourceAsMap", {}) if hits else None
        cache[external_id] = hit
        return hit

    rows = list(csv.DictReader(open(args.links)))
    op = "remove" if args.remove else "add"
    print(f"# {len(rows)} rows from {args.links}  (operation={op})\n")

    plan, missing = [], 0
    for n, row in enumerate(rows, 1):
        src, tgt = identity(row["Source"].strip()), identity(row["Target"].strip())
        assoc = row["Association"].strip()
        if not src or not tgt:
            missing += 1
            print(f"  row{n}  UNRESOLVED  {'source' if not src else 'target'}")
            continue
        print(f"  row{n}  {src['core.name']} -> {tgt['core.name']}  ({assoc})")
        plan.append((src["core.identity"], tgt["core.identity"], assoc))

    if missing:
        sys.exit(f"\n{missing} row(s) did not resolve — fix links.csv before applying")
    if args.dry_run:
        print("\n--dry-run: nothing written")
        return 0

    print()
    failed = 0
    for src_id, tgt_id, assoc in plan:
        # Body MUST be a list; the single-object form is rejected by the service.
        payload = [{
            "operation": op,
            "segment": "relationship",
            "items": [{"fromIdentity": src_id, "toIdentity": tgt_id, "association": assoc}],
        }]
        r = requests.patch(f"{cdgc}/data360/content/v1/assets/{src_id}",
                           params={"scheme": "internal"}, json=payload,
                           headers=write_hdr, verify=False, timeout=120)
        # 409 "Relationship already exists" makes re-runs idempotent, not failed.
        exists = r.status_code == 409 and "already exists" in r.text
        ok = r.status_code < 300 or exists
        failed += 0 if ok else 1
        label = "OK  " if r.status_code < 300 else ("SAME" if exists else "FAIL")
        print(f"  {label} {assoc:28} {src_id} -> {tgt_id}  [{r.status_code}]")
        if not ok:
            print("       ", r.text[:300].replace("\n", " "))

    print(f"\n{len(plan) - failed}/{len(plan)} edges {op}ed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
