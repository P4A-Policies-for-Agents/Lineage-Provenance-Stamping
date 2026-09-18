// Copyright 2026 Salesforce, Inc. All rights reserved.
//! Data Product Provenance Stamping — inbound, headers-only, fail-open Omni/Flex
//! Gateway policy. Works on MCP and REST/HTTP.
//!
//! Given only a CDGC schema-asset id (a scanned flat file, table, etc.), it derives
//! the data product's *provenance* from Informatica CDGC (via the ccgf-searchv2
//! search API): where the data came from (catalog origin + source-of-record path),
//! how fresh it is (source-modified + last-scanned), its governance state
//! (lifecycle, certification) and the marketplace collection it belongs to — plus,
//! when present, its data *lineage* (upstream/downstream neighbours). It stamps that
//! onto the response as x-dp-provenance-* / x-dp-lineage-* headers so the output
//! port is self-describing about its origin and trust.
//!
//! The CDGC fetch runs on the REQUEST leg (await-safe under enable_stop_iteration)
//! and is stamped synchronously on the response leg — avoiding the response-leg
//! fetch racing the streamed response-head commit. Cached (lazy refresh,
//! single-flight, distributed opt-in). Enrichment, fail-open: never blocks.
//!
//! Lineage is resolved type-agnostically: any relationship touching the asset or its
//! columns whose class type is NOT one of the known structural/governance edges
//! (STRUCTURAL_REL_TYPES) is treated as lineage. CDGC custom lineage writes
//! core.DataSetDataFlow (object-level) and core.DirectionalDataFlow (column-level)
//! edges — neither is structural — so seeded lineage surfaces here with no rebuild.

mod cdgc;
mod claims;
mod derive;
mod generated;

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Result};
use pdk::data_storage::{DataStorage, DataStorageBuilder, DataStorageError, StoreMode};
use pdk::hl::timer::Clock;
use pdk::hl::*;
use pdk::logger;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cdgc::{nonce_from_time, CachedMeta, RefreshLock};
use crate::generated::config::Config;
use crate::derive::{dedupe_doubled, is_content_method, neighbor_file, num_or_str, rel_is_structural, s};

const META_CACHE_NAMESPACE: &str = "prov-metadata";
const REFRESH_LOCK_NAMESPACE: &str = "prov-refresh-lock";
const META_CACHE_KEY_PREFIX: &str = "prov-meta-";
const REFRESH_LOCK_KEY_PREFIX: &str = "prov-lock-";
const REFRESH_LOCK_TTL_SECONDS: i64 = 30;
const REFRESH_LOCK_TTL_MS: u32 = (REFRESH_LOCK_TTL_SECONDS as u32) * 1000;
const META_STORE_MIN_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;
const CAS_MAX_RETRIES: u32 = 3;
const DEFAULT_TIMEOUT_MS: i64 = 5_000;
const CDGC_REFRESH_BUDGET_MS: i64 = 15_000;
const DEFAULT_REFRESH_INTERVAL_SECONDS: i64 = 86_400;
const SEARCH_PATH: &str = "/ccgf-searchv2/api/v1/search";
const CT_FLATFIELD: &str = "com.infa.odin.models.file.flat.FlatField";

#[derive(Deserialize)]
struct CdgcLoginResponse {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "orgId")]
    org_id: String,
}
#[derive(Deserialize)]
struct CdgcJwtResponse {
    jwt_token: String,
}

#[derive(Clone)]
struct Ctx {
    fields: BTreeMap<String, String>,
}


fn now_secs(clock: &Clock) -> i64 {
    clock.now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}
