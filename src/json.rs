//! Reading someone else's JSON: an API's answer, a tool's dump, a file
//! another program writes.
//!
//! **Read field by field, never deserialised into a struct.**
//! `#[serde(default)]` covers a field that is *absent*; one that is present and
//! `null` fails the whole struct, and a struct is one row of a list: a single
//! Sentry issue with a `null` culprit emptied the whole page. A shape that
//! grows a field, or writes `null` in one, must not lose the fields we read —
//! so each is taken on its own, and one that is missing, `null` or of another
//! type reads as empty. A field without which the row means nothing is
//! demanded with [`string`] or [`number`] and `?`.

use serde_json::Value;

/// A text field, when there is one: absent, `null` and not-a-string are
/// `None`.
pub fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// A text field: absent, `null` and not-a-string all read as empty.
pub fn text(value: &Value, key: &str) -> String {
    string(value, key).unwrap_or_default().to_string()
}

/// A list field: absent, `null` and not-a-list all read as empty.
pub fn items<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// A number, written as one or as a **string** — Sentry writes its counts as
/// strings in the issue list and as numbers elsewhere: both are read,
/// otherwise half the answers fail.
pub fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

/// A number field, absent, `null` or unreadable reading as zero.
pub fn count(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(number).unwrap_or(0)
}

/// A boolean field, absent or `null` reading as false.
pub fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absent, `null` and of another type read the same: as nothing.
    #[test]
    fn a_missing_field_and_a_null_one_read_alike() {
        let row: Value = serde_json::from_str(
            r#"{"title": "Boom", "culprit": null, "count": "12", "users": 3,
                "tags": null, "inApp": true, "odd": 4}"#,
        )
        .expect("JSON");
        assert_eq!(text(&row, "title"), "Boom");
        assert_eq!(text(&row, "culprit"), "");
        assert_eq!(text(&row, "absent"), "");
        assert_eq!(text(&row, "odd"), "");
        assert_eq!(string(&row, "culprit"), None);
        assert!(items(&row, "tags").is_empty());
        assert_eq!(count(&row, "count"), 12);
        assert_eq!(count(&row, "users"), 3);
        assert_eq!(count(&row, "title"), 0);
        assert!(flag(&row, "inApp"));
        assert!(!flag(&row, "culprit"));
    }
}
