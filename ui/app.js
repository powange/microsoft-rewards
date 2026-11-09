const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const numberFormat = new Intl.NumberFormat("fr-FR");
const timeFormat = new Intl.DateTimeFormat("fr-FR", { hour: "2-digit", minute: "2-digit" });
const dateFormat = new Intl.DateTimeFormat("fr-FR", { weekday: "long", day: "numeric", month: "long" });

/** Crée un élément ; les textes passent par textContent, jamais par innerHTML. */
function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") el.className = value;
    else if (key.startsWith("on")) el.addEventListener(key.slice(2), value);
    else el.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}

let toastTimer;
function toast(message) {
  const el = $("toast");
  el.textContent = message;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), 4000);
}

const state = { summary: null, loggedIn: null, view: "today", settings: null };

// ---------- Navigation ----------

function showView(view) {
  state.view = view;
  for (const item of document.querySelectorAll(".nav-item")) {
    if (item.dataset.view === view) item.setAttribute("aria-current", "page");
    else item.removeAttribute("aria-current");
  }
  const todayView = state.loggedIn === false ? "login" : "today";
  $("view-login").hidden = !(view === "today" && todayView === "login");
  $("view-today").hidden = !(view === "today" && todayView === "today");
  $("view-settings").hidden = view !== "settings";
}

for (const item of document.querySelectorAll(".nav-item")) {
  item.addEventListener("click", () => showView(item.dataset.view));
}

// ---------- Rewards ----------

function setLoggedIn(loggedIn) {
  state.loggedIn = loggedIn;
  $("account-line").textContent = loggedIn ? "Compte Microsoft connecté" : "Non connecté";
  $("logout-btn").disabled = !loggedIn;
  showView(state.view);
}

function openUrl(url) {
  invoke("open_url", { url }).catch((err) => toast(String(err)));
}

function activityItem(activity) {
  const title = activity.url
    ? h("a", { href: "#", onclick: (e) => { e.preventDefault(); openUrl(activity.url); } }, activity.title)
    : activity.title;
  return h("li", { class: activity.complete ? "done" : "" },
    h("span", { class: `state${activity.complete ? " done" : ""}`, "aria-label": activity.complete ? "Terminé" : "À faire" }),
    h("div", {},
      h("div", { class: "title" }, title, " ",
        activity.xbox ? h("span", { class: "tag" }, "Xbox") : null,
        activity.locked ? h("span", { class: "tag lock" }, "Verrouillé") : null),
      activity.description ? h("div", { class: "desc" }, activity.description) : null),
    h("span", { class: "pts" }, `${activity.points} pts`));
}

function renderSummary(summary) {
  state.summary = summary;
  $("points").textContent = numberFormat.format(summary.availablePoints);
  const level = [];
  if (summary.level) level.push(`Niveau : ${summary.level.replace(/^Level/i, "")}`);
  if (summary.levelProgressMax) level.push(`${numberFormat.format(summary.levelProgress ?? 0)} / ${numberFormat.format(summary.levelProgressMax)} points ce mois-ci`);
  level.push(`${numberFormat.format(summary.lifetimePoints)} points gagnés au total`);
  $("level").textContent = level.join(" · ");
  $("fetched-at").textContent = `Actualisé à ${timeFormat.format(new Date(summary.fetchedAt))}`;
  $("fetch-error").hidden = true;

  const searches = $("searches");
  searches.replaceChildren(...(summary.searches.length ? summary.searches.map((c) => {
    const pct = c.max ? Math.min(100, (c.progress / c.max) * 100) : 0;
    const bar = h("div", { class: "bar" }, h("i", { style: `width:${pct}%` }));
    return h("div", {},
      h("div", { class: "counter-top" }, h("span", {}, c.label), h("b", {}, `${c.progress} / ${c.max} pts`)),
      bar);
  }) : [h("p", { class: "muted" }, "Aucun compteur de recherche renvoyé par Microsoft.")]));

  const daily = summary.dailySet;
  $("daily-set").replaceChildren(...(daily.length ? daily.map(activityItem)
    : [h("li", { class: "empty" }, "Pas d'activités du jour pour le moment.")]));

  const todo = summary.more.filter((a) => !a.complete);
  const done = summary.more.length - todo.length;
  todo.sort((a, b) => Number(b.xbox) - Number(a.xbox) || b.points - a.points);
  $("more").replaceChildren(...(todo.length ? todo.map(activityItem)
    : [h("li", { class: "empty" }, "Toutes les autres activités sont terminées.")]));
  $("more-done").textContent = done ? `${done} activité${done > 1 ? "s" : ""} déjà terminée${done > 1 ? "s" : ""}.` : "";

  const cards = summary.punchCards;
  $("punch-panel").hidden = cards.length === 0;
  $("punch-cards").replaceChildren(...cards.map((card) => h("li", { class: card.complete ? "done" : "" },
    h("span", { class: `state${card.complete ? " done" : ""}` }),
    h("div", {},
      h("div", { class: "title" },
        card.url ? h("a", { href: "#", onclick: (e) => { e.preventDefault(); openUrl(card.url); } }, card.title) : card.title,
        " ", card.xbox ? h("span", { class: "tag" }, "Xbox") : null),
      h("div", { class: "desc" }, `${card.done} / ${card.total} étapes`)),
    h("span", { class: "pts" }, card.points ? `${card.points} pts` : ""))));
}

