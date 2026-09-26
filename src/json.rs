use std::{collections::HashSet, fmt};

use anyhow::{Result, anyhow, bail};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};

const NUMBER_MARKER: &str = "$serde_json::private::Number";

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

pub fn contains_any(document: &Value, protected: &[Value]) -> bool {
    match document {
        Value::Array(values) => values.iter().any(|value| contains_any(value, protected)),
        Value::Object(values) => values.values().any(|value| contains_any(value, protected)),
        Value::String(candidate) => protected.iter().any(|value| match value {
            Value::String(secret) => !secret.is_empty() && candidate.contains(secret),
            _ => false,
        }),
        scalar => protected.contains(scalar),
    }
}

pub fn removable_pointers(document: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    visit_containers(document, String::new(), &mut paths);
    paths
}

fn visit_containers(value: &Value, pointer: String, paths: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_pointer = format!("{pointer}/{}", escape_pointer(key));
                paths.push(child_pointer.clone());
                visit_containers(child, child_pointer, paths);
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                let child_pointer = format!("{pointer}/{index}");
                paths.push(child_pointer.clone());
                visit_containers(child, child_pointer, paths);
            }
        }
        _ => {}
    }
}

pub fn remove_pointer(document: &mut Value, pointer: &str) -> Result<()> {
    let segments = decode_pointer(pointer)?;
    let (last, parents) = segments
        .split_last()
        .ok_or_else(|| anyhow!("the document root cannot be removed"))?;
    let mut parent = document;
    for segment in parents {
        parent = match parent {
            Value::Object(object) => object
                .get_mut(segment)
                .ok_or_else(|| anyhow!("candidate path became invalid"))?,
            Value::Array(array) => array
                .get_mut(parse_index(segment)?)
                .ok_or_else(|| anyhow!("candidate path became invalid"))?,
            _ => bail!("candidate path became invalid"),
        };
    }
    match parent {
        Value::Object(object) => object
            .remove(last)
            .map(|_| ())
            .ok_or_else(|| anyhow!("candidate path became invalid")),
        Value::Array(array) => {
            let index = parse_index(last)?;
            if index >= array.len() {
                bail!("candidate path became invalid");
            }
            array.remove(index);
            Ok(())
        }
        _ => bail!("candidate path became invalid"),
    }
}

fn parse_index(segment: &str) -> Result<usize> {
    anyhow::ensure!(
        segment == "0" || !segment.starts_with('0'),
        "invalid array index"
    );
    segment.parse().map_err(|_| anyhow!("invalid array index"))
}

fn decode_pointer(pointer: &str) -> Result<Vec<String>> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    anyhow::ensure!(pointer.starts_with('/'), "invalid JSON Pointer");
    pointer[1..]
        .split('/')
        .map(|segment| {
            let mut decoded = String::with_capacity(segment.len());
            let mut chars = segment.chars();
            while let Some(ch) = chars.next() {
                if ch != '~' {
                    decoded.push(ch);
                    continue;
                }
                match chars.next() {
                    Some('0') => decoded.push('~'),
                    Some('1') => decoded.push('/'),
                    _ => bail!("invalid JSON Pointer escape"),
                }
            }
            Ok(decoded)
        })
        .collect()
}

fn escape_pointer(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}
