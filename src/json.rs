use std::{collections::HashSet, fmt};

use anyhow::{Result, anyhow};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};

const NUMBER_MARKER: &str = "$serde_json::private::Number";

/// A protected value plus the rule used to detect it in a candidate.
///
/// `Exact` (the default) requires a protected string to disappear completely: it
/// must not survive as the same whole string anywhere in the document. A
/// non-string value is only guarded at the location you declared, because a bare
/// number or boolean identifies nobody and matching it document-wide would make
/// ordinary documents unusable.
///
/// `Contains` is the strict rule for real secrets: the value must also not appear
/// inside a longer string, as an object key, or — for non-strings — anywhere in
/// the document.
#[derive(Clone, Debug, PartialEq)]
pub struct Protected {
    pub value: Value,
    /// Reject the value embedded in a longer string or used as an object key.
    pub substring: bool,
    /// Compare a non-string value against the whole document, not just its
    /// declared location.
    pub scan_everywhere: bool,
}

impl Protected {
    /// A default (`exact`) protection rule.
    pub fn new(value: Value) -> Self {
        Self {
            value,
            substring: false,
            scan_everywhere: false,
        }
    }

    /// The strict (`contains`) rule. Always applied to the review bundle, which
    /// is Ghostcase's own output.
    pub fn strict(value: Value) -> Self {
        Self {
            value,
            substring: true,
            scan_everywhere: true,
        }
    }

    /// Selects the strict or the default rule.
    pub fn with_mode(value: Value, strict: bool) -> Self {
        if strict {
            Self::strict(value)
        } else {
            Self::new(value)
        }
    }

    fn matches(&self, candidate: &Value) -> bool {
        match (&self.value, candidate) {
            (Value::String(secret), Value::String(text)) => {
                if self.substring {
                    !secret.is_empty() && text.contains(secret.as_str())
                } else {
                    text == secret
                }
            }
            _ => self.scan_everywhere && candidate == &self.value,
        }
    }

    /// Object keys are only inspected in the strict `contains` mode.
    fn matches_key(&self, key: &str) -> bool {
        self.substring
            && matches!(&self.value, Value::String(secret) if !secret.is_empty() && key.contains(secret.as_str()))
    }
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ValueVisitor)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value with no duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_i128<E>(self, value: i128) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_i128(value)
            .map(|number| StrictValue(Value::Number(number)))
            .ok_or_else(|| E::custom("integer is out of range"))
    }

    fn visit_u128<E>(self, value: u128) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_u128(value)
            .map(|number| StrictValue(Value::Number(number)))
            .ok_or_else(|| E::custom("integer is out of range"))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(|number| StrictValue(Value::Number(number)))
            .ok_or_else(|| E::custom("non-finite number"))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(StrictValue(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        let mut seen = HashSet::new();
        let Some(first_key) = object.next_key::<String>()? else {
            return Ok(StrictValue(Value::Object(values)));
        };
        if first_key == NUMBER_MARKER {
            return parse_arbitrary_number(first_key, &mut object);
        }
        seen.insert(first_key.clone());
        let StrictValue(first_value) = object.next_value()?;
        values.insert(first_key, first_value);
        while let Some(key) = object.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom("duplicate object key"));
            }
            let StrictValue(value) = object.next_value()?;
            values.insert(key, value);
        }
        Ok(StrictValue(Value::Object(values)))
    }
}

fn parse_arbitrary_number<'de, A>(
    _marker: String,
    object: &mut A,
) -> std::result::Result<StrictValue, A::Error>
where
    A: MapAccess<'de>,
{
    let encoded: String = object.next_value()?;
    if object.next_key::<de::IgnoredAny>()?.is_some() {
        return Err(de::Error::custom("invalid arbitrary-precision number"));
    }
    let number = serde_json::from_str::<Number>(&encoded)
        .map_err(|_| de::Error::custom("invalid arbitrary-precision number"))?;
    Ok(StrictValue(Value::Number(number)))
}

pub fn parse(bytes: &[u8]) -> Result<Value> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let StrictValue(value) = StrictValue::deserialize(&mut deserializer)
        .map_err(|_| anyhow!("invalid JSON, duplicate key, or unsupported number"))?;
    deserializer
        .end()
        .map_err(|_| anyhow!("trailing data after JSON value"))?;
    Ok(value)
}

pub fn is_scalar(value: &Value) -> bool {
    !value.is_array() && !value.is_object()
}

