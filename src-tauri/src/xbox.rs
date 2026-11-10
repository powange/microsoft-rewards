//! Connexion Xbox Live et quêtes du Rewards Hub de l'app Xbox (expérimental).
//!
//! La connexion utilise l'inscription d'application « Rewards Autoplay » (identité propre,
//! portée `XboxLive.signin`) : code d'autorisation avec PKCE dans une fenêtre Microsoft, puis
//! jeton utilisateur Xbox Live et jeton XSTS pour `http://xboxlive.com`.
//!
//! Les quêtes viennent de `xgrant.xboxlive.com/users/xuid(…)/programInfo`, le service que
//! l'app Xbox PC interroge pour son Rewards Hub. Ce service n'est pas documenté ; son format
//! a été décrit par le projet xau-mobile (github.com/xdzleo/xau-mobile). Rien ne garantit
//! qu'il accepte un jeton émis pour une autre application que l'app Xbox : un refus est
//! signalé tel quel à l'interface.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Instant;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};
use url::Url;

use crate::AppState;

/// Inscription d'application « Rewards Autoplay » (comptes Microsoft personnels).
const CLIENT_ID: &str = "3719f943-65c9-4986-b986-6ef7650a08cc";
const AUTHORIZE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const REDIRECT_URL: &str = "https://login.microsoftonline.com/common/oauth2/nativeclient";
const SCOPE: &str = "XboxLive.signin offline_access";
const USER_AUTH_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const PROGRAM_INFO_URL: &str = "https://xgrant.xboxlive.com/users/xuid({xuid})/programInfo\
    ?filter=profile,catalog,activities,spotlights";
const LOGIN_LABEL: &str = "xbox-login";
/// Le jeton de rafraîchissement est rangé dans le gestionnaire d'identifiants de Windows.
const KEYRING_SERVICE: &str = "com.powange.rewardsautoplay";
const KEYRING_USER: &str = "xbox-refresh-token";

#[derive(Debug, thiserror::Error)]
pub enum XboxError {
    #[error("xbox_not_connected")]
    NotConnected,
    /// Le service a répondu 401/403 : il n'accepte pas ce jeton.
    #[error("xbox_refused: {0}")]
    Refused(String),
    #[error("Erreur réseau : {0}")]
    Network(String),
    #[error("Réponse inattendue d'Xbox Live : {0}")]
    Unexpected(String),
}

#[derive(Debug, Clone)]
struct XboxToken {
    uhs: String,
    token: String,
    xuid: String,
    gamertag: Option<String>,
    expires_at: DateTime<Utc>,
}

