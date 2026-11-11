//! Connexion au compte Microsoft via la vraie page de connexion, dans une fenêtre WebView2.
//!
//! L'app ne voit jamais le mot de passe : elle lit seulement les cookies de session une fois
//! la connexion terminée. Cette fenêtre n'a accès à aucune commande de l'application.

use std::sync::Mutex;
use tauri::webview::{NewWindowResponse, PageLoadEvent};
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

/// Lien de la tâche sur laquelle cliquer au prochain chargement de la fenêtre Rewards.
static PENDING_CLICK: Mutex<Option<String>> = Mutex::new(None);

/// Script injecté dans la page Rewards : attend que la tâche dont le lien est `target` soit
/// affichée et active (React branché), puis clique dessus comme l'utilisateur le ferait.
/// C'est ce clic qui fait valider l'activité par le site, qui ouvre ensuite la recherche Bing.
const CLICK_SCRIPT: &str = r#"(() => {
  const target = __TARGET__;
  const norm = (u) => {
    try {
      const x = new URL(u, location.href);
      const params = [...x.searchParams].map(([k, v]) => [k.toLowerCase(), v])
        .filter(([k]) => k !== "rnoreward").sort();
      return (x.host.replace(/^www\./, "") + x.pathname.replace(/\/$/, "") + "?"
        + params.map((p) => p.join("=")).join("&")).toLowerCase();
    } catch { return ""; }
  };
  const want = norm(target);
  // Repli : même page de destination et même requête `q`, quand les autres paramètres du
  // lien ont changé entre l'actualisation et l'affichage de la page.
  const loose = (u) => {
    try {
      const x = new URL(u, location.href);
      return (x.host.replace(/^www\./, "") + x.pathname.replace(/\/$/, "") + "?"
        + (x.searchParams.get("q") || "")).toLowerCase();
    } catch { return ""; }
  };
  const wantLoose = (() => {
    try { return new URL(target).searchParams.get("q") ? loose(target) : null; } catch { return null; }
  })();
  const props = (el) => {
    const key = Object.keys(el).find((k) => k.startsWith("__reactProps"));
    return key ? el[key] : null;
  };
  const find = () => exact() || looseMatch();
  const looseMatch = () => {
    if (!wantLoose) return null;
    const hits = [...document.querySelectorAll("a[href]")]
      .filter((el) => loose(el.getAttribute("href")) === wantLoose);
    return hits.length === 1 ? { el: hits[0], ready: Boolean(props(hits[0])) } : null;
  };
  const exact = () => {
    for (const el of document.querySelectorAll("a[href], button, [role=button], [role=link]")) {
      const p = props(el);
      const link = el.getAttribute("href") || (p && (p.href || p.destination));
      if (link && norm(link) === want) return { el, ready: Boolean(p) };
    }
    for (const el of document.querySelectorAll("*")) {
      const p = props(el);
      const link = p && (p.href || p.destination);
      if (typeof link === "string" && norm(link) === want) {
        return { el: el.querySelector("a, button, [role=button]") || el, ready: true };
      }
    }
    return null;
  };
  const started = Date.now();
  const tick = () => {
    const found = find();
    if (found && found.ready) {
      found.el.scrollIntoView({ block: "center" });
      found.el.style.outline = "3px solid #4cc28a";
      setTimeout(() => found.el.click(), 700);
      return;
    }
    if (Date.now() - started < 20000) setTimeout(tick, 300);
  };
  tick();
})();"#;

/// Ouvre une page du site Rewards dans une fenêtre de l'application, connectée au compte.
///
/// C'est le site qui valide une activité au moment du clic (le lien de destination seul ne
/// rapporte rien, il porte souvent `rnoreward=1`). Avec `click`, l'application clique
/// elle-même sur la tâche qui a ce lien une fois la page chargée ; sinon l'utilisateur clique
/// dans la fenêtre. Chaque page chargée et la fermeture de la fenêtre sont signalées à
/// l'interface, qui actualise les données.
pub fn open_site<R: Runtime>(
    app: &AppHandle<R>,
    url: Url,
    click: Option<String>,
) -> tauri::Result<()> {
    *PENDING_CLICK.lock().unwrap() = click;
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
        // Le site ouvre la recherche Bing, le quiz… dans une nouvelle fenêtre après avoir
        // validé le clic. Sans ce gestionnaire, WebView2 bloque ces fenêtres en silence.
        // La fenêtre ouverte partage la session : la recherche est faite avec le compte connecté.
        .on_new_window({
            let handle = app.clone();
            move |url, _| {
                if url.scheme() != "https" {
                    return NewWindowResponse::Deny;
                }
                let _ = handle.emit("rewards-site-navigated", url.as_str());
                NewWindowResponse::Allow
            }
        })
        .on_page_load(move |window, payload| {
            if payload.event() != PageLoadEvent::Finished {
                return;
            }
            let _ = handle.emit("rewards-site-navigated", payload.url().as_str());
            if payload.url().host_str() == Some("rewards.bing.com") {
                if let Some(target) = PENDING_CLICK.lock().unwrap().take() {
                    let target = serde_json::to_string(&target).unwrap_or_default();
                    let _ = window.eval(CLICK_SCRIPT.replace("__TARGET__", &target));
                }
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
