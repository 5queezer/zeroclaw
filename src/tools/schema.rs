//! JSON Schema cleaning and validation for LLM tool-calling compatibility.
//!
//! Different providers support different subsets of JSON Schema. This module
//! normalizes tool schemas to improve cross-provider compatibility while
//! preserving semantic intent.
//!
//! ## What this module does
//!
//! 1. Removes unsupported keywords per provider strategy
//! 2. Resolves local `$ref` entries from `$defs` and `definitions`
//! 3. Flattens literal `anyOf` / `oneOf` unions into `enum`
//! 4. Strips nullable variants from unions and `type` arrays
//! 5. Converts `const` to single-value `enum`
//! 6. Detects circular references and stops recursion safely
//!
//! # Example
//!
//! ```rust
//! use serde_json::json;
//! use hrafn::tools::schema::SchemaCleanr;
//!
//! let dirty_schema = json!({
//!     "type": "object",
//!     "properties": {
//!         "name": {
//!             "type": "string",
//!             "minLength": 1,  // Gemini rejects this
//!             "pattern": "^[a-z]+$"  // Gemini rejects this
//!         },
//!         "age": {
//!             "$ref": "#/$defs/Age"  // Needs resolution
//!         }
//!     },
//!     "$defs": {
//!         "Age": {
//!             "type": "integer",
//!             "minimum": 0  // Gemini rejects this
//!         }
//!     }
//! });
//!
//! let cleaned = SchemaCleanr::clean_for_gemini(dirty_schema);
//!
//! // Result:
//! // {
//! //   "type": "object",
//! //   "properties": {
//! //     "name": { "type": "string" },
//! //     "age": { "type": "integer" }
//! //   }
//! // }
//! ```
//!
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

/// Keywords that Gemini rejects for tool schemas.
pub const GEMINI_UNSUPPORTED_KEYWORDS: &[&str] = &[
    // Schema composition
    "$ref",
    "$schema",
    "$id",
    "$defs",
    "definitions",
    // Property constraints
    "additionalProperties",
    "patternProperties",
    // String constraints
    "minLength",
    "maxLength",
    "pattern",
    "format",
    // Number constraints
    "minimum",
    "maximum",
    "multipleOf",
    // Array constraints
    "minItems",
    "maxItems",
    "uniqueItems",
    // Object constraints
    "minProperties",
    "maxProperties",
    // Non-standard
    "examples", // OpenAPI keyword, not JSON Schema
];

/// Keywords that should be preserved during cleaning (metadata).
const SCHEMA_META_KEYS: &[&str] = &["description", "title", "default"];

/// Schema cleaning strategies for different LLM providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleaningStrategy {
    /// Gemini (Google AI / Vertex AI) - Most restrictive
    Gemini,
    /// Anthropic Claude - Moderately permissive
    Anthropic,
    /// OpenAI GPT - Most permissive
    OpenAI,
    /// Conservative: Remove only universally unsupported keywords
    Conservative,
}

impl CleaningStrategy {
    /// Get the list of unsupported keywords for this strategy.
    pub fn unsupported_keywords(self) -> &'static [&'static str] {
        match self {
            Self::Gemini | Self::Anthropic => GEMINI_UNSUPPORTED_KEYWORDS,
            Self::OpenAI => &[], // OpenAI is most permissive
            Self::Conservative => &["$ref", "$defs", "definitions", "additionalProperties"],
        }
    }
}

/// JSON Schema cleaner optimized for LLM tool calling.
pub struct SchemaCleanr;

impl SchemaCleanr {
    /// Clean schema for Gemini compatibility (strictest).
    ///
    /// This is the most aggressive cleaning strategy, removing all keywords
    /// that Gemini's API rejects.
    pub fn clean_for_gemini(schema: Value) -> Value {
        Self::clean(schema, CleaningStrategy::Gemini)
    }

    /// Clean schema for Anthropic compatibility.
    pub fn clean_for_anthropic(schema: Value) -> Value {
        Self::clean(schema, CleaningStrategy::Anthropic)
    }

    /// Clean schema for OpenAI compatibility (most permissive).
    pub fn clean_for_openai(schema: Value) -> Value {
        Self::clean(schema, CleaningStrategy::OpenAI)
    }

    /// Clean schema with specified strategy.
    pub fn clean(schema: Value, strategy: CleaningStrategy) -> Value {
        // Extract $defs for reference resolution
        let defs = if let Some(obj) = schema.as_object() {
            Self::extract_defs(obj)
        } else {
            HashMap::new()
        };

        Self::clean_with_defs(schema, &defs, strategy, &mut HashSet::new())
    }

