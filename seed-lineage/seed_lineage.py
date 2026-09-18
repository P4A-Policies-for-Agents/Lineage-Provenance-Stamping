"""Seed CDGC custom lineage: dim_product.csv -> fact_order_line.csv.

Two-phase, because CDGC ingests hand-authored lineage through a Custom Metadata
Integration *custom catalog source* fed a GenericLinks.zip (links.csv) — there is
no inline lineage API (re-probed 2026-09-18; see SEED-LINEAGE.md).

  Phase A (one-time, Metadata Command Center UI): create a Custom Catalog Source
          Type, then a catalog source of that type with Metadata Source Type =
          "CSV Files", Source Type = "Upload", and GenericLinks.zip attached under
          File Details. Note the catalog source id from the browser URL.
  Phase B (this script): re-run the scan, then verify the edges landed.

Usage:
  python seed_lineage.py --zip                 # rebuild GenericLinks.zip from links.csv
  python seed_lineage.py --validate            # check every reference id resolves
  CUSTOM_LINEAGE_DS_ID=<id> python seed_lineage.py --sync
  python seed_lineage.py --verify              # did the lineage edges appear?

Note: the zip itself is uploaded through the MCC UI. Re-uploading it there is the
only way to change the links after Phase A — "Provide a Local Path" was tested on
this tenant and the Secure Agent could not see the demo SFTP share, so "Upload"
is the supported route here.

Creds come from the environment (never persisted). Read SEED-LINEAGE.md first.
"""
import argparse
import os
import subprocess
import sys
import zipfile

import requests

# The idmc auth client lives in the sibling training toolkit, not here. Set
# IDMC_TOOLKIT to the dir containing the `idmc/` package so `import idmc.*` works.
_TOOLKIT = os.environ.get("IDMC_TOOLKIT")
if _TOOLKIT and _TOOLKIT not in sys.path:
    sys.path.insert(0, _TOOLKIT)

from idmc.config import load_config
from idmc.client import login

requests.packages.urllib3.disable_warnings()

HERE = os.path.dirname(os.path.abspath(__file__))
ZIP = os.path.join(HERE, "GenericLinks.zip")
LINKS = os.path.join(HERE, "links.csv")

# The scanned schema whose lineage the policy stamps — the same id the policy is
# configured with. Tenant-specific, so it comes from the environment.
SCHEMA_ID = os.environ.get("PROV_SCHEMA_ID", "")
# Structural/governance edges that are NOT lineage — the same denylist the policy uses.
STRUCTURAL = {
    "com.infa.odin.models.file.FileToField",
    "com.infa.odin.models.file.FolderToFile",
    "core.QuasiClassifiedAs",
    "com.infa.ccgf.models.governance.asscIClassDQResult",
    "com.infa.ccgf.models.governance.asscParentDataElementRuleInstance",
    "com.infa.ccgf.models.governance.IClassTechnicalGlossaryBase",
    "com.infa.ccgf.models.governance.asscSecondaryObjectofRuleInstance",
    "com.infa.cdmp.marketplace.asscDataCollection",
}


def _session():
    cfg = load_config()
    sess = login(cfg, dry_run=False)
    cdgc = cfg.cdgc_api_url.replace("idmc-api", "cdgc-api")
    return sess, cdgc


def _mgmt_headers(sess):
    """Catalog-source writes are PEP-gated: they need the JWT *and* the session cookie."""
    return {
        "Authorization": f"Bearer {sess.bearer}",
        "X-INFA-ORG-ID": sess.org_id,
        "IDS-SESSION-ID": sess.session_id,
        "Cookie": f"USER_SESSION={sess.session_id}; IDS_TOKEN={sess.bearer}",
        "Accept": "application/json",
        "Content-Type": "application/json",
    }


def _search_headers(sess):
    return {
        "Authorization": f"Bearer {sess.bearer}",
        "X-INFA-ORG-ID": sess.org_id,
        "X-INFA-SEARCH-LANGUAGE": "elasticsearch",
        "Content-Type": "application/json",
        "Accept": "application/json",
    }


def build_zip():
    """Repackage links.csv as GenericLinks.zip.

    The scanner only accepts these two names — GenericLinks.zip containing
    links.csv (KB 000192802), and the zip name may not contain extra dots.
    """
    with zipfile.ZipFile(ZIP, "w", zipfile.ZIP_DEFLATED) as z:
        z.write(LINKS, "links.csv")
    print(f"built {ZIP} <- {LINKS}")
    print("  re-upload it under File Details on the catalog source in MCC, then --sync")


def validate():
    """Delegate to the read-only pre-flight so there is one implementation."""
    return subprocess.call([sys.executable, os.path.join(HERE, "validate_links.py")])


