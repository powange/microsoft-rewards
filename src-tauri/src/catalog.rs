//! Catalogue d'échange Microsoft Rewards (cartes cadeaux, monnaies de jeux…).
//!
//! La page rewards.bing.com/redeem/shop liste un article par produit. Les variantes d'un
//! produit (tailles de packs, montants) ne figurent que sur la page de l'article
//! (/redeem/sku/…) : chaque page d'article est donc lue aussi. Dans les deux cas, les articles
//! sont un tableau JSON `"items":[…]` dans les données Next.js de la page. Les pages sont
//! publiques ; avec la session de l'utilisateur, les prix tiennent compte de ses réductions.

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, Runtime};

use crate::flight;
use crate::rewards::{session_cookie_header, FetchError};

const SHOP_URL: &str = "https://rewards.bing.com/redeem/shop";
const REWARDS_ORIGIN: &str = "https://rewards.bing.com";

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
    let cookie = match session_cookie_header(app).await {
        Ok(cookie) => Some(cookie),
        Err(FetchError::NotLoggedIn) => None,
        Err(err) => return Err(err),
    };
    let html = get_page(client, SHOP_URL, cookie.as_deref()).await?;
    if let Ok(dir) = app.path().app_log_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("rewards-catalog.html"), &html);
    }
    let mut catalog = parse(&html);
    if catalog.items.is_empty() {
        return Err(FetchError::Unexpected(
            "aucun article trouvé dans la page du catalogue".into(),
        ));
    }

    // Pages d'articles lues en parallèle pour récupérer les variantes.
    let pages: Vec<_> = catalog
        .items
        .iter()
        .map(|item| {
            let client = client.clone();
            let cookie = cookie.clone();
            let url = item.url.clone();
            tauri::async_runtime::spawn(
                async move { get_page(&client, &url, cookie.as_deref()).await },
            )
        })
        .collect();
    for page in pages {
        match page.await {
            Ok(Ok(html)) => merge(&mut catalog.items, parse(&html).items),
            Ok(Err(err)) => log::warn!("Page d'article illisible : {err}"),
            Err(err) => log::warn!("Lecture d'une page d'article interrompue : {err}"),
        }
    }
    // Les articles à montant libre ont un titre modèle (« d'une valeur de {0} € ») :
    // leurs montants fixes sont déjà listés comme variantes.
    catalog.items.retain(|item| !item.title.contains("{0}"));
    catalog
        .items
        .sort_by(|a, b| a.price.cmp(&b.price).then(a.title.cmp(&b.title)));
    Ok(catalog)
}

async fn get_page(
    client: &reqwest::Client,
    url: &str,
    cookie: Option<&str>,
) -> Result<String, FetchError> {
    let mut request = client
        .get(url)
        .header("Accept", "text/html")
        .header("Accept-Language", "fr-FR,fr;q=0.9,en;q=0.8");
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    let response = request
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    if !response.status().is_success() {
        return Err(FetchError::Http(
            response.status().as_u16(),
            url.to_string(),
        ));
    }
    response
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))
}

fn merge(items: &mut Vec<CatalogItem>, new_items: Vec<CatalogItem>) {
    for item in new_items {
        if !items.iter().any(|i| i.id == item.id) {
            items.push(item);
        }
    }
}

fn parse(html: &str) -> Catalog {
    let text = flight::text(html);
    let mut items = Vec::new();
    for list in flight::values_after_key(&text, "items") {
        merge(
            &mut items,
            list.as_array()
                .into_iter()
                .flatten()
                .filter_map(catalog_item)
                .collect(),
        );
    }
    items.sort_by_key(|i| i.price);
    Catalog {
        items,
        signed_in: text.contains("\"isSignedIn\":true"),
    }
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
        // Sur une page d'article, le nom de la variante (« 150 pièces… ») décrit mieux l'offre.
        description: item["denominationTitle"]
            .as_str()
            .or_else(|| item["description"].as_str())
            .unwrap_or_default()
            .to_string(),
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

    /// Extrait réduit d'une page d'article (rewards.bing.com/redeem/sku/000403000123) :
    /// le bloc ne commence pas par "balance" et liste toutes les variantes du produit.
    const SKU_SAMPLE: &str = r#"<script>self.__next_f.push([1,"65:[\"$\",\"$L68\",null,{\"subscriptionPromise\":\"$@69\",\"balance\":0,\"isSignedIn\":false,\"userCountry\":\"fr\",\"items\":[{\"id\":\"000403000123\",\"title\":\"Sea of Thieves: pack de pièces anciennes de l’Outsetter\",\"price\":1700,\"href\":\"/redeem/sku/000403000123\",\"fullTitle\":\"Sea of Thieves - Pack de Réserve Secrète de l’Outsetter\",\"denominationTitle\":\"150 pièces - Le Pack Secret de l’Outsetter\"},{\"id\":\"000403000124\",\"title\":\"Sea of Thieves: pack de pièces anciennes de l’Outsetter\",\"price\":5100,\"href\":\"/redeem/sku/000403000124\",\"fullTitle\":\"Sea of Thieves - Pack de coffre perdu de l’Outsetter\"}]}]\n"])</script>"#;

    #[test]
    fn parses_variants_from_item_page() {
        let catalog = parse(SKU_SAMPLE);
        let titles: Vec<_> = catalog.items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Sea of Thieves - Pack de Réserve Secrète de l’Outsetter",
                "Sea of Thieves - Pack de coffre perdu de l’Outsetter"
            ]
        );
        assert_eq!(
            catalog.items[0].description,
            "150 pièces - Le Pack Secret de l’Outsetter"
        );
        assert_eq!(catalog.items[1].price, 5100);
    }

    #[test]
    fn page_without_items_gives_empty_catalog() {
        assert!(parse("<html>Se connecter</html>").items.is_empty());
    }
}
