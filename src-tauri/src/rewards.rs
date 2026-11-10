//! Lecture du tableau de bord Microsoft Rewards.
//!
//! La connexion se fait dans une fenêtre WebView2 sur la vraie page Microsoft. Les cookies de
//! cette session sont ensuite lus depuis le magasin de cookies de WebView2 et envoyés avec les
//! requêtes vers les pages du site rewards.bing.com (/earn, /dashboard, /earn/quest/…).
//! Ces pages sont rendues par Next.js : leurs données sont lues dans le flux intégré au HTML
//! (voir [`crate::flight`]). Ce format n'est pas documenté et peut changer.

use std::collections::HashSet;

use chrono::{DateTime, Local};
use serde::Serialize;
use serde_json::{Map, Value};
use tauri::{AppHandle, Manager, Runtime};
use url::Url;

use crate::flight;
use crate::streaks::{self, Streaks};

const REWARDS_ORIGIN: &str = "https://rewards.bing.com/";
const QUEST_PATH: &str = "/earn/quest/";
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
    pub image_url: Option<String>,
    /// Activité liée à Xbox ou au Game Pass, mise en avant dans l'interface.
    pub xbox: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PunchCard {
    /// Identifiant de la quête, qui contient souvent le nom du jeu
    /// (ex. `WW_evergreen_pcparent_SeaofThieves_CeruleanSheenCutlass_punchcard`).
    pub id: String,
    pub title: String,
    pub description: String,
    pub image_url: Option<String>,
    pub points: i64,
    pub done: usize,
    pub total: usize,
    pub complete: bool,
    pub expires_at: Option<String>,
    /// Page de la quête sur rewards.bing.com.
    pub url: Option<String>,
    pub xbox: bool,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub title: String,
    pub description: String,
    pub complete: bool,
    /// Étape pas encore accessible : il faut d'abord terminer les précédentes.
    pub locked: bool,
    /// Texte et lien du bouton de l'étape (« Commencez à rechercher »…).
    pub link_text: Option<String>,
    pub url: Option<String>,
}

/// Contenu de la page d'une quête.
#[derive(Debug, Default)]
struct QuestPage {
    title: Option<String>,
    description: Option<String>,
    image_url: Option<String>,
    steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewardsSummary {
    pub fetched_at: DateTime<Local>,
    pub available_points: i64,
    pub level: Option<String>,
    /// Points gagnés vers le niveau suivant.
    pub level_progress: Option<i64>,
    pub level_progress_max: Option<i64>,
    pub searches: Vec<Counter>,
    pub daily_set: Vec<Activity>,
    pub more: Vec<Activity>,
    pub punch_cards: Vec<PunchCard>,
    pub streaks: Streaks,
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
    let (earn, final_url) = get(client, &cookie, &format!("{REWARDS_ORIGIN}earn")).await?;
    save_diagnostic(app, "earn", &earn);
    // Sans session valide, le site renvoie vers sa page de présentation ou de connexion.
    if final_url.host_str() != Some("rewards.bing.com") || !final_url.path().starts_with("/earn") {
        return Err(FetchError::NotLoggedIn);
    }
    let dashboard = match get(client, &cookie, &format!("{REWARDS_ORIGIN}dashboard")).await {
        Ok((html, _)) => {
            save_diagnostic(app, "dashboard", &html);
            Some(html)
        }
        Err(err) => {
            log::warn!("Page dashboard illisible : {err}");
            None
        }
    };
    let mut summary = parse(&earn, dashboard.as_deref())?;

    // Les étapes de chaque quête ne figurent que sur la page de la quête.
    let pages: Vec<_> = summary
        .punch_cards
        .iter()
        .map(|card| {
            let client = client.clone();
            let cookie = cookie.clone();
            let url = card.url.clone();
            tauri::async_runtime::spawn(async move {
                match url {
                    Some(url) => get(&client, &cookie, &url).await.map(|(html, _)| html),
                    None => Err(FetchError::Unexpected("quête sans adresse".into())),
                }
            })
        })
        .collect();
    for (card, page) in summary.punch_cards.iter_mut().zip(pages) {
        match page.await {
            Ok(Ok(html)) => {
                let page = quest_page(&html);
                if let Some(title) = page.title {
                    card.title = title;
                }
                if let Some(description) = page.description {
                    card.description = description;
                }
                if page.image_url.is_some() {
                    card.image_url = page.image_url;
                }
                card.steps = page.steps;
                // Quête venue d'une bannière : sa progression se déduit des étapes.
                if card.total == 0 && !card.steps.is_empty() {
                    card.total = card.steps.len();
                    card.done = card.steps.iter().filter(|s| s.complete).count();
                    card.complete = card.done >= card.total;
                }
            }
            Ok(Err(err)) => log::warn!("Quête {} illisible : {err}", card.id),
            Err(err) => log::warn!("Lecture de la quête {} interrompue : {err}", card.id),
        }
    }
    Ok(summary)
}

async fn get(
    client: &reqwest::Client,
    cookie: &str,
    url: &str,
) -> Result<(String, Url), FetchError> {
    let response = client
        .get(url)
        .header("Cookie", cookie)
        .header("Accept", "text/html")
        .header("Accept-Language", "fr-FR,fr;q=0.9,en;q=0.8")
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    if !response.status().is_success() {
        return Err(FetchError::Unexpected(format!(
            "{url} : statut HTTP {}",
            response.status()
        )));
    }
    let final_url = response.url().clone();
    let html = response
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    Ok((html, final_url))
}

/// Garde la dernière page lue pour diagnostiquer un changement de format côté Microsoft.
fn save_diagnostic<R: Runtime>(app: &AppHandle<R>, name: &str, html: &str) {
    if let Ok(dir) = app.path().app_log_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(format!("rewards-{name}.html")), html);
    }
}