fn elapsed_ms(start: SystemTime, now: SystemTime) -> i64 {
    now.duration_since(start).map(|d| d.as_millis() as i64).unwrap_or(0)
}
fn next_call_timeout(per_call_ms: i64, elapsed: i64) -> Option<Duration> {
    let remaining = CDGC_REFRESH_BUDGET_MS - elapsed;
    if remaining <= 0 {
        return None;
    }
    Some(Duration::from_millis(per_call_ms.min(remaining).max(1) as u64))
}
fn meta_store_ttl_ms(config: &Config) -> u32 {
    let refresh = config.refresh_interval_seconds.unwrap_or(DEFAULT_REFRESH_INTERVAL_SECONDS).max(0) as u64;
    refresh.saturating_mul(2).saturating_mul(1000).max(META_STORE_MIN_TTL_MS).min(u32::MAX as u64) as u32
}


async fn cdgc_auth(client: &HttpClient, config: &Config, clock: &Clock, start: SystemTime) -> Result<(String, String)> {
    let per_call = config.timeout.unwrap_or(DEFAULT_TIMEOUT_MS);
    let login_body = serde_json::to_vec(&json!({
        "username": config.cdgc_org_username, "password": config.cdgc_org_password,
    }))?;
    let t = next_call_timeout(per_call, elapsed_ms(start, clock.now())).ok_or_else(|| anyhow!("budget before Login"))?;
    let login_resp = client.request(&config.cdgc_login_url).path("/identity-service/api/v1/Login")
        .headers(vec![("Content-Type", "application/json")]).body(&login_body).timeout(t).post().await
        .map_err(|e| anyhow!("CDGC login failed: {e}"))?;
    if login_resp.status_code() >= 300 {
        return Err(anyhow!("CDGC login status {}", login_resp.status_code()));
    }
    let login: CdgcLoginResponse = serde_json::from_slice(login_resp.body()).map_err(|e| anyhow!("parse login: {e}"))?;
    let nonce = nonce_from_time(clock.now());
    let cookie = format!("USER_SESSION={}", login.session_id);
    let t = next_call_timeout(per_call, elapsed_ms(start, clock.now())).ok_or_else(|| anyhow!("budget before JWT"))?;
    let jwt_resp = client.request(&config.cdgc_login_url)
        .path(&format!("/identity-service/api/v1/jwt/Token?client_id=idmc_api&nonce={nonce}"))
        .headers(vec![("cookie", cookie.as_str()), ("IDS-SESSION-ID", login.session_id.as_str())])
        .timeout(t).get().await.map_err(|e| anyhow!("CDGC JWT failed: {e}"))?;
    if jwt_resp.status_code() >= 300 {
        return Err(anyhow!("CDGC JWT status {}", jwt_resp.status_code()));
    }
    let jwt: CdgcJwtResponse = serde_json::from_slice(jwt_resp.body()).map_err(|e| anyhow!("parse jwt: {e}"))?;
    Ok((jwt.jwt_token, login.org_id))
}

async fn cdgc_search(
    client: &HttpClient, config: &Config, clock: &Clock, start: SystemTime,
    jwt: &str, org: &str, body: &Value,
) -> Result<Vec<Value>> {
    let per_call = config.timeout.unwrap_or(DEFAULT_TIMEOUT_MS);
    let authz = format!("Bearer {jwt}");
    let payload = serde_json::to_vec(body)?;
    let t = next_call_timeout(per_call, elapsed_ms(start, clock.now())).ok_or_else(|| anyhow!("budget before search"))?;
    let resp = client.request(&config.cdgc_search_url).path(SEARCH_PATH)
        .headers(vec![
            ("Authorization", authz.as_str()),
            ("X-INFA-ORG-ID", org),
            ("X-INFA-SEARCH-LANGUAGE", "elasticsearch"),
            ("Content-Type", "application/json"),
        ])
        .body(&payload).timeout(t).post().await
        .map_err(|e| anyhow!("CDGC search failed: {e}"))?;
    if resp.status_code() >= 300 {
        return Err(anyhow!("CDGC search status {}", resp.status_code()));
    }
    let v: Value = serde_json::from_slice(resp.body()).map_err(|e| anyhow!("parse search: {e}"))?;
    Ok(v.get("hits").and_then(|h| h.get("hits")).and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|h| h.get("sourceAsMap").cloned()).collect())
        .unwrap_or_default())
}

