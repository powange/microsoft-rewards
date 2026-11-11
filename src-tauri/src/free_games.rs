//! Jeux du catalogue Game Pass qui sont aussi gratuits sans abonnement (Fortnite, Aniimo,
//! Microsoft Solitaire Collection…). Ils comptent pour les packs mensuels de jeux Game Pass
//! sans rien acheter.
//!
//! Deux services publics du Store, sans connexion :
//! - `catalog.gamepass.com/sigls` : identifiants des jeux des listes « Tous les jeux PC » et
//!   « Tous les jeux de console » ;
//! - `displaycatalog.mp.microsoft.com` : fiche de chaque jeu (titre, jaquette, offres).
//!
//! Un jeu est retenu si son offre d'achat publique coûte 0 €. Sont écartées les offres à
//! 0 € réservées aux abonnés (elles exigent une « remédiation », l'abonnement) et les
//! versions d'essai.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

/// Listes Game Pass « Tous les jeux PC » et « Tous les jeux de console ».
const LISTS: [(&str, &str); 2] = [
    ("PC", "fdd9e2a7-0fee-49f6-ad69-4354098401ff"),
    ("Console", "f6f1f99f-9b49-4ccd-b3bf-4d9767a77f5e"),
];
const SIGL_URL: &str = "https://catalog.gamepass.com/sigls/v2?language=fr-fr&market=FR&id=";
const PRODUCTS_URL: &str = "https://displaycatalog.mp.microsoft.com/v7.0/products?market=FR&languages=fr-fr&MS-CV=RewardsAutoplay.1&bigIds=";
/// Le Store accepte 20 identifiants par requête.
const BATCH: usize = 20;
/// La liste change rarement : relue au plus une fois par jour.
const MAX_AGE: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeGame {
    pub id: String,
    pub title: String,
    pub image_url: Option<String>,
    /// « PC », « Console ».
    pub platforms: Vec<String>,
    /// Page du jeu sur le Store Xbox.
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeGames {
    pub games: Vec<FreeGame>,
    /// Jeux du catalogue examinés, et jeux dont le prix n'a pas pu être lu.
    pub checked: usize,
    pub unknown: usize,
}

static CACHE: Mutex<Option<(Instant, FreeGames)>> = Mutex::new(None);

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    let response = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("Store Microsoft injoignable : {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Store Microsoft : erreur {}",
            response.status().as_u16()
        ));
    }
    response.json().await.map_err(|e| e.to_string())
}

/// Liste des jeux gratuits, reprise du cache de moins d'un jour sauf si `force`.
pub async fn list(client: &reqwest::Client, force: bool) -> Result<FreeGames, String> {
    if !force {
        if let Some((at, games)) = CACHE.lock().unwrap().as_ref() {
            if at.elapsed() < MAX_AGE {
                return Ok(games.clone());
            }
        }
    }
    let games = fetch(client).await?;
    *CACHE.lock().unwrap() = Some((Instant::now(), games.clone()));
    Ok(games)
}

async fn fetch(client: &reqwest::Client) -> Result<FreeGames, String> {
    // Identifiant → plateformes où le jeu est dans le Game Pass.
    let mut platforms: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (platform, list) in LISTS {
        let json = get_json(client, &format!("{SIGL_URL}{list}")).await?;
        for item in json.as_array().into_iter().flatten().skip(1) {
            if let Some(id) = item.get("id").and_then(Value::as_str) {
                platforms
                    .entry(id.to_string())
                    .or_default()
                    .push(platform.to_string());
            }
        }
    }
    let ids: Vec<String> = platforms.keys().cloned().collect();

    // Fiches lues par lots, plusieurs lots à la fois.
    let tasks: Vec<_> = ids
        .chunks(BATCH)
        .map(|chunk| {
            let client = client.clone();
            let url = format!("{PRODUCTS_URL}{}", chunk.join(","));
            tauri::async_runtime::spawn(async move { get_json(&client, &url).await })
        })
        .collect();
    let mut games = Vec::new();
    let mut unknown = 0;
    for task in tasks {
        let json = task.await.map_err(|e| e.to_string())??;
        for product in json
            .get("Products")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match public_price(product) {
                Some(0.0) => {
                    if let Some(game) = free_game(product, &platforms) {
                        games.push(game);
                    }
                }
                Some(_) => {}
                None => unknown += 1,
            }
        }
    }
    // Un même jeu peut avoir deux fiches (versions PC et console) : une seule ligne.
    games.sort_by_key(|g| g.title.to_lowercase());
    games.dedup_by(|b, a| {
        if a.title.eq_ignore_ascii_case(&b.title) {
            for p in &b.platforms {
                if !a.platforms.contains(p) {
                    a.platforms.push(p.clone());
                }
            }
            true
        } else {
            false
        }
    });
    Ok(FreeGames {
        checked: ids.len(),
        unknown,
        games,
    })
}