fn parse(earn_html: &str, dashboard_html: Option<&str>) -> Result<RewardsSummary, FetchError> {
    let earn = flight::text(earn_html);
    let dashboard = dashboard_html.map(flight::text).unwrap_or_default();

    let available_points = first_int(&earn, "balance").ok_or(FetchError::NotLoggedIn)?;
    let level = first_str(&dashboard, "memberTitle").or_else(|| first_str(&earn, "memberTitle"));
    let level_progress = first_int(&dashboard, "pointsProgress");
    let level_progress_max = level_progress
        .zip(first_int(&dashboard, "pointsRemaining"))
        .map(|(done, left)| done + left);

    let (daily_set, daily_ids) = daily_set(&earn);
    let mut more: Vec<Activity> = Vec::new();
    for object in flight::objects_with_key(&earn, "offerId") {
        let Some(activity) = activity(&object) else {
            continue;
        };
        let id = activity.id.to_lowercase();
        if daily_ids.contains(&activity.id)
            || id.contains("pcparent")
            || id.contains("pcchild")
            || more.iter().any(|a| a.id == activity.id)
        {
            continue;
        }
        more.push(activity);
    }

    Ok(RewardsSummary {
        fetched_at: Local::now(),
        available_points,
        level,
        level_progress,
        level_progress_max,
        searches: searches(&earn),
        daily_set,
        more,
        punch_cards: {
            let mut cards = quest_cards(&earn);
            add_banner_quests(&earn, &mut cards);
            cards
        },
        streaks: streaks::parse(&earn),
    })
}

/// Compteurs de recherche : `"pointsCounters":{"pc":{"max":15,"progress":9},…}`.
fn searches(text: &str) -> Vec<Counter> {
    let Some(Value::Object(counters)) = flight::values_after_key(text, "pointsCounters")
        .into_iter()
        .next()
    else {
        return Vec::new();
    };
    counters
        .iter()
        .filter_map(|(key, value)| {
            let max = value.get("max")?.as_i64()?;
            let label = match key.as_str() {
                "pc" => "Recherches PC".to_string(),
                "mobile" => "Recherches mobile".to_string(),
                other => format!("Recherches ({other})"),
            };
            Some(Counter {
                label,
                progress: value.get("progress").and_then(Value::as_i64).unwrap_or(0),
                max,
            })
        })
        .collect()
}

