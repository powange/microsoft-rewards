//! Mise à jour depuis les GitHub Releases, vérifiée par la signature de l'updater Tauri.

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

/// Vérification périodique : prévient une seule fois par version trouvée.
///
/// Renvoie `true` quand une mise à jour est disponible. Avec `auto_install`, elle sera
/// installée par [`auto_install_when_idle`] dès que l'application n'a plus de fenêtre ouverte.
pub async fn background_check<R: Runtime>(app: &AppHandle<R>, auto_install: bool) -> bool {
    match check(app).await {
        Ok(Some(info)) => {
            let _ = app.emit("update-available", &info);
            let state = app.state::<crate::AppState>();
            let mut notified = state.notified_update.lock().unwrap();
            if notified.as_deref() != Some(info.version.as_str()) {
                let body = if auto_install {
                    format!(
                        "La version {} de Rewards Autoplay s'installera dès que la fenêtre sera fermée.",
                        info.version
                    )
                } else {
                    format!(
                        "La version {} de Rewards Autoplay est prête à être installée.",
                        info.version
                    )
                };
                crate::notify(app, "Mise à jour disponible", &body);
                *notified = Some(info.version);
            }
            true
        }
        Ok(None) => false,
        Err(err) => {
            log::warn!("Vérification des mises à jour impossible : {err}");
            false
        }
    }
}

/// Installe la mise à jour en attente si aucune fenêtre de l'application n'est affichée
/// (fenêtre principale, fenêtre Rewards, connexion), pour ne pas interrompre l'utilisateur.
/// Renvoie `true` si l'installation reste à faire.
pub async fn auto_install_when_idle<R: Runtime>(app: &AppHandle<R>) -> bool {
    let busy = app
        .webview_windows()
        .values()
        .any(|w| w.is_visible().unwrap_or(false));
    if busy {
        return true;
    }
    log::info!("Installation automatique de la mise à jour");
    crate::notify(
        app,
        "Mise à jour en cours",
        "Rewards Autoplay installe la nouvelle version et va redémarrer.",
    );
    if let Err(err) = install(app).await {
        // Nouvel essai à la prochaine vérification des mises à jour.
        log::warn!("Installation automatique impossible : {err}");
    }
    false
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
