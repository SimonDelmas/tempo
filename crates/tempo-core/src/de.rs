//! Désérialisation tolérante des réponses du modèle (équivalent des validateurs Pydantic) :
//! une valeur absente, nulle ou d'un type inattendu est ramenée à une valeur par défaut
//! plutôt que de faire échouer toute la réponse.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::util::iso_date;
use crate::vault::value_str;

fn scalar(value: Value) -> Option<String> {
    match value {
        Value::Null | Value::Array(_) | Value::Object(_) => None,
        other => Some(value_str(&other)),
    }
}

/// Texte ; `null` devient une chaîne vide.
pub fn string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<Value>::deserialize(d)?.and_then(scalar).unwrap_or_default())
}

/// Texte facultatif ; une chaîne vide devient `None`.
pub fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<Value>::deserialize(d)?.and_then(scalar).filter(|s| !s.is_empty()))
}

/// Liste de textes ; `null` devient une liste vide.
pub fn string_list<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::Array(items)) => items.into_iter().filter_map(scalar).collect(),
        Some(other) => scalar(other).into_iter().collect(),
        None => Vec::new(),
    })
}

/// Date AAAA-MM-JJ valide, sinon `None`.
pub fn opt_date<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(opt_string(d)?.and_then(|s| iso_date(&s)).map(|date| date.to_string()))
}

/// Nombre, éventuellement écrit sous forme de texte ; 0 à défaut.
pub fn number<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().replace(',', ".").parse().unwrap_or(0.0),
        _ => 0.0,
    })
}

/// Booléen, vrai par défaut.
pub fn bool_true<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(!matches!(Option::<Value>::deserialize(d)?, Some(Value::Bool(false))))
}

pub fn yes() -> bool {
    true
}