/// Activités du jour : `"dailySetItems":[…]`, chaque élément portant sa date (MM/JJ/AAAA).
/// Renvoie aussi les identifiants de toute la série, pour l'exclure des autres activités.
fn daily_set(text: &str) -> (Vec<Activity>, HashSet<String>) {
    let items: Vec<Map<String, Value>> = flight::values_after_key(text, "dailySetItems")
        .into_iter()
        .find_map(|v| match v {
            Value::Array(items) => Some(items),
            _ => None,
        })
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| match v {
            Value::Object(object) => Some(object),
            _ => None,
        })
        .collect();
    let today = Local::now().format("%m/%d/%Y").to_string();
    let is_today =
        |item: &Map<String, Value>| item.get("date").and_then(Value::as_str) == Some(&today);
    // Le site peut dater la série selon un autre fuseau : sans élément du jour, on garde tout.
    let keep_all = !items.iter().any(is_today);
    let ids = items
        .iter()
        .filter_map(|item| item.get("offerId")?.as_str().map(str::to_string))
        .collect();
    let today_items = items
        .iter()
        .filter(|item| keep_all || is_today(item))
        .filter_map(activity)
        .collect();
    (today_items, ids)
}

fn activity(object: &Map<String, Value>) -> Option<Activity> {
    let title = object
        .get("title")?
        .as_str()
        .filter(|t| !t.is_empty())?
        .to_string();
    let points = object.get("points").and_then(as_int).unwrap_or(0);
    if points <= 0 {
        return None;
    }
    let id = object
        .get("offerId")
        .and_then(Value::as_str)
        .unwrap_or(&title)
        .to_string();
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(Activity {
        xbox: is_xbox(&[&title, &description, &id]),
        complete: is_true(object, "isCompleted") || is_true(object, "complete"),
        locked: is_true(object, "isLocked"),
        url: object
            .get("destination")
            .and_then(Value::as_str)
            .filter(|u| u.starts_with("https://"))
            .map(str::to_string),
        image_url: object
            .get("imageUrl")
            .and_then(Value::as_str)
            .filter(|u| u.starts_with("https://"))
            .map(str::to_string),
        id,
        title,
        description,
        points,
    })
}

/// Ce qu'on retient d'une carte de quête en parcourant ses éléments.
#[derive(Default)]
struct CardInfo {
    heading: Option<String>,
    alt: Option<String>,
    image_url: Option<String>,
    tasks: Option<(usize, usize)>,
    points: Option<i64>,
    expires_at: Option<String>,
}

/// Cartes de quête de la page /earn : chaque carte est un lien `/earn/quest/<id>`.
fn quest_cards(text: &str) -> Vec<PunchCard> {
    let marker = format!("\"href\":\"{QUEST_PATH}");
    let mut seen = HashSet::new();
    let mut cards = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find(&marker) {
        let at = from + found;
        from = at + marker.len();
        let Some(id_len) = text[from..].find('"') else {
            break;
        };
        let id = &text[from..from + id_len];
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        let Some(start) = text[..at].rfind('{') else {
            continue;
        };
        let Some(Ok(props)) = serde_json::Deserializer::from_str(&text[start..])
            .into_iter::<Value>()
            .next()
        else {
            continue;
        };
        let mut info = CardInfo::default();
        walk_card(&props, text, &mut info, &mut HashSet::new(), 0);

        let title = info.heading.or(info.alt).unwrap_or_else(|| id.to_string());
        let (done, total) = info.tasks.unwrap_or((0, 0));
        cards.push(PunchCard {
            xbox: is_xbox(&[&title, id]),
            id: id.to_string(),
            title,
            description: String::new(),
            image_url: info.image_url,
            points: info.points.unwrap_or(0),
            done,
            total,
            complete: total > 0 && done >= total,
            expires_at: info.expires_at,
            url: Some(quest_url(id)),
            steps: Vec::new(),
        });
    }
    cards
}

/// Certaines quêtes (Roblox, par exemple) ne sont présentes que sous forme de bannière dont le
/// lien `destination` mène à la page de la quête. Ces bannières complètent aussi la
/// description et l'image des quêtes déjà trouvées.
fn add_banner_quests(text: &str, cards: &mut Vec<PunchCard>) {
    for banner in flight::objects_with_key(text, "destination") {
        let Some(destination) = banner.get("destination").and_then(Value::as_str) else {
            continue;
        };
        let Some((_, rest)) = destination.split_once(QUEST_PATH) else {
            continue;
        };
        let id = rest.split(['?', '#', '/']).next().unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let text_field = |key: &str| {
            banner
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && !s.starts_with('$'))
                .map(str::to_string)
        };
        let description = text_field("description").unwrap_or_default();
        let image_url = text_field("imageUrl").filter(|u| u.starts_with("https://"));
        if let Some(card) = cards.iter_mut().find(|c| c.id == id) {
            if card.description.is_empty() {
                card.description = description;
            }
            if card.image_url.is_none() {
                card.image_url = image_url;
            }
            continue;
        }
        let title = text_field("title").unwrap_or_else(|| id.to_string());
        cards.push(PunchCard {
            xbox: is_xbox(&[&title, &description, id]),
            id: id.to_string(),
            title,
            description,
            image_url,
            points: 0,
            done: 0,
            total: 0,
            complete: false,
            expires_at: None,
            url: Some(quest_url(id)),
            steps: Vec::new(),
        });
    }
}

