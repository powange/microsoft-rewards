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
    /// Second rappel, le soir, si une série n'a pas son étape du jour.
    pub remind_streaks_evening: bool,
    /// Heure du rappel du soir pour les séries, au format `HH:MM`.
    pub streak_reminder_time: String,
    /// Alerte dans les derniers jours du mois si le niveau actuel n'est pas assuré.
    pub notify_level: bool,
    /// Bilan de la semaine, le dimanche soir.
    pub weekly_summary: bool,
    /// Cocher les jours joués d'après l'historique des jeux Xbox Live (compte connecté).
    pub auto_xbox_days: bool,
    /// Appareils joués : seules leurs quêtes Xbox sont affichées et rappelées.
    pub xbox_pc: bool,
    pub xbox_console: bool,
    pub xbox_mobile: bool,
    /// Anniversaire au format `MM-JJ` (vide : pas de rappel du cadeau Xbox Rewards).
    pub birthday: String,
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
            remind_streaks_evening: true,
            streak_reminder_time: "22:00".into(),
            notify_level: true,
            weekly_summary: true,
            auto_xbox_days: true,
            xbox_pc: true,
            xbox_console: false,
            xbox_mobile: true,
            birthday: String::new(),
            subscription: Subscription::None,
            auto_check_updates: true,
            update_check_interval_minutes: 360,
            auto_install_updates: false,
            refresh_interval_minutes: 30,
        }
    }
}

impl Settings {
    pub fn streak_reminder_time(&self) -> NaiveTime {
        NaiveTime::parse_from_str(&self.streak_reminder_time, "%H:%M")
            .unwrap_or_else(|_| NaiveTime::from_hms_opt(22, 0, 0).unwrap())
    }

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

/// Ancien format : une journée jouée sur PC, avec le nom du jeu.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XboxSession {
    pub game: String,
}

/// Une quête ou série du Rewards Hub de l'app Xbox, telle qu'on la coche pour un jour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum XboxActivity {
    /// 15 minutes de jeu sur PC (quête du jour et bonus hebdomadaire PC).
    Pc,
    /// 15 minutes de jeu sur console (quête du jour et bonus console hebdomadaire).
    Console,
    /// Connexion à l'application mobile Xbox (série mobile quotidienne).
    MobileApp,
    /// Un niveau de Jewel dans l'application mobile (quête du jour).
    Jewel,
    /// 15 minutes d'un jeu Game Pass (quête du jour et série hebdomadaire Game Pass).
    GamePass,
}

/// Ce qui a été fait un jour donné.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DayPlay {
    pub pc: bool,
    pub console: bool,
    pub mobile_app: bool,
    pub jewel: bool,
    pub game_pass: bool,
}

impl DayPlay {
    pub fn get(&self, activity: XboxActivity) -> bool {
        match activity {
            XboxActivity::Pc => self.pc,
            XboxActivity::Console => self.console,
            XboxActivity::MobileApp => self.mobile_app,
            XboxActivity::Jewel => self.jewel,
            XboxActivity::GamePass => self.game_pass,
        }
    }

    fn set(&mut self, activity: XboxActivity, value: bool) {
        match activity {
            XboxActivity::Pc => self.pc = value,
            XboxActivity::Console => self.console = value,
            XboxActivity::MobileApp => self.mobile_app = value,
            XboxActivity::Jewel => self.jewel = value,
            XboxActivity::GamePass => self.game_pass = value,
        }
    }
}

fn month_key(day: NaiveDate) -> String {
    day.format("%Y-%m").to_string()
}

