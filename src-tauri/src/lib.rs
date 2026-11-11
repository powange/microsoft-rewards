mod auth;
mod catalog;
mod flight;
mod mini;
mod reminders;
mod rewards;
mod store;
mod streaks;
mod tray;
mod updater;
mod xbox;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_notification::NotificationExt as _;
use tauri_plugin_opener::OpenerExt as _;

use catalog::Catalog;
use rewards::{FetchError, RewardsSummary};
use store::{FollowedQuest, Settings, Store, XboxProgress};
use updater::UpdateInfo;

/// Argument passé par le démarrage automatique de Windows : l'app démarre dans le tray.
const MINIMIZED_ARG: &str = "--minimized";
/// Identifiant de l'app Xbox pour PC, utilisé pour l'ouvrir sur le Rewards Hub.
const XBOX_APP_AUMID: &str = "Microsoft.GamingApp_8wekyb3d8bbwe!Microsoft.Xbox.App";
/// Le catalogue d'échange change rarement : il est relu au plus toutes les 6 heures.
const CATALOG_MAX_AGE: Duration = Duration::from_secs(6 * 3600);

pub struct AppState {
    pub store: Store,
    pub http: reqwest::Client,
    pub summary: Mutex<Option<RewardsSummary>>,
    pub catalog: Mutex<Option<(Instant, Catalog)>>,
    pub notified_update: Mutex<Option<String>>,
    /// Dernière actualisation des données, manuelle ou automatique.
    pub last_refresh: Mutex<Option<Instant>>,
    /// Une seule actualisation à la fois : les demandes arrivées pendant qu'une actualisation
    /// tourne attendent sa fin et reprennent son résultat.
    pub refresh_lock: tauri::async_runtime::Mutex<Option<Instant>>,
    pub xbox: xbox::XboxState,
}

pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Marqueur laissé avant une mise à jour automatique faite fenêtre fermée : au redémarrage
/// qui suit, l'application reste dans la zone de notification.
const RESTART_MINIMIZED_FILE: &str = "restart-minimized";

pub fn set_restart_minimized<R: Runtime>(app: &AppHandle<R>, minimized: bool) {
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    let path = dir.join(RESTART_MINIMIZED_FILE);
    if minimized {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(path, "");
    } else {
        let _ = std::fs::remove_file(path);
    }
}

/// Lit et efface le marqueur de redémarrage dans la zone de notification.
fn take_restart_minimized<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.path()
        .app_data_dir()
        .map(|dir| std::fs::remove_file(dir.join(RESTART_MINIMIZED_FILE)).is_ok())
        .unwrap_or(false)
}

/// Ce que fait un clic sur une notification.
#[derive(Debug, Clone)]
pub enum ClickAction {
    /// Ouvre la fenêtre principale, éventuellement sur une vue (« today », « history »…).
    OpenMain(Option<&'static str>),
    /// Ouvre une tâche dans la fenêtre Rewards, clique dessus, puis referme les fenêtres une
    /// fois la recherche Bing chargée.
    OpenTask(TaskTarget),
}

/// Tâche à ouvrir : page du site, lien de la tâche à cliquer, titre pour la retrouver et
/// texte à mettre dans le champ de recherche de Bing.
#[derive(Debug, Clone)]
pub struct TaskTarget {
    pub page: String,
    pub click: Option<String>,
    pub title: String,
    pub query: Option<String>,
}

/// Actions des notifications affichées, par identifiant de notification.
static CLICK_ACTIONS: Mutex<Vec<(i32, ClickAction)>> = Mutex::new(Vec::new());
static NEXT_NOTIFICATION_ID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);

/// Notification dont un clic ouvre la fenêtre principale.
pub fn notify<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) {
    notify_with(app, title, body, ClickAction::OpenMain(None));
}