fn quest_url(id: &str) -> String {
    let mut url = Url::parse(REWARDS_ORIGIN).unwrap();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .extend(["earn", "quest", id]);
    url.to_string()
}

/// Parcourt les éléments React d'une carte, en suivant les références `$id` vers d'autres lignes.
fn walk_card(
    value: &Value,
    text: &str,
    info: &mut CardInfo,
    visited: &mut HashSet<String>,
    depth: usize,
) {
    if depth > 60 {
        return;
    }
    match value {
        Value::String(s) => {
            if let Some(id) = row_reference(s) {
                if visited.insert(id.to_string()) {
                    if let Some(row) = flight::row(text, id) {
                        walk_card(&row, text, info, visited, depth + 1);
                    }
                }
            } else if info.tasks.is_none() {
                info.tasks = parse_tasks(s);
            }
        }
        Value::Array(items) => {
            // Élément React : ["$", "h3", clé, {"children": "Titre"}].
            if items.len() == 4 && items[0] == "$" && items[1] == "h3" {
                if let Some(title) = items[3].get("children").and_then(Value::as_str) {
                    info.heading.get_or_insert_with(|| title.to_string());
                }
            }
            // Points affichés en deux morceaux : ["+", "50"].
            if items.len() == 2 && items[0] == "+" {
                if let Some(points) = items[1].as_str().and_then(parse_points) {
                    info.points.get_or_insert(points);
                }
            }
            for item in items {
                walk_card(item, text, info, visited, depth + 1);
            }
        }
        Value::Object(object) => {
            if let Some(alt) = object.get("alt").and_then(Value::as_str) {
                info.alt.get_or_insert_with(|| alt.to_string());
            }
            if let Some(src) = object
                .get("src")
                .and_then(Value::as_str)
                .filter(|s| s.starts_with("https://"))
            {
                info.image_url.get_or_insert_with(|| src.to_string());
            }
            if let Some(date) = object
                .get("expiresAt")
                .and_then(Value::as_str)
                .and_then(|d| d.strip_prefix("$D"))
            {
                info.expires_at.get_or_insert_with(|| date.to_string());
            }
            for item in object.values() {
                walk_card(item, text, info, visited, depth + 1);
            }
        }
        _ => {}
    }
}

/// « 1/4 tâches » → (1, 4).
fn parse_tasks(text: &str) -> Option<(usize, usize)> {
    let (done, rest) = text.split_once('/')?;
    let total: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = &rest[total.len()..];
    if done.is_empty() || !done.chars().all(|c| c.is_ascii_digit()) || total.is_empty() {
        return None;
    }
    if !after.is_empty() && !after.starts_with(' ') {
        return None;
    }
    Some((done.parse().ok()?, total.parse().ok()?))
}

/// « 1 500 », « 1,500 » ou « 95 » → nombre.
fn parse_points(text: &str) -> Option<i64> {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    let rest_is_separators = text
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | ' ' | '\u{a0}' | '\u{202f}'));
    if digits.is_empty() || !rest_is_separators {
        return None;
    }
    digits.parse().ok()
}