fn week_start(day: NaiveDate) -> NaiveDate {
    day - chrono::Duration::days(day.weekday().num_days_from_monday() as i64)
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
    /// Ancien suivi (un jeu PC par jour), repris dans `xbox_days` au chargement.
    xbox_sessions: BTreeMap<NaiveDate, XboxSession>,
    /// Ce qui a été fait chaque jour pour les quêtes Xbox.
    xbox_days: BTreeMap<NaiveDate, DayPlay>,
    /// Jeux joués chaque mois (« 2026-11 »), pour les packs de 4 et 8 jeux Game Pass.
    month_games: BTreeMap<String, Vec<String>>,
    /// Jeux joués ce mois-là sans être nommés, ajoutés à la main.
    month_extra: BTreeMap<String, u32>,
    last_reminder: Option<NaiveDate>,
    /// Jour du dernier rappel du soir pour les séries.
    last_streak_reminder: Option<NaiveDate>,
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
    /// Dernière détection réussie de la formule Game Pass.
    subscription_checked: Option<NaiveDate>,
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
    /// Ce qui est fait aujourd'hui.
    pub today: DayPlay,
    /// Les sept jours de la semaine en cours (lundi à dimanche, remise à zéro le lundi).
    pub week: Vec<WeekDay>,
    /// Jours de la semaine joués sur PC, sur console, et à un jeu Game Pass.
    pub pc_days: u32,
    pub console_days: u32,
    pub game_pass_days: u32,
    /// Jours consécutifs de connexion à l'application mobile, jusqu'à aujourd'hui (ou hier si
    /// aujourd'hui n'est pas encore fait).
    pub mobile_streak: u32,
    /// Jeux joués ce mois-ci (packs de 4 et 8 jeux Game Pass), et jeux non nommés en plus.
    pub month_games: Vec<String>,
    pub month_extra: u32,
    pub month_count: u32,
    /// Noms déjà vus, proposés en suggestion.
    pub known_games: Vec<String>,
    /// Résumé pour le rappel et la mini-fenêtre : jeu PC du jour fait, jours PC de la semaine.
    pub today_done: bool,
    pub week_days: u32,
    pub week_target: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeekDay {
    pub date: NaiveDate,
    pub play: DayPlay,
    /// Jour à venir : il ne peut pas encore être coché.
    pub future: bool,
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

/// Reprend l'ancien suivi (un jeu PC par jour) dans le suivi par quête.
fn migrate_xbox_sessions(data: &mut Persisted) {
    for (day, session) in std::mem::take(&mut data.xbox_sessions) {
        data.xbox_days.entry(day).or_default().pc = true;
        let game = session.game.trim().to_string();
        if !game.is_empty() {
            let games = data.month_games.entry(month_key(day)).or_default();
            if !games.iter().any(|g| g.eq_ignore_ascii_case(&game)) {
                games.push(game);
            }
        }
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
        let mut data = data;
        migrate_xbox_sessions(&mut data);
        Self {
            path,
            data: Mutex::new(data),
            load_warning,
        }
    }

    /// Sauvegarde complète (réglages, quêtes suivies, objectif, historique, suivi Xbox), au
    /// format du fichier d'état. Aucun mot de passe ni jeton n'y figure.
    pub fn export_json(&self) -> String {
        let data = self.data.lock().unwrap();
        serde_json::to_string_pretty(&*data).unwrap_or_default()
    }

    /// Remplace tout l'état par une sauvegarde, après avoir vérifié qu'elle se lit.
    pub fn import_json(&self, text: &str) -> Result<(), String> {
        let imported: Persisted = serde_json::from_str(text)
            .map_err(|e| format!("fichier de sauvegarde illisible : {e}"))?;
        let mut data = self.data.lock().unwrap();
        *data = imported;
        self.save(&data);
        Ok(())
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

    pub fn last_streak_reminder(&self) -> Option<NaiveDate> {
        self.data.lock().unwrap().last_streak_reminder
    }

    pub fn set_last_streak_reminder(&self, day: NaiveDate) {
        let mut data = self.data.lock().unwrap();
        data.last_streak_reminder = Some(day);
        self.save(&data);
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

    /// Points gagnés entre deux dates incluses : somme des hausses du solde d'un relevé à
    /// l'autre (une baisse est un échange, pas une perte de gains).
    pub fn points_gained(&self, from: NaiveDate, to: NaiveDate) -> i64 {
        let data = self.data.lock().unwrap();
        let mut previous: Option<i64> = None;
        let mut gained = 0;
        for (day, balance) in &data.points_history {
            if let Some(before) = previous {
                if *day >= from && *day <= to {
                    gained += (balance - before).max(0);
                }
            }
            previous = Some(*balance);
        }
        gained
    }

    pub fn subscription_checked(&self) -> Option<NaiveDate> {
        self.data.lock().unwrap().subscription_checked
    }

    pub fn set_subscription_checked(&self, day: NaiveDate) {
        let mut data = self.data.lock().unwrap();
        data.subscription_checked = Some(day);
        self.save(&data);
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

    /// Coche ou décoche une quête pour un jour (aujourd'hui ou un jour passé, pour se recaler
    /// sur l'app Xbox). Les jours à venir sont ignorés.
    pub fn set_xbox_activity(&self, day: NaiveDate, activity: XboxActivity, done: bool) {
        if day > Local::now().date_naive() {
            return;
        }
        let mut data = self.data.lock().unwrap();
        let entry = data.xbox_days.entry(day).or_default();
        entry.set(activity, done);
        if *entry == DayPlay::default() {
            data.xbox_days.remove(&day);
        }
        self.save(&data);
    }

    /// Ajoute un jeu à la liste du mois en cours (sans doublon, casse ignorée).
    pub fn add_month_game(&self, game: &str) {
        let game = game.trim();
        if game.is_empty() {
            return;
        }
        let mut data = self.data.lock().unwrap();
        let games = data
            .month_games
            .entry(month_key(Local::now().date_naive()))
            .or_default();
        if !games.iter().any(|g| g.eq_ignore_ascii_case(game)) {
            games.push(game.to_string());
            self.save(&data);
        }
    }

    pub fn remove_month_game(&self, game: &str) {
        let mut data = self.data.lock().unwrap();
        if let Some(games) = data
            .month_games
            .get_mut(&month_key(Local::now().date_naive()))
        {
            games.retain(|g| !g.eq_ignore_ascii_case(game.trim()));
        }
        self.save(&data);
    }

    /// Jeux joués ce mois-ci sans être nommés (les packs ne comptent que le nombre).
    pub fn set_month_extra(&self, extra: u32) {
        let mut data = self.data.lock().unwrap();
        data.month_extra
            .insert(month_key(Local::now().date_naive()), extra.min(8));
        self.save(&data);
    }

    /// Partie détectée automatiquement : coche l'activité ce jour-là et ajoute le jeu au mois.
    /// Rien n'est jamais décoché ni retiré. Renvoie `true` si le suivi a changé.
    pub fn record_detected(&self, day: NaiveDate, activity: XboxActivity, game: &str) -> bool {
        let mut data = self.data.lock().unwrap();
        let mut changed = false;
        let entry = data.xbox_days.entry(day).or_default();
        if !entry.get(activity) {
            entry.set(activity, true);
            changed = true;
        }
        let game = game.trim();
        if !game.is_empty() {
            let games = data.month_games.entry(month_key(day)).or_default();
            if !games.iter().any(|g| g.eq_ignore_ascii_case(game)) {
                games.push(game.to_string());
                changed = true;
            }
        }
        if changed {
            self.save(&data);
        }
        changed
    }

    pub fn xbox_progress(&self) -> XboxProgress {
        let today = Local::now().date_naive();
        let start = week_start(today);
        let data = self.data.lock().unwrap();
        let day = |d: NaiveDate| data.xbox_days.get(&d).cloned().unwrap_or_default();

        let week: Vec<WeekDay> = (0..7)
            .map(|i| {
                let date = start + chrono::Duration::days(i);
                WeekDay {
                    date,
                    play: day(date),
                    future: date > today,
                }
            })
            .collect();
        let count = |f: fn(&DayPlay) -> bool| week.iter().filter(|d| f(&d.play)).count() as u32;

        let mut mobile_streak = 0;
        let mut cursor = if day(today).mobile_app {
            today
        } else {
            today - chrono::Duration::days(1)
        };
        while day(cursor).mobile_app {
            mobile_streak += 1;
            cursor -= chrono::Duration::days(1);
        }

        let month = month_key(today);
        let month_games = data.month_games.get(&month).cloned().unwrap_or_default();
        let month_extra = data.month_extra.get(&month).copied().unwrap_or(0);
        let mut seen = BTreeSet::new();
        let known_games = data
            .month_games
            .values()
            .rev()
            .flatten()
            .filter(|g| seen.insert(g.to_lowercase()))
            .cloned()
            .collect();

        let today_play = day(today);
        let pc_days = count(|d| d.pc);
        XboxProgress {
            today_done: today_play.pc,
            today: today_play,
            pc_days,
            console_days: count(|d| d.console),
            game_pass_days: count(|d| d.game_pass),
            mobile_streak,
            month_count: (month_games.len() as u32 + month_extra).min(8),
            month_games,
            month_extra,
            known_games,
            week,
            week_days: pc_days,
            week_target: 5,
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

    #[test]
    fn jours_xbox_recales_apres_coup() {
        let path =
            std::env::temp_dir().join(format!("rewards-autoplay-xbox-{}.json", std::process::id()));
        let store = Store::load(path.clone());
        let today = Local::now().date_naive();
        let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
        store.set_xbox_activity(monday, XboxActivity::Pc, true);
        store.set_xbox_activity(today + chrono::Duration::days(1), XboxActivity::Pc, true);
        store.set_xbox_activity(today, XboxActivity::MobileApp, true);
        let progress = store.xbox_progress();
        assert_eq!(progress.week.len(), 7);
        assert!(progress.week[0].play.pc);
        assert_eq!(progress.pc_days, 1, "un jour à venir ne compte pas");
        assert_eq!(progress.mobile_streak, 1);
        // La détection ajoute sans retirer, et remplit la liste du mois.
        assert!(store.record_detected(today, XboxActivity::Pc, "Sea of Thieves"));
        assert!(!store.record_detected(today, XboxActivity::Pc, "sea of thieves"));
        store.set_month_extra(2);
        let progress = store.xbox_progress();
        assert_eq!(progress.month_games, vec!["Sea of Thieves"]);
        assert_eq!(progress.month_count, 3);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn points_gagnes_sans_compter_les_echanges() {
        let path =
            std::env::temp_dir().join(format!("rewards-autoplay-gain-{}.json", std::process::id()));
        let store = Store::load(path.clone());
        let d = |n| NaiveDate::from_ymd_opt(2026, 11, n).unwrap();
        store.record_balance(d(1), 1000);
        store.record_balance(d(2), 1100);
        store.record_balance(d(3), 400); // échange
        store.record_balance(d(4), 450);
        assert_eq!(store.points_gained(d(2), d(4)), 150);
        assert_eq!(store.points_gained(d(4), d(4)), 50);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sauvegarde_et_restauration() {
        let path = std::env::temp_dir().join(format!(
            "rewards-autoplay-backup-{}.json",
            std::process::id()
        ));
        let store = Store::load(path.clone());
        store.set_followed("sot", "Sea of Thieves", None, None, true);
        let backup = store.export_json();
        store.set_followed("sot", "Sea of Thieves", None, None, false);
        assert!(store.import_json("pas du json").is_err());
        assert!(
            store.followed_quests().is_empty(),
            "un fichier illisible ne change rien"
        );
        store.import_json(&backup).unwrap();
        assert_eq!(store.followed_quests().len(), 1);
        for ext in ["", ".bak"] {
            let _ = std::fs::remove_file(with_extension(&path, ext));
        }
    }

    #[test]
    fn reprend_l_ancien_suivi_xbox() {
        let path = std::env::temp_dir().join(format!(
            "rewards-autoplay-migrate-{}.json",
            std::process::id()
        ));
        let today = Local::now().date_naive();
        std::fs::write(
            &path,
            format!(r#"{{"xboxSessions":{{"{today}":{{"game":"Sea of Thieves"}}}}}}"#),
        )
        .unwrap();
        let store = Store::load(path.clone());
        let progress = store.xbox_progress();
        assert!(progress.today.pc);
        assert_eq!(progress.month_games, vec!["Sea of Thieves"]);
        let _ = std::fs::remove_file(path);
    }
}
