//! Lecture des données intégrées aux pages Next.js de rewards.bing.com.
//!
//! Ces pages transportent leurs données dans des blocs `self.__next_f.push([1,"…"])`, chacun
//! étant une chaîne JSON. Une fois recollés, ces blocs contiennent des objets JSON qu'on
//! retrouve à partir d'une clé caractéristique.

use serde_json::{Map, Value};

const FLIGHT_MARKER: &str = "self.__next_f.push([1,";

/// Reconstitue le texte des blocs Next.js d'une page HTML.
pub fn text(html: &str) -> String {
    let mut text = String::new();
    let mut rest = html;
    while let Some(index) = rest.find(FLIGHT_MARKER) {
        rest = &rest[index + FLIGHT_MARKER.len()..];
        if let Some(Ok(chunk)) = serde_json::Deserializer::from_str(rest)
            .into_iter::<String>()
            .next()
        {
            text.push_str(&chunk);
        }
    }
    text
}

/// Toutes les valeurs JSON qui suivent `"key":` dans le texte.
pub fn values_after_key(text: &str, key: &str) -> Vec<Value> {
    let needle = format!("\"{key}\":");
    let mut values = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find(&needle) {
        rest = &rest[index + needle.len()..];
        if let Some(Ok(value)) = serde_json::Deserializer::from_str(rest)
            .into_iter::<Value>()
            .next()
        {
            values.push(value);
        }
    }
    values
}

/// Les objets JSON qui contiennent directement la clé `key`, dans l'ordre du texte.
///
/// Pour chaque occurrence de la clé, on remonte aux accolades précédentes jusqu'à trouver
/// l'objet qui la porte.
pub fn objects_with_key(text: &str, key: &str) -> Vec<Map<String, Value>> {
    const MAX_ATTEMPTS: usize = 40;
    let needle = format!("\"{key}\":");
    let mut objects = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find(&needle) {
        let at = from + found;
        from = at + needle.len();
        let mut end = at;
        for _ in 0..MAX_ATTEMPTS {
            let Some(start) = text[..end].rfind('{') else {
                break;
            };
            end = start;
            let parsed = serde_json::Deserializer::from_str(&text[start..])
                .into_iter::<Value>()
                .next();
            if let Some(Ok(Value::Object(object))) = parsed {
                if object.contains_key(key) {
                    objects.push(object);
                    break;
                }
            }
        }
    }
    objects
}

/// Contenu d'une ligne `id:valeur` du flux Next.js, utilisé pour suivre les références `$id`.
pub fn row(text: &str, id: &str) -> Option<Value> {
    let prefix = format!("{id}:");
    let start = if text.starts_with(&prefix) {
        0
    } else {
        text.find(&format!("\n{prefix}"))? + 1
    };
    serde_json::Deserializer::from_str(&text[start + prefix.len()..])
        .into_iter::<Value>()
        .next()?
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_chunks_and_finds_values() {
        let html = r#"<script>self.__next_f.push([1,"1:{\"items\":[{\"id\":\"a\"}],"])</script><script>self.__next_f.push([1,"\"balance\":12}\n"])</script>"#;
        let text = text(html);
        assert_eq!(text, "1:{\"items\":[{\"id\":\"a\"}],\"balance\":12}\n");
        assert_eq!(values_after_key(&text, "balance"), vec![Value::from(12)]);
        assert_eq!(values_after_key(&text, "items")[0][0]["id"], "a");
    }

    #[test]
    fn finds_enclosing_objects_and_rows() {
        let text = "1:{\"a\":{\"offerId\":\"x\",\"title\":\"T\"},\"b\":[{\"offerId\":\"y\"}]}\n80:[\"$\",\"img\",null,{\"alt\":\"Duolingo\"}]\n";
        let objects = objects_with_key(text, "offerId");
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0]["title"], "T");
        assert_eq!(objects[1]["offerId"], "y");
        assert_eq!(row(text, "80").unwrap()[3]["alt"], "Duolingo");
        assert_eq!(row(text, "1").unwrap()["b"][0]["offerId"], "y");
        assert!(row(text, "99").is_none());
    }
}
