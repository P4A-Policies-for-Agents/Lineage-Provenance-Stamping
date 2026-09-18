// Copyright 2026 Salesforce, Inc. All rights reserved.
//! Pure derivation helpers (no PDK imports) — parsing CDGC search hits into the
//! values stamped as headers, and classifying relationships as structural vs
//! lineage. Kept separate from lib.rs so they are unit-testable on the host target.

use serde_json::Value;

/// Relationship class types that are structural/governance edges on a scanned asset,
/// NOT data lineage. Anything touching the asset/columns whose type is none of these
/// is treated as lineage (upstream/downstream). Derived from a live CDGC probe of a
/// scanned flat file (FileToField, classification, DQ, glossary, marketplace, system).
/// CDGC lineage uses core.DataSetDataFlow / core.DirectionalDataFlow, which are
/// deliberately absent here so they read as lineage.
pub const STRUCTURAL_REL_TYPES: &[&str] = &[
    "com.infa.odin.models.file.FileToField",
    "com.infa.odin.models.file.FolderToFile",
    "core.QuasiClassifiedAs",
    "com.infa.ccgf.models.governance.asscIClassDQResult",
    "com.infa.ccgf.models.governance.asscParentDataElementRuleInstance",
    "com.infa.ccgf.models.governance.asscSecondaryObjectofRuleInstance",
    "com.infa.ccgf.models.governance.IClassTechnicalGlossaryBase",
    "com.infa.ccgf.models.governance.asscSystemDataSet",
    "com.infa.cdmp.marketplace.asscDataCollection",
];

/// JSON-RPC methods carrying data-product content worth enriching.
pub fn is_content_method(method: &str) -> bool {
    matches!(
        method,
        "tools/call" | "resources/read" | "prompts/get"
            | "message/send" | "message/stream" | "SendMessage" | "SendStreamingMessage"
    )
}

/// Read a string attribute.
pub fn s(map: &Value, key: &str) -> Option<String> {
    map.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Read a scalar attribute that may be a string OR a JSON number (e.g. epoch ms).
pub fn num_or_str(map: &Value, key: &str) -> Option<String> {
    match map.get(key) {
        Some(Value::String(v)) if !v.is_empty() => Some(v.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// CDGC sometimes doubles resourceName ("Foo BarFoo Bar"); halve it when it is an
/// exact concatenation of two equal halves.
pub fn dedupe_doubled(v: &str) -> String {
    let n = v.len();
    if n % 2 == 0 {
        let (a, b) = v.split_at(n / 2);
        if a == b {
            return a.to_string();
        }
    }
    v.to_string()
}

/// The display name of a lineage neighbour, resolved to its parent FILE so a
/// column-level edge (e.g. sku->sku) still reports as "fact_order_line.csv". Parses
/// the *.csv segment out of the external id; falls back to core.name.
pub fn neighbor_file(obj: &Value) -> Option<String> {
    if let Some(ext) = s(obj, "core.externalId") {
        let base = ext.split('~').next().unwrap_or(&ext);
        if let Some(seg) = base.split('/').rev().find(|p| p.ends_with(".csv")) {
            return Some(seg.to_string());
        }
    }
    s(obj, "core.name")
}

/// The relationship's class type can be a single string or an array (the class
/// hierarchy). A relationship is structural if ANY of its type entries is known.
pub fn rel_is_structural(rel: &Value) -> bool {
    let t = rel.get("type").or_else(|| rel.get("core.classType"));
    let entries: Vec<&str> = match t {
        Some(Value::String(v)) => vec![v.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    entries.iter().any(|e| STRUCTURAL_REL_TYPES.contains(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dedupe_doubled_halves_exact_duplication() {
        assert_eq!(dedupe_doubled("Product Catalog dim_productProduct Catalog dim_product"),
                   "Product Catalog dim_product");
        assert_eq!(dedupe_doubled("abcabc"), "abc");
    }
    #[test]
    fn dedupe_doubled_leaves_non_doubled_untouched() {
        assert_eq!(dedupe_doubled("File System"), "File System");
        assert_eq!(dedupe_doubled("abcd"), "abcd"); // even len, halves differ
        assert_eq!(dedupe_doubled("aba"), "aba");   // odd len
        assert_eq!(dedupe_doubled(""), "");
    }
    #[test]
    fn num_or_str_reads_numbers_and_strings() {
        let v = json!({"core.modifiedOn": 1788347759583i64, "core.sourceModifiedOn": "2026-08-31", "empty": ""});
        assert_eq!(num_or_str(&v, "core.modifiedOn").as_deref(), Some("1788347759583"));
        assert_eq!(num_or_str(&v, "core.sourceModifiedOn").as_deref(), Some("2026-08-31"));
        assert_eq!(num_or_str(&v, "empty"), None);
        assert_eq!(num_or_str(&v, "missing"), None);
    }
    #[test]
    fn neighbor_file_resolves_file_and_column_to_file_name() {
        let file = json!({"core.name":"fact_order_line.csv",
            "core.externalId":"adb://FileServer/data/csv/order-transactions/fact_order_line.csv~com.infa.odin.models.file.flat.FlatFile"});
        let col = json!({"core.name":"sku",
            "core.externalId":"adb://FileServer/data/csv/order-transactions/fact_order_line.csv/sku~com.infa.odin.models.file.flat.FlatField"});
        assert_eq!(neighbor_file(&file).as_deref(), Some("fact_order_line.csv"));
        assert_eq!(neighbor_file(&col).as_deref(), Some("fact_order_line.csv"));
    }
    #[test]
    fn neighbor_file_falls_back_to_core_name() {
        let rel = json!({"core.name":"customer_master","core.externalId":"sql://SQLDB/dbo/customer_master~com.infa.odin.models.relational.Table"});
        assert_eq!(neighbor_file(&rel).as_deref(), Some("customer_master"));
        let noext = json!({"core.name":"thing"});
        assert_eq!(neighbor_file(&noext).as_deref(), Some("thing"));
    }
    #[test]
    fn rel_is_structural_matches_string_and_array_types() {
        assert!(rel_is_structural(&json!({"type":"com.infa.odin.models.file.FileToField"})));
        assert!(rel_is_structural(&json!({"type":["core.IClass","core.QuasiClassifiedAs"]})));
        assert!(rel_is_structural(&json!({"core.classType":"com.infa.cdmp.marketplace.asscDataCollection"})));
    }
    #[test]
    fn rel_is_structural_false_for_lineage_edges() {
        assert!(!rel_is_structural(&json!({"type":"core.DataSetDataFlow"})));
        assert!(!rel_is_structural(&json!({"type":"core.DirectionalDataFlow"})));
        assert!(!rel_is_structural(&json!({"type":["core.IClass","core.DirectionalDataFlow"]})));
        assert!(!rel_is_structural(&json!({})));
    }
    #[test]
    fn is_content_method_gates_mcp_and_a2a() {
        assert!(is_content_method("tools/call"));
        assert!(is_content_method("resources/read"));
        assert!(is_content_method("message/send"));
        assert!(!is_content_method("tools/list"));
        assert!(!is_content_method("initialize"));
    }
}
