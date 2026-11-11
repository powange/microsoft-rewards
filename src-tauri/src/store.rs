//! Réglages et historique local, enregistrés dans un fichier JSON du dossier de données de l'app.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{Datelike, Local, NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// L'app ne lance rien : elle envoie un rappel quotidien de ce qu'il reste à faire.
    Reminders,
    /// Lancement automatique des jeux (pas encore disponible, traité comme `Reminders`).
    Automatic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Subscription {
    None,
    Essential,
    Premium,
    Ultimate,
    PcGamePass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub mode: Mode,
    /// Heure du rappel quotidien, au format `HH:MM`.
    pub reminder_time: String,
    pub remind_bing: bool,
    pub remind_xbox: bool,
    /// Rappeler les séries dont l'étape du jour n'est pas faite.
    pub remind_streaks: bool,
    /// Notifier quand une tâche verrouillée devient accessible.
    pub notify_unlocks: bool,
    /// Notifier quand une quête pas terminée expire dans moins de 48 heures.
    pub notify_expiring: bool,
    /// Notifier chaque nouvelle recherche Bing à faire ; un clic sur la notification la lance.
    pub notify_new_searches: bool,
    pub subscription: Subscription,
    pub auto_check_updates: bool,
    /// Délai entre deux recherches de mise à jour, en minutes.
    pub update_check_interval_minutes: u32,
    /// Installer seule une mise à jour dès qu'elle est trouvée, puis redémarrer.
    pub auto_install_updates: bool,
    /// Actualisation automatique des points, séries et quêtes, en minutes (0 = désactivée).
    pub refresh_interval_minutes: u32,
}

/// En dessous, on solliciterait trop le site Rewards (une actualisation lit une dizaine de pages).
pub const MIN_REFRESH_MINUTES: u32 = 5;
/// Le fichier de mise à jour est petit et hébergé par GitHub : une recherche par minute reste raisonnable.
pub const MIN_UPDATE_CHECK_MINUTES: u32 = 1;

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Reminders,
            reminder_time: "19:00".into(),
            remind_bing: true,
            remind_xbox: true,
            remind_streaks: true,
            notify_unlocks: true,
            notify_expiring: true,
            notify_new_searches: true,
            subscription: Subscription::None,
            auto_check_updates: true,
            update_check_interval_minutes: 360,
            auto_install_updates: false,
            refresh_interval_minutes: 30,
        }
    }
}

impl Settings {
    pub fn reminder_time(&self) -> NaiveTime {
        NaiveTime::parse_from_str(&self.reminder_time, "%H:%M")
            .unwrap_or_else(|_| NaiveTime::from_hms_opt(19, 0, 0).unwrap())
    }

    pub fn update_check_interval(&self) -> std::time::Duration {
        let minutes = self
            .update_check_interval_minutes
            .max(MIN_UPDATE_CHECK_MINUTES);
        std::time::Duration::from_secs(u64::from(minutes) * 60)
    }

    /// Délai entre deux actualisations automatiques, `None` si elles sont désactivées.
    pub fn refresh_interval(&self) -> Option<std::time::Duration> {
        match self.refresh_interval_minutes {
            0 => None,
            minutes => Some(std::time::Duration::from_secs(
                u64::from(minutes.max(MIN_REFRESH_MINUTES)) * 60,
            )),
        }
    }
}

/// Une journée où l'utilisateur a joué au moins 15 minutes à un jeu Game Pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XboxSession {
    pub game: String,
}

/// Quête que l'utilisateur suit. Titre et lien sont gardés pour pouvoir l'afficher même
/// quand Microsoft ne la propose plus (terminée ou expirée).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowedQuest {
    pub id: String,
    pub title: String,
    pub url: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    pub followed_on: NaiveDate,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Persisted {
    settings: Settings,
    xbox_sessions: BTreeMap<NaiveDate, XboxSession>,
    last_reminder: Option<NaiveDate>,
    followed_quests: Vec<FollowedQuest>,
    /// Tâches verrouillées lors de la dernière actualisation, pour repérer les déverrouillages.
    locked_tasks: BTreeSet<String>,
    /// Solde de points relevé à la dernière actualisation de chaque jour.
    points_history: BTreeMap<NaiveDate, i64>,
    /// Article du catalogue que l'utilisateur vise.
    goal: Option<Goal>,
    /// Alertes déjà envoyées (quête qui expire…), pour ne prévenir qu'une fois.
    sent_alerts: BTreeSet<String>,
    /// Recherches Bing à faire lors de la dernière actualisation (`None` : jamais relevées).
    known_searches: Option<BTreeSet<String>>,
}