/// État en mémoire de la connexion Xbox Live.
#[derive(Default)]
pub struct XboxState {
    token: Mutex<Option<XboxToken>>,
    /// (state, code_verifier) de la connexion en cours.
    pending_login: Mutex<Option<(String, String)>>,
    cache: Mutex<Option<(Instant, XboxRewards)>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XboxStatus {
    pub connected: bool,
    pub gamertag: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XboxRewards {
    pub fetched_at: DateTime<Local>,
    pub gamertag: Option<String>,
    pub balance: Option<i64>,
    /// Points gagnés mais pas encore réclamés dans l'app Xbox.
    pub unclaimed: i64,
    pub game_pass: Option<String>,
    pub tier: Option<String>,
    pub tier_progress: Option<i64>,
    pub tier_max: Option<i64>,
    pub missions: Vec<Mission>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mission {
    pub id: String,
    pub title: String,
    pub description: String,
    /// État brut du service : active, completed, claimed, locked, expired…
    pub status: String,
    /// Libellé de l'état tel qu'affiché par l'app Xbox (« Non réclamé »…).
    pub status_label: String,
    pub points: i64,
    /// `single` (quête simple), `streak` (cases jour par jour) ou `pack` (jeux du mois).
    pub kind: String,
    /// Faite, avec des points en attente de réclamation.
    pub claimable: bool,
    /// Faite et réclamée.
    pub claimed: bool,
    pub locked: bool,
    pub expired: bool,
    pub count: Option<i64>,
    pub target: Option<i64>,
    /// Case du jour, pour une série ou un pack encore ouvert.
    pub current: Option<usize>,
    pub reset_date: Option<String>,
    pub image_url: Option<String>,
    /// Condition à remplir pour débloquer une quête verrouillée.
    pub requirement: Option<String>,
    pub steps: Vec<MissionStep>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissionStep {
    pub label: String,
    pub points: i64,
    pub done: bool,
    pub note: Option<String>,
}

// ---------- Connexion ----------

fn random_string(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("générateur aléatoire du système");
    URL_SAFE_NO_PAD.encode(buffer)
}

/// Ouvre la page de connexion Microsoft ; la suite se fait dans [`finish_login`].
pub fn open_login<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(LOGIN_LABEL) {
        window.show()?;
        return window.set_focus();
    }
    let verifier = random_string(32);
    let state = random_string(16);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    *app.state::<AppState>().xbox.pending_login.lock().unwrap() = Some((state.clone(), verifier));

    let mut url = Url::parse(AUTHORIZE_URL).unwrap();
    url.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", REDIRECT_URL)
        .append_pair("scope", SCOPE)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("prompt", "select_account");

    let handle = app.clone();
    WebviewWindowBuilder::new(app, LOGIN_LABEL, WebviewUrl::External(url))
        .title("Connexion à Xbox Live")
        .inner_size(520.0, 720.0)
        .center()
        .on_navigation(move |url| {
            // Microsoft renvoie vers l'adresse de redirection avec le code : on l'intercepte.
            if !url.as_str().starts_with(REDIRECT_URL) {
                return true;
            }
            let app = handle.clone();
            let url = url.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(window) = app.get_webview_window(LOGIN_LABEL) {
                    let _ = window.close();
                }
                match finish_login(&app, &url).await {
                    Ok(status) => {
                        let _ = app.emit("xbox-connected", &status);
                        // Premier appel au service des quêtes : c'est ici qu'un refus apparaît.
                        if let Err(err) = refresh(&app).await {
                            let _ = app.emit("xbox-error", err.to_string());
                        }
                    }
                    Err(err) => {
                        log::warn!("Connexion Xbox Live impossible : {err}");
                        let _ = app.emit("xbox-error", err.to_string());
                    }
                }
            });
            false
        })
        .build()?;
    Ok(())
}

async fn finish_login<R: Runtime>(app: &AppHandle<R>, url: &Url) -> Result<XboxStatus, XboxError> {
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    if let Some(error) = params.get("error") {
        let detail = params.get("error_description").cloned().unwrap_or_default();
        return Err(XboxError::Unexpected(format!("{error} {detail}")));
    }
    let code = params
        .get("code")
        .ok_or_else(|| XboxError::Unexpected("code d'autorisation absent".into()))?;
    let state = app.state::<AppState>();
    let (expected_state, verifier) = state
        .xbox
        .pending_login
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| XboxError::Unexpected("aucune connexion en cours".into()))?;
    if params.get("state") != Some(&expected_state) {
        return Err(XboxError::Unexpected(
            "réponse de connexion inattendue".into(),
        ));
    }

    let client = state.http.clone();
    let tokens = oauth_token(
        &client,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT_URL),
            ("code_verifier", &verifier),
        ],
    )
    .await?;
    let token = xbox_live_token(&client, &tokens.access_token).await?;
    save_refresh_token(&tokens.refresh_token)?;
    let status = XboxStatus {
        connected: true,
        gamertag: token.gamertag.clone(),
    };
    *state.xbox.token.lock().unwrap() = Some(token);
    Ok(status)
}

pub fn logout<R: Runtime>(app: &AppHandle<R>) {
    let state = app.state::<AppState>();
    *state.xbox.token.lock().unwrap() = None;
    *state.xbox.cache.lock().unwrap() = None;
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
        let _ = entry.delete_credential();
    }
}

pub fn status<R: Runtime>(app: &AppHandle<R>) -> XboxStatus {
    let gamertag = app
        .state::<AppState>()
        .xbox
        .token
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|t| t.gamertag.clone());
    XboxStatus {
        connected: gamertag.is_some() || load_refresh_token().is_some(),
        gamertag,
    }
}

fn load_refresh_token() -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .ok()?
        .get_password()
        .ok()
        .filter(|t| !t.is_empty())
}

