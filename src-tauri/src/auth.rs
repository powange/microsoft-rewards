//! Connexion au compte Microsoft via la vraie page de connexion, dans une fenêtre WebView2.
//!
//! L'app ne voit jamais le mot de passe : elle lit seulement les cookies de session une fois
//! la connexion terminée. Cette fenêtre n'a accès à aucune commande de l'application.

use std::sync::atomic::{AtomicUsize, Ordering};
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

/// Lien et titre de la tâche sur laquelle cliquer au prochain chargement de la fenêtre Rewards.
static PENDING_CLICK: Mutex<Option<(String, Option<String>)>> = Mutex::new(None);

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
  // Plusieurs éléments peuvent avoir le même lien : toutes les cartes « Explorer sur Bing »
  // mènent à l'accueil de Bing, plusieurs étapes d'une quête aussi. On garde celui qui
  // contient le titre de la tâche, dans son propre bloc. Si l'ambiguïté reste, on ne clique pas.
  const clean = (s) => (s || "").replace(/[\u200b\s]+/g, " ").trim().toLowerCase();
  const rawTitle = __TITLE__;
  const wantTitle = clean(rawTitle);
  const owns = (el, all) => {
    if (!wantTitle) return false;
    for (let n = el, depth = 0; n && depth < 8; n = n.parentElement, depth++) {
      if (all.some((o) => o !== el && n.contains(o))) return false;
      if (clean(n.textContent).includes(wantTitle)) return true;
    }
    return false;
  };
  const pick = (els) => {
    const list = [...new Set(els)];
    if (!list.length) return null;
    const mine = list.filter((el) => owns(el, list));
    if (mine.length) return mine[0];
    return list.length === 1 ? list[0] : null;
  };
  const result = (el) => (el ? { el, ready: Boolean(props(el)) } : null);
  const find = () => exact() || looseMatch();
  const looseMatch = () => {
    if (!wantLoose) return null;
    return result(pick([...document.querySelectorAll("a[href]")]
      .filter((el) => loose(el.getAttribute("href")) === wantLoose)));
  };
  const exact = () => {
    const direct = [...document.querySelectorAll("a[href], button, [role=button], [role=link]")]
      .filter((el) => {
        const p = props(el);
        const link = el.getAttribute("href") || (p && (p.href || p.destination));
        return link && norm(link) === want;
      });
    if (direct.length) return result(pick(direct));
    const nested = [...document.querySelectorAll("*")]
      .filter((el) => {
        const p = props(el);
        const link = p && (p.href || p.destination);
        return typeof link === "string" && norm(link) === want;
      })
      .map((el) => el.querySelector("a, button, [role=button]") || el);
    const el = pick(nested);
    return el ? { el, ready: true } : null;
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
    else notFound();
  };
  // Tâche introuvable : on le dit plutôt que de laisser l'utilisateur attendre un clic.
  const notFound = () => {
    const bar = document.createElement("div");
    bar.setAttribute("role", "status");
    bar.style.cssText = "position:fixed;left:50%;top:16px;transform:translateX(-50%);z-index:2147483647;"
      + "max-width:min(640px,calc(100% - 32px));padding:12px 44px 12px 16px;border-radius:10px;"
      + "background:#1f2a24;color:#fff;font:14px/1.4 system-ui,sans-serif;box-shadow:0 6px 24px rgba(0,0,0,.35)";
    bar.textContent = "Rewards Autoplay n'a pas trouvé "
      + (rawTitle ? "« " + rawTitle + " »" : "la tâche")
      + " sur cette page. Cliquez dessus vous-même : c'est ce clic qui valide les points.";
    const close = document.createElement("button");
    close.type = "button";
    close.textContent = "×";
    close.setAttribute("aria-label", "Fermer");
    close.style.cssText = "position:absolute;right:8px;top:6px;border:0;background:none;color:#fff;font-size:20px;cursor:pointer";
    close.onclick = () => bar.remove();
    bar.append(close);
    document.body.append(bar);
  };
  tick();
})();"#;

/// Étape en cours d'une session guidée (« Faire mes tâches »).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct GuideStep {
    pub index: usize,
    pub total: usize,
    pub title: String,
}

static GUIDE: Mutex<Option<GuideStep>> = Mutex::new(None);

/// Arrête la session guidée et retire son bandeau de la fenêtre Rewards.
pub fn stop_guide<R: Runtime>(app: &AppHandle<R>) {
    *GUIDE.lock().unwrap() = None;
    if let Some(window) = app.get_webview_window(SITE_LABEL) {
        let _ = window.eval("document.getElementById('rewards-autoplay-guide')?.remove()");
    }
}

