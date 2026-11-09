//! Catalogue d'échange Microsoft Rewards (cartes cadeaux, monnaies de jeux…).
//!
//! La page rewards.bing.com/redeem/shop est rendue par Next.js : les articles sont dans les
//! blocs `self.__next_f.push([1,"…"])` du HTML, sous la forme d'un objet JSON
//! `{"balance":…,"isSignedIn":…,"items":[…]}`. La page est publique ; avec la session de
//! l'utilisateur, les prix tiennent compte de ses réductions.

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, Runtime};

use crate::rewards::{session_cookie_header, FetchError};

const SHOP_URL: &str = "https://rewards.bing.com/redeem/shop";
const REWARDS_ORIGIN: &str = "https://rewards.bing.com";
const FLIGHT_MARKER: &str = "self.__next_f.push([1,";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogItem {
    pub id: String,
    pub title: String,
    pub description: String,
    pub image_url: Option<String>,
    pub price: i64,
    pub original_price: i64,
    pub url: String,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub items: Vec<CatalogItem>,
    pub signed_in: bool,
}

pub async fn fetch<R: Runtime>(
    app: &AppHandle<R>,
    client: &reqwest::Client,
) -> Result<Catalog, FetchError> {
    let mut request = client
        .get(SHOP_URL)
        .header("Accept", "text/html")
        .header("Accept-Language", "fr-FR,fr;q=0.9,en;q=0.8");
    match session_cookie_header(app).await {
        Ok(cookie) => request = request.header("Cookie", cookie),
        Err(FetchError::NotLoggedIn) => {}
        Err(err) => return Err(err),
    }
    let response = request
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    if !response.status().is_success() {
        return Err(FetchError::Unexpected(format!(
            "catalogue : statut HTTP {}",
            response.status()
        )));
    }
    let html = response
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    if let Ok(dir) = app.path().app_log_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("rewards-catalog.html"), &html);
    }
    let catalog = parse(&html);
    if catalog.items.is_empty() {
        return Err(FetchError::Unexpected(
            "aucun article trouvé dans la page du catalogue".into(),
        ));
    }
    Ok(catalog)
}

/// Reconstitue le texte des blocs Next.js, chacun étant une chaîne JSON.
fn flight_text(html: &str) -> String {
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

fn parse(html: &str) -> Catalog {
    let text = flight_text(html);
    let mut items: Vec<CatalogItem> = Vec::new();
    let mut signed_in = false;
    let mut rest = text.as_str();
    while let Some(index) = rest.find("{\"balance\":") {
        rest = &rest[index..];
        if let Some(Ok(block)) = serde_json::Deserializer::from_str(rest)
            .into_iter::<Value>()
            .next()
        {
            signed_in |= block["isSignedIn"].as_bool() == Some(true);
            for item in block["items"].as_array().into_iter().flatten() {
                if let Some(item) = catalog_item(item) {
                    if !items.iter().any(|i| i.id == item.id) {
                        items.push(item);
                    }
                }
            }
        }
        rest = &rest[1..];
    }
    items.sort_by_key(|i| i.price);
    Catalog { items, signed_in }
}

fn catalog_item(item: &Value) -> Option<CatalogItem> {
    let id = item["id"].as_str().filter(|s| !s.is_empty())?.to_string();
    let title = item["fullTitle"]
        .as_str()
        .or_else(|| item["title"].as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    let price = item["price"].as_i64()?;
    let href = item["href"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("/redeem/sku/{id}"));
    Some(CatalogItem {
        description: item["description"].as_str().unwrap_or_default().to_string(),
        image_url: item["imageUrl"]
            .as_str()
            .filter(|u| u.starts_with("https://"))
            .map(str::to_string),
        original_price: item["originalPrice"].as_i64().unwrap_or(price),
        url: if href.starts_with("https://") {
            href
        } else {
            format!("{REWARDS_ORIGIN}{href}")
        },
        disabled: item["isDisabled"].as_bool() == Some(true),
        id,
        title,
        price,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extrait réduit de la vraie page (rewards.bing.com/redeem/fr/shop, novembre 2026).
    const SAMPLE: &str = r#"<script>self.__next_f.push([1,"67:{\"balance\":0,\"isSignedIn\":false,\"items\":[{\"id\":\"000403000090\",\"title\":\"Sea of Thieves: pack de pièces anciennes de l’Outsetter\",\"description\":\"Pièces anciennes\",\"imageUrl\":\"https://bing.com/th?id=OMR.Redeem.SoT.jpg&pid=Rewards\",\"price\":1700,\"originalPrice\":1700,\"href\":\"/redeem/sku/000403000090\",\"isDisabled\":\"$undefined\",\"fullTitle\":\"Sea of Thieves: pack de pièces anciennes de l’Outsetter\"},{\"id\":\"000403000093\",\"title\":\"Minecoins Minecraft\",\"price\":2500,\"href\":\"/redeem/sku/000403000093\"}]}\n"])</script><script>self.__next_f.push([1,"68:\"$undefined\"\n"])</script>"#;

    #[test]
    fn parses_catalog_items_from_next_flight_data() {
        let catalog = parse(SAMPLE);
        assert!(!catalog.signed_in);
        assert_eq!(catalog.items.len(), 2);
        let sot = &catalog.items[0];
        assert_eq!(sot.price, 1700);
        assert!(sot.title.starts_with("Sea of Thieves"));
        assert_eq!(sot.url, "https://rewards.bing.com/redeem/sku/000403000090");
        assert_eq!(
            sot.image_url.as_deref(),
            Some("https://bing.com/th?id=OMR.Redeem.SoT.jpg&pid=Rewards")
        );
        assert!(!sot.disabled);
        assert_eq!(catalog.items[1].title, "Minecoins Minecraft");
    }

    #[test]
    fn page_without_items_gives_empty_catalog() {
        assert!(parse("<html>Se connecter</html>").items.is_empty());
    }
}
