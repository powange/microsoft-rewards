//! Réglages et historique local, enregistrés dans un fichier JSON du dossier de données de l'app.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
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
    pub subscription: Subscription,
    pub auto_check_updates: bool,
    /// Délai entre deux recherches de mise à jour, en minutes.
    pub update_check_interval_minutes: u32,
    /// Installer seul une mise à jour trouvée, quand la fenêtre est fermée.
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
}

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
}

impl Store {
    pub fn load(path: PathBuf) -> Self {
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            path,
            data: Mutex::new(data),
        }
    }

    fn save(&self, data: &Persisted) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(data) {
            Ok(text) => {
                if let Err(err) = std::fs::write(&self.path, text) {
                    log::error!("Impossible d'enregistrer {}: {err}", self.path.display());
                }
            }
            Err(err) => log::error!("Sérialisation de l'état impossible: {err}"),
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
}