/// Page d'une quête : en-tête (titre, description, image) et liste des étapes.
fn quest_page(html: &str) -> QuestPage {
    let text = flight::text(html);
    let mut page = QuestPage::default();

    // L'en-tête est un composant {"description", "title"} dont le titre est aussi le texte
    // alternatif de la grande image. La page contient d'autres objets de même forme
    // (message d'erreur prêt à l'emploi), d'où la vérification par l'image.
    let images: Vec<(String, String)> = flight::objects_with_key(&text, "alt")
        .into_iter()
        .filter_map(|o| {
            let alt = o.get("alt")?.as_str()?.to_string();
            let src = o.get("src")?.as_str()?.to_string();
            src.starts_with("https://").then_some((alt, src))
        })
        .collect();
    for header in flight::objects_with_key(&text, "title") {
        let (Some(title), Some(description)) = (
            header.get("title").and_then(Value::as_str),
            header.get("description").and_then(Value::as_str),
        ) else {
            continue;
        };
        if let Some((_, src)) = images.iter().find(|(alt, _)| alt == title) {
            page.title = Some(title.to_string());
            page.description = Some(description.to_string());
            page.image_url = Some(src.clone());
            break;
        }
    }

    // Chaque étape est un élément ["$","div","<rang>",{"children":[icône, contenu]}], le
    // contenu portant un titre h3. Étapes et contenus peuvent être des références $id.
    let marker = "[\"$\",\"div\",\"";
    let mut steps: Vec<(u32, Step)> = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find(marker) {
        let at = from + found;
        from = at + marker.len();
        let Some(Ok(Value::Array(element))) = serde_json::Deserializer::from_str(&text[at..])
            .into_iter::<Value>()
            .next()
        else {
            continue;
        };
        let Some(rank) = element
            .get(2)
            .and_then(Value::as_str)
            .and_then(|k| k.parse().ok())
        else {
            continue;
        };
        let Some([icon, content]) = element
            .get(3)
            .and_then(|props| props.get("children"))
            .and_then(Value::as_array)
            .and_then(|children| <&[Value; 2]>::try_from(children.as_slice()).ok())
        else {
            continue;
        };
        if let Some(step) = quest_step(icon, content, &text) {
            if !steps.iter().any(|(_, s)| s.title == step.title) {
                steps.push((rank, step));
            }
        }
    }
    steps.sort_by_key(|(rank, _)| *rank);
    page.steps = steps.into_iter().map(|(_, step)| step).collect();
    page
}

fn quest_step(icon: &Value, content: &Value, text: &str) -> Option<Step> {
    let mut title = None;
    let mut description = None;
    let mut button: Option<Map<String, Value>> = None;
    visit(content, text, &mut |node| {
        if let Value::Array(items) = node {
            if items.len() == 4 && items[0] == "$" {
                let children = items[3].get("children").and_then(Value::as_str);
                match items[1].as_str() {
                    Some("h3") if title.is_none() => title = children.map(str::to_string),
                    Some("p") if title.is_some() && description.is_none() => {
                        description = children.map(str::to_string)
                    }
                    _ => {}
                }
            }
        }
        if let Value::Object(object) = node {
            if button.is_none() && object.contains_key("linkText") {
                button = Some(object.clone());
            }
        }
    });
    let title = title.filter(|t| !t.is_empty())?;

    // L'icône d'une étape terminée est un cercle « succès » ; une étape terminée n'a plus de bouton.
    let mut success_icon = false;
    let mut icon_has_title = false;
    visit(icon, text, &mut |node| {
        if let Some(class) = node.get("className").and_then(Value::as_str) {
            success_icon |= class.contains("statusSuccess");
        }
        if let Value::Array(items) = node {
            icon_has_title |= items.len() == 4 && items[0] == "$" && items[1] == "h3";
        }
    });
    if icon_has_title {
        return None;
    }
    let button = button.unwrap_or_default();
    let complete = success_icon || is_true(&button, "isCompleted");
    let url = button
        .get("href")
        .and_then(Value::as_str)
        .and_then(|href| Url::parse(REWARDS_ORIGIN).ok()?.join(href).ok())
        .map(|u| u.to_string());
    Some(Step {
        title,
        description: description.unwrap_or_default(),
        complete,
        locked: !complete && is_true(&button, "isLocked"),
        link_text: button
            .get("linkText")
            .and_then(Value::as_str)
            .map(str::to_string),
        url,
    })
}