fn save_refresh_token(token: &str) -> Result<(), XboxError> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .and_then(|entry| entry.set_password(token))
        .map_err(|e| XboxError::Unexpected(format!("enregistrement du jeton impossible : {e}")))
}

#[derive(Deserialize)]
struct OAuthTokens {
    access_token: String,
    refresh_token: String,
}

async fn oauth_token(
    client: &reqwest::Client,
    grant: &[(&str, &str)],
) -> Result<OAuthTokens, XboxError> {
    let mut form = vec![("client_id", CLIENT_ID), ("scope", SCOPE)];
    form.extend_from_slice(grant);
    let response = client
        .post(TOKEN_URL)
        .form(&form)
        .send()
        .await
        .map_err(|e| XboxError::Network(e.to_string()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| XboxError::Network(e.to_string()))?;
    if status.as_u16() == 400 && body.contains("invalid_grant") {
        // Jeton de rafraîchissement expiré ou révoqué : il faut se reconnecter.
        return Err(XboxError::NotConnected);
    }
    if !status.is_success() {
        return Err(XboxError::Unexpected(format!(
            "jeton Microsoft : {status} {body}"
        )));
    }
    serde_json::from_str(&body).map_err(|e| XboxError::Unexpected(e.to_string()))
}

/// Jeton utilisateur Xbox Live puis jeton XSTS pour `http://xboxlive.com`.
async fn xbox_live_token(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<XboxToken, XboxError> {
    let user = post_json(
        client,
        USER_AUTH_URL,
        json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": format!("d={access_token}"),
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        }),
    )
    .await?;
    let user_token = user["Token"]
        .as_str()
        .ok_or_else(|| XboxError::Unexpected("jeton utilisateur Xbox absent".into()))?;

    let xsts = post_json(
        client,
        XSTS_URL,
        json!({
            "Properties": { "SandboxId": "RETAIL", "UserTokens": [user_token] },
            "RelyingParty": "http://xboxlive.com",
            "TokenType": "JWT",
        }),
    )
    .await?;
    let claims = &xsts["DisplayClaims"]["xui"][0];
    let field = |key: &str| claims[key].as_str().map(str::to_string);
    Ok(XboxToken {
        token: xsts["Token"]
            .as_str()
            .ok_or_else(|| XboxError::Unexpected("jeton XSTS absent".into()))?
            .to_string(),
        uhs: field("uhs").ok_or_else(|| XboxError::Unexpected("uhs absent".into()))?,
        xuid: field("xid").ok_or_else(|| XboxError::Unexpected("xuid absent".into()))?,
        gamertag: field("gtg"),
        expires_at: xsts["NotAfter"]
            .as_str()
            .and_then(|d| d.parse().ok())
            .unwrap_or_else(|| Utc::now() + chrono::Duration::hours(1)),
    })
}

async fn post_json(client: &reqwest::Client, url: &str, body: Value) -> Result<Value, XboxError> {
    let response = client
        .post(url)
        .header("x-xbl-contract-version", "1")
        .json(&body)
        .send()
        .await
        .map_err(|e| XboxError::Network(e.to_string()))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| XboxError::Network(e.to_string()))?;
    if status.as_u16() == 401 {
        // XSTS répond 401 avec un code XErr (compte sans profil Xbox, compte enfant…).
        return Err(XboxError::Refused(format!("{url} : {text}")));
    }
    if !status.is_success() {
        return Err(XboxError::Unexpected(format!("{url} : {status} {text}")));
    }
    serde_json::from_str(&text).map_err(|e| XboxError::Unexpected(e.to_string()))
}

/// Jeton XSTS valide, renouvelé avec le jeton de rafraîchissement si besoin.
async fn valid_token<R: Runtime>(app: &AppHandle<R>) -> Result<XboxToken, XboxError> {
    let state = app.state::<AppState>();
    if let Some(token) = state.xbox.token.lock().unwrap().as_ref() {
        if token.expires_at > Utc::now() + chrono::Duration::minutes(5) {
            return Ok(token.clone());
        }
    }
    let refresh_token = load_refresh_token().ok_or(XboxError::NotConnected)?;
    let client = state.http.clone();
    let tokens = oauth_token(
        &client,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token),
        ],
    )
    .await?;
    save_refresh_token(&tokens.refresh_token)?;
    let token = xbox_live_token(&client, &tokens.access_token).await?;
    *state.xbox.token.lock().unwrap() = Some(token.clone());
    Ok(token)
}

