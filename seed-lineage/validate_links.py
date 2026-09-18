"""READ-ONLY pre-flight: check every reference ID in links.csv exists in CDGC.

A Custom Lineage scan does not fail on an unknown reference ID — it just writes
no edge for that row, so a typo shows up as a green job and no lineage. Run this
before staging the zip.

Usage:
  export IDMC_TOOLKIT=<dir containing the idmc/ package>
  source ../demo/env.local.sh
  python validate_links.py [links.csv]

Read-only; creds from the environment; nothing is persisted.
"""
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
LINKS = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "links.csv")

cfg = load_config()
sess = login(cfg, dry_run=False)
url = f"{cfg.cdgc_api_url.replace('idmc-api', 'cdgc-api')}/ccgf-searchv2/api/v1/search"
hdr = {
    "Authorization": f"Bearer {sess.bearer}",
    "X-INFA-ORG-ID": sess.org_id,
    "X-INFA-SEARCH-LANGUAGE": "elasticsearch",
    "Content-Type": "application/json",
    "Accept": "application/json",
}


def by_external_id(ext):
    body = {"from": 0, "size": 2, "query": {"bool": {"must": [
        {"terms": {"elementType": ["OBJECT"]}},
        {"terms": {"core.externalId": [ext]}}]}}}
    r = requests.post(url, json=body, headers=hdr, verify=False, timeout=120)
    if r.status_code >= 300:
        print("  ! search error", r.status_code, r.text[:200])
        return None
    hits = r.json().get("hits", {}).get("hits", [])
    return hits[0].get("sourceAsMap", {}) if hits else None


rows = list(csv.DictReader(open(LINKS)))
print(f"# {len(rows)} link rows in {LINKS}\n")

resolved = {}
missing = 0
for n, row in enumerate(rows, 1):
    for side in ("Source", "Target"):
        ext = row[side].strip()
        if ext not in resolved:
            resolved[ext] = by_external_id(ext)
        hit = resolved[ext]
        if hit:
            print(f"  row{n} {side:<6} ok       {hit.get('core.name')} "
                  f"[{str(hit.get('core.classType')).rsplit('.', 1)[-1]}]")
        else:
            missing += 1
            print(f"  row{n} {side:<6} MISSING  {ext}")
    assoc = row["Association"].strip()
    known = {"core.ResourceParentChild", "core.DataSourceParentChild",
             "core.DataSetToDataElementParentship", "core.DataSetDataFlow",
             "core.DirectionalDataFlow"}
    if assoc not in known:
        missing += 1
        print(f"  row{n} Assoc  UNKNOWN  {assoc!r} (case-sensitive; expected one of {sorted(known)})")

print(f"\n# {len(resolved) - sum(1 for v in resolved.values() if v is None)}"
      f"/{len(resolved)} reference ids resolved")
sys.exit(1 if missing else 0)