/// Notification dont un clic déclenche `action` (tant qu'elle est à l'écran : Windows ne
/// signale pas les clics dans le centre de notifications).
pub fn notify_with<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str, action: ClickAction) {
    let id = NEXT_NOTIFICATION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    {
        let mut actions = CLICK_ACTIONS.lock().unwrap();
        actions.push((id, action));
        // Une notification disparaît de l'écran en quelques secondes : inutile d'en garder beaucoup.
        let excess = actions.len().saturating_sub(50);
        actions.drain(..excess);
    }
    if let Err(err) = app
        .notification()
        .builder()
        .id(id)
        .title(title)
        .body(body)
        .show()
    {
        log::warn!("Notification impossible : {err}");
    }
}

fn on_notification_click<R: Runtime>(app: &AppHandle<R>, id: i32) {
    let action = CLICK_ACTIONS
        .lock()
        .unwrap()
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, a)| a.clone());
    match action {
        Some(ClickAction::OpenTask(task)) => {
            let app = app.clone();
            // Création de fenêtre hors du thread de la notification (voir `login`).
            tauri::async_runtime::spawn(async move {
                let Ok(url) = url::Url::parse(&task.page) else {
                    return;
                };
                if let Err(err) = auth::open_site(
                    &app,
                    url,
                    task.click,
                    Some(task.title),
                    task.query,
                    None,
                    true,
                ) {
                    log::warn!("Ouverture de la tâche impossible : {err}");
                }
            });
        }
        Some(ClickAction::OpenMain(view)) => {
            show_main(app);
            if let Some(view) = view {
                let _ = app.emit("show-view", view);
            }
        }
        None => show_main(app),
    }
}

/// Une actualisation terminée depuis moins longtemps que ce délai est reprise telle quelle.
const REFRESH_REUSE: Duration = Duration::from_secs(5);

/// Relit le tableau de bord Rewards, met à jour le cache, le tray et l'interface.
///
/// Les appels simultanés (démarrage, bouton, rappel, connexion…) ne relancent pas une lecture
/// complète : ils attendent celle en cours et reprennent son résultat.
pub async fn refresh<R: Runtime>(app: &AppHandle<R>) -> Result<RewardsSummary, FetchError> {
    let state = app.state::<AppState>();
    let mut finished = state.refresh_lock.lock().await;
    if finished.is_some_and(|t| t.elapsed() < REFRESH_REUSE) {
        if let Some(summary) = state.summary.lock().unwrap().clone() {
            return Ok(summary);
        }
    }
    let result = refresh_now(app).await;
    *finished = Some(Instant::now());
    result
}

async fn refresh_now<R: Runtime>(app: &AppHandle<R>) -> Result<RewardsSummary, FetchError> {
    let client = app.state::<AppState>().http.clone();
    *app.state::<AppState>().last_refresh.lock().unwrap() = Some(Instant::now());
    let result = rewards::fetch(app, &client).await;
    let state = app.state::<AppState>();
    match &result {
        Ok(summary) => {
            *state.summary.lock().unwrap() = Some(summary.clone());
            tray::set_status(
                app,
                summary.available_points,
                summary.tasks_left(),
                summary.streak_pending(),
            );
            notify_unlocked_tasks(app, summary);
            state
                .store
                .record_balance(chrono::Local::now().date_naive(), summary.available_points);
            check_goal(app, summary.available_points);
            notify_expiring_quests(app, summary);
            notify_new_searches(app, summary);
            let _ = app.emit("rewards-updated", summary);
        }
        Err(FetchError::NotLoggedIn) => {
            *state.summary.lock().unwrap() = None;
            tray::set_points(app, None);
            let _ = app.emit("rewards-logged-out", ());
        }
        Err(err) => log::warn!("Lecture Rewards impossible : {err}"),
    }
    result
}

/// Prévient quand une tâche verrouillée à la dernière actualisation est devenue accessible.
fn notify_unlocked_tasks<R: Runtime>(app: &AppHandle<R>, summary: &RewardsSummary) {
    let tasks = summary.task_states();
    let locked = tasks
        .iter()
        .filter(|t| t.locked)
        .map(|t| t.key.clone())
        .collect();
    let state = app.state::<AppState>();
    let previously_locked = state.store.swap_locked_tasks(locked);
    if !state.store.settings().notify_unlocks {
        return;
    }
    let unlocked: Vec<&str> = tasks
        .iter()
        .filter(|t| !t.locked && !t.complete && previously_locked.contains(&t.key))
        .map(|t| t.label.as_str())
        .collect();
    match unlocked.as_slice() {
        [] => {}
        [one] => notify(app, "Tâche déverrouillée", one),
        many => notify(
            app,
            &format!("{} tâches déverrouillées", many.len()),
            &many.join("\n"),
        ),
    }
}