// ---------- Quêtes ----------

/// Relit les quêtes Xbox, met le cache à jour et prévient l'interface.
pub async fn refresh<R: Runtime>(app: &AppHandle<R>) -> Result<XboxRewards, XboxError> {
    let result = fetch(app).await;
    let state = app.state::<AppState>();
    match &result {
        Ok(rewards) => {
            *state.xbox.cache.lock().unwrap() = Some((Instant::now(), rewards.clone()));
            let _ = app.emit("xbox-rewards-updated", rewards);
        }
        Err(XboxError::NotConnected) => *state.xbox.cache.lock().unwrap() = None,
        Err(err) => log::warn!("Quêtes Xbox illisibles : {err}"),
    }
    result
}

/// Données en cache si elles ont moins de `max_age`.
pub fn cached<R: Runtime>(app: &AppHandle<R>, max_age: std::time::Duration) -> Option<XboxRewards> {
    let state = app.state::<AppState>();
    let cache = state.xbox.cache.lock().unwrap();
    cache
        .as_ref()
        .filter(|(at, _)| at.elapsed() < max_age)
        .map(|(_, rewards)| rewards.clone())
}

async fn fetch<R: Runtime>(app: &AppHandle<R>) -> Result<XboxRewards, XboxError> {
    let token = valid_token(app).await?;
    let client = app.state::<AppState>().http.clone();
    let url = PROGRAM_INFO_URL.replace("{xuid}", &token.xuid);
    let response = client
        .get(&url)
        .header(
            "Authorization",
            format!("XBL3.0 x={};{}", token.uhs, token.token),
        )
        .header("x-xbl-contract-version", "5")
        .header("Accept", "application/json")
        .header("Accept-Language", "fr-FR")
        .send()
        .await
        .map_err(|e| XboxError::Network(e.to_string()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| XboxError::Network(e.to_string()))?;
    if let Ok(dir) = app.path().app_log_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            dir.join("xbox-programinfo.json"),
            format!("HTTP {status}\n{body}"),
        );
    }
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(XboxError::Refused(format!("programInfo : HTTP {status}")));
    }
    if !status.is_success() {
        return Err(XboxError::Unexpected(format!(
            "programInfo : HTTP {status}"
        )));
    }
    let json: Value =
        serde_json::from_str(&body).map_err(|e| XboxError::Unexpected(e.to_string()))?;
    let mut rewards = parse_program_info(&json);
    rewards.gamertag = token.gamertag;
    Ok(rewards)
}

fn parse_program_info(json: &Value) -> XboxRewards {
    let mut missions = Vec::new();
    let mut seen = HashSet::new();
    collect_missions(json, &mut missions, &mut seen);

    let program = &json["tiersProfile"]["programs"][0];
    let progression = &program["progressions"][0];
    XboxRewards {
        fetched_at: Local::now(),
        gamertag: None,
        balance: json["balance"].as_i64(),
        unclaimed: json["unclaimedBalance"].as_i64().unwrap_or(0),
        game_pass: json["gamePassMembership"]
            .as_str()
            .filter(|g| !g.eq_ignore_ascii_case("none"))
            .map(str::to_string),
        tier: program["currentTierIndex"]
            .as_u64()
            .and_then(|i| program["allTiers"][i as usize]["title"].as_str())
            .map(str::to_string),
        tier_progress: progression["progress"].as_i64(),
        tier_max: progression["max"].as_i64(),
        missions,
    }
}