/// Prix de l'offre d'achat publique la moins chère, hors versions d'essai et offres
/// réservées aux abonnés. `None` si aucune offre publique n'a de prix.
fn public_price(product: &Value) -> Option<f64> {
    product
        .get("DisplaySkuAvailabilities")?
        .as_array()?
        .iter()
        .filter(|sku| {
            sku.pointer("/Sku/Properties/IsTrial")
                .and_then(Value::as_bool)
                != Some(true)
        })
        .flat_map(|sku| {
            sku.get("Availabilities")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|av| {
            let purchase = av
                .get("Actions")
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().any(|x| x == "Purchase"));
            let subscribers_only =
                av.get("RemediationRequired").and_then(Value::as_bool) == Some(true);
            purchase && !subscribers_only
        })
        .filter_map(|av| av.pointer("/OrderManagementData/Price/ListPrice")?.as_f64())
        .reduce(f64::min)
}

fn free_game(product: &Value, platforms: &BTreeMap<String, Vec<String>>) -> Option<FreeGame> {
    let id = product.get("ProductId")?.as_str()?.to_string();
    let props = product.get("LocalizedProperties")?.get(0)?;
    let title = props.get("ProductTitle")?.as_str()?.to_string();
    let images = props.get("Images").and_then(Value::as_array);
    let image_url = ["Poster", "BoxArt", "Tile"].iter().find_map(|purpose| {
        images?
            .iter()
            .find(|i| i.get("ImagePurpose").and_then(Value::as_str) == Some(purpose))?
            .get("Uri")?
            .as_str()
            .map(|u| format!("https:{u}?w=180"))
    });
    Some(FreeGame {
        url: format!("https://www.xbox.com/fr-FR/games/store/x/{id}"),
        platforms: platforms.get(&id).cloned().unwrap_or_default(),
        id,
        title,
        image_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn product(skus: Value) -> Value {
        json!({"ProductId": "9X", "LocalizedProperties": [{"ProductTitle": "Jeu"}], "DisplaySkuAvailabilities": skus})
    }

    #[test]
    fn prix_public_hors_offres_abonnes_et_essais() {
        // Jeu payant : l'offre à 0 € est réservée aux abonnés.
        let paid = product(json!([{"Sku": {}, "Availabilities": [
            {"Actions": ["Purchase"], "OrderManagementData": {"Price": {"ListPrice": 19.99}}},
            {"Actions": ["Purchase"], "RemediationRequired": true, "OrderManagementData": {"Price": {"ListPrice": 0.0}}}
        ]}]));
        assert_eq!(public_price(&paid), Some(19.99));
        // Version d'essai gratuite : ignorée.
        let trial = product(json!([
            {"Sku": {"Properties": {"IsTrial": true}}, "Availabilities": [
                {"Actions": ["Purchase"], "OrderManagementData": {"Price": {"ListPrice": 0.0}}}]},
            {"Sku": {}, "Availabilities": [
                {"Actions": ["Purchase"], "OrderManagementData": {"Price": {"ListPrice": 29.99}}}]}
        ]));
        assert_eq!(public_price(&trial), Some(29.99));
        // Jeu gratuit pour tous.
        let free = product(json!([{"Sku": {}, "Availabilities": [
            {"Actions": ["Purchase"], "OrderManagementData": {"Price": {"ListPrice": 0.0}}}]}]));
        assert_eq!(public_price(&free), Some(0.0));
        assert_eq!(public_price(&product(json!([]))), None);
    }
}
