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
    /// Précision affichée par le site, par exemple « Déverrouille demain ».
    pub note: Option<String>,
    /// Activité liée à Xbox ou au Game Pass, mise en avant dans l'interface.
    pub xbox: bool,
    /// Nature de la tâche (recherche Bing, quiz…), déduite de son lien et de son texte.
    pub kind: Option<TaskKind>,
}

/// Ce que demande une tâche, pour la signaler dans l'interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskKind {
    /// Le lien ouvre une recherche Bing toute faite : le clic suffit.
    Search,
    /// Il faut faire soi-même une ou plusieurs recherches sur Bing.
    SearchManual,
    /// Quiz à jouer sur Bing.
    Quiz,
    /// Puzzle d'images Bing.
    Puzzle,
    /// Site partenaire (Duolingo, YouTube…).
    Partner,
}

impl TaskKind {
    pub fn is_search(self) -> bool {
        matches!(self, TaskKind::Search | TaskKind::SearchManual)
    }
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
    pub kind: Option<TaskKind>,
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
    /// Nom du compte Microsoft, tel qu'affiché dans l'en-tête du site.
    pub account_name: Option<String>,
    pub available_points: i64,
    pub level: Option<String>,
    /// Points gagnés vers le niveau suivant.
    pub level_progress: Option<i64>,
    pub level_progress_max: Option<i64>,
    /// Points gagnés en attente sur le tableau de bord (« Prêt à réclamer »).
    pub claimable_points: i64,
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

    /// Recherches Bing encore à faire dans les activités et les étapes de quêtes.
    pub fn searches_to_do(&self) -> usize {
        let activities = self
            .daily_set
            .iter()
            .chain(&self.more)
            .filter(|a| !a.complete && !a.locked && a.kind.is_some_and(TaskKind::is_search))
            .count();
        let steps = self
            .punch_cards
            .iter()
            .flat_map(|c| &c.steps)
            .filter(|s| !s.complete && !s.locked && s.kind.is_some_and(TaskKind::is_search))
            .count();
        activities + steps
    }

    /// Classe chaque activité et chaque étape de quête d'après son lien et son texte.
    fn classify_tasks(&mut self) {
        for a in self.daily_set.iter_mut().chain(self.more.iter_mut()) {
            a.kind = task_kind(a.url.as_deref(), &[&a.title, &a.description]);
        }
        for card in &mut self.punch_cards {
            for s in &mut card.steps {
                let link = s.link_text.as_deref().unwrap_or_default();
                s.kind = task_kind(s.url.as_deref(), &[&s.title, &s.description, link]);
            }
        }
    }

    pub fn daily_set_left(&self) -> usize {
        self.daily_set
            .iter()
            .filter(|a| !a.complete && !a.locked)
            .count()
    }
}

/// Une tâche qui peut être verrouillée : étape de quête ou activité.
pub struct TaskState {
    /// Identifiant stable d'une actualisation à l'autre.
    pub key: String,
    pub label: String,
    pub locked: bool,
    pub complete: bool,
}