async function refresh() {
  const button = $("refresh-btn");
  button.disabled = true;
  try {
    renderSummary(await invoke("refresh_rewards"));
    setLoggedIn(true);
  } catch (err) {
    if (String(err) === "not_logged_in") {
      setLoggedIn(false);
    } else {
      $("fetch-error").textContent = String(err);
      $("fetch-error").hidden = false;
    }
  } finally {
    button.disabled = false;
  }
}

$("refresh-btn").addEventListener("click", refresh);
$("login-btn").addEventListener("click", async () => {
  try {
    await invoke("login");
    $("login-hint").hidden = false;
  } catch (err) {
    toast(String(err));
  }
});
$("logout-btn").addEventListener("click", async () => {
  try {
    await invoke("logout");
    toast("Compte déconnecté.");
  } catch (err) {
    toast(String(err));
  }
});

listen("rewards-updated", (event) => {
  renderSummary(event.payload);
  setLoggedIn(true);
  $("login-hint").hidden = true;
});
listen("rewards-logged-out", () => setLoggedIn(false));

// ---------- Quêtes Xbox ----------

const PACK_TIERS = ["premium", "ultimate"];

function dots(container, filled, total) {
  container.replaceChildren(...Array.from({ length: total }, (_, i) => h("i", { class: i < filled ? "f" : "" })));
}

function renderXbox(progress) {
  const today = $("xbox-today-state");
  if (progress.todayDone) {
    today.replaceChildren(h("span", { class: "ok" }, "Jeu quotidien fait"),
      progress.todayGame ? ` · ${progress.todayGame}` : "");
  } else {
    today.replaceChildren(h("b", {}, "Jeu quotidien à faire"),
      h("div", { class: "muted small" }, "Jouez au moins 15 minutes à un jeu du catalogue Game Pass, lancé depuis l'app Xbox (pas Steam ni un lanceur tiers)."));
  }
  $("xbox-form").hidden = progress.todayDone;
  $("xbox-undo").hidden = !progress.todayDone;

  $("week-count").textContent = `${progress.weekDays} / ${progress.weekTarget} jours`;
  dots($("week-dots"), progress.weekDays, progress.weekTarget);

  const games = progress.monthGames.length;
  const packs = PACK_TIERS.includes(state.settings?.subscription);
  $("month-goal").hidden = !packs;
  $("month-count").textContent = `${games} jeu${games > 1 ? "x" : ""} différent${games > 1 ? "s" : ""} · pack 4 ${games >= 4 ? "✓" : `${games}/4`} · pack 8 ${games >= 8 ? "✓" : `${games}/8`}`;
  dots($("month-dots"), games, 8);
  $("month-games").textContent = progress.monthGames.join(", ");

  $("known-games").replaceChildren(...progress.knownGames.map((g) => h("option", { value: g })));
}

$("xbox-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const game = $("xbox-game").value.trim();
  renderXbox(await invoke("set_xbox_today", { game }));
  $("xbox-game").value = "";
});
$("xbox-undo").addEventListener("click", async () => renderXbox(await invoke("set_xbox_today", { game: null })));
$("open-xbox").addEventListener("click", () => invoke("open_xbox_app").catch((err) => toast(String(err))));