/// Objectif d'échange : un article du catalogue Rewards.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    pub id: String,
    pub title: String,
    pub price: i64,
    pub url: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    /// Notification « objectif atteignable » déjà envoyée.
    #[serde(default)]
    pub notified: bool,
}

/// Un an d'historique suffit pour les moyennes et le graphique.
const HISTORY_DAYS: i64 = 400;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XboxProgress {
    pub today_done: bool,
    pub today_game: Option<String>,
    /// Jours joués dans la semaine en cours (lundi à dimanche).
    pub week_days: u32,
    pub week_target: u32,
    /// Jeux différents joués dans le mois en cours, pour les packs 4 et 8 jeux.
    pub month_games: Vec<String>,
    /// Noms déjà saisis, proposés en suggestion.
    pub known_games: Vec<String>,
}

pub struct Store {
    path: PathBuf,
    data: Mutex<Persisted>,
    /// Problème rencontré à la lecture du fichier d'état, à signaler à l'utilisateur.
    load_warning: Option<String>,
}

/// Lit un fichier d'état. `Ok(None)` : le fichier n'existe pas (premier lancement).
fn read_state(path: &Path) -> Result<Option<Persisted>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

fn with_extension(path: &Path, extension: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(extension);
    PathBuf::from(name)
}

impl Store {
    /// Charge l'état. Un fichier illisible n'est jamais écrasé en silence : il est mis de côté
    /// (`.bad`), la dernière sauvegarde (`.bak`) est reprise si elle existe, et l'utilisateur
    /// est prévenu.
    pub fn load(path: PathBuf) -> Self {
        let (data, load_warning) = match read_state(&path) {
            Ok(data) => (data.unwrap_or_default(), None),
            Err(err) => {
                log::error!("Fichier d'état illisible {} : {err}", path.display());
                let _ = std::fs::copy(&path, with_extension(&path, ".bad"));
                match read_state(&with_extension(&path, ".bak")) {
                    Ok(Some(backup)) => (
                        backup,
                        Some("Le fichier des réglages était abîmé : la dernière sauvegarde a été reprise.".to_string()),
                    ),
                    _ => (
                        Persisted::default(),
                        Some("Le fichier des réglages était abîmé : réglages et quêtes suivies sont repartis de zéro. L'ancien fichier est gardé à côté (state.json.bad).".to_string()),
                    ),
                }
            }
        };
        Self {
            path,
            data: Mutex::new(data),
            load_warning,
        }
    }

    pub fn load_warning(&self) -> Option<&str> {
        self.load_warning.as_deref()
    }