    /// Validate that a schema is suitable for LLM tool calling.
    ///
    /// Returns an error if the schema is invalid or missing required fields.
    pub fn validate(schema: &Value) -> anyhow::Result<()> {
        let obj = schema
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Schema must be an object"))?;

        // Must have 'type' field
        if !obj.contains_key("type") {
            anyhow::bail!("Schema missing required 'type' field");
        }

        // If type is 'object', should have 'properties'
        if let Some(Value::String(t)) = obj.get("type") {
            if t == "object" && !obj.contains_key("properties") {
                tracing::warn!("Object schema without 'properties' field may cause issues");
            }
        }

        Ok(())
    }

    // --------------------------------------------------------------------
    // Internal implementation
    // --------------------------------------------------------------------

    /// Extract $defs and definitions into a flat map for reference resolution.
    fn extract_defs(obj: &Map<String, Value>) -> HashMap<String, Value> {
        let mut defs = HashMap::new();

        // Extract from $defs (JSON Schema 2019-09+)
        if let Some(Value::Object(defs_obj)) = obj.get("$defs") {
            for (key, value) in defs_obj {
                defs.insert(key.clone(), value.clone());
            }
        }

        // Extract from definitions (JSON Schema draft-07)
        if let Some(Value::Object(defs_obj)) = obj.get("definitions") {
            for (key, value) in defs_obj {
                defs.insert(key.clone(), value.clone());
            }
        }

        defs
    }