def sync(ds_id):
    """Run (scan) the custom lineage catalog source, replaying its stored config."""
    sess, cdgc = _session()
    base = f"{cdgc}/ccgf-catalog-source-management/api/v1"
    hdr = _mgmt_headers(sess)

    g = requests.get(f"{base}/datasources/{ds_id}", headers=hdr, verify=False, timeout=120)
    if g.status_code >= 400:
        g = requests.get(f"{base}/datasource/{ds_id}", headers=hdr, verify=False, timeout=120)
    g.raise_for_status()
    ds = g.json()
    print(f"catalog source: {ds.get('name')}  type={ds.get('type')}  custom={ds.get('custom')}")

    body = {k: ds[k] for k in ("capabilities", "typeCapabilities",
                               "globalConfigOptions", "typeOptions") if k in ds}
    body["customAttributes"] = None
    body["overrides"] = []
    body["CMSWorkFlowName"] = "ccgf-catalog-source-sync-workflow"
    sync_hdr = dict(hdr)
    sync_hdr["X-INFA-TRACEEVENT"] = f"{ds_id}.Sync"
    sync_hdr["X-Requested-With"] = "XMLHttpRequest"
    r = requests.post(f"{base}/datasource/{ds_id}/operations/sync", json=body,
                      headers=sync_hdr, verify=False, timeout=120)
    print("POST operations/sync:", r.status_code)
    try:
        j = r.json()
        print("jobId:", j.get("jobId"), "status:", j.get("status"))
    except ValueError:
        print(r.text[:800])
    print("watch it in MCC under Job Monitoring, then run --verify")


def verify():
    """Report any non-structural (i.e. real lineage) edge touching the schema."""
    if not SCHEMA_ID:
        sys.exit("set PROV_SCHEMA_ID to the CDGC schema asset id the policy stamps "
                 "(the same `schemaId` in demo/config.json)")
    sess, cdgc = _session()
    url = f"{cdgc}/ccgf-searchv2/api/v1/search"
    hdr = _search_headers(sess)

    def search(body):
        r = requests.post(url, json=body, headers=hdr, verify=False, timeout=120)
        if r.status_code >= 300:
            print("  search error", r.status_code, r.text[:200])
            return []
        return [h.get("sourceAsMap", {}) for h in r.json().get("hits", {}).get("hits", [])]

    asset = search({"from": 0, "size": 1, "query": {"bool": {"must": [
        {"terms": {"elementType": ["OBJECT"]}},
        {"terms": {"core.identity": [SCHEMA_ID]}}]}}})
    if not asset:
        sys.exit(f"schema asset {SCHEMA_ID} not found")
    loc = asset[0].get("core.location")
    print(f"schema: {asset[0].get('core.name')}  ({SCHEMA_ID})")

    cols = search({"from": 0, "size": 1000, "query": {"bool": {
        "must": [{"terms": {"core.classType": ["com.infa.odin.models.file.flat.FlatField"]}}],
        "filter": [{"terms": {"core.location::path_hierarchy.parent": [loc]}}]}}})
    ids = [SCHEMA_ID] + [c["core.identity"] for c in cols if c.get("core.identity")]

    lineage = {}
    for field in ("core.sourceIdentity", "core.targetIdentity"):
        for rel in search({"from": 0, "size": 5000, "query": {"bool": {"must": [
                {"terms": {"elementType": ["RELATIONSHIP"]}},
                {"terms": {field: ids}}]}}}):
            t = rel.get("type") or rel.get("core.classType")
            for tt in (t if isinstance(t, list) else [t]):
                if tt and tt not in STRUCTURAL:
                    lineage[tt] = lineage.get(tt, 0) + 1

    if lineage:
        print("\nLINEAGE PRESENT — non-structural edges:")
        for t, n in sorted(lineage.items(), key=lambda x: -x[1]):
            print(f"  {n:5d}  {t}")
        print("\nthe deployed policy will now stamp x-dp-lineage-status: present")
        return 0
    print("\nno lineage edges yet — x-dp-lineage-status stays 'none'")
    return 1


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--zip", action="store_true", help="rebuild GenericLinks.zip from links.csv")
    ap.add_argument("--validate", action="store_true", help="check every reference id resolves in CDGC")
    ap.add_argument("--sync", action="store_true", help="run the custom lineage catalog source scan")
    ap.add_argument("--verify", action="store_true", help="check whether lineage edges now exist")
    a = ap.parse_args()
    if not (a.zip or a.validate or a.sync or a.verify):
        ap.error("nothing to do: pass --zip, --validate, --sync and/or --verify")

    if a.zip:
        build_zip()
    if a.validate and validate() != 0:
        sys.exit("links.csv has unresolvable reference ids — fix them before scanning")
    if a.sync:
        ds = os.environ.get("CUSTOM_LINEAGE_DS_ID")
        if not ds:
            sys.exit("set CUSTOM_LINEAGE_DS_ID to the custom lineage catalog source id (Phase A)")
        sync(ds)
    if a.verify:
        sys.exit(verify())