/// Toute quête est un objet portant `activityId` et `title`, où qu'il soit dans la réponse.
fn collect_missions(value: &Value, out: &mut Vec<Mission>, seen: &mut HashSet<String>) {
    match value {
        Value::Object(map) => {
            if let (Some(id), Some(_)) = (str_of(map, "activityId"), str_of(map, "title")) {
                if seen.insert(id) {
                    out.push(parse_mission(map));
                }
                return;
            }
            for child in map.values() {
                collect_missions(child, out, seen);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_missions(v, out, seen)),
        _ => {}
    }
}

fn str_of(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn int_of(map: &Map<String, Value>, key: &str) -> Option<i64> {
    map.get(key).and_then(Value::as_i64)
}

fn parse_mission(map: &Map<String, Value>) -> Mission {
    let status = str_of(map, "status").unwrap_or_default().to_lowercase();
    let obj = |key: &str| map.get(key).and_then(Value::as_object);

    // Cases d'une série ou d'un pack : soit `punchcard.punches`, soit (série de connexion)
    // `streakLabels` + `streakAdditions` remplis jusqu'à `currentStreak`.
    let punches: Vec<&Map<String, Value>> = obj("punchcard")
        .and_then(|p| p.get("punches"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_object).collect())
        .unwrap_or_default();
    let steps: Vec<MissionStep> = if !punches.is_empty() {
        punches
            .iter()
            .map(|p| MissionStep {
                label: str_of(p, "text").unwrap_or_default(),
                points: int_of(p, "pointValue").unwrap_or(0),
                done: str_of(p, "state").is_some_and(|s| s.eq_ignore_ascii_case("punched")),
                note: str_of(p, "hintText"),
            })
            .collect()
    } else {
        let labels: Vec<String> = map
            .get("streakLabels")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let additions: Vec<i64> = map
            .get("streakAdditions")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default();
        let streak = int_of(map, "currentStreak").unwrap_or(0).max(0) as usize;
        labels
            .into_iter()
            .enumerate()
            .map(|(i, label)| MissionStep {
                label,
                points: additions.get(i).copied().unwrap_or(0),
                done: i < streak,
                note: None,
            })
            .collect()
    };

    let inline = obj("displayTemplates").and_then(|d| str_of(d, "inline"));
    let kind = match inline.as_deref() {
        Some("gallery") => "pack",
        _ if !steps.is_empty() => "streak",
        _ => "single",
    };
    let (count, target) = if kind == "single" {
        let info = obj("progressInfo");
        (
            info.and_then(|i| int_of(i, "completedSteps")),
            info.and_then(|i| int_of(i, "totalSteps")),
        )
    } else {
        (
            Some(steps.iter().filter(|s| s.done).count() as i64),
            Some(steps.len() as i64),
        )
    };
    let claimed = status == "claimed";
    let claimable = status == "completed";
    let locked = status == "locked";
    let expired = status == "expired" || status == "failed";
    let open = !(claimed || claimable || locked || expired);

    Mission {
        id: str_of(map, "activityId").unwrap_or_default(),
        title: str_of(map, "title").unwrap_or_default(),
        description: str_of(map, "description").unwrap_or_default(),
        status_label: str_of(map, "statusLabel").unwrap_or_default(),
        // Une série paie 0 tant qu'elle n'est pas finie : on affiche son maximum.
        points: int_of(map, "rewardsPoints")
            .filter(|p| *p > 0)
            .or_else(|| int_of(map, "maximumBasePoints"))
            .or_else(|| int_of(map, "totalPoints"))
            .unwrap_or(0),
        kind: kind.to_string(),
        claimable,
        claimed,
        locked,
        expired,
        count,
        target,
        current: if open && kind != "single" {
            steps.iter().position(|s| !s.done)
        } else {
            None
        },
        reset_date: str_of(map, "resetDate")
            .or_else(|| obj("dates").and_then(|d| str_of(d, "resetDate"))),
        image_url: obj("imageUris")
            .and_then(|i| str_of(i, "tile").or_else(|| str_of(i, "thumbnail")))
            .filter(|u| u.starts_with("https://")),
        requirement: if locked {
            obj("powerUpStrings").and_then(|p| str_of(p, "description"))
        } else {
            None
        },
        status,
        steps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forme de réponse décrite par xau-mobile (quêtes du jour, bonus hebdomadaire, appli mobile).
    const PROGRAM_INFO: &str = r#"{
      "hasProgramAccess": true,
      "gamePassMembership": "GamePassUltimate",
      "unclaimedBalance": 5,
      "balance": 3345,
      "tiersProfile": { "programs": [{ "currentTierIndex": 0,
        "progressions": [{ "title": "Membre", "progress": 143, "max": 500 }],
        "allTiers": [{ "title": "Membre" }, { "title": "Membre Argent" }] }] },
      "programOfferings": [{ "displayTemplate": "channel", "activities": [
        { "activityId": "PCGamePlay", "title": "Jouer à un jeu PC", "rewardsPoints": 5,
          "status": "completed", "statusLabel": "Non réclamé",
          "progressInfo": { "completedSteps": 1, "totalSteps": 1 } },
        { "activityId": "playConsole", "title": "Jouer à un jeu sur la console", "rewardsPoints": 5,
          "status": "active", "statusLabel": "Disponible",
          "progressInfo": { "completedSteps": 0, "totalSteps": 1 } },
        { "activityId": "pcWeeklyBonus", "title": "Bonus hebdomadaire sur PC", "rewardsPoints": 110,
          "status": "active", "displayTemplates": { "inline": "streakedPunchCard" },
          "resetDate": "2026-11-16T08:00:00Z",
          "punchcard": { "punches": [
            { "pointValue": 0, "state": "punched", "text": "Jour 1" },
            { "pointValue": 0, "state": "active", "text": "Jour 2" },
            { "pointValue": 110, "state": "active", "text": "Jour 5" } ] } },
        { "activityId": "griffinLogin", "title": "Utilisez l’application mobile XBOX",
          "status": "claimed", "rewardsPoints": 2, "currentStreak": 1,
          "displayTemplates": { "inline": "streakedPunchCard" },
          "streakLabels": ["Jour 1", "Jour 2", "Jour 3"], "streakAdditions": [2, 6, 10] },
        { "activityId": "gamepassMonthly8Titles", "title": "Pack mensuel de 8 jeux",
          "status": "locked", "displayTemplates": { "inline": "gallery" },
          "maximumBasePoints": 280, "powerUpStrings": { "description": "Réservé à Ultimate" },
          "punchcard": { "punches": [ { "state": "punched" }, { "state": "active" } ] } }
      ]}]
    }"#;

    #[test]
    fn parses_program_info() {
        let rewards = parse_program_info(&serde_json::from_str(PROGRAM_INFO).unwrap());
        assert_eq!((rewards.balance, rewards.unclaimed), (Some(3345), 5));
        assert_eq!(rewards.game_pass.as_deref(), Some("GamePassUltimate"));
        assert_eq!(rewards.tier.as_deref(), Some("Membre"));
        assert_eq!(
            (rewards.tier_progress, rewards.tier_max),
            (Some(143), Some(500))
        );
        assert_eq!(rewards.missions.len(), 5);

        let pc = &rewards.missions[0];
        assert!(pc.claimable && !pc.claimed);
        assert_eq!(
            (pc.kind.as_str(), pc.points, pc.count, pc.target),
            ("single", 5, Some(1), Some(1))
        );
        assert_eq!(pc.status_label, "Non réclamé");

        let weekly = &rewards.missions[2];
        assert_eq!(weekly.kind, "streak");
        assert_eq!(
            (weekly.count, weekly.target, weekly.current),
            (Some(1), Some(3), Some(1))
        );
        assert_eq!(weekly.steps[2].points, 110);
        assert_eq!(weekly.reset_date.as_deref(), Some("2026-11-16T08:00:00Z"));

        let mobile = &rewards.missions[3];
        assert!(mobile.claimed);
        assert_eq!(mobile.steps.len(), 3);
        assert!(mobile.steps[0].done && !mobile.steps[1].done);
        assert_eq!(mobile.steps[1].points, 6);
        assert_eq!(
            mobile.current, None,
            "réclamée aujourd'hui : pas de case du jour"
        );

        let pack = &rewards.missions[4];
        assert_eq!(pack.kind, "pack");
        assert!(pack.locked);
        assert_eq!(pack.points, 280);
        assert_eq!(pack.requirement.as_deref(), Some("Réservé à Ultimate"));
    }

    #[test]
    fn odd_payloads_do_not_panic() {
        for body in ["{}", "[]", "null", r#"{"a":{"b":[1,2,3]}}"#] {
            let rewards = parse_program_info(&serde_json::from_str(body).unwrap());
            assert!(rewards.missions.is_empty());
        }
    }
}
