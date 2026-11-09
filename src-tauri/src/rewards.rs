//! Lecture du tableau de bord Microsoft Rewards.
//!
//! La connexion se fait dans une fenêtre WebView2 sur la vraie page Microsoft. Les cookies de
//! cette session sont ensuite lus depuis le magasin de cookies de WebView2 et envoyés à
//! l'API JSON interne du site rewards.bing.com (non documentée, susceptible de changer).

use chrono::{DateTime, Local};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, Runtime};
use url::Url;

const REWARDS_ORIGIN: &str = "https://rewards.bing.com/";
const USER_INFO_URL: &str = "https://rewards.bing.com/api/getuserinfo?type=1";
const QUEST_URL: &str = "https://rewards.bing.com/earn/quest/";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 Edg/142.0.0.0";

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("not_logged_in")]
    NotLoggedIn,
    #[error("Erreur réseau : {0}")]
    Network(String),
    #[error("Réponse inattendue de Microsoft Rewards : {0}")]
    Unexpected(String),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counter {
    pub label: String,
    pub progress: i64,
    pub max: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub id: String,
    pub title: String,
    pub description: String,
    pub points: i64,
    pub complete: bool,
    pub locked: bool,
    pub url: Option<String>,
    /// Activité liée à Xbox ou au Game Pass, mise en avant dans l'interface.
    pub xbox: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PunchCard {
    /// Identifiant de l'offre, qui contient souvent le nom du jeu
    /// (ex. `WW_evergreen_pcparent_SeaofThieves_CeruleanSheenCutlass_punchcard`).
    pub id: String,
    pub title: String,
    pub description: String,
    pub points: i64,
    pub done: usize,
    pub total: usize,
    pub complete: bool,
    /// Page de la quête sur rewards.bing.com.
    pub url: Option<String>,
    pub xbox: bool,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub title: String,
    pub points: i64,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewardsSummary {
    pub fetched_at: DateTime<Local>,
    pub available_points: i64,
    pub lifetime_points: i64,
    pub level: Option<String>,
    pub level_progress: Option<i64>,
    pub level_progress_max: Option<i64>,
    pub country: Option<String>,
    pub searches: Vec<Counter>,
    pub daily_set: Vec<Activity>,
    pub more: Vec<Activity>,
    pub punch_cards: Vec<PunchCard>,
}

impl RewardsSummary {
    /// Points de recherche encore disponibles aujourd'hui.
    pub fn search_points_left(&self) -> i64 {
        self.searches
            .iter()
            .map(|c| (c.max - c.progress).max(0))
            .sum()
    }

    pub fn daily_set_left(&self) -> usize {
        self.daily_set
            .iter()
            .filter(|a| !a.complete && !a.locked)
            .count()
    }
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("client HTTP")
}

/// Cookies de la session Microsoft, lus depuis WebView2.
///
/// L'API cookies de WebView2 se bloque si on l'appelle depuis le thread principal :
/// la lecture se fait donc sur un thread dédié.
pub async fn session_cookie_header<R: Runtime>(app: &AppHandle<R>) -> Result<String, FetchError> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| FetchError::Unexpected("fenêtre principale introuvable".into()))?;
    let url = Url::parse(REWARDS_ORIGIN).unwrap();
    let cookies = tauri::async_runtime::spawn_blocking(move || window.cookies_for_url(url))
        .await
        .map_err(|e| FetchError::Unexpected(e.to_string()))?
        .map_err(|e| FetchError::Unexpected(e.to_string()))?;
    if cookies.is_empty() {
        return Err(FetchError::NotLoggedIn);
    }
    Ok(cookies
        .iter()
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect::<Vec<_>>()
        .join("; "))
}

pub async fn fetch<R: Runtime>(
    app: &AppHandle<R>,
    client: &reqwest::Client,
) -> Result<RewardsSummary, FetchError> {
    let cookie = session_cookie_header(app).await?;
    let response = client
        .get(USER_INFO_URL)
        .header("Cookie", cookie)
        .header("Accept", "application/json")
        .header("Accept-Language", "fr-FR,fr;q=0.9,en;q=0.8")
        .header("Referer", REWARDS_ORIGIN)
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    save_diagnostic(app, &body);

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(FetchError::NotLoggedIn);
    }
    if !status.is_success() {
        return Err(FetchError::Unexpected(format!("statut HTTP {status}")));
    }
    if body.trim_start().starts_with('<') {
        // Page HTML de connexion au lieu du JSON : la session n'est pas (ou plus) valide.
        return Err(FetchError::NotLoggedIn);
    }
    let json: Value =
        serde_json::from_str(&body).map_err(|e| FetchError::Unexpected(e.to_string()))?;
    parse_dashboard(&json)
}