/// Resolve upstream/downstream lineage neighbour names for the asset + its columns.
/// Type-agnostic: any relationship whose class type is not structural is lineage.
#[allow(clippy::too_many_arguments)]
async fn fetch_lineage(
    client: &HttpClient, config: &Config, clock: &Clock, start: SystemTime,
    jwt: &str, org: &str, our_ids: &[String],
) -> Result<(Vec<String>, Vec<String>)> {
    let ours: BTreeSet<&String> = our_ids.iter().collect();
    // Edges leaving us (we are the source) -> the neighbour is downstream.
    let out_rels = cdgc_search(client, config, clock, start, jwt, org, &json!({
        "from":0,"size":5000,"query":{"bool":{"must":[
            {"terms":{"elementType":["RELATIONSHIP"]}},
            {"terms":{"core.sourceIdentity":our_ids}}]}}
    })).await?;
    // Edges arriving at us (we are the target) -> the neighbour is upstream.
    let in_rels = cdgc_search(client, config, clock, start, jwt, org, &json!({
        "from":0,"size":5000,"query":{"bool":{"must":[
            {"terms":{"elementType":["RELATIONSHIP"]}},
            {"terms":{"core.targetIdentity":our_ids}}]}}
    })).await?;

    let mut downstream_ids: BTreeSet<String> = BTreeSet::new();
    let mut upstream_ids: BTreeSet<String> = BTreeSet::new();
    for r in &out_rels {
        if rel_is_structural(r) { continue; }
        if let Some(n) = s(r, "core.targetIdentity") {
            if !ours.contains(&n) { downstream_ids.insert(n); }
        }
    }
    for r in &in_rels {
        if rel_is_structural(r) { continue; }
        if let Some(n) = s(r, "core.sourceIdentity") {
            if !ours.contains(&n) { upstream_ids.insert(n); }
        }
    }
    let all: Vec<String> = downstream_ids.iter().chain(upstream_ids.iter()).cloned().collect();
    if all.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    // Resolve neighbour names. A column neighbour resolves to its own name (e.g.
    // "sku"); a file neighbour to the file name (e.g. "fact_order_line.csv").
    let objs = cdgc_search(client, config, clock, start, jwt, org, &json!({
        "from":0,"size":5000,"query":{"bool":{"must":[
            {"terms":{"elementType":["OBJECT"]}},{"terms":{"core.identity":all}}]}}
    })).await?;
    let mut id_to_name: BTreeMap<String, String> = BTreeMap::new();
    for o in &objs {
        if let (Some(id), Some(name)) = (s(o, "core.identity"), neighbor_file(o)) {
            id_to_name.insert(id, name);
        }
    }
    let resolve = |ids: &BTreeSet<String>| -> Vec<String> {
        let mut names: Vec<String> = ids.iter()
            .filter_map(|i| id_to_name.get(i).cloned())
            .collect();
        names.sort();
        names.dedup();
        names
    };
    Ok((resolve(&upstream_ids), resolve(&downstream_ids)))
}