    /// Recursively clean a schema value.
    fn clean_with_defs(
        schema: Value,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Value {
        match schema {
            Value::Object(obj) => Self::clean_object(obj, defs, strategy, ref_stack),
            Value::Array(arr) => Value::Array(
                arr.into_iter()
                    .map(|v| Self::clean_with_defs(v, defs, strategy, ref_stack))
                    .collect(),
            ),
            other => other,
        }
    }

    /// Clean an object schema.
    fn clean_object(
        obj: Map<String, Value>,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Value {
        // Handle $ref resolution
        if let Some(Value::String(ref_value)) = obj.get("$ref") {
            return Self::resolve_ref(ref_value, &obj, defs, strategy, ref_stack);
        }

        // Handle anyOf/oneOf simplification
        if obj.contains_key("anyOf") || obj.contains_key("oneOf") {
            if let Some(simplified) = Self::try_simplify_union(&obj, defs, strategy, ref_stack) {
                return simplified;
            }
        }

        // Handle allOf (intersection semantics — different from anyOf/oneOf)
        if obj.contains_key("allOf") {
            if let Some(merged) = Self::try_merge_allof(&obj, defs, strategy, ref_stack) {
                return merged;
            }
        }

        // Build cleaned object
        let mut cleaned = Map::new();
        let unsupported: HashSet<&str> = strategy.unsupported_keywords().iter().copied().collect();
        let has_union = obj.contains_key("anyOf") || obj.contains_key("oneOf");

        for (key, value) in obj {
            // Skip unsupported keywords
            if unsupported.contains(key.as_str()) {
                continue;
            }

            // Special handling for specific keys
            match key.as_str() {
                // Convert const to enum
                "const" => {
                    cleaned.insert("enum".to_string(), json!([value]));
                }
                // Skip type for disjunction unions (anyOf/oneOf) — they define the type.
                // allOf is conjunctive and must not suppress top-level type.
                "type" if has_union => {
                    // Skip
                }
                // Handle type arrays (remove null)
                "type" if matches!(value, Value::Array(_)) => {
                    let cleaned_value = Self::clean_type_array(value);
                    cleaned.insert(key, cleaned_value);
                }
                // Recursively clean nested schemas
                "properties" => {
                    let cleaned_value = Self::clean_properties(value, defs, strategy, ref_stack);
                    cleaned.insert(key, cleaned_value);
                }
                "items" => {
                    let cleaned_value = Self::clean_with_defs(value, defs, strategy, ref_stack);
                    cleaned.insert(key, cleaned_value);
                }
                "anyOf" | "oneOf" | "allOf" => {
                    let cleaned_value = Self::clean_union(value, defs, strategy, ref_stack);
                    cleaned.insert(key, cleaned_value);
                }
                // Keep all other keys, cleaning nested objects/arrays recursively.
                _ => {
                    let cleaned_value = match value {
                        Value::Object(_) | Value::Array(_) => {
                            Self::clean_with_defs(value, defs, strategy, ref_stack)
                        }
                        other => other,
                    };
                    cleaned.insert(key, cleaned_value);
                }
            }
        }

        Value::Object(cleaned)
    }

    /// Resolve a $ref to its definition.
    fn resolve_ref(
        ref_value: &str,
        obj: &Map<String, Value>,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Value {
        // Prevent circular references
        if ref_stack.contains(ref_value) {
            tracing::warn!("Circular $ref detected: {}", ref_value);
            return Self::preserve_meta(obj, Value::Object(Map::new()));
        }

        // Try to resolve local ref (#/$defs/Name or #/definitions/Name)
        if let Some(def_name) = Self::parse_local_ref(ref_value) {
            if let Some(definition) = defs.get(def_name.as_str()) {
                ref_stack.insert(ref_value.to_string());
                let cleaned = Self::clean_with_defs(definition.clone(), defs, strategy, ref_stack);
                ref_stack.remove(ref_value);
                return Self::preserve_meta(obj, cleaned);
            }
        }

        // Can't resolve: return empty object with metadata
        tracing::warn!("Cannot resolve $ref: {}", ref_value);
        Self::preserve_meta(obj, Value::Object(Map::new()))
    }

    /// Parse a local JSON Pointer ref (#/$defs/Name).
    fn parse_local_ref(ref_value: &str) -> Option<String> {
        ref_value
            .strip_prefix("#/$defs/")
            .or_else(|| ref_value.strip_prefix("#/definitions/"))
            .map(Self::decode_json_pointer)
    }

    /// Decode JSON Pointer escaping (`~0` = `~`, `~1` = `/`).
    fn decode_json_pointer(segment: &str) -> String {
        if !segment.contains('~') {
            return segment.to_string();
        }

        let mut decoded = String::with_capacity(segment.len());
        let mut chars = segment.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '~' {
                match chars.peek().copied() {
                    Some('0') => {
                        chars.next();
                        decoded.push('~');
                    }
                    Some('1') => {
                        chars.next();
                        decoded.push('/');
                    }
                    _ => decoded.push('~'),
                }
            } else {
                decoded.push(ch);
            }
        }

        decoded
    }

    /// Try to simplify anyOf/oneOf to a simpler form.
    ///
    /// Collects variants from both `anyOf` and `oneOf` if both are present —
    /// having both simultaneously is unusual but valid JSON Schema.
    fn try_simplify_union(
        obj: &Map<String, Value>,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Option<Value> {
        if !obj.contains_key("anyOf") && !obj.contains_key("oneOf") {
            return None;
        }

        // Collect from both keys — if both are present their variants are unioned
        let mut raw_variants: Vec<Value> = Vec::new();
        for key in &["anyOf", "oneOf"] {
            if let Some(Value::Array(arr)) = obj.get(*key) {
                raw_variants.extend(arr.iter().cloned());
            }
        }
        if raw_variants.is_empty() {
            return None;
        }

        // Clean all variants first
        let cleaned_variants: Vec<Value> = raw_variants
            .iter()
            .map(|v| Self::clean_with_defs(v.clone(), defs, strategy, ref_stack))
            .collect();

        // Strip null variants
        let non_null: Vec<Value> = cleaned_variants
            .into_iter()
            .filter(|v| !Self::is_null_schema(v))
            .collect();

        // If only one variant remains after stripping nulls, return it
        if non_null.len() == 1 {
            return Some(Self::preserve_meta(obj, non_null[0].clone()));
        }

        // Try to flatten to enum if all variants are literals
        if let Some(enum_value) = Self::try_flatten_literal_union(&non_null) {
            return Some(Self::preserve_meta(obj, enum_value));
        }

        // Gemini and Anthropic don't support anyOf/oneOf — apply progressively
        // more aggressive simplifications until something works.
        if (strategy == CleaningStrategy::Gemini || strategy == CleaningStrategy::Anthropic)
            && !non_null.is_empty()
        {
            // All-object union → merge all properties into one object
            if let Some(merged) = Self::try_merge_object_union(&non_null, obj) {
                return Some(merged);
            }
            // All same primitive type → collapse (union enums if present)
            if let Some(typed) = Self::try_same_primitive_type_union(&non_null, obj) {
                return Some(typed);
            }
            // Heterogeneous union → best-effort property union or richest variant
            return Some(Self::union_fallback(&non_null, obj));
        }

        None
    }

    /// Check if a schema represents null type.
    fn is_null_schema(value: &Value) -> bool {
        if let Some(obj) = value.as_object() {
            // { const: null }
            if let Some(Value::Null) = obj.get("const") {
                return true;
            }
            // { enum: [null] }
            if let Some(Value::Array(arr)) = obj.get("enum") {
                if arr.len() == 1 && matches!(arr[0], Value::Null) {
                    return true;
                }
            }
            // { type: "null" }
            if let Some(Value::String(t)) = obj.get("type") {
                if t == "null" {
                    return true;
                }
            }
        }
        false
    }

    /// Try to flatten anyOf/oneOf with only literal values to enum.
    ///
    /// Example: `anyOf: [{const: "a"}, {const: "b"}]` -> `{type: "string", enum: ["a", "b"]}`
    fn try_flatten_literal_union(variants: &[Value]) -> Option<Value> {
        if variants.is_empty() {
            return None;
        }

        let mut all_values = Vec::new();
        let mut common_type: Option<String> = None;

        for variant in variants {
            let obj = variant.as_object()?;

            // Extract literal value from const or single-item enum
            let literal_value = if let Some(const_val) = obj.get("const") {
                const_val.clone()
            } else if let Some(Value::Array(arr)) = obj.get("enum") {
                if arr.len() == 1 {
                    arr[0].clone()
                } else {
                    return None;
                }
            } else {
                return None;
            };

            // Check type consistency — infer from the literal value when not explicit
            let variant_type = if let Some(t) = obj.get("type").and_then(|v| v.as_str()) {
                t.to_string()
            } else {
                match &literal_value {
                    Value::String(_) => "string".to_string(),
                    Value::Bool(_) => "boolean".to_string(),
                    Value::Number(n) if n.is_i64() || n.is_u64() => "integer".to_string(),
                    Value::Number(_) => "number".to_string(),
                    _ => return None,
                }
            };
            match &common_type {
                None => common_type = Some(variant_type),
                Some(t) if *t != variant_type => return None,
                _ => {}
            }

            all_values.push(literal_value);
        }

        common_type.map(|t| {
            json!({
                "type": t,
                "enum": all_values
            })
        })
    }

    /// Merge an all-object union into a single object schema.
    ///
    /// All variants must be `type: "object"`. Properties are merged (first
    /// definition wins on key collision). `required` becomes the intersection
    /// of all variants' required lists.
    fn try_merge_object_union(variants: &[Value], source: &Map<String, Value>) -> Option<Value> {
        if variants.is_empty() {
            return None;
        }

        // All variants must be type: "object"
        for v in variants {
            let obj = v.as_object()?;
            match obj.get("type") {
                Some(Value::String(t)) if t == "object" => {}
                _ => return None,
            }
        }

        // Required intersection: fields required by ALL variants stay required
        let required_intersection: HashSet<String> = {
            let per_variant: Vec<HashSet<String>> = variants
                .iter()
                .map(|v| {
                    v.get("required")
                        .and_then(|r| r.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect();
            per_variant.iter().skip(1).fold(
                per_variant.first().cloned().unwrap_or_default(),
                |acc, s| acc.intersection(s).cloned().collect(),
            )
        };

        // Merge properties — bail on type conflicts so the caller can try a
        // less aggressive simplification (e.g. picking the richest variant).
        let mut merged_props: Map<String, Value> = Map::new();
        for variant in variants {
            if let Some(Value::Object(props)) = variant.get("properties") {
                for (k, v) in props {
                    if let Some(existing) = merged_props.get(k) {
                        if existing != v {
                            return None; // conflicting definitions for the same key
                        }
                    } else {
                        merged_props.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        let mut result = Map::new();
        result.insert("type".to_string(), json!("object"));
        if !merged_props.is_empty() {
            result.insert("properties".to_string(), Value::Object(merged_props));
        }
        if !required_intersection.is_empty() {
            let mut req_vec: Vec<String> = required_intersection.into_iter().collect();
            req_vec.sort(); // deterministic ordering for tests
            let req_vec: Vec<Value> = req_vec.into_iter().map(Value::String).collect();
            result.insert("required".to_string(), Value::Array(req_vec));
        }

        Some(Self::preserve_meta(source, Value::Object(result)))
    }

    /// Collapse a union where every variant shares the same primitive type.
    ///
    /// When all variants agree on a primitive type:
    /// - If every variant carries an `enum`, the values are unioned into one list.
    /// - Otherwise the type is returned without enum restriction so no data is lost.
    ///
    /// Does not handle `object` or `array` — those go through
    /// `try_merge_object_union` instead.
    fn try_same_primitive_type_union(
        variants: &[Value],
        source: &Map<String, Value>,
    ) -> Option<Value> {
        if variants.is_empty() {
            return None;
        }

        let mut common_type: Option<String> = None;

        for variant in variants {
            let obj = variant.as_object()?;
            let t = obj.get("type")?.as_str()?;
            // Object/array unions are handled by try_merge_object_union
            if matches!(t, "object" | "array") {
                return None;
            }
            match &common_type {
                None => common_type = Some(t.to_string()),
                Some(ct) if ct == t => {}
                _ => return None,
            }
        }

        let t = common_type?;

        // If every variant carries an enum, union all values into one enum.
        let all_have_enum = variants.iter().all(|v| {
            v.as_object()
                .map(|o| o.contains_key("enum"))
                .unwrap_or(false)
        });

        let result = if all_have_enum {
            let mut combined: Vec<Value> = Vec::new();
            for variant in variants {
                if let Some(Value::Array(vals)) = variant.get("enum") {
                    for v in vals {
                        if !combined.contains(v) {
                            combined.push(v.clone());
                        }
                    }
                }
            }
            json!({ "type": t, "enum": combined })
        } else {
            json!({ "type": t })
        };

        Some(Self::preserve_meta(source, result))
    }

    /// Merge an `allOf` schema into a single flat schema.
    ///
    /// `allOf` means every sub-schema must hold simultaneously (intersection),
    /// so:
    /// - **Properties** are the union of all variants' properties (all apply).
    /// - **`required`** is the union of all variants' required lists.
    ///
    /// For non-Gemini/Anthropic strategies only trivial cases are flattened
    /// (single effective variant after stripping empty schemas); providers that
    /// understand `allOf` natively keep it for multi-variant cases.
    fn try_merge_allof(
        obj: &Map<String, Value>,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Option<Value> {
        let raw = obj.get("allOf")?.as_array()?;

        // Clean variants first (resolves $refs, strips unsupported keywords, etc.)
        let cleaned: Vec<Value> = raw
            .iter()
            .map(|v| Self::clean_with_defs(v.clone(), defs, strategy, ref_stack))
            .collect();

        // Drop empty schemas {} — cleaning may have removed all meaningful content
        let effective: Vec<Value> = cleaned
            .into_iter()
            .filter(|v| v.as_object().map_or(true, |o| !o.is_empty()))
            .collect();

        if effective.is_empty() {
            return Some(Self::preserve_meta(obj, Value::Object(Map::new())));
        }

        // Single effective schema → unwrap directly (applies for all strategies)
        if effective.len() == 1 {
            return Some(Self::preserve_meta(
                obj,
                effective.into_iter().next().unwrap(),
            ));
        }

        // Multi-variant: only Gemini/Anthropic must flatten (other providers support allOf)
        if strategy != CleaningStrategy::Gemini && strategy != CleaningStrategy::Anthropic {
            return None;
        }

        // All variants are objects → merge (union of properties + union of required)
        let all_objects = effective.iter().all(|v| {
            v.as_object()
                .and_then(|o| o.get("type"))
                .and_then(|t| t.as_str())
                == Some("object")
        });

        if all_objects {
            let mut merged_props: Map<String, Value> = Map::new();
            let mut all_required: HashSet<String> = HashSet::new();
            let mut has_conflict = false;

            for variant in &effective {
                if let Some(Value::Object(props)) = variant.get("properties") {
                    for (k, v) in props {
                        if let Some(existing) = merged_props.get(k) {
                            if existing != v {
                                has_conflict = true;
                                break;
                            }
                        } else {
                            merged_props.insert(k.clone(), v.clone());
                        }
                    }
                    if has_conflict {
                        break;
                    }
                }
                if let Some(Value::Array(req)) = variant.get("required") {
                    for r in req {
                        if let Some(s) = r.as_str() {
                            all_required.insert(s.to_string());
                        }
                    }
                }
            }

            if !has_conflict {
                let mut result = Map::new();
                result.insert("type".to_string(), json!("object"));
                if !merged_props.is_empty() {
                    result.insert("properties".to_string(), Value::Object(merged_props));
                }
                if !all_required.is_empty() {
                    let mut req_vec: Vec<String> = all_required.into_iter().collect();
                    req_vec.sort();
                    let req_vec: Vec<Value> = req_vec.into_iter().map(Value::String).collect();
                    result.insert("required".to_string(), Value::Array(req_vec));
                }
                return Some(Self::preserve_meta(obj, Value::Object(result)));
            }
        }

        // Heterogeneous allOf — key-level merge, first definition wins on collision.
        // Handles the common add-constraint pattern: allOf: [{type:"T"}, {enum:[...]}]
        let mut merged: Map<String, Value> = Map::new();
        for variant in &effective {
            if let Some(variant_obj) = variant.as_object() {
                for (k, v) in variant_obj {
                    merged.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
        Some(Self::preserve_meta(obj, Value::Object(merged)))
    }

    /// Last-resort flattening for unions that couldn't be simplified.
    ///
    /// Strategy:
    /// 1. Build a merged property map from every variant that has `properties`.
    /// 2. Collect a common `type` if all variants agree on one.
    /// 3. Fall back to the richest variant (most top-level keys) when the
    ///    variants are too heterogeneous to merge cleanly.
    fn union_fallback(variants: &[Value], source: &Map<String, Value>) -> Value {
        // Collect merged properties from all object-bearing variants
        let mut merged_props: Map<String, Value> = Map::new();
        let mut common_type: Option<String> = None;
        let mut type_conflict = false;

        for variant in variants {
            if let Some(obj) = variant.as_object() {
                // Accumulate properties (first definition wins on collision)
                if let Some(Value::Object(props)) = obj.get("properties") {
                    for (k, v) in props {
                        merged_props.entry(k.clone()).or_insert_with(|| v.clone());
                    }
                }

                // Track whether all variants agree on a type
                if let Some(Value::String(t)) = obj.get("type") {
                    match &common_type {
                        None => common_type = Some(t.clone()),
                        Some(ct) if ct == t => {}
                        _ => type_conflict = true,
                    }
                }
            }
        }

        // If we accumulated properties, emit a merged object schema
        if !merged_props.is_empty() {
            let mut result = Map::new();
            result.insert("type".to_string(), json!("object"));
            result.insert("properties".to_string(), Value::Object(merged_props));
            return Self::preserve_meta(source, Value::Object(result));
        }

        // No properties to merge — use common type if all variants agreed,
        // otherwise fall back to the variant with the most top-level keys.
        if !type_conflict {
            if let Some(t) = common_type {
                if t == "array" {
                    let first_items = variants
                        .iter()
                        .find_map(|v| v.as_object().and_then(|o| o.get("items")).cloned());
                    if let Some(items) = first_items {
                        return Self::preserve_meta(
                            source,
                            json!({ "type": "array", "items": items }),
                        );
                    }
                }
                return Self::preserve_meta(source, json!({ "type": t }));
            }
        }

        let richest = variants
            .iter()
            .max_by_key(|v| v.as_object().map_or(0, |o| o.len()))
            .cloned()
            .unwrap_or(Value::Object(Map::new()));

        Self::preserve_meta(source, richest)
    }

    /// Clean type array, removing null.
    fn clean_type_array(value: Value) -> Value {
        if let Value::Array(types) = value {
            let non_null: Vec<Value> = types
                .into_iter()
                .filter(|v| v.as_str() != Some("null"))
                .collect();

            match non_null.len() {
                0 => Value::String("null".to_string()),
                1 => non_null
                    .into_iter()
                    .next()
                    .unwrap_or(Value::String("null".to_string())),
                _ => Value::Array(non_null),
            }
        } else {
            value
        }
    }

    /// Clean properties object.
    fn clean_properties(
        value: Value,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Value {
        if let Value::Object(props) = value {
            let cleaned: Map<String, Value> = props
                .into_iter()
                .map(|(k, v)| (k, Self::clean_with_defs(v, defs, strategy, ref_stack)))
                .collect();
            Value::Object(cleaned)
        } else {
            value
        }
    }

    /// Clean union (anyOf/oneOf/allOf).
    fn clean_union(
        value: Value,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Value {
        if let Value::Array(variants) = value {
            let cleaned: Vec<Value> = variants
                .into_iter()
                .map(|v| Self::clean_with_defs(v, defs, strategy, ref_stack))
                .collect();
            Value::Array(cleaned)
        } else {
            value
        }
    }

    /// Preserve metadata (description, title, default) from source to target.
    fn preserve_meta(source: &Map<String, Value>, mut target: Value) -> Value {
        if let Value::Object(target_obj) = &mut target {
            for &key in SCHEMA_META_KEYS {
                if let Some(value) = source.get(key) {
                    target_obj.insert(key.to_string(), value.clone());
                }
            }
        }
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remove_unsupported_keywords() {
        let schema = json!({
            "type": "string",
            "minLength": 1,
            "maxLength": 100,
            "pattern": "^[a-z]+$",
            "description": "A lowercase string"
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "string");
        assert_eq!(cleaned["description"], "A lowercase string");
        assert!(cleaned.get("minLength").is_none());
        assert!(cleaned.get("maxLength").is_none());
        assert!(cleaned.get("pattern").is_none());
    }

    #[test]
    fn test_resolve_ref() {
        let schema = json!({
            "type": "object",
            "properties": {
                "age": {
                    "$ref": "#/$defs/Age"
                }
            },
            "$defs": {
                "Age": {
                    "type": "integer",
                    "minimum": 0
                }
            }
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["properties"]["age"]["type"], "integer");
        assert!(cleaned["properties"]["age"].get("minimum").is_none()); // Stripped by Gemini strategy
        assert!(cleaned.get("$defs").is_none());
    }

    #[test]
    fn test_flatten_literal_union() {
        let schema = json!({
            "anyOf": [
                { "const": "admin", "type": "string" },
                { "const": "user", "type": "string" },
                { "const": "guest", "type": "string" }
            ]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "string");
        assert!(cleaned["enum"].is_array());
        let enum_values = cleaned["enum"].as_array().unwrap();
        assert_eq!(enum_values.len(), 3);
        assert!(enum_values.contains(&json!("admin")));
        assert!(enum_values.contains(&json!("user")));
        assert!(enum_values.contains(&json!("guest")));
    }

    #[test]
    fn test_strip_null_from_union() {
        let schema = json!({
            "oneOf": [
                { "type": "string" },
                { "type": "null" }
            ]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        // Should simplify to just { type: "string" }
        assert_eq!(cleaned["type"], "string");
        assert!(cleaned.get("oneOf").is_none());
    }

    #[test]
    fn test_const_to_enum() {
        let schema = json!({
            "const": "fixed_value",
            "description": "A constant"
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["enum"], json!(["fixed_value"]));
        assert_eq!(cleaned["description"], "A constant");
        assert!(cleaned.get("const").is_none());
    }

    #[test]
    fn test_preserve_metadata() {
        let schema = json!({
            "$ref": "#/$defs/Name",
            "description": "User's name",
            "title": "Name Field",
            "default": "Anonymous",
            "$defs": {
                "Name": {
                    "type": "string"
                }
            }
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "string");
        assert_eq!(cleaned["description"], "User's name");
        assert_eq!(cleaned["title"], "Name Field");
        assert_eq!(cleaned["default"], "Anonymous");
    }

    #[test]
    fn test_circular_ref_prevention() {
        let schema = json!({
            "type": "object",
            "properties": {
                "parent": {
                    "$ref": "#/$defs/Node"
                }
            },
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "child": {
                            "$ref": "#/$defs/Node"
                        }
                    }
                }
            }
        });

        // Should not panic on circular reference
        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["properties"]["parent"]["type"], "object");
        // Circular reference should be broken
    }

    #[test]
    fn test_validate_schema() {
        let valid = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            }
        });

        assert!(SchemaCleanr::validate(&valid).is_ok());

        let invalid = json!({
            "properties": {
                "name": { "type": "string" }
            }
        });

        assert!(SchemaCleanr::validate(&invalid).is_err());
    }

    #[test]
    fn test_strategy_differences() {
        let schema = json!({
            "type": "string",
            "minLength": 1,
            "description": "A string field"
        });

        // Gemini: Most restrictive (removes minLength)
        let gemini = SchemaCleanr::clean_for_gemini(schema.clone());
        assert!(gemini.get("minLength").is_none());
        assert_eq!(gemini["type"], "string");
        assert_eq!(gemini["description"], "A string field");

        // OpenAI: Most permissive (keeps minLength)
        let openai = SchemaCleanr::clean_for_openai(schema.clone());
        assert_eq!(openai["minLength"], 1); // OpenAI allows validation keywords
        assert_eq!(openai["type"], "string");
    }

    #[test]
    fn test_nested_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "user": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "minLength": 1
                        }
                    },
                    "additionalProperties": false
                }
            }
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert!(
            cleaned["properties"]["user"]["properties"]["name"]
                .get("minLength")
                .is_none()
        );
        assert!(
            cleaned["properties"]["user"]
                .get("additionalProperties")
                .is_none()
        );
    }

    #[test]
    fn test_type_array_null_removal() {
        let schema = json!({
            "type": ["string", "null"]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        // Should simplify to just "string"
        assert_eq!(cleaned["type"], "string");
    }

    #[test]
    fn test_type_array_only_null_preserved() {
        let schema = json!({
            "type": ["null"]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "null");
    }

    #[test]
    fn test_ref_with_json_pointer_escape() {
        let schema = json!({
            "$ref": "#/$defs/Foo~1Bar",
            "$defs": {
                "Foo/Bar": {
                    "type": "string"
                }
            }
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "string");
    }

    #[test]
    fn test_all_object_oneof_merged_for_gemini() {
        // All-object oneOf is now merged into a single object schema for Gemini
        // (Gemini doesn't support oneOf at all).
        let schema = json!({
            "type": "object",
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "a": { "type": "string" }
                    }
                },
                {
                    "type": "object",
                    "properties": {
                        "b": { "type": "number" }
                    }
                }
            ]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        // Merged into one object with all properties; oneOf is gone
        assert_eq!(cleaned["type"], "object");
        assert!(cleaned.get("oneOf").is_none());
        assert_eq!(cleaned["properties"]["a"]["type"], "string");
        assert_eq!(cleaned["properties"]["b"]["type"], "number");
    }

    #[test]
    fn test_all_object_oneof_kept_for_openai() {
        // Non-Gemini strategies keep oneOf when it can't be simplified to enum/single.
        let schema = json!({
            "type": "object",
            "oneOf": [
                {
                    "type": "object",
                    "properties": { "a": { "type": "string" } }
                },
                {
                    "type": "object",
                    "properties": { "b": { "type": "number" } }
                }
            ]
        });

        let cleaned = SchemaCleanr::clean_for_openai(schema);

        // OpenAI supports oneOf natively — keep it
        assert!(cleaned.get("oneOf").is_some());
    }

    #[test]
    fn test_merge_object_union_no_discriminator() {
        // No common const fields — properties merged, required is intersection (empty here)
        let schema = json!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": { "a": { "type": "string" } },
                    "required": ["a"]
                },
                {
                    "type": "object",
                    "properties": { "b": { "type": "integer" } },
                    "required": ["b"]
                }
            ]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "object");
        assert!(cleaned.get("oneOf").is_none());
        // required intersection is empty (a and b are not shared)
        assert!(cleaned.get("required").is_none());
        assert_eq!(cleaned["properties"]["a"]["type"], "string");
        assert_eq!(cleaned["properties"]["b"]["type"], "integer");
    }

    #[test]
    fn test_allof_merged_for_gemini() {
        let schema = json!({
            "allOf": [
                {
                    "type": "object",
                    "properties": { "a": { "type": "string" } },
                    "required": ["a"]
                },
                {
                    "type": "object",
                    "properties": { "b": { "type": "integer" } },
                    "required": ["b"]
                }
            ]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "object");
        assert!(cleaned.get("allOf").is_none());
        // allOf union: both required lists are merged
        let req = cleaned["required"].as_array().unwrap();
        assert!(req.contains(&json!("a")));
        assert!(req.contains(&json!("b")));
        assert_eq!(cleaned["properties"]["a"]["type"], "string");
        assert_eq!(cleaned["properties"]["b"]["type"], "integer");
    }

    #[test]
    fn test_allof_heterogeneous_key_merge() {
        // allOf with mixed types: one has type, another has enum constraint
        let schema = json!({
            "allOf": [
                { "type": "string" },
                { "enum": ["a", "b", "c"] }
            ]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "string");
        assert_eq!(cleaned["enum"], json!(["a", "b", "c"]));
        assert!(cleaned.get("allOf").is_none());
    }

    #[test]
    fn test_same_primitive_type_union_for_gemini() {
        let schema = json!({
            "oneOf": [
                { "type": "string", "enum": ["a", "b"] },
                { "type": "string", "enum": ["c", "d"] }
            ]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "string");
        let enums = cleaned["enum"].as_array().unwrap();
        assert!(enums.contains(&json!("a")));
        assert!(enums.contains(&json!("d")));
    }

    #[test]
    fn test_array_union_preserves_items() {
        // Union of array types should preserve items from the first variant
        let schema = json!({
            "oneOf": [
                { "type": "array", "items": { "type": "string" } },
                { "type": "array", "items": { "type": "number" } }
            ]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "array");
        assert_eq!(cleaned["items"]["type"], "string");
    }

    #[test]
    fn test_anthropic_cleans_constraint_keywords() {
        // Anthropic strategy should now strip the same keywords as Gemini
        let schema = json!({
            "type": "string",
            "minLength": 1,
            "maxLength": 100,
            "pattern": "^[a-z]+$",
            "description": "A string field"
        });

        let cleaned = SchemaCleanr::clean_for_anthropic(schema);

        assert!(cleaned.get("minLength").is_none());
        assert!(cleaned.get("maxLength").is_none());
        assert!(cleaned.get("pattern").is_none());
        assert_eq!(cleaned["type"], "string");
        assert_eq!(cleaned["description"], "A string field");
    }

    #[test]
    fn test_clean_nested_unknown_schema_keyword() {
        let schema = json!({
            "not": {
                "$ref": "#/$defs/Age"
            },
            "$defs": {
                "Age": {
                    "type": "integer",
                    "minimum": 0
                }
            }
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["not"]["type"], "integer");
        assert!(cleaned["not"].get("minimum").is_none());
    }

    #[test]
    fn clean_for_gemini_bails_on_conflicting_property_types() {
        let schema = json!({
            "anyOf": [
                {
                    "type": "object",
                    "properties": {
                        "value": { "type": "string" }
                    },
                    "required": ["value"]
                },
                {
                    "type": "object",
                    "properties": {
                        "value": { "type": "integer" }
                    },
                    "required": ["value"]
                }
            ]
        });
        let cleaned = SchemaCleanr::clean_for_gemini(schema);
        let obj = cleaned.as_object().unwrap();
        // Should NOT produce an anyOf (Gemini doesn't support it),
        // but should not silently merge conflicting types either.
        // Fallback picks the richest variant or builds a best-effort object.
        assert!(obj.get("anyOf").is_none(), "Gemini must not emit anyOf");
        assert_eq!(obj.get("type").unwrap(), "object");
    }

    #[test]
    fn clean_for_openai_preserves_type_with_allof() {
        let schema = json!({
            "type": "object",
            "allOf": [
                {
                    "properties": {
                        "name": { "type": "string" }
                    }
                },
                {
                    "properties": {
                        "age": { "type": "integer" }
                    }
                }
            ]
        });
        let cleaned = SchemaCleanr::clean_for_openai(schema);
        let obj = cleaned.as_object().unwrap();
        // type must be preserved — allOf is conjunctive, not disjunctive
        assert_eq!(obj.get("type").unwrap(), "object");
        assert!(obj.get("allOf").is_some());
    }
}
