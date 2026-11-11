mod auth;
mod catalog;
mod flight;
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
    pub xbox: xbox::XboxState,
}

pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn notify<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) {
    if let Err(err) = app.notification().builder().title(title).body(body).show() {
        log::warn!("Notification impossible : {err}");
    }
}

/// Relit le tableau de bord Rewards, met à jour le cache, le tray et l'interface.
pub async fn refresh<R: Runtime>(app: &AppHandle<R>) -> Result<RewardsSummary, FetchError> {
    let client = app.state::<AppState>().http.clone();
    *app.state::<AppState>().last_refresh.lock().unwrap() = Some(Instant::now());
    let result = rewards::fetch(app, &client).await;
    let state = app.state::<AppState>();
    match &result {
        Ok(summary) => {
            *state.summary.lock().unwrap() = Some(summary.clone());
            tray::set_points(app, Some(summary.available_points));
            notify_unlocked_tasks(app, summary);
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

#[tauri::command]
async fn refresh_rewards(app: AppHandle) -> Result<RewardsSummary, String> {
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
    query: Option<String>,
) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|e| e.to_string())?;
    if parsed.scheme() != "https" {
        return Err(format!("Adresse non autorisée : {url}"));
    }
    auth::open_site(&app, parsed, click, query).map_err(|e| e.to_string())
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
                xbox: xbox::XboxState::default(),
            });
            tray::create(app.handle())?;
            if !std::env::args().any(|arg| arg == MINIMIZED_ARG) {
                show_main(app.handle());
            }
            reminders::spawn(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            // Fermer la fenêtre principale la cache : l'app continue dans le tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
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
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de Rewards Autoplay");
}