/// Derive the provenance summary (header name -> value) for a schema asset.
async fn fetch_provenance(client: &HttpClient, config: &Config, clock: &Clock, schema_id: &str) -> Result<BTreeMap<String, String>> {
    let start = clock.now();
    let (jwt, org) = cdgc_auth(client, config, clock, start).await?;

    let files = cdgc_search(client, config, clock, start, &jwt, &org, &json!({
        "from":0,"size":1,"query":{"bool":{"must":[
            {"terms":{"elementType":["OBJECT"]}},{"terms":{"core.identity":[schema_id]}}]}}
    })).await?;
    let file = files.into_iter().next().ok_or_else(|| anyhow!("schema asset '{schema_id}' not found"))?;

    let mut fields = BTreeMap::new();
    if let Some(v) = s(&file, "core.name") { fields.insert("x-dp-provenance-name".to_string(), v); }
    if let Some(v) = s(&file, "core.origin") { fields.insert("x-dp-provenance-origin".to_string(), v); }
    if let Some(v) = s(&file, "core.externalId") { fields.insert("x-dp-provenance-external-id".to_string(), v); }
    if let Some(v) = s(&file, "core.resourceType") { fields.insert("x-dp-provenance-source-type".to_string(), v); }
    if let Some(v) = s(&file, "core.resourceName") { fields.insert("x-dp-provenance-catalog-source".to_string(), dedupe_doubled(&v)); }
    if let Some(v) = s(&file, "com.infa.odin.models.file.path") { fields.insert("x-dp-provenance-path".to_string(), v); }
    if let Some(v) = s(&file, "core.sourceModifiedOn") { fields.insert("x-dp-provenance-source-modified".to_string(), v); }
    if let Some(v) = num_or_str(&file, "core.modifiedOn") { fields.insert("x-dp-provenance-catalog-modified-ms".to_string(), v); }
    if let Some(v) = s(&file, "core.assetLifecycle") { fields.insert("x-dp-provenance-lifecycle".to_string(), v); }
    if let Some(c) = file.get("core.supplement.certified").and_then(Value::as_array)
        .and_then(|a| a.first()).and_then(Value::as_bool) {
        fields.insert("x-dp-provenance-certified".to_string(), c.to_string());
    }

    // Marketplace collection (if any): find the aggregating DataCollection and resolve its name.
    let coll_id = file.get("core.aggregatedByObjects").and_then(Value::as_array).and_then(|a| {
        a.iter().find_map(|o| {
            let ct = o.get("core.objectClassType").and_then(Value::as_str)?;
            if ct.contains("DataCollection") {
                o.get("core.objectIdentity").and_then(Value::as_str).map(str::to_string)
            } else { None }
        })
    });
    if let Some(cid) = coll_id {
        let colls = cdgc_search(client, config, clock, start, &jwt, &org, &json!({
            "from":0,"size":1,"query":{"bool":{"must":[
                {"terms":{"elementType":["OBJECT"]}},{"terms":{"core.identity":[cid]}}]}}
        })).await?;
        if let Some(name) = colls.first().and_then(|c| s(c, "core.name")) {
            fields.insert("x-dp-provenance-collection".to_string(), name);
        }
    }

    // Lineage (opt-out): resolve upstream/downstream neighbours for the asset + columns.
    if config.include_lineage.unwrap_or(true) {
        let mut ids = vec![schema_id.to_string()];
        if let Some(location) = s(&file, "core.location") {
            let cols = cdgc_search(client, config, clock, start, &jwt, &org, &json!({
                "from":0,"size":1000,"query":{"bool":{
                    "must":[{"terms":{"core.classType":[CT_FLATFIELD]}}],
                    "filter":[{"terms":{"core.location::path_hierarchy.parent":[location]}}]}}
            })).await?;
            for c in &cols {
                if let Some(id) = s(c, "core.identity") { ids.push(id); }
            }
        }
        let (upstream, downstream) = fetch_lineage(client, config, clock, start, &jwt, &org, &ids).await?;
        if !upstream.is_empty() { fields.insert("x-dp-lineage-upstream".to_string(), upstream.join(",")); }
        if !downstream.is_empty() { fields.insert("x-dp-lineage-downstream".to_string(), downstream.join(",")); }
        let status = if upstream.is_empty() && downstream.is_empty() { "none" } else { "present" };
        fields.insert("x-dp-lineage-status".to_string(), status.to_string());
    }

    Ok(fields)
}

