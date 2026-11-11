//! Mise à jour depuis les GitHub Releases, vérifiée par la signature de l'updater Tauri.

use std::sync::Mutex;

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_updater::UpdaterExt;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
}

pub async fn check<R: Runtime>(app: &AppHandle<R>) -> Result<Option<UpdateInfo>, String> {
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?;
    Ok(update.map(|u| UpdateInfo {
        version: u.version.clone(),
        current_version: u.current_version.clone(),
        notes: u.body.clone(),
        date: u.date.map(|d| d.to_string()),
    }))
}

/// Manifeste lu par l'updater (même adresse que `plugins.updater.endpoints`).
const MANIFEST_URL: &str =
    "https://github.com/powange/microsoft-rewards/releases/latest/download/latest.json";

/// ETag de la dernière lecture du manifeste.
static MANIFEST_ETAG: Mutex<Option<String>> = Mutex::new(None);

/// Lecture légère avant la vérification complète, qui peut tourner chaque minute : requête
/// conditionnelle (ETag), puis comparaison de la version annoncée. En cas de doute (réseau,
/// format inattendu), la vérification complète a lieu.
async fn manifest_announces_new_version<R: Runtime>(app: &AppHandle<R>) -> bool {
    let client = app.state::<crate::AppState>().http.clone();
    let mut request = client.get(MANIFEST_URL);
    if let Some(etag) = MANIFEST_ETAG.lock().unwrap().clone() {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let Ok(response) = request.send().await else {
        return true;
    };
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return false;
    }
    if !response.status().is_success() {
        return true;
    }
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let version = response
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|json| json.get("version")?.as_str().map(str::to_string));
    *MANIFEST_ETAG.lock().unwrap() = etag;
    version.is_none_or(|v| v != app.package_info().version.to_string())
}

/// Vérification périodique.
///
/// Avec `auto_install`, une mise à jour trouvée est installée tout de suite, puis
/// l'application redémarre. Sinon, l'utilisateur est prévenu une seule fois par version.
pub async fn background_check<R: Runtime>(app: &AppHandle<R>, auto_install: bool) {
    if !manifest_announces_new_version(app).await {
        return;
    }
    let info = match check(app).await {
        Ok(Some(info)) => info,
        Ok(None) => return,
        Err(err) => {
            log::warn!("Vérification des mises à jour impossible : {err}");
            return;
        }
    };
    // Une version déjà signalée a échoué à s'installer seule : pas de nouvel essai chaque
    // minute, la bannière permet de l'installer à la main.
    let already_tried = app
        .state::<crate::AppState>()
        .notified_update
        .lock()
        .unwrap()
        .as_deref()
        == Some(info.version.as_str());
    if auto_install && !already_tried {
        log::info!("Installation automatique de la version {}", info.version);
        crate::notify(
            app,
            "Mise à jour en cours",
            &format!(
                "Rewards Autoplay installe la version {} et va redémarrer.",
                info.version
            ),
        );
        // Fenêtre fermée : l'application redémarre directement dans la zone de notification.
        let hidden = !app
            .get_webview_window("main")
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false);
        crate::set_restart_minimized(app, hidden);
        match install(app).await {
            Ok(()) => return,
            Err(err) => {
                crate::set_restart_minimized(app, false);
                log::warn!("Installation automatique impossible : {err}");
            }
        }
    }
    let _ = app.emit("update-available", &info);
    let state = app.state::<crate::AppState>();
    let mut notified = state.notified_update.lock().unwrap();
    if notified.as_deref() != Some(info.version.as_str()) {
        crate::notify(
            app,
            "Mise à jour disponible",
            &format!(
                "La version {} de Rewards Autoplay est prête à être installée.",
                info.version
            ),
        );
        *notified = Some(info.version);
    }
}

/// Télécharge, installe puis redémarre l'application.
pub async fn install<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Aucune mise à jour disponible.")?;

    let progress = app.clone();
    let mut downloaded: u64 = 0;
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress.emit(
                    "update-progress",
                    json!({ "downloaded": downloaded, "total": total }),
                );
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;
    app.restart()
}