impl RewardsSummary {
    pub fn task_states(&self) -> Vec<TaskState> {
        let steps = self.punch_cards.iter().flat_map(|card| {
            card.steps.iter().map(move |step| TaskState {
                key: format!("{}#{}", card.id, step.title),
                label: format!("{} ({})", step.title, card.title),
                locked: step.locked,
                complete: step.complete,
            })
        });
        let activities = self.daily_set.iter().chain(&self.more).map(|a| TaskState {
            key: format!("activite#{}", a.id),
            label: a.title.clone(),
            locked: a.locked,
            complete: a.complete,
        });
        steps.chain(activities).collect()
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
    summary.classify_tasks();
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
    // « Continuer à gagner » : la liste exacte du site, cartes à 0 point comprises. Sans elle
    // (ancien format), on retombe sur tous les objets portant un `offerId`.
    let mut more = more_activities(&earn);
    // Les activités du jour ont leur propre liste.
    more.retain(|a| !daily_ids.contains(&a.id));
    let from_section = !more.is_empty();
    for object in flight::objects_with_key(&earn, "offerId") {
        // Les cartes dont le contenu n'est que dans le rendu (« Explorer sur Bing ») s'ajoutent.
        let candidate = if from_section {
            card_activity(&object, &earn)
        } else {
            activity(&object).or_else(|| card_activity(&object, &earn))
        };
        let Some(activity) = candidate else {
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

    let mut summary = RewardsSummary {
        fetched_at: Local::now(),
        account_name: account_name(&earn),
        available_points,
        level,
        level_progress,
        level_progress_max,
        claimable_points: claimable_points(&dashboard),
        searches: searches(&earn),
        daily_set,
        more,
        punch_cards: {
            let mut cards = quest_cards(&earn);
            add_banner_quests(&earn, &mut cards);
            cards
        },
        streaks: streaks::parse(&earn),
    };
    summary.classify_tasks();
    Ok(summary)
}

/// Nom du compte : composant d'en-tête `{"name":"…","profilePictureUrl":"…"}`.
fn account_name(text: &str) -> Option<String> {
    flight::objects_with_key(text, "profilePictureUrl")
        .iter()
        .find_map(|o| {
            o.get("name")?
                .as_str()
                .filter(|n| !n.is_empty())
                .map(str::to_string)
        })
}

/// Points « Prêt à réclamer » : `"pointClaim":{"points":99,…}` sur le tableau de bord,
/// `"pointClaim":null` quand il n'y a rien à réclamer.
fn claimable_points(dashboard: &str) -> i64 {
    flight::values_after_key(dashboard, "pointClaim")
        .iter()
        .filter_map(|claim| claim.get("points").and_then(as_int))
        .next_back()
        .unwrap_or(0)
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

/// Cartes de la section « Continuer à gagner » : `"activityCards":[…]`. Une carte verrouillée
/// porte une clé `unlockCriteria` (`newLevel2`…) dont le texte français est dans la page.
fn more_activities(text: &str) -> Vec<Activity> {
    let Some(Value::Array(cards)) = flight::values_after_key(text, "activityCards")
        .into_iter()
        .find(Value::is_array)
    else {
        return Vec::new();
    };
    cards
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|card| {
            let mut activity = activity_with(card, true)?;
            activity.note = card
                .get("unlockCriteria")
                .and_then(Value::as_str)
                .filter(|key| !key.is_empty() && !key.starts_with('$'))
                .and_then(|key| {
                    flight::values_after_key(text, key)
                        .into_iter()
                        .find_map(|v| v.as_str().map(str::to_string))
                });
            Some(activity)
        })
        .collect()
}

fn activity(object: &Map<String, Value>) -> Option<Activity> {
    activity_with(object, false)
}

/// `allow_zero` garde les cartes sans points (annonces, offres partenaires).
fn activity_with(object: &Map<String, Value>, allow_zero: bool) -> Option<Activity> {
    let title = object
        .get("title")?
        .as_str()
        .filter(|t| !t.is_empty())?
        .to_string();
    let points = object.get("points").and_then(as_int).unwrap_or(0);
    if points < 0 || (points == 0 && !allow_zero) {
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
        note: None,
        kind: None,
        id,
        title,
        description,
        points,
    })
}

/// Carte d'activité dont titre, description, image et points ne figurent que dans son rendu,
/// comme celles de la section « Explorer sur Bing » : seuls `offerId`, `isCompleted`,
/// `isLocked` et `href` sont des propriétés de l'élément.
fn card_activity(object: &Map<String, Value>, text: &str) -> Option<Activity> {
    let id = object.get("offerId")?.as_str()?.to_string();
    let children = object.get("children")?;
    let mut paragraphs: Vec<String> = Vec::new();
    let mut image_url = None;
    let mut points = None;
    let mut note = None;
    visit(children, text, &mut |node| match node {
        Value::Array(items) => {
            if items.len() == 4 && items[0] == "$" && items[1] == "p" {
                if let Some(text) = items[3].get("children").and_then(Value::as_str) {
                    // Les titres se terminent parfois par une espace sans chasse (U+200B).
                    let text = text.trim_matches(|c: char| c == '\u{200b}' || c.is_whitespace());
                    if !text.is_empty() {
                        paragraphs.push(text.to_string());
                    }
                }
            }
            // Points : ["+", "10"] à faire, ["", "10"] (avec une coche) une fois fait.
            if items.len() == 2 && (items[0] == "+" || items[0] == "") {
                if let Some(value) = items[1].as_str().and_then(parse_points) {
                    points.get_or_insert(value);
                }
            }
        }
        Value::Object(props) if image_url.is_none() => {
            image_url = props
                .get("src")
                .and_then(Value::as_str)
                .filter(|u| u.starts_with("https://"))
                .map(str::to_string);
        }
        Value::String(text) if note.is_none() && text.to_lowercase().contains("verrouill") => {
            note = Some(text.clone());
        }
        _ => {}
    });
    let title = paragraphs.first()?.clone();
    let description = paragraphs.get(1).cloned().unwrap_or_default();
    let points = points.filter(|p| *p > 0)?;
    Some(Activity {
        xbox: is_xbox(&[&title, &description, &id]),
        complete: is_true(object, "isCompleted"),
        locked: is_true(object, "isLocked"),
        url: object
            .get("href")
            .and_then(Value::as_str)
            .filter(|u| u.starts_with("https://"))
            .map(str::to_string),
        image_url,
        note,
        kind: None,
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
        kind: None,
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

/// Nature d'une tâche d'après son lien de destination, puis son texte.
///
/// - `bing.com/search?q=…` : recherche toute faite, ou quiz quand la requête en est un ;
/// - accueil de Bing (« Explorer sur Bing », « Commencez à rechercher ») : recherche à faire soi-même ;
/// - `bing.com/spotlight/imagepuzzle` : puzzle ;
/// - autre site que Microsoft : partenaire ;
/// - sans lien parlant, un texte qui parle de recherche (« Rechercher avec Bing pendant
///   7 jours », « Effectuez une recherche visuelle ») : recherche à faire soi-même.
fn task_kind(url: Option<&str>, texts: &[&str]) -> Option<TaskKind> {
    let url = url.and_then(|u| Url::parse(u).ok());
    let host = url
        .as_ref()
        .and_then(|u| u.host_str())
        .map(str::to_lowercase)
        .unwrap_or_default();
    let mentions_search = || {
        texts.iter().any(|t| {
            let t = t.to_lowercase();
            t.contains("recherch") || t.contains("search")
        })
    };
    match (&url, host.as_str()) {
        (Some(url), "bing.com" | "www.bing.com") => {
            let path = url.path().trim_end_matches('/');
            if path == "/search" {
                let query = url
                    .query_pairs()
                    .find(|(k, _)| k == "q")
                    .map(|(_, v)| v.to_lowercase())
                    .unwrap_or_default();
                let raw = url.query().unwrap_or_default().to_lowercase();
                let quiz = query.contains("quiz")
                    || raw.contains("wqoskey")
                    || raw.contains("form=dsetqu");
                Some(if quiz {
                    TaskKind::Quiz
                } else {
                    TaskKind::Search
                })
            } else if path.starts_with("/spotlight/imagepuzzle") {
                Some(TaskKind::Puzzle)
            } else if path.is_empty() {
                Some(TaskKind::SearchManual)
            } else {
                None
            }
        }
        (None, _) => mentions_search().then_some(TaskKind::SearchManual),
        (Some(url), _) if url.scheme() == "microsoft-edge" => {
            mentions_search().then_some(TaskKind::SearchManual)
        }
        (Some(_), host) => {
            let microsoft = [
                "bing.com",
                "microsoft.com",
                "xbox.com",
                "aka.ms",
                "live.com",
                "msn.com",
                "office.com",
                "skype.com",
                "windows.com",
                "microsoftedge.com",
            ];
            let ours = microsoft
                .iter()
                .any(|d| host == *d || host.ends_with(&format!(".{d}")));
            (!ours && !host.is_empty()).then_some(TaskKind::Partner)
        }
    }
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
c:{{"UnlockCriteria":{{"newLevel2":"Niveau Argent requis","tomorrow":"Déverrouille demain"}}}}
55:{{"activityCards":[{{"offerId":"WW_Rewards_locked_level2_offer1","title":"À la découverte de Mars","points":15,"isCompleted":false,"isLocked":true,"unlockCriteria":"newLevel2","destination":"https://rewards.bing.com/level"}},{{"offerId":"Xbox_offer","title":"Jouez sur Xbox","points":20,"isCompleted":false,"unlockCriteria":"$undefined","destination":"https://www.xbox.com"}},{{"offerId":"FRstar_Encourage_Redemption","title":"Il est temps de racheter !","points":0,"isCompleted":false,"isPromotional":true}},{{"offerId":"Global_DailySet_Child1","title":"Événements musicaux","points":10}}]}}
6b:["$","$L59",null,{{"name":"Pierre Comble","profilePictureUrl":"https://storage.live.com/users/x/myprofile"}}]
6:{{"memberTitle":"Membre"}}
7:{{"pointsCounters":{{"dailyOffer":101,"pc":{{"max":15,"progress":9,"originalMax":15}},"totalPoints":110,"boostActivated":false}}}}
8:{{"dailySetItems":[{{"date":"{today}","description":"Rock, pop","destination":"https://www.bing.com/search?q=concerts","imageUrl":"https://bing.com/th?id=OMR.Music_Tile.jpg&pid=Rewards","isCompleted":false,"isLocked":"$undefined","offerId":"Global_DailySet_Child1","points":10,"title":"Événements musicaux"}},{{"date":"{today}","isCompleted":true,"offerId":"Global_DailySet_Child2","points":10,"title":"Perth"}},{{"date":"01/01/2020","isCompleted":false,"offerId":"Old","points":10,"title":"Ancienne"}}]}}
9:[{{"offerId":"WW_Rewards_locked_level2_offer1","title":"À la découverte de Mars","points":15,"isCompleted":false,"isLocked":true,"destination":"https://rewards.bing.com/level"}},{{"offerId":"WW_banner","title":"Bannière","points":0,"isCompleted":false}},{{"offerId":"Xbox_offer","title":"Jouez sur Xbox","points":20,"isCompleted":false,"destination":"https://www.xbox.com"}},{{"offerId":"Global_DailySet_Child1","title":"Événements musicaux","points":10}}]
10:[{{"destination":"https://rewards.bing.com/earn/quest/WW_pcparent_Roblox_Soundwave_HP_punchcard?form=ML2Y1R","imageUrl":"https://bing.com/th?id=OMR.RobloxEx.png&pid=Rewards","isCompleted":false,"name":"WW_Rewards_Campaign_Roblox_Awareness","points":0,"title":"Équipement d’avatar exclusif","description":"Offrez un nouveau look à votre avatar Roblox"}},{{"destination":"https://rewards.bing.com/earn/quest/WW_pcparent_FY27_Duolingo3M_punchcard?form=ML2Y43","points":0,"title":"Obtenez Super Duolingo","description":"Profitez de 2 mois gratuits"}}]
7a:["$","$L78","3",{{"href":"https://www.bing.com/?form=ML2PCR&rwAutoFlyout=exb","offerId":"ENUS_bankaccounts_exploreonbing_activation_Evergreen","isCompleted":false,"isLocked":true,"children":["$","$L2b",null,{{"children":["$","div",null,{{"children":[["$","$L41",null,{{"src":"https://bing.com/th?id=OMR.Bank_accounts.png&pid=Rewards","alt":"Banquez plus intelligemment​"}}],["$","p",null,{{"className":"line-clamp-3","children":"Banquez plus intelligemment​"}}],["$","p",null,{{"className":"line-clamp-3","children":"Recherchez sur Bing pour comparer les comptes"}}],["$","p",null,{{"children":["+","10"]}}],["$","div",null,{{"children":"Déverrouille demain"}}]]}}]}}]}}]
80:["$","$L3f",null,{{"src":"https://bing.com/th?id=OMR.hero_superduolingo.png&pid=Rewards","alt":"Débloquez Super Duolingo","fill":true}}]
82:["$","$L2b","1",{{"href":"/earn/quest/WW_pcparent_FY27_Duolingo3M_punchcard","instrument":{{"data":{{"offerId":"WW_pcparent_FY27_Duolingo3M_punchcard"}}}},"children":["$","div",null,{{"children":["$80",["$","span",null,{{"children":["$","$L90",null,{{"expiresAt":"$D2026-12-04T08:00:00.000Z","prefix":"Date d’expiration"}}]}}],["$","div",null,{{"children":"0/6 tâches"}}]]}}]}}]
83:["$","$L2b","2",{{"href":"/earn/quest/WW_evergreen_pcparent_SeaofThieves_CeruleanSheenCutlass_punchcard","children":["$","div",null,{{"children":[["$","$L3f",null,{{"src":"https://bing.com/th?id=OMR.SoT_1200x675.jpg&pid=Rewards","alt":"Sea of Thieves - Sabre"}}],["$","h3",null,{{"className":"text-globalBody2Strong","children":"Sea of Thieves - Sabre à éclat céruléen"}}],["$","div",null,{{"children":["+","50"]}}],["$","div",null,{{"children":"1/4 tâches"}}]]}}]}}]
"#
        )
    }

    #[test]
    fn parses_earn_and_dashboard_pages() {
        let dashboard = page(
            r#"3:{"memberTitle":"Membre","pointsProgress":143,"pointsRemaining":357,"targetLevel":2}
4:["$","$L91",null,{"type":"pointsclaim","model":{"pointClaim":{"points":99}}}]"#,
        );
        let s = parse(&page(&earn_rows()), Some(&dashboard)).unwrap();
        assert_eq!(s.available_points, 3345);
        assert_eq!(s.account_name.as_deref(), Some("Pierre Comble"));
        assert_eq!(s.level.as_deref(), Some("Membre"));
        assert_eq!(
            (s.level_progress, s.level_progress_max),
            (Some(143), Some(500))
        );
        assert_eq!(s.claimable_points, 99);

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
        assert_eq!(
            more,
            [
                "WW_Rewards_locked_level2_offer1",
                "Xbox_offer",
                "FRstar_Encourage_Redemption",
                "ENUS_bankaccounts_exploreonbing_activation_Evergreen"
            ]
        );
        assert_eq!(s.more[0].note.as_deref(), Some("Niveau Argent requis"));
        assert_eq!(s.more[1].note, None);
        assert_eq!(s.more[2].points, 0, "les cartes sans points sont gardées");
        let explore = &s.more[3];
        assert_eq!(explore.title, "Banquez plus intelligemment");
        assert_eq!(
            explore.description,
            "Recherchez sur Bing pour comparer les comptes"
        );
        assert_eq!(explore.points, 10);
        assert!(explore.locked && !explore.complete);
        assert_eq!(explore.note.as_deref(), Some("Déverrouille demain"));
        assert!(explore
            .image_url
            .as_deref()
            .unwrap()
            .contains("Bank_accounts"));
        assert!(s.more[0].locked);
        assert!(s.more[1].xbox);
    }

    #[test]
    fn parses_quest_cards_including_referenced_rows() {
        let s = parse(&page(&earn_rows()), None).unwrap();
        assert_eq!(
            s.claimable_points, 0,
            "rien à réclamer sans tableau de bord"
        );
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

    #[test]
    fn classe_les_taches_d_apres_leur_lien() {
        use TaskKind::*;
        let kind = |url: &str| task_kind(Some(url), &["Titre"]);
        assert_eq!(
            kind("https://www.bing.com/search?q=Narval&form=ML2Y2H&rnoreward=1"),
            Some(Search)
        );
        assert_eq!(
            kind("https://www.bing.com/search?q=Bing%20Homepage%20quiz&form=ML2BF1"),
            Some(Quiz)
        );
        assert_eq!(
            kind("https://www.bing.com/search?q=Slumdog&form=dsetqu&filters=WQOskey:%22x%22"),
            Some(Quiz)
        );
        assert_eq!(
            kind("https://www.bing.com/spotlight/imagepuzzle?form=ML2BF0"),
            Some(Puzzle)
        );
        assert_eq!(kind("https://www.bing.com?form=ML2X8X"), Some(SearchManual));
        assert_eq!(
            kind("https://www.bing.com/?form=ML2PCR&rwAutoFlyout=exb"),
            Some(SearchManual)
        );
        assert_eq!(kind("https://duolingo.com/math?form=ML2Y3R"), Some(Partner));
        assert_eq!(kind("https://www.youtube.com/watch?v=x"), Some(Partner));
        assert_eq!(
            kind("https://rewards.bing.com/earn/quest/WW_x_punchcard"),
            None
        );
        assert_eq!(kind("https://www.xbox.com/xbox-game-pass"), None);
        assert_eq!(kind("https://www.bing.com/apps/wallpaper?pc=w422"), None);
    }

    #[test]
    fn classe_les_taches_sans_lien_d_apres_leur_texte() {
        assert_eq!(
            task_kind(
                None,
                &["Rechercher avec Bing pendant 7 jours dans une ligne"]
            ),
            Some(TaskKind::SearchManual)
        );
        assert_eq!(task_kind(None, &["Parrainez vos amis"]), None);
    }
}