/// Page du site où les activités du jour sont cliquables (voir l'interface).
const DAILY_URL: &str = "https://rewards.bing.com/dashboard?section=dailyset";
const EARN_URL: &str = "https://rewards.bing.com/earn";

/// Recherches Bing à faire (activités et étapes de quêtes non verrouillées), par clé stable.
fn search_tasks(summary: &RewardsSummary) -> Vec<(String, TaskTarget, i64)> {
    let is_search =
        |kind: Option<rewards::TaskKind>| kind.is_some_and(rewards::TaskKind::is_search);
    let mut tasks = Vec::new();
    for (activities, page) in [(&summary.daily_set, DAILY_URL), (&summary.more, EARN_URL)] {
        for a in activities
            .iter()
            .filter(|a| !a.complete && !a.locked && is_search(a.kind))
        {
            tasks.push((
                format!("a#{}", a.id),
                TaskTarget {
                    page: page.to_string(),
                    click: a.url.clone(),
                    title: a.title.clone(),
                    query: a.search_query.clone(),
                },
                a.points,
            ));
        }
    }
    for card in &summary.punch_cards {
        let Some(page) = &card.url else { continue };
        for step in card
            .steps
            .iter()
            .filter(|s| !s.complete && !s.locked && is_search(s.kind))
        {
            tasks.push((
                format!("s#{}#{}", card.id, step.title),
                TaskTarget {
                    page: page.clone(),
                    click: step.url.clone(),
                    title: step.title.clone(),
                    query: step.search_query.clone(),
                },
                0,
            ));
        }
    }
    tasks
}

/// Au plus autant de notifications à la fois ; au-delà, une seule qui ouvre l'application.
const MAX_SEARCH_NOTIFICATIONS: usize = 3;

/// Une notification par recherche Bing apparue depuis la dernière actualisation. Un clic
/// ouvre la tâche, clique dessus dans la fenêtre Rewards et referme les fenêtres une fois la
/// recherche faite. Rien ne se lance sans ce clic.
fn notify_new_searches<R: Runtime>(app: &AppHandle<R>, summary: &RewardsSummary) {
    let state = app.state::<AppState>();
    let tasks = search_tasks(summary);
    let current = tasks.iter().map(|(key, _, _)| key.clone()).collect();
    // Premier relevé : on note l'existant sans prévenir.
    let Some(previous) = state.store.swap_known_searches(current) else {
        return;
    };
    if !state.store.settings().notify_new_searches {
        return;
    }
    let new: Vec<_> = tasks
        .into_iter()
        .filter(|(key, _, _)| !previous.contains(key))
        .collect();
    if new.len() > MAX_SEARCH_NOTIFICATIONS {
        notify_with(
            app,
            &format!("{} nouvelles recherches Bing", new.len()),
            "Cliquez pour les voir dans « Recherches à faire aujourd'hui ».",
            ClickAction::OpenMain(Some("today")),
        );
        return;
    }
    for (_, task, points) in new {
        let points = if points > 0 {
            format!(" · {points} pts")
        } else {
            String::new()
        };
        notify_with(
            app,
            "Nouvelle recherche Bing",
            &format!("{}{points}\nCliquez pour la faire.", task.title),
            ClickAction::OpenTask(task),
        );
    }
}