/// Parcourt un élément et tous ses descendants, en suivant les références `$id`.
fn visit(value: &Value, text: &str, f: &mut dyn FnMut(&Value)) {
    fn go(
        value: &Value,
        text: &str,
        f: &mut dyn FnMut(&Value),
        seen: &mut HashSet<String>,
        depth: usize,
    ) {
        if depth > 60 {
            return;
        }
        f(value);
        match value {
            Value::String(s) => {
                if let Some(id) = row_reference(s) {
                    if seen.insert(id.to_string()) {
                        if let Some(row) = flight::row(text, id) {
                            go(&row, text, f, seen, depth + 1);
                        }
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|v| go(v, text, f, seen, depth + 1)),
            Value::Object(object) => object
                .values()
                .for_each(|v| go(v, text, f, seen, depth + 1)),
            _ => {}
        }
    }
    go(value, text, f, &mut HashSet::new(), 0);
}

/// `"$6a"` ou `"$L6a"` → `6a` ; les autres chaînes (`$undefined`, `$D2026…`) ne sont pas des références.
fn row_reference(s: &str) -> Option<&str> {
    let id = s.strip_prefix("$L").or_else(|| s.strip_prefix('$'))?;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit())).then_some(id)
}

fn first_int(text: &str, key: &str) -> Option<i64> {
    flight::values_after_key(text, key).iter().find_map(as_int)
}

fn first_str(text: &str, key: &str) -> Option<String> {
    flight::values_after_key(text, key)
        .iter()
        .find_map(|v| v.as_str().filter(|s| !s.is_empty() && !s.starts_with('$')))
        .map(str::to_string)
}

fn is_true(object: &Map<String, Value>, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool) == Some(true)
}

fn is_xbox(texts: &[&str]) -> bool {
    texts.iter().any(|t| {
        let t = t.to_lowercase();
        t.contains("xbox") || t.contains("game pass") || t.contains("gamepass")
    })
}

