//! Détection de la formule Game Pass sur la page « Services et abonnements » du compte
//! Microsoft, ouverte dans la même session WebView2 que Microsoft Rewards.
//!
//! La page est une application web dont les données ne sont pas documentées : on lit son
//! texte affiché, une fois chargé, et on y cherche le nom des formules Game Pass. La page
//! n'a aucun accès à l'application ; le résultat revient par une navigation vers une adresse
//! réservée, interceptée ici.

use std::sync::Mutex;
use std::time::Duration;

use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;
use url::Url;

use crate::store::Subscription;
use crate::AppState;

const LABEL: &str = "gamepass-detect";
const SERVICES_URL: &str = "https://account.microsoft.com/services?lang=fr-FR";
const RESULT_PATH: &str = "/__rewards-autoplay/gamepass";

/// Résultat attendu par la détection en cours.
static PENDING: Mutex<Option<oneshot::Sender<String>>> = Mutex::new(None);

/// Script injecté dans la page : attend que la liste des services s'affiche, puis renvoie la
/// formule trouvée (`ultimate`, `pcGamePass`, `premium`, `essential`), `none` si la page des
/// services est affichée sans Game Pass, ou rien tant que la page n'est pas prête (connexion
/// demandée…).
const DETECT_SCRIPT: &str = r#"(() => {
  const started = Date.now();
  const tiers = [
    ["ultimate", /game\s*pass\s*ultimate/],
    ["pcGamePass", /pc\s*game\s*pass/],
    ["premium", /game\s*pass\s*(premium|standard)/],
    ["essential", /game\s*pass\s*(essential|core)/],
  ];
  const send = (tier) => {
    location.href = location.origin + "/__rewards-autoplay/gamepass?tier=" + tier;
  };
  const tick = () => {
    const text = (document.body?.innerText || "").toLowerCase();
    const found = tiers.find(([, re]) => re.test(text));
    if (found) return send(found[0]);
    // La liste est affichée (titre de la page et au moins un abonnement), sans Game Pass.
    const listed = /services et abonnements|services & subscriptions/.test(text)
      && /(renouvel|abonnement|subscription|g[ée]rer)/.test(text) && Date.now() - started > 8000;
    if (listed) return send("none");
    if (Date.now() - started < 30000) setTimeout(tick, 1000);
  };
  tick();
})();"#;

fn tier_from(value: &str) -> Option<Subscription> {
    Some(match value {
        "ultimate" => Subscription::Ultimate,
        "pcGamePass" => Subscription::PcGamePass,
        "premium" => Subscription::Premium,
        "essential" => Subscription::Essential,
        "none" => Subscription::None,
        _ => return None,
    })
}

/// Lit la formule sur la page du compte. `visible` : la fenêtre est montrée (détection
/// demandée par l'utilisateur, qui peut s'y reconnecter si Microsoft le demande) ; sinon
/// elle reste cachée et abandonne au bout d'une minute.
pub async fn detect<R: Runtime>(app: &AppHandle<R>, visible: bool) -> Result<Subscription, String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.close();
    }
    let (sender, receiver) = oneshot::channel();
    *PENDING.lock().unwrap() = Some(sender);

    let handle = app.clone();
    WebviewWindowBuilder::new(
        app,
        LABEL,
        WebviewUrl::External(Url::parse(SERVICES_URL).unwrap()),
    )
    .title("Détection de la formule Game Pass")
    .inner_size(1000.0, 760.0)
    .center()
    .visible(visible)
    .on_navigation(move |url| {
        if url.path() != RESULT_PATH {
            return true;
        }
        let tier = url
            .query_pairs()
            .find(|(k, _)| k == "tier")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        if let Some(sender) = PENDING.lock().unwrap().take() {
            let _ = sender.send(tier);
        }
        if let Some(window) = handle.get_webview_window(LABEL) {
            let _ = window.close();
        }
        false
    })
    .on_page_load(|window, payload| {
        if payload.event() == PageLoadEvent::Finished
            && payload.url().host_str() == Some("account.microsoft.com")
        {
            let _ = window.eval(DETECT_SCRIPT);
        }
    })
    .build()
    .map_err(|e| e.to_string())?;

    let limit = if visible { 180 } else { 60 };
    let result = tokio::time::timeout(Duration::from_secs(limit), receiver).await;
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.close();
    }
    let tier = match result {
        Ok(Ok(tier)) => tier,
        _ => {
            *PENDING.lock().unwrap() = None;
            return Err("la page « Services et abonnements » ne s'est pas affichée à temps".into());
        }
    };
    let subscription = tier_from(&tier).ok_or_else(|| format!("réponse inattendue : {tier}"))?;
    let store = &app.state::<AppState>().store;
    let mut settings = store.settings();
    settings.subscription = subscription;
    store.set_settings(settings);
    store.set_subscription_checked(chrono::Local::now().date_naive());
    log::info!("Formule Game Pass détectée : {tier}");
    Ok(subscription)
}

/// Détection discrète une fois par semaine, si le compte Microsoft est connecté.
pub async fn detect_weekly<R: Runtime>(app: &AppHandle<R>) {
    let store = &app.state::<AppState>().store;
    let today = chrono::Local::now().date_naive();
    if store
        .subscription_checked()
        .is_some_and(|d| (today - d).num_days() < 7)
    {
        return;
    }
    if let Err(err) = detect(app, false).await {
        log::info!("Formule Game Pass non détectée : {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formules_reconnues() {
        assert!(matches!(
            tier_from("ultimate"),
            Some(Subscription::Ultimate)
        ));
        assert!(matches!(tier_from("none"), Some(Subscription::None)));
        assert!(tier_from("autre").is_none());
    }
}