/// Prévient une fois quand le solde permet d'échanger l'article visé.
fn check_goal<R: Runtime>(app: &AppHandle<R>, balance: i64) {
    let store = &app.state::<AppState>().store;
    let Some(mut goal) = store.goal() else {
        return;
    };
    let reachable = balance >= goal.price;
    if reachable && !goal.notified {
        notify(
            app,
            "Objectif atteint",
            &format!(
                "Vous avez assez de points pour échanger « {} ».",
                goal.title
            ),
        );
    }
    // Après un échange, le solde repasse sous le prix : la prochaine fois sera signalée aussi.
    if reachable != goal.notified {
        goal.notified = reachable;
        store.set_goal(Some(goal));
    }
}

/// Une quête pas terminée qui expire dans moins de 48 heures est signalée une fois.
fn notify_expiring_quests<R: Runtime>(app: &AppHandle<R>, summary: &RewardsSummary) {
    let state = app.state::<AppState>();
    if !state.store.settings().notify_expiring {
        return;
    }
    let followed: Vec<String> = state
        .store
        .followed_quests()
        .into_iter()
        .map(|q| q.id)
        .collect();
    let now = chrono::Utc::now();
    for card in summary.punch_cards.iter().filter(|c| !c.complete) {
        let Some(expires) = card
            .expires_at
            .as_deref()
            .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
        else {
            continue;
        };
        let left = expires.with_timezone(&chrono::Utc) - now;
        if left <= chrono::Duration::zero() || left > chrono::Duration::hours(48) {
            continue;
        }
        if !state
            .store
            .first_alert(&format!("expire#{}#{}", card.id, expires.to_rfc3339()))
        {
            continue;
        }
        let star = if followed.contains(&card.id) {
            "★ "
        } else {
            ""
        };
        let progress = if card.total > 0 {
            format!("{}/{} tâches, ", card.done, card.total)
        } else {
            String::new()
        };
        notify(
            app,
            "Quête bientôt terminée",
            &format!(
                "{star}{} : {progress}expire dans {} h",
                card.title,
                left.num_hours().max(1)
            ),
        );
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryDay {
    date: chrono::NaiveDate,
    balance: i64,
}

/// Solde relevé chaque jour, du plus ancien au plus récent.
#[tauri::command]
fn points_history(state: State<AppState>) -> Vec<HistoryDay> {
    state
        .store
        .points_history()
        .into_iter()
        .map(|(date, balance)| HistoryDay { date, balance })
        .collect()
}

/// Depuis la mini-fenêtre : ouvre la fenêtre principale, sur une vue donnée.
#[tauri::command]
fn open_main(app: AppHandle, view: Option<String>) {
    mini::hide(&app);
    show_main(&app);
    if let Some(view) = view {
        let _ = app.emit("show-view", view);
    }
}

/// Depuis la mini-fenêtre : lance la session guidée, pilotée par la fenêtre principale.
#[tauri::command]
fn start_guided(app: AppHandle) {
    mini::hide(&app);
    let _ = app.emit("guided-start", ());
}

#[tauri::command]
fn hide_mini(app: AppHandle) {
    mini::hide(&app);
}

#[tauri::command]
fn stop_guided(app: AppHandle) {
    auth::stop_guide(&app);
}

#[tauri::command]
fn get_goal(state: State<AppState>) -> Option<store::Goal> {
    state.store.goal()
}

/// Choisit (ou retire avec `None`) l'article visé.
#[tauri::command]
fn set_goal(app: AppHandle, goal: Option<store::Goal>) -> Option<store::Goal> {
    let state = app.state::<AppState>();
    state.store.set_goal(goal.map(|g| store::Goal {
        notified: false,
        ..g
    }));
    if let Some(summary) = state.summary.lock().unwrap().clone() {
        check_goal(&app, summary.available_points);
    }
    state.store.goal()
}

#[tauri::command]
async fn refresh_rewards(app: AppHandle, force: Option<bool>) -> Result<RewardsSummary, String> {
    if force.unwrap_or(false) {
        rewards::forget_quest_pages();
    }
    refresh(&app).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn cached_rewards(state: State<AppState>) -> Option<RewardsSummary> {
    state.summary.lock().unwrap().clone()
}

#[tauri::command]
async fn redeem_catalog(app: AppHandle, force: bool) -> Result<Catalog, String> {
    let state = app.state::<AppState>();
    if !force {
        if let Some((fetched, catalog)) = state.catalog.lock().unwrap().as_ref() {
            if fetched.elapsed() < CATALOG_MAX_AGE {
                return Ok(catalog.clone());
            }
        }
    }
    let client = state.http.clone();
    let catalog = catalog::fetch(&app, &client)
        .await
        .map_err(|e| e.to_string())?;
    *state.catalog.lock().unwrap() = Some((Instant::now(), catalog.clone()));
    Ok(catalog)
}

/// Doit rester `async` : sous Windows, créer une fenêtre depuis une commande synchrone
/// bloque WebView2 et la fenêtre de connexion reste blanche.
#[tauri::command]
async fn login(app: AppHandle) -> Result<(), String> {
    auth::open_login(&app).map_err(|e| e.to_string())
}

#[tauri::command]
async fn logout(app: AppHandle) -> Result<(), String> {
    auth::logout(&app).await?;
    // La déconnexion vaut aussi pour Xbox Live, dont le jeton est rangé à part.
    xbox::logout(&app);
    let state = app.state::<AppState>();
    *state.summary.lock().unwrap() = None;
    *state.catalog.lock().unwrap() = None;
    tray::set_points(&app, None);
    let _ = app.emit("rewards-logged-out", ());
    Ok(())
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.store.settings()
}

#[tauri::command]
fn save_settings(state: State<AppState>, settings: Settings) {
    state.store.set_settings(settings);
}

#[tauri::command]
fn xbox_progress(state: State<AppState>) -> XboxProgress {
    state.store.xbox_progress()
}

#[tauri::command]
fn set_xbox_today(state: State<AppState>, game: Option<String>) -> XboxProgress {
    state.store.set_xbox_today(game);
    state.store.xbox_progress()
}

/// Coche ou décoche un jour de la semaine en cours dans le suivi Xbox manuel.
#[tauri::command]
fn set_xbox_day(state: State<AppState>, date: chrono::NaiveDate, played: bool) -> XboxProgress {
    // Un jour coché après coup garde le jeu déjà noté, sinon reste sans nom de jeu.
    let game = played.then(String::new);
    if !(played
        && state
            .store
            .xbox_progress()
            .week
            .iter()
            .any(|d| d.date == date && d.played))
    {
        state.store.set_xbox_day(date, game);
    }
    state.store.xbox_progress()
}

#[tauri::command]
fn xbox_status(app: AppHandle) -> xbox::XboxStatus {
    xbox::status(&app)
}

/// Doit rester `async` : la fenêtre de connexion est créée ici (voir `login`).
#[tauri::command]
async fn xbox_login(app: AppHandle) -> Result<(), String> {
    xbox::open_login(&app).map_err(|e| e.to_string())
}

#[tauri::command]
fn xbox_logout(app: AppHandle) {
    xbox::logout(&app);
}

/// Quêtes Xbox ; les données de moins de 5 minutes sont reprises du cache sauf si `force`.
#[tauri::command]
async fn xbox_rewards(app: AppHandle, force: bool) -> Result<xbox::XboxRewards, String> {
    if !force {
        if let Some(rewards) = xbox::cached(&app, Duration::from_secs(300)) {
            return Ok(rewards);
        }
    }
    xbox::refresh(&app).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn followed_quests(state: State<AppState>) -> Vec<FollowedQuest> {
    state.store.followed_quests()
}

#[tauri::command]
fn follow_quest(
    state: State<AppState>,
    id: String,
    title: String,
    url: Option<String>,
    image_url: Option<String>,
    follow: bool,
) -> Vec<FollowedQuest> {
    state
        .store
        .set_followed(&id, &title, url, image_url, follow)
}

#[tauri::command]
async fn send_test_reminder(app: AppHandle) -> Vec<String> {
    let settings = app.state::<AppState>().store.settings();
    reminders::send(&app, &settings).await
}

#[tauri::command]
fn autostart_enabled(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let launcher = app.autolaunch();
    let result = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result.map_err(|e| e.to_string())
}

#[tauri::command]
async fn check_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    updater::check(&app).await
}

#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    updater::install(&app).await
}

/// Ouvre une page du site Rewards (activité, quête, série) dans la fenêtre intégrée.
/// Doit rester `async` : la fenêtre est créée ici (voir `login`).
#[tauri::command]
async fn open_rewards_site(
    app: AppHandle,
    url: String,
    click: Option<String>,
    click_title: Option<String>,
    query: Option<String>,
    guide: Option<auth::GuideStep>,
) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|e| e.to_string())?;
    if parsed.scheme() != "https" {
        return Err(format!("Adresse non autorisée : {url}"));
    }
    auth::open_site(&app, parsed, click, click_title, query, guide, false)
        .map_err(|e| e.to_string())
}