    /// Écrit dans un fichier temporaire puis le renomme : une coupure pendant l'écriture laisse
    /// l'ancien fichier intact. La version précédente est gardée en `.bak`.
    fn save(&self, data: &Persisted) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = match serde_json::to_string_pretty(data) {
            Ok(text) => text,
            Err(err) => {
                log::error!("Sérialisation de l'état impossible : {err}");
                return;
            }
        };
        let tmp = with_extension(&self.path, ".tmp");
        let result = std::fs::write(&tmp, text).and_then(|()| {
            if read_state(&self.path).is_ok_and(|d| d.is_some()) {
                let _ = std::fs::copy(&self.path, with_extension(&self.path, ".bak"));
            }
            std::fs::rename(&tmp, &self.path)
        });
        if let Err(err) = result {
            log::error!("Impossible d'enregistrer {} : {err}", self.path.display());
        }
    }

    pub fn settings(&self) -> Settings {
        self.data.lock().unwrap().settings.clone()
    }

    pub fn set_settings(&self, settings: Settings) {
        let mut data = self.data.lock().unwrap();
        data.settings = settings;
        self.save(&data);
    }

    pub fn last_reminder(&self) -> Option<NaiveDate> {
        self.data.lock().unwrap().last_reminder
    }

    pub fn set_last_reminder(&self, day: NaiveDate) {
        let mut data = self.data.lock().unwrap();
        data.last_reminder = Some(day);
        self.save(&data);
    }

    #[cfg(test)]
    fn reload(&self) -> Self {
        Self::load(self.path.clone())
    }

    /// Remplace la liste des tâches verrouillées et renvoie la précédente.
    pub fn swap_locked_tasks(&self, locked: BTreeSet<String>) -> BTreeSet<String> {
        let mut data = self.data.lock().unwrap();
        let previous = std::mem::replace(&mut data.locked_tasks, locked);
        if previous != data.locked_tasks {
            self.save(&data);
        }
        previous
    }

    /// Relève le solde du jour (le dernier de la journée l'emporte).
    pub fn record_balance(&self, day: NaiveDate, balance: i64) {
        let mut data = self.data.lock().unwrap();
        if data.points_history.get(&day) == Some(&balance) {
            return;
        }
        data.points_history.insert(day, balance);
        let newest = data
            .points_history
            .keys()
            .next_back()
            .copied()
            .unwrap_or(day);
        let oldest = newest - chrono::Duration::days(HISTORY_DAYS);
        data.points_history.retain(|d, _| *d >= oldest);
        self.save(&data);
    }

    pub fn points_history(&self) -> Vec<(NaiveDate, i64)> {
        self.data
            .lock()
            .unwrap()
            .points_history
            .iter()
            .map(|(d, b)| (*d, *b))
            .collect()
    }

    /// Remplace la liste des recherches à faire et renvoie la précédente (`None` la première fois).
    pub fn swap_known_searches(&self, current: BTreeSet<String>) -> Option<BTreeSet<String>> {
        let mut data = self.data.lock().unwrap();
        if data.known_searches.as_ref() == Some(&current) {
            return Some(current);
        }
        let previous = data.known_searches.replace(current);
        self.save(&data);
        previous
    }

    pub fn goal(&self) -> Option<Goal> {
        self.data.lock().unwrap().goal.clone()
    }

    pub fn set_goal(&self, goal: Option<Goal>) {
        let mut data = self.data.lock().unwrap();
        data.goal = goal;
        self.save(&data);
    }

    /// Vrai la première fois seulement pour une même clé : sert à ne prévenir qu'une fois.
    pub fn first_alert(&self, key: &str) -> bool {
        let mut data = self.data.lock().unwrap();
        if !data.sent_alerts.insert(key.to_string()) {
            return false;
        }
        // Les clés portent une date ou un identifiant de quête : on garde les 500 dernières.
        while data.sent_alerts.len() > 500 {
            let first = data.sent_alerts.iter().next().cloned();
            if let Some(first) = first {
                data.sent_alerts.remove(&first);
            }
        }
        self.save(&data);
        true
    }

    pub fn followed_quests(&self) -> Vec<FollowedQuest> {
        self.data.lock().unwrap().followed_quests.clone()
    }

    /// Ajoute (`follow`) ou retire une quête des quêtes suivies, et renvoie la liste à jour.
    pub fn set_followed(
        &self,
        id: &str,
        title: &str,
        url: Option<String>,
        image_url: Option<String>,
        follow: bool,
    ) -> Vec<FollowedQuest> {
        let mut data = self.data.lock().unwrap();
        data.followed_quests.retain(|q| q.id != id);
        if follow {
            data.followed_quests.push(FollowedQuest {
                id: id.to_string(),
                title: title.to_string(),
                url,
                image_url,
                followed_on: Local::now().date_naive(),
            });
        }
        self.save(&data);
        data.followed_quests.clone()
    }

    /// Marque (ou démarque avec `None`) la session Xbox du jour.
    pub fn set_xbox_today(&self, game: Option<String>) {
        let today = Local::now().date_naive();
        let mut data = self.data.lock().unwrap();
        match game {
            Some(game) => {
                data.xbox_sessions.insert(
                    today,
                    XboxSession {
                        game: game.trim().to_string(),
                    },
                );
            }
            None => {
                data.xbox_sessions.remove(&today);
            }
        }
        self.save(&data);
    }

    pub fn xbox_progress(&self) -> XboxProgress {
        let today = Local::now().date_naive();
        let week_start =
            today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
        let data = self.data.lock().unwrap();

        let today_game = data.xbox_sessions.get(&today).map(|s| s.game.clone());
        let week_days = data.xbox_sessions.range(week_start..=today).count() as u32;

        let mut seen = BTreeSet::new();
        let mut month_games = Vec::new();
        for (day, session) in &data.xbox_sessions {
            if day.year() == today.year()
                && day.month() == today.month()
                && !session.game.is_empty()
                && seen.insert(session.game.to_lowercase())
            {
                month_games.push(session.game.clone());
            }
        }

        let mut known = BTreeSet::new();
        let known_games = data
            .xbox_sessions
            .values()
            .rev()
            .filter(|s| !s.game.is_empty() && known.insert(s.game.to_lowercase()))
            .map(|s| s.game.clone())
            .collect();

        XboxProgress {
            today_done: today_game.is_some(),
            today_game,
            week_days,
            week_target: 5,
            month_games,
            known_games,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_and_unfollows_quests_persistently() {
        let path =
            std::env::temp_dir().join(format!("rewards-autoplay-test-{}.json", std::process::id()));
        let store = Store::load(path.clone());
        let url = Some("https://rewards.bing.com/earn/quest/sot".to_string());
        store.set_followed("sot", "Sea of Thieves", url.clone(), None, true);
        store.set_followed("duo", "Duolingo", None, None, true);
        // Suivre à nouveau ne crée pas de doublon.
        let followed = store.set_followed("sot", "Sea of Thieves", url, None, true);
        assert_eq!(followed.len(), 2);

        let reloaded = store.reload();
        let ids: Vec<_> = reloaded
            .followed_quests()
            .into_iter()
            .map(|q| q.id)
            .collect();
        assert_eq!(ids, ["duo", "sot"]);

        assert_eq!(
            reloaded
                .set_followed("duo", "Duolingo", None, None, false)
                .len(),
            1
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remembers_locked_tasks_between_refreshes() {
        let path = std::env::temp_dir().join(format!(
            "rewards-autoplay-locked-{}.json",
            std::process::id()
        ));
        let store = Store::load(path.clone());
        let first: BTreeSet<String> = ["sot#Recherche".to_string()].into();
        assert!(store.swap_locked_tasks(first.clone()).is_empty());
        // Relancée, l'application retrouve la liste de la dernière actualisation.
        assert_eq!(store.reload().swap_locked_tasks(BTreeSet::new()), first);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn un_fichier_abime_n_est_pas_ecrase_en_silence() {
        let path = std::env::temp_dir().join(format!(
            "rewards-autoplay-corrupt-{}.json",
            std::process::id()
        ));
        let store = Store::load(path.clone());
        store.set_followed("sot", "Sea of Thieves", None, None, true);
        // Deuxième enregistrement : la version précédente part en sauvegarde.
        store.set_followed("duo", "Duolingo", None, None, true);
        std::fs::write(&path, "{ abîmé").unwrap();

        let reloaded = Store::load(path.clone());
        assert!(reloaded.load_warning().is_some());
        assert_eq!(reloaded.followed_quests().len(), 1);
        assert_eq!(
            std::fs::read_to_string(with_extension(&path, ".bad")).unwrap(),
            "{ abîmé"
        );
        for ext in ["", ".bak", ".bad"] {
            let _ = std::fs::remove_file(with_extension(&path, ext));
        }
    }

    #[test]
    fn historique_et_alertes() {
        let path = std::env::temp_dir().join(format!(
            "rewards-autoplay-history-{}.json",
            std::process::id()
        ));
        let store = Store::load(path.clone());
        let day = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        store.record_balance(day, 3000);
        store.record_balance(day, 3050);
        store.record_balance(day - chrono::Duration::days(500), 10);
        assert_eq!(store.points_history(), vec![(day, 3050)]);
        let searches: BTreeSet<String> = ["a#telescopes".to_string()].into();
        assert!(store.swap_known_searches(searches.clone()).is_none());
        assert_eq!(store.swap_known_searches(BTreeSet::new()), Some(searches));
        assert!(store.first_alert("expire#sot"));
        assert!(!store.reload().first_alert("expire#sot"));
        let _ = std::fs::remove_file(path);
    }
}