/// Garde la dernière réponse brute pour diagnostiquer un changement de format côté Microsoft.
fn save_diagnostic<R: Runtime>(app: &AppHandle<R>, body: &str) {
    if let Ok(dir) = app.path().app_log_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("rewards-getuserinfo.json"), body);
    }
}

fn parse_dashboard(json: &Value) -> Result<RewardsSummary, FetchError> {
    let dashboard = json
        .get("dashboard")
        .filter(|d| d.is_object())
        .ok_or(FetchError::NotLoggedIn)?;
    let status = &dashboard["userStatus"];
    if status.get("isRewardsUser").and_then(Value::as_bool) == Some(false) {
        return Err(FetchError::NotLoggedIn);
    }

    let level_info = &status["levelInfo"];
    let level = str_field(level_info, "activeLevelName")
        .or_else(|| str_field(level_info, "activeLevel"))
        .filter(|s| !s.is_empty());

    let counters = &status["counters"];
    let searches = [
        ("pcSearch", "Recherches PC"),
        ("mobileSearch", "Recherches mobile"),
    ]
    .into_iter()
    .filter_map(|(key, label)| {
        let items = counters.get(key)?.as_array()?;
        if items.is_empty() {
            return None;
        }
        Some(Counter {
            label: label.into(),
            progress: items.iter().map(|i| int_field(i, "pointProgress")).sum(),
            max: items.iter().map(|i| int_field(i, "pointProgressMax")).sum(),
        })
    })
    .collect();

    // Le jeu d'activités du jour est indexé par date au format MM/JJ/AAAA.
    let today_key = Local::now().format("%m/%d/%Y").to_string();
    let daily_set = dashboard["dailySetPromotions"]
        .get(&today_key)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(activity).collect())
        .unwrap_or_default();

    let mut more: Vec<Activity> = Vec::new();
    for key in [
        "morePromotions",
        "morePromotionsWithoutPromotionalItems",
        "promotionalItems",
    ] {
        if let Some(items) = dashboard[key].as_array() {
            for item in items.iter().filter_map(activity) {
                if !more.iter().any(|a| a.id == item.id) {
                    more.push(item);
                }
            }
        }
    }

    let punch_cards = dashboard["punchCards"]
        .as_array()
        .map(|cards| cards.iter().filter_map(punch_card).collect())
        .unwrap_or_default();

    Ok(RewardsSummary {
        fetched_at: Local::now(),
        available_points: int_field(status, "availablePoints"),
        lifetime_points: int_field(status, "lifetimePoints"),
        level,
        level_progress: level_info.get("progress").and_then(as_int),
        level_progress_max: level_info.get("progressMax").and_then(as_int),
        country: str_field(&dashboard["userProfile"]["attributes"], "country"),
        searches,
        daily_set,
        more,
        punch_cards,
    })
}

fn activity(item: &Value) -> Option<Activity> {
    let title = str_field(item, "title")?;
    let points = int_field(item, "pointProgressMax");
    if title.is_empty() || points <= 0 {
        return None;
    }
    let description = str_field(item, "description").unwrap_or_default();
    let id = str_field(item, "offerId")
        .or_else(|| str_field(item, "name"))
        .unwrap_or_else(|| title.clone());
    let xbox = is_xbox(&[&title, &description, &id]);
    Some(Activity {
        id,
        description,
        points,
        complete: item["complete"].as_bool().unwrap_or(false),
        locked: str_field(item, "exclusiveLockedFeatureStatus")
            .is_some_and(|s| s.eq_ignore_ascii_case("locked")),
        url: str_field(item, "destinationUrl").filter(|u| u.starts_with("https://")),
        xbox,
        title,
    })
}