/// Ouvre un lien dans le navigateur. Les liens viennent des pages Microsoft Rewards
/// (boutons des quêtes vers Bing, YouTube, Spotify…) : seul le https est exigé.
#[tauri::command]
fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|e| e.to_string())?;
    if parsed.scheme() != "https" {
        return Err(format!("Adresse non autorisée : {url}"));
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_xbox_app() -> Result<(), String> {
    std::process::Command::new("explorer.exe")
        .arg(format!("shell:AppsFolder\\{XBOX_APP_AUMID}"))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Impossible d'ouvrir l'app Xbox : {e}"))
}

#[tauri::command]
fn open_logs(app: AppHandle) -> Result<(), String> {
    let dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    let _ = std::fs::create_dir_all(&dir);
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: String,
}

#[tauri::command]
fn app_info(app: AppHandle) -> AppInfo {
    AppInfo {
        version: app.package_info().version.to_string(),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Un second lancement réaffiche la fenêtre existante au lieu d'ouvrir une autre instance.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app)
        }))
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                // Assez d'historique pour diagnostiquer un problème survenu il y a quelques jours.
                .max_file_size(1_000_000)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(3))
                .timezone_strategy(tauri_plugin_log::TimezoneStrategy::UseLocal)
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![MINIMIZED_ARG]),
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let state_file = app.path().app_data_dir()?.join("state.json");
            app.manage(AppState {
                store: Store::load(state_file),
                http: rewards::http_client(),
                summary: Mutex::new(None),
                catalog: Mutex::new(None),
                notified_update: Mutex::new(None),
                last_refresh: Mutex::new(None),
                refresh_lock: tauri::async_runtime::Mutex::new(None),
                xbox: xbox::XboxState::default(),
            });
            // Clic sur une notification encore à l'écran.
            let handle = app.handle().clone();
            let _ = app.notification().on_action(move |performed| {
                if performed.action_id() == "tap" {
                    if let Some(notification) = performed.notification() {
                        on_notification_click(&handle, notification.id());
                    }
                }
            });
            if let Some(warning) = app.state::<AppState>().store.load_warning() {
                notify(app.handle(), "Rewards Autoplay", warning);
            }
            tray::create(app.handle())?;
            mini::create(app.handle())?;
            let restarted_minimized = take_restart_minimized(app.handle());
            if !restarted_minimized && !std::env::args().any(|arg| arg == MINIMIZED_ARG) {
                show_main(app.handle());
            }
            reminders::spawn(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            // Fermer la fenêtre principale (ou la mini-fenêtre) la cache : l'app continue dans le tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" || window.label() == mini::LABEL {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            refresh_rewards,
            cached_rewards,
            redeem_catalog,
            login,
            logout,
            get_settings,
            save_settings,
            xbox_progress,
            set_xbox_today,
            set_xbox_day,
            followed_quests,
            follow_quest,
            xbox_status,
            xbox_login,
            xbox_logout,
            xbox_rewards,
            send_test_reminder,
            autostart_enabled,
            set_autostart,
            check_update,
            install_update,
            open_url,
            open_rewards_site,
            open_xbox_app,
            open_logs,
            app_info,
            points_history,
            get_goal,
            set_goal,
            open_main,
            start_guided,
            stop_guided,
            hide_mini,
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de Rewards Autoplay");
}