fn as_int(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|f| f as i64))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Emballe des lignes du flux Next.js dans une page HTML, comme le fait le site.
    fn page(rows: &str) -> String {
        format!(
            "<html><script>self.__next_f.push([1,{}])</script></html>",
            serde_json::to_string(rows).unwrap()
        )
    }

    /// Extrait réduit et anonymisé de la vraie page /earn (novembre 2026).
    fn earn_rows() -> String {
        let today = Local::now().format("%m/%d/%Y");
        format!(
            r#"5:{{"balance":3345,"level":1}}
6:{{"memberTitle":"Membre"}}
7:{{"pointsCounters":{{"dailyOffer":101,"pc":{{"max":15,"progress":9,"originalMax":15}},"totalPoints":110,"boostActivated":false}}}}
8:{{"dailySetItems":[{{"date":"{today}","description":"Rock, pop","destination":"https://www.bing.com/search?q=concerts","imageUrl":"https://bing.com/th?id=OMR.Music_Tile.jpg&pid=Rewards","isCompleted":false,"isLocked":"$undefined","offerId":"Global_DailySet_Child1","points":10,"title":"Événements musicaux"}},{{"date":"{today}","isCompleted":true,"offerId":"Global_DailySet_Child2","points":10,"title":"Perth"}},{{"date":"01/01/2020","isCompleted":false,"offerId":"Old","points":10,"title":"Ancienne"}}]}}
9:[{{"offerId":"WW_Rewards_locked_level2_offer1","title":"À la découverte de Mars","points":15,"isCompleted":false,"isLocked":true,"destination":"https://rewards.bing.com/level"}},{{"offerId":"WW_banner","title":"Bannière","points":0,"isCompleted":false}},{{"offerId":"Xbox_offer","title":"Jouez sur Xbox","points":20,"isCompleted":false,"destination":"https://www.xbox.com"}},{{"offerId":"Global_DailySet_Child1","title":"Événements musicaux","points":10}}]
10:[{{"destination":"https://rewards.bing.com/earn/quest/WW_pcparent_Roblox_Soundwave_HP_punchcard?form=ML2Y1R","imageUrl":"https://bing.com/th?id=OMR.RobloxEx.png&pid=Rewards","isCompleted":false,"name":"WW_Rewards_Campaign_Roblox_Awareness","points":0,"title":"Équipement d’avatar exclusif","description":"Offrez un nouveau look à votre avatar Roblox"}},{{"destination":"https://rewards.bing.com/earn/quest/WW_pcparent_FY27_Duolingo3M_punchcard?form=ML2Y43","points":0,"title":"Obtenez Super Duolingo","description":"Profitez de 2 mois gratuits"}}]
80:["$","$L3f",null,{{"src":"https://bing.com/th?id=OMR.hero_superduolingo.png&pid=Rewards","alt":"Débloquez Super Duolingo","fill":true}}]
82:["$","$L2b","1",{{"href":"/earn/quest/WW_pcparent_FY27_Duolingo3M_punchcard","instrument":{{"data":{{"offerId":"WW_pcparent_FY27_Duolingo3M_punchcard"}}}},"children":["$","div",null,{{"children":["$80",["$","span",null,{{"children":["$","$L90",null,{{"expiresAt":"$D2026-12-04T08:00:00.000Z","prefix":"Date d’expiration"}}]}}],["$","div",null,{{"children":"0/6 tâches"}}]]}}]}}]
83:["$","$L2b","2",{{"href":"/earn/quest/WW_evergreen_pcparent_SeaofThieves_CeruleanSheenCutlass_punchcard","children":["$","div",null,{{"children":[["$","$L3f",null,{{"src":"https://bing.com/th?id=OMR.SoT_1200x675.jpg&pid=Rewards","alt":"Sea of Thieves - Sabre"}}],["$","h3",null,{{"className":"text-globalBody2Strong","children":"Sea of Thieves - Sabre à éclat céruléen"}}],["$","div",null,{{"children":["+","50"]}}],["$","div",null,{{"children":"1/4 tâches"}}]]}}]}}]
"#
        )
    }

    #[test]
    fn parses_earn_and_dashboard_pages() {
        let dashboard = page(
            r#"3:{"memberTitle":"Membre","pointsProgress":143,"pointsRemaining":357,"targetLevel":2}"#,
        );
        let s = parse(&page(&earn_rows()), Some(&dashboard)).unwrap();
        assert_eq!(s.available_points, 3345);
        assert_eq!(s.level.as_deref(), Some("Membre"));
        assert_eq!(
            (s.level_progress, s.level_progress_max),
            (Some(143), Some(500))
        );

        assert_eq!(s.searches.len(), 1);
        assert_eq!(s.searches[0].label, "Recherches PC");
        assert_eq!(s.search_points_left(), 6);

        let daily: Vec<_> = s.daily_set.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(daily, ["Global_DailySet_Child1", "Global_DailySet_Child2"]);
        assert_eq!(s.daily_set_left(), 1);
        assert!(s.daily_set[0]
            .image_url
            .as_deref()
            .unwrap()
            .contains("Music_Tile"));
        assert_eq!(
            s.daily_set[0].url.as_deref(),
            Some("https://www.bing.com/search?q=concerts")
        );

        let more: Vec<_> = s.more.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(more, ["WW_Rewards_locked_level2_offer1", "Xbox_offer"]);
        assert!(s.more[0].locked);
        assert!(s.more[1].xbox);
    }

    #[test]
    fn parses_quest_cards_including_referenced_rows() {
        let s = parse(&page(&earn_rows()), None).unwrap();
        assert_eq!(s.punch_cards.len(), 3);

        let duolingo = &s.punch_cards[0];
        assert_eq!(duolingo.title, "Débloquez Super Duolingo");
        assert_eq!(duolingo.description, "Profitez de 2 mois gratuits");
        assert_eq!((duolingo.done, duolingo.total), (0, 6));
        assert_eq!(
            duolingo.expires_at.as_deref(),
            Some("2026-12-04T08:00:00.000Z")
        );
        assert!(duolingo
            .image_url
            .as_deref()
            .unwrap()
            .contains("superduolingo"));

        let sot = &s.punch_cards[1];
        assert_eq!(sot.title, "Sea of Thieves - Sabre à éclat céruléen");
        assert_eq!((sot.done, sot.total, sot.points), (1, 4, 50));
        assert!(!sot.complete);
        assert_eq!(
            sot.url.as_deref(),
            Some("https://rewards.bing.com/earn/quest/WW_evergreen_pcparent_SeaofThieves_CeruleanSheenCutlass_punchcard")
        );

        // Quête présente seulement dans une bannière, sans les paramètres de suivi du lien.
        let roblox = &s.punch_cards[2];
        assert_eq!(roblox.id, "WW_pcparent_Roblox_Soundwave_HP_punchcard");
        assert_eq!(roblox.title, "Équipement d’avatar exclusif");
        assert!(roblox.description.contains("Roblox"));
        assert_eq!(
            roblox.url.as_deref(),
            Some("https://rewards.bing.com/earn/quest/WW_pcparent_Roblox_Soundwave_HP_punchcard")
        );
    }

    /// Extrait réduit de la vraie page de la quête Sea of Thieves (novembre 2026).
    const QUEST_ROWS: &str = r#"1:["$","$L20",null,{"title":"Nous avons rencontré un problème.","description":"Désolé..."}]