pub fn contains_protected(document: &Value, protected: &[Protected]) -> bool {
    match document {
        Value::Array(values) => values
            .iter()
            .any(|value| contains_protected(value, protected)),
        Value::Object(map) => map.iter().any(|(key, value)| {
            contains_protected(value, protected)
                || protected.iter().any(|item| item.matches_key(key))
        }),
        scalar => protected.iter().any(|item| item.matches(scalar)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn secret(value: &str) -> Protected {
        Protected::new(json!(value))
    }

    fn secret_anywhere(value: &str) -> Protected {
        Protected::strict(json!(value))
    }

    #[test]
    fn parses_and_preserves_key_order() {
        let parsed = parse(br#"{"zebra":1,"alpha":2,"middle":3}"#).expect("valid JSON");
        let keys: Vec<_> = parsed.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["zebra", "alpha", "middle"]);
    }

    #[test]
    fn preserves_number_spelling_beyond_64_bits() {
        let huge = "1234567890123456789012345678901234567890";
        let parsed = parse(format!("{{\"n\":{huge}}}").as_bytes()).expect("valid JSON");
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            format!("{{\"n\":{huge}}}")
        );
    }

    #[test]
    fn rejects_duplicate_object_keys() {
        let error = parse(br#"{"records":[],"records":[]}"#).expect_err("must be rejected");
        assert!(error.to_string().contains("duplicate key"));
    }

    #[test]
    fn rejects_trailing_data() {
        let error = parse(br#"{"a":1} {"b":2}"#).expect_err("must be rejected");
        assert!(error.to_string().contains("trailing data"));
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse(b"{").is_err());
        assert!(parse(b"").is_err());
    }

    #[test]
    fn exact_mode_ignores_unrelated_prefix_overlapping_identifiers() {
        // The regression that made Ghostcase unusable on sequential identifiers:
        // protecting `user-1` must not be treated as leaking into `user-10`.
        let document = json!({ "records": [{ "id": "user-10" }, { "id": "user-100" }] });
        assert!(!contains_protected(&document, &[secret("user-1")]));
    }

    #[test]
    fn exact_mode_still_rejects_the_value_itself() {
        let document = json!({ "records": [{ "id": "user-1" }] });
        assert!(contains_protected(&document, &[secret("user-1")]));
    }

    #[test]
    fn exact_mode_rejects_the_same_value_in_a_second_location() {
        let document = json!({ "a": "x", "b": { "c": "user-1" } });
        assert!(contains_protected(&document, &[secret("user-1")]));
    }

    #[test]
    fn contains_mode_rejects_the_value_inside_a_longer_string() {
        let document = json!({ "note": "archive-customer-user-secret-export" });
        assert!(contains_protected(
            &document,
            &[secret_anywhere("customer-user-secret")]
        ));
    }

    #[test]
    fn contains_mode_also_inspects_object_keys() {
        let document = json!({ "prefix-customer-user-secret-suffix": 1 });
        assert!(contains_protected(
            &document,
            &[secret_anywhere("customer-user-secret")]
        ));
        assert!(!contains_protected(
            &document,
            &[secret("customer-user-secret")]
        ));
    }

    #[test]
    fn exact_mode_leaves_non_string_values_at_their_declared_location() {
        // Protecting a bare `true` or `42` must not make an ordinary document
        // unusable just because the same literal appears elsewhere.
        let flag = Protected::new(json!(true));
        assert!(!contains_protected(
            &json!({ "a": { "debug": true }, "b": { "verbose": true } }),
            std::slice::from_ref(&flag)
        ));
        let number = Protected::new(json!(42));
        assert!(!contains_protected(
            &json!({ "limit": 42, "seen": 1425 }),
            std::slice::from_ref(&number)
        ));
    }

    #[test]
    fn contains_mode_scans_non_string_values_document_wide() {
        // Opting into the strict rule for a sensitive number has to mean the
        // number is checked everywhere.
        let account = Protected::strict(json!(12_345_678_901_u64));
        assert!(contains_protected(
            &json!({ "a": { "owner": 12_345_678_901_u64 } }),
            std::slice::from_ref(&account)
        ));
        // Equality only, never a substring search over unrelated numbers.
        assert!(!contains_protected(
            &json!({ "n": 1_234_567_890_123_u64 }),
            std::slice::from_ref(&account)
        ));
    }

    #[test]
    fn booleans_never_match_a_similar_looking_string() {
        let yes = Protected::new(json!(true));
        assert!(!contains_protected(
            &json!({ "flag": "true" }),
            std::slice::from_ref(&yes)
        ));
    }

    #[test]
    fn an_empty_protected_string_never_matches_everything() {
        // Otherwise every candidate would be rejected as leaking.
        assert!(!contains_protected(
            &json!({ "a": "anything" }),
            &[secret_anywhere("")]
        ));
    }

    #[test]
    fn scalars_are_identified_without_treating_containers_as_scalars() {
        assert!(is_scalar(&json!(1)));
        assert!(is_scalar(&json!("s")));
        assert!(is_scalar(&json!(null)));
        assert!(!is_scalar(&json!([1])));
        assert!(!is_scalar(&json!({})));
    }
}