async fn read_cached<S: DataStorage>(store: &S, key: &str) -> Option<CachedMeta> {
    match store.get::<CachedMeta>(key).await {
        Ok(Some((c, _))) => Some(c),
        Ok(None) => None,
        Err(e) => {
            logger::warn!("prov: cache read failed: {e}");
            None
        }
    }
}
async fn write_cached<S: DataStorage>(store: &S, key: &str, entry: &CachedMeta) {
    for _ in 0..CAS_MAX_RETRIES {
        match store.get::<CachedMeta>(key).await {
            Ok(Some((_, v))) => match store.store(key, &StoreMode::Cas(v), entry).await {
                Ok(()) => return,
                Err(DataStorageError::CasMismatch) => continue,
                Err(e) => { logger::warn!("prov: persist failed: {e}"); return; }
            },
            Ok(None) => match store.store(key, &StoreMode::Absent, entry).await {
                Ok(()) => return,
                Err(DataStorageError::CasMismatch) => continue,
                Err(e) => { logger::warn!("prov: persist failed: {e}"); return; }
            },
            Err(e) => { logger::warn!("prov: read-before-persist failed: {e}"); return; }
        }
    }
}
async fn try_acquire_refresh_lock<S: DataStorage>(store: &S, key: &str, now: i64) -> Result<bool, DataStorageError> {
    let entry = RefreshLock { acquired_at: now };
    match store.store(key, &StoreMode::Absent, &entry).await {
        Ok(()) => Ok(true),
        Err(DataStorageError::CasMismatch) => match store.get::<RefreshLock>(key).await? {
            Some((existing, v)) => {
                if now - existing.acquired_at < REFRESH_LOCK_TTL_SECONDS { Ok(false) }
                else {
                    match store.store(key, &StoreMode::Cas(v), &entry).await {
                        Ok(()) => Ok(true),
                        Err(DataStorageError::CasMismatch) => Ok(false),
                        Err(e) => Err(e),
                    }
                }
            }
            None => match store.store(key, &StoreMode::Absent, &entry).await {
                Ok(()) => Ok(true),
                Err(DataStorageError::CasMismatch) => Ok(false),
                Err(e) => Err(e),
            },
        },
        Err(e) => Err(e),
    }
}