4d:["$","div",null,{"children":[["$","$L3f",null,{"alt":"Sea of Thieves - Sabre","src":"https://bing.com/th?id=OMR.SoT_1200x675.jpg&pid=Rewards"}],["$","$L67",null,{"description":"Émerveillez vos amis. La quête se termine le 15 janvier 2027.","title":"Sea of Thieves - Sabre"}],["$","div",null,{"children":[["$","h3",null,{"children":"Conditions générales"}],["$","p",null,{"children":"Offre valable…"}]]}]]}]
50:["$","div",null,{"className":"flex flex-col gap-4","children":[["$","div","0",{"className":"flex flex-row","children":[["$","div",null,{"children":["$","div",null,{"className":"flex rounded-cornerCircular bg-statusSuccessRewardsBg","children":["$","svg",null,{"children":"$L69"}]}]}],"$L6a"]}],"$L6b"]}]
69:["$","path",null,{"d":"m1.147 2.919 1.788 1.953 3.25-3.743"}]
6a:["$","div",null,{"children":[["$","div",null,{"children":[["$","h3",null,{"children":"Mettez les voiles vers le site officiel"}],["$","p",null,{"children":"Découvrez le site officiel."}],false,null]}],false]}]
6b:["$","div","1",{"className":"flex flex-row","children":[["$","div",null,{"children":["$","svg",null,{"children":["$","path",null,{"d":"M5 3.5a3 3 0 0 1 6 0V4h.5"}]}]}],["$","div",null,{"children":[["$","div",null,{"children":[["$","h3",null,{"children":"Recherche à terminer"}],["$","p",null,{"children":"Recherchez avec Bing sur ordinateur (2/5 jours terminés)."}]]}],["$","$L6f",null,{"href":"https://www.bing.com/search?q=Sea+of+Thieves","isCompleted":false,"isLocked":true,"linkText":"Commencez à rechercher","offerId":"WW_evergreen_pcchild2_SeaofThieves"}]]}]]}]
"#;

    #[test]
    fn parses_quest_page_header_and_steps() {
        let quest = quest_page(&page(QUEST_ROWS));
        assert_eq!(quest.title.as_deref(), Some("Sea of Thieves - Sabre"));
        assert!(quest.description.unwrap().contains("15 janvier 2027"));
        assert!(quest.image_url.unwrap().contains("SoT_1200x675"));

        let titles: Vec<_> = quest.steps.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Mettez les voiles vers le site officiel",
                "Recherche à terminer"
            ]
        );
        let first = &quest.steps[0];
        assert!(first.complete && !first.locked);
        assert_eq!(first.description, "Découvrez le site officiel.");
        assert_eq!(first.link_text, None);

        let search = &quest.steps[1];
        assert!(!search.complete && search.locked);
        assert!(search.description.contains("2/5 jours"));
        assert_eq!(search.link_text.as_deref(), Some("Commencez à rechercher"));
        assert_eq!(
            search.url.as_deref(),
            Some("https://www.bing.com/search?q=Sea+of+Thieves")
        );
    }

    #[test]
    fn page_without_balance_means_logged_out() {
        assert!(matches!(
            parse("<html>Se connecter</html>", None),
            Err(FetchError::NotLoggedIn)
        ));
    }

    #[test]
    fn reads_task_counts_and_points() {
        assert_eq!(parse_tasks("1/4 tâches"), Some((1, 4)));
        assert_eq!(parse_tasks("0/6 tasks"), Some((0, 6)));
        assert_eq!(parse_tasks("12/31/2026"), None);
        assert_eq!(parse_tasks("https://x/y"), None);
        assert_eq!(parse_points("1\u{202f}500"), Some(1500));
        assert_eq!(parse_points("95"), Some(95));
        assert_eq!(parse_points("abc"), None);
    }
}