/// Adresse réservée : les boutons du bandeau y naviguent, l'application intercepte la
/// navigation (la page Rewards n'a aucun accès aux commandes de l'application).
const GUIDE_PATH: &str = "/__rewards-autoplay/";

/// Bandeau de la session guidée, en bas de la page Rewards.
const GUIDE_SCRIPT: &str = r##"(() => {
  const step = __STEP__;
  document.getElementById("rewards-autoplay-guide")?.remove();
  const bar = document.createElement("div");
  bar.id = "rewards-autoplay-guide";
  bar.setAttribute("role", "region");
  bar.setAttribute("aria-label", "Session guidée Rewards Autoplay");
  bar.style.cssText = "position:fixed;left:50%;bottom:16px;transform:translateX(-50%);z-index:2147483647;"
    + "display:flex;align-items:center;gap:12px;flex-wrap:wrap;max-width:calc(100% - 32px);"
    + "padding:10px 12px 10px 16px;border-radius:12px;background:#1f2a24;color:#fff;"
    + "font:14px/1.4 system-ui,sans-serif;box-shadow:0 6px 24px rgba(0,0,0,.35)";
  const text = document.createElement("span");
  text.textContent = "Tâche " + (step.index + 1) + " / " + step.total + " · " + step.title;
  const go = (action) => () => { location.href = location.origin + "/__rewards-autoplay/" + action; };
  const button = (label, action, primary) => {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = label;
    b.onclick = go(action);
    b.style.cssText = "border:0;border-radius:8px;padding:6px 12px;cursor:pointer;font:600 14px system-ui,sans-serif;"
      + (primary ? "background:#4cc28a;color:#0b130f" : "background:transparent;color:#fff;border:1px solid #5b6b62");
    return b;
  };
  const last = step.index + 1 >= step.total;
  bar.append(text, button(last ? "Terminer ✓" : "Tâche suivante ▸", last ? "end" : "next", true));
  if (!last) bar.append(button("Arrêter", "end", false));
  document.body.append(bar);
})();"##;

/// Texte à chercher pour une tâche « recherchez sur Bing … », mis dans le champ de recherche
/// de Bing quand la page s'ouvre. Il n'est jamais envoyé : l'utilisateur valide lui-même.
static PENDING_QUERY: Mutex<Option<String>> = Mutex::new(None);

/// Préfixe des fenêtres de recherche ouvertes par le site Rewards.
const SEARCH_LABEL: &str = "bing-search";
static SEARCH_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Script injecté dans une page Bing : si le champ de recherche est vide, y met `query`,
/// lui donne le focus et sélectionne le texte, sans lancer la recherche.
const FILL_SCRIPT: &str = r##"(() => {
  const query = __QUERY__;
  const started = Date.now();
  const tick = () => {
    const box = document.querySelector("#sb_form_q, textarea[name=q], input[name=q]");
    if (box) {
      if (!box.value.trim()) {
        const proto = box instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(proto, "value").set.call(box, query);
        box.dispatchEvent(new Event("input", { bubbles: true }));
      }
      box.focus();
      box.select();
      return;
    }
    if (Date.now() - started < 15000) setTimeout(tick, 300);
  };
  tick();
})();"##;

/// Remplit le champ de recherche si la page chargée est Bing et qu'un texte est en attente.
fn fill_pending_query<R: Runtime>(window: &tauri::WebviewWindow<R>, url: &Url) {
    let host = url.host_str().unwrap_or_default();
    let is_bing = host == "bing.com" || host == "www.bing.com";
    if !is_bing {
        return;
    }
    if let Some(query) = PENDING_QUERY.lock().unwrap().take() {
        let query = serde_json::to_string(&query).unwrap_or_default();
        let _ = window.eval(FILL_SCRIPT.replace("__QUERY__", &query));
    }
}