async fn get_provenance<S: DataStorage>(
    client: &HttpClient, config: &Config, clock: &Clock, meta_store: &S, lock_store: &S, schema_id: &str,
) -> Option<BTreeMap<String, String>> {
    let key = format!("{META_CACHE_KEY_PREFIX}{schema_id}");
    let ttl = config.refresh_interval_seconds.unwrap_or(DEFAULT_REFRESH_INTERVAL_SECONDS).max(0);
    let now = now_secs(clock);
    let cached = read_cached(meta_store, &key).await;
    if let Some(c) = &cached {
        if now - c.timestamp < ttl {
            return Some(c.fields.clone());
        }
    }
    let lock_key = format!("{REFRESH_LOCK_KEY_PREFIX}{schema_id}");
    if !try_acquire_refresh_lock(lock_store, &lock_key, now).await.unwrap_or(true) {
        return cached.map(|c| c.fields);
    }
    match fetch_provenance(client, config, clock, schema_id).await {
        Ok(fields) => {
            write_cached(meta_store, &key, &CachedMeta { fields: fields.clone(), timestamp: now }).await;
            Some(fields)
        }
        Err(e) => {
            logger::warn!("prov: provenance refresh failed for '{schema_id}': {e}");
            if config.fail_open_on_cdgc_error.unwrap_or(true) { cached.map(|c| c.fields) } else { None }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn request_filter<S: DataStorage>(
    request_state: RequestState,
    config: Rc<Config>,
    client: Rc<HttpClient>,
    clock: Rc<Clock>,
    meta_store: Rc<S>,
    lock_store: Rc<S>,
) -> Flow<Option<Ctx>> {
    let hs = request_state.into_headers_state().await;
    // Optionally bind the schema id to a validated JWT claim (decoded here, verified
    // by an upstream JWT Validation policy). A configured claim wins over the header;
    // absent config or absent claim falls back to the header (backward compatible).
    let claim_schema = config.schema_id_claim.as_deref().and_then(|name| {
        let auth = hs.handler().header("authorization");
        claims::decode_bearer_claims(auth.as_deref())
            .and_then(|c| claims::claim_str(&c, name))
    });
    let header_name = config.schema_id_header.as_deref().unwrap_or("x-dp-schema-id");
    let schema_id = claim_schema
        .or_else(|| hs.handler().header(header_name).filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| config.schema_id.clone());

    let ct = hs.handler().header("content-type").unwrap_or_default();
    if ct.starts_with("application/json") && hs.method().as_str() == "POST" {
        let bs = hs.into_body_state().await;
        if let Ok(v) = serde_json::from_slice::<Value>(&bs.handler().body()) {
            if let Some(method) = v.get("method").and_then(Value::as_str) {
                if !is_content_method(method) {
                    return Flow::Continue(None);
                }
            }
        }
    }
    let fields = get_provenance(&client, &config, &clock, &*meta_store, &*lock_store, &schema_id)
        .await
        .unwrap_or_default();
    Flow::Continue(Some(Ctx { fields }))
}

async fn response_filter(response_state: ResponseState, request_data: RequestData<Option<Ctx>>, config: Rc<Config>) {
    let ctx = match request_data {
        RequestData::Continue(Some(c)) => c,
        _ => return,
    };
    let hs = response_state.into_headers_state().await;
    let h = hs.handler();
    if !ctx.fields.is_empty() {
        for (name, value) in &ctx.fields {
            h.set_header(name, value);
        }
        h.set_header("x-dp-provenance-source", "cdgc");
        h.set_header("x-dp-provenance-status", "ok");
    } else if config.header_on_miss.unwrap_or(true) {
        h.set_header("x-dp-provenance-status", "unavailable");
    }
}

fn launch_policy<S: DataStorage + 'static>(
    launcher: Launcher, config: Rc<Config>, client: Rc<HttpClient>, clock: Rc<Clock>,
    meta_store: Rc<S>, lock_store: Rc<S>,
) -> impl std::future::Future<Output = Result<()>> {
    let cfg_req = config.clone();
    let filter = on_request(move |rs| {
        let c = cfg_req.clone();
        let cl = client.clone();
        let ck = clock.clone();
        let ms = meta_store.clone();
        let ls = lock_store.clone();
        async move { request_filter(rs, c, cl, ck, ms, ls).await }
    })
    .on_response(move |rs, rd| {
        let c = config.clone();
        async move { response_filter(rs, rd, c).await }
    });
    async move { launcher.launch(filter).await.map_err(Into::into) }
}

#[entrypoint]
async fn configure(
    launcher: Launcher,
    Configuration(bytes): Configuration,
    client: HttpClient,
    storage_builder: DataStorageBuilder,
    clock: Clock,
) -> Result<()> {
    let config: Config = serde_json::from_slice(&bytes)
        .map_err(|err| anyhow!("Failed to parse configuration '{}'. Cause: {}", String::from_utf8_lossy(&bytes), err))?;
    let config = Rc::new(config);
    let client = Rc::new(client);
    let clock = Rc::new(clock);
    if config.distributed.unwrap_or(false) {
        let meta = Rc::new(storage_builder.remote(META_CACHE_NAMESPACE, meta_store_ttl_ms(&config)));
        let lock = Rc::new(storage_builder.remote(REFRESH_LOCK_NAMESPACE, REFRESH_LOCK_TTL_MS));
        launch_policy(launcher, config, client, clock, meta, lock).await
    } else {
        let meta = Rc::new(storage_builder.local(META_CACHE_NAMESPACE));
        let lock = Rc::new(storage_builder.local(REFRESH_LOCK_NAMESPACE));
        launch_policy(launcher, config, client, clock, meta, lock).await
    }
}