fn punch_card(card: &Value) -> Option<PunchCard> {
    let parent = &card["parentPromotion"];
    let title = str_field(parent, "title").filter(|t| !t.is_empty())?;
    let children = card["childPromotions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let description = str_field(parent, "description").unwrap_or_default();
    let id = str_field(parent, "offerId")
        .or_else(|| str_field(parent, "name"))
        .or_else(|| str_field(card, "name"))
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| title.clone());
    let xbox = is_xbox(&[&title, &description, &id]);
    let url = if id != title {
        let mut url = Url::parse(QUEST_URL).unwrap();
        url.path_segments_mut().unwrap().pop_if_empty().push(&id);
        Some(url.to_string())
    } else {
        str_field(parent, "destinationUrl").filter(|u| u.starts_with("https://"))
    };
    let steps: Vec<Step> = children
        .iter()
        .filter_map(|child| {
            Some(Step {
                title: str_field(child, "title").filter(|t| !t.is_empty())?,
                points: int_field(child, "pointProgressMax"),
                complete: child["complete"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    Some(PunchCard {
        points: int_field(parent, "pointProgressMax"),
        done: children
            .iter()
            .filter(|c| c["complete"].as_bool() == Some(true))
            .count(),
        total: children.len(),
        complete: parent["complete"].as_bool().unwrap_or(false),
        id,
        url,
        title,
        description,
        xbox,
        steps,
    })
}

fn is_xbox(texts: &[&str]) -> bool {
    texts.iter().any(|t| {
        let t = t.to_lowercase();
        t.contains("xbox") || t.contains("game pass") || t.contains("gamepass")
    })
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn as_int(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|f| f as i64))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn int_field(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(as_int).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_minimal_dashboard() {
        let today = Local::now().format("%m/%d/%Y").to_string();
        let json = json!({
            "dashboard": {
                "userStatus": {
                    "availablePoints": 12345,
                    "lifetimePoints": 50000,
                    "isRewardsUser": true,
                    "levelInfo": { "activeLevel": "Level2", "progress": 600, "progressMax": 500 },
                    "counters": {
                        "pcSearch": [
                            { "pointProgress": 30, "pointProgressMax": 90 },
                            { "pointProgress": 0, "pointProgressMax": 12 }
                        ]
                    }
                },
                "dailySetPromotions": {
                    today: [
                        { "offerId": "a", "title": "Quiz du jour", "pointProgressMax": 10, "complete": true },
                        { "offerId": "b", "title": "Sondage", "pointProgressMax": 10, "complete": false }
                    ]
                },
                "morePromotions": [
                    { "offerId": "x", "title": "Jouez sur Xbox", "pointProgressMax": 50, "complete": false },
                    { "offerId": "z", "title": "Sans points", "pointProgressMax": 0 }
                ],
                "punchCards": [{
                    "parentPromotion": {
                        "name": "WW_evergreen_pcparent_SeaofThieves_CeruleanSheenCutlass_punchcard",
                        "title": "Sabre Cerulean Sheen",
                        "pointProgressMax": 100
                    },
                    "childPromotions": [
                        { "title": "Jouez 15 min", "pointProgressMax": 0, "complete": true },
                        { "title": "Jouez encore", "complete": false }
                    ]
                }]
            }
        });
        let s = parse_dashboard(&json).unwrap();
        assert_eq!(s.available_points, 12345);
        assert_eq!(s.level.as_deref(), Some("Level2"));
        assert_eq!(s.search_points_left(), 72);
        assert_eq!(s.daily_set.len(), 2);
        assert_eq!(s.daily_set_left(), 1);
        assert_eq!(s.more.len(), 1);
        assert!(s.more[0].xbox);
        let card = &s.punch_cards[0];
        assert_eq!((card.done, card.total, card.steps.len()), (1, 2, 2));
        assert_eq!(
            card.url.as_deref(),
            Some("https://rewards.bing.com/earn/quest/WW_evergreen_pcparent_SeaofThieves_CeruleanSheenCutlass_punchcard")
        );
    }

    #[test]
    fn html_or_missing_dashboard_means_logged_out() {
        assert!(matches!(
            parse_dashboard(&json!({})),
            Err(FetchError::NotLoggedIn)
        ));
        let json = json!({ "dashboard": { "userStatus": { "isRewardsUser": false } } });
        assert!(matches!(
            parse_dashboard(&json),
            Err(FetchError::NotLoggedIn)
        ));
    }
}