// ---------- Paramètres ----------

async function saveSettings() {
  state.settings = {
    mode: document.querySelector('input[name="mode"]:checked')?.value ?? "reminders",
    reminderTime: $("reminder-time").value || "19:00",
    remindBing: $("remind-bing").checked,
    remindXbox: $("remind-xbox").checked,
    subscription: $("subscription").value,
    autoCheckUpdates: $("auto-update").checked,
  };
  await invoke("save_settings", { settings: state.settings });
  renderXbox(await invoke("xbox_progress"));
}

function renderSettings(settings) {
  state.settings = settings;
  $(`mode-${settings.mode}`).checked = true;
  $("reminder-time").value = settings.reminderTime;
  $("remind-bing").checked = settings.remindBing;
  $("remind-xbox").checked = settings.remindXbox;
  $("subscription").value = settings.subscription;
  $("auto-update").checked = settings.autoCheckUpdates;
}

for (const id of ["reminder-time", "remind-bing", "remind-xbox", "subscription", "auto-update", "mode-reminders"]) {
  $(id).addEventListener("change", saveSettings);
}

$("autostart").addEventListener("change", async (e) => {
  try {
    await invoke("set_autostart", { enabled: e.target.checked });
  } catch (err) {
    e.target.checked = !e.target.checked;
    toast(String(err));
  }
});

$("test-reminder").addEventListener("click", async () => {
  const lines = await invoke("send_test_reminder");
  $("test-reminder-result").textContent = lines.length ? "Notification envoyée." : "Rien à faire aujourd'hui, aucune notification envoyée.";
});

$("open-logs").addEventListener("click", () => invoke("open_logs").catch((err) => toast(String(err))));

// ---------- Mises à jour ----------

function showUpdate(info) {
  $("update-title").textContent = `Version ${info.version} disponible (vous avez la ${info.currentVersion})`;
  $("update-notes").textContent = info.notes ?? "";
  $("update-box").hidden = false;
  $("update-banner-text").textContent = `La version ${info.version} de Rewards Autoplay est disponible.`;
  $("update-banner").hidden = false;
}

async function checkUpdate() {
  const status = $("update-status");
  const button = $("check-update");
  button.disabled = true;
  status.textContent = "Recherche…";
  try {
    const info = await invoke("check_update");
    if (info) {
      showUpdate(info);
      status.textContent = "";
    } else {
      status.textContent = "Vous avez la dernière version.";
    }
  } catch (err) {
    status.textContent = `Recherche impossible : ${err}`;
  } finally {
    button.disabled = false;
  }
}

async function installUpdate() {
  for (const id of ["install-update", "update-banner-install"]) $(id).disabled = true;
  $("update-bar").hidden = false;
  $("update-status").textContent = "Téléchargement…";
  try {
    await invoke("install_update");
  } catch (err) {
    $("update-status").textContent = `Installation impossible : ${err}`;
    for (const id of ["install-update", "update-banner-install"]) $(id).disabled = false;
  }
}

$("check-update").addEventListener("click", checkUpdate);
$("install-update").addEventListener("click", installUpdate);
$("update-banner-install").addEventListener("click", installUpdate);

listen("update-available", (event) => showUpdate(event.payload));
listen("update-progress", (event) => {
  const { downloaded, total } = event.payload;
  if (total) $("update-bar").querySelector("i").style.width = `${Math.min(100, (downloaded / total) * 100)}%`;
});
listen("check-update-requested", () => {
  showView("settings");
  checkUpdate();
});

// ---------- Démarrage ----------

(async function init() {
  $("today-date").textContent = dateFormat.format(new Date());
  const [info, settings, autostart, xbox, cached] = await Promise.all([
    invoke("app_info"),
    invoke("get_settings"),
    invoke("autostart_enabled"),
    invoke("xbox_progress"),
    invoke("cached_rewards"),
  ]);
  $("version-line").textContent = `Version ${info.version}`;
  $("current-version").textContent = info.version;
  renderSettings(settings);
  $("autostart").checked = autostart;
  renderXbox(xbox);
  if (cached) {
    renderSummary(cached);
    setLoggedIn(true);
  } else {
    showView("today");
  }
  refresh();
})();
