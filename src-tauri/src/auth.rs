//! Connexion au compte Microsoft via la vraie page de connexion, dans une fenêtre WebView2.
//!
//! L'app ne voit jamais le mot de passe : elle lit seulement les cookies de session une fois
//! la connexion terminée. Cette fenêtre n'a accès à aucune commande de l'application.

use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use url::Url;

const LOGIN_LABEL: &str = "login";
const LOGIN_URL: &str = "https://rewards.bing.com/auth/login";

pub fn open_login<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(LOGIN_LABEL) {
        window.show()?;
        return window.set_focus();
    }
    WebviewWindowBuilder::new(
        app,
        LOGIN_LABEL,
        WebviewUrl::External(Url::parse(LOGIN_URL).unwrap()),
    )
    .title("Connexion à Microsoft Rewards")
    .inner_size(520.0, 760.0)
    .center()
    .on_page_load(|window, payload| {
        // Le retour sur rewards.bing.com signale une connexion probablement réussie :
        // on le confirme en lisant le tableau de bord.
        if payload.event() != PageLoadEvent::Finished
            || payload.url().host_str() != Some("rewards.bing.com")
        {
            return;
        }
        let app = window.app_handle().clone();
        tauri::async_runtime::spawn(async move {
            if crate::refresh(&app).await.is_ok() {
                if let Some(login) = app.get_webview_window(LOGIN_LABEL) {
                    let _ = login.close();
                }
                crate::show_main(&app);
            }
        });
    })
    .build()?;
    Ok(())
}

/// Supprime les cookies et données de navigation, ce qui déconnecte le compte Microsoft.
pub async fn logout<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or("fenêtre principale introuvable")?;
    tauri::async_runtime::spawn_blocking(move || window.clear_all_browsing_data())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

const SITE_LABEL: &str = "rewards-site";

/// Ouvre une page du site Rewards dans une fenêtre de l'application, connectée au compte.
///
/// C'est le site qui valide une activité au moment du clic (le lien de destination seul ne
/// rapporte rien, il porte souvent `rnoreward=1`) : l'utilisateur clique donc sur l'activité
/// dans cette fenêtre, comme sur le site. Chaque page chargée et la fermeture de la fenêtre
/// sont signalées à l'interface, qui actualise les données.
pub fn open_site<R: Runtime>(app: &AppHandle<R>, url: Url) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(SITE_LABEL) {
        window.navigate(url)?;
        window.unminimize()?;
        window.show()?;
        return window.set_focus();
    }
    let handle = app.clone();
    let window = WebviewWindowBuilder::new(app, SITE_LABEL, WebviewUrl::External(url))
        .title("Microsoft Rewards")
        .inner_size(1100.0, 820.0)
        .center()
        .on_page_load(move |_, payload| {
            if payload.event() == PageLoadEvent::Finished {
                let _ = handle.emit("rewards-site-navigated", payload.url().as_str());
            }
        })
        .build()?;
    let handle = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Destroyed = event {
            let _ = handle.emit("rewards-site-closed", ());
        }
    });
    Ok(())
}