/// Fenêtre de recherche demandée par le site (recherche Bing, quiz…). L'application la crée
/// elle-même, dans la même session WebView2, pour pouvoir y remplir le champ de recherche.
/// Une seule à la fois : la précédente est fermée, pour ne pas les accumuler.
fn open_search_window<R: Runtime>(
    app: &AppHandle<R>,
    features: tauri::webview::NewWindowFeatures,
) -> tauri::Result<tauri::WebviewWindow<R>> {
    for (label, window) in app.webview_windows() {
        if label.starts_with(SEARCH_LABEL) {
            let _ = window.close();
        }
    }
    let label = format!(
        "{SEARCH_LABEL}-{}",
        SEARCH_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let handle = app.clone();
    WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::External(Url::parse("about:blank").unwrap()),
    )
    .title("Recherche Bing")
    .inner_size(1100.0, 820.0)
    .window_features(features)
    .on_document_title_changed(|window, title| {
        let _ = window.set_title(&title);
    })
    .on_new_window(|url, _| {
        if url.scheme() == "https" {
            NewWindowResponse::Allow
        } else {
            NewWindowResponse::Deny
        }
    })
    .on_page_load(move |window, payload| {
        if payload.event() != PageLoadEvent::Finished {
            return;
        }
        let _ = handle.emit("rewards-site-navigated", payload.url().as_str());
        fill_pending_query(&window, payload.url());
    })
    .build()
}

/// Ouvre une page du site Rewards dans une fenêtre de l'application, connectée au compte.
///
/// C'est le site qui valide une activité au moment du clic (le lien de destination seul ne
/// rapporte rien, il porte souvent `rnoreward=1`). Avec `click`, l'application clique
/// elle-même sur la tâche qui a ce lien une fois la page chargée ; sinon l'utilisateur clique
/// dans la fenêtre. Avec `query`, le champ de recherche de la page Bing ouverte ensuite est
/// prérempli. Chaque page chargée et la fermeture de la fenêtre sont signalées à
/// l'interface, qui actualise les données.
pub fn open_site<R: Runtime>(
    app: &AppHandle<R>,
    url: Url,
    click: Option<String>,
    click_title: Option<String>,
    query: Option<String>,
    guide: Option<GuideStep>,
) -> tauri::Result<()> {
    // Une tâche ouverte hors session guidée met fin à la session.
    *GUIDE.lock().unwrap() = guide;
    *PENDING_CLICK.lock().unwrap() = click.map(|url| (url, click_title));
    *PENDING_QUERY.lock().unwrap() = query.filter(|q| !q.trim().is_empty());
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
        // Boutons du bandeau de la session guidée.
        .on_navigation({
            let handle = app.clone();
            move |url| {
                let Some(action) = url.path().strip_prefix(GUIDE_PATH) else {
                    return true;
                };
                match action {
                    "next" => {
                        let _ = handle.emit("guided-next", ());
                    }
                    _ => {
                        stop_guide(&handle);
                        let _ = handle.emit("guided-end", ());
                    }
                }
                false
            }
        })
        // Le site ouvre la recherche Bing, le quiz… dans une nouvelle fenêtre après avoir
        // validé le clic. Sans ce gestionnaire, WebView2 bloque ces fenêtres en silence.
        .on_new_window({
            let handle = app.clone();
            move |url, features| {
                if url.scheme() != "https" {
                    return NewWindowResponse::Deny;
                }
                let _ = handle.emit("rewards-site-navigated", url.as_str());
                match open_search_window(&handle, features) {
                    Ok(window) => NewWindowResponse::Create { window },
                    Err(err) => {
                        log::warn!("Fenêtre de recherche impossible à créer : {err}");
                        NewWindowResponse::Allow
                    }
                }
            }
        })
        .on_page_load(move |window, payload| {
            if payload.event() != PageLoadEvent::Finished {
                return;
            }
            let _ = handle.emit("rewards-site-navigated", payload.url().as_str());
            if payload.url().host_str() == Some("rewards.bing.com") {
                if let Some((target, title)) = PENDING_CLICK.lock().unwrap().take() {
                    let target = serde_json::to_string(&target).unwrap_or_default();
                    let title = serde_json::to_string(&title).unwrap_or_default();
                    let script = CLICK_SCRIPT
                        .replace("__TARGET__", &target)
                        .replace("__TITLE__", &title);
                    let _ = window.eval(script);
                }
            }
            if payload.url().host_str() == Some("rewards.bing.com") {
                if let Some(step) = GUIDE.lock().unwrap().clone() {
                    let step = serde_json::to_string(&step).unwrap_or_default();
                    let _ = window.eval(GUIDE_SCRIPT.replace("__STEP__", &step));
                }
            }
            // Certains liens s'ouvrent dans la même fenêtre plutôt que dans une nouvelle.
            fill_pending_query(&window, payload.url());
        })
        .build()?;
    let handle = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Destroyed = event {
            // Fermer la fenêtre Rewards arrête la session guidée.
            if GUIDE.lock().unwrap().take().is_some() {
                let _ = handle.emit("guided-end", ());
            }
            let _ = handle.emit("rewards-site-closed", ());
        }
    });
    Ok(())
}
