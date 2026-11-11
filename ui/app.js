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

const state = { summary: null, loggedIn: null, view: "today", settings: null, followed: [] };

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
  $("view-history").hidden = view !== "history";
  $("view-games").hidden = view !== "games";
  $("view-followed").hidden = view !== "followed";
  $("view-settings").hidden = view !== "settings";
  if (view === "games" && !games.catalog && !games.loading) loadCatalog(false);
  if (view === "history") loadHistory();
}

for (const item of document.querySelectorAll(".nav-item")) {
  item.addEventListener("click", () => showView(item.dataset.view));
}

// ---------- Rewards ----------

function setLoggedIn(loggedIn) {
  state.loggedIn = loggedIn;
  const name = state.summary?.accountName;
  $("account-line").replaceChildren(...(loggedIn
    ? [name ? h("b", {}, name) : "Compte Microsoft", h("div", { class: "small" }, "Compte Microsoft connecté")]
    : ["Non connecté"]));
  $("logout-btn").disabled = !loggedIn;
  showView(state.view);
}

function openUrl(url) {
  invoke("open_url", { url }).catch((err) => toast(String(err)));
}

/** Délai avant l'actualisation qui suit une page ouverte dans la fenêtre Rewards. */
const TASK_REFRESH_DELAY_MS = 10_000;
const EARN_URL = "https://rewards.bing.com/earn";
/**
 * Les activités du jour (« Ensemble du jour », série quotidienne) ne sont affichées comme des
 * liens que sur le tableau de bord : sur la page Gagner, elles sont cachées dans le panneau de
 * la série. C'est donc sur le tableau de bord qu'on clique dessus.
 */
const DAILY_URL = "https://rewards.bing.com/dashboard?section=dailyset";
/** Les recherches libres se font dans la fenêtre connectée au compte, pour être comptées. */
const BING_URL = "https://www.bing.com/";
let taskRefreshTimer;

/**
 * Ouvre une tâche dans la fenêtre Rewards de l'application, déjà connectée au compte.
 * C'est le clic sur le site qui valide une activité : ouvrir son lien ailleurs ne rapporte rien.
 * Avec `click` (le lien de la tâche), l'application clique elle-même sur la tâche une fois la
 * page chargée : la recherche Bing, le quiz… s'ouvrent alors directement, et sont comptés.
 */
function openTask(url, click = null, query = null, title = null, guide = null) {
  // Une tâche ouverte à la main met fin à la session guidée (côté application aussi).
  if (!guide && guided.index >= 0) endGuided({ quiet: true });
  invoke("open_rewards_site", { url, click, clickTitle: click ? title : null, query, guide }).catch((err) => toast(String(err)));
}

// Chaque page chargée dans la fenêtre Rewards (un clic sur une activité, une étape…)
// déclenche une actualisation 10 secondes plus tard ; la fermeture de la fenêtre aussi.
/**
 * Certaines recherches sont créditées plus tard : si rien n'a changé 10 s après une tâche,
 * une seconde actualisation a lieu 60 s après. Le délai est affiché dans la barre latérale.
 */
const TASK_RECHECK_DELAY_MS = 60_000;
const summarySignature = (s) => s ? `${s.availablePoints}|${JSON.stringify(s.searches)}|${[...s.dailySet, ...s.more].filter((a) => a.complete).length}` : "";
let refreshCountdown;

function scheduleTaskRefresh(delay, recheck) {
  clearTimeout(taskRefreshTimer);
  clearInterval(refreshCountdown);
  const due = Date.now() + delay;
  const label = $("side-refresh");
  const tick = () => {
    if (refreshing) return;
    label.textContent = `↻ Actualisation dans ${Math.max(0, Math.ceil((due - Date.now()) / 1000))} s`;
  };
  tick();
  refreshCountdown = setInterval(tick, 1000);
  taskRefreshTimer = setTimeout(async () => {
    clearInterval(refreshCountdown);
    const before = summarySignature(state.summary);
    await refresh();
    if (recheck && summarySignature(state.summary) === before) scheduleTaskRefresh(TASK_RECHECK_DELAY_MS, false);
  }, delay);
}

listen("rewards-site-navigated", () => scheduleTaskRefresh(TASK_REFRESH_DELAY_MS, true));
listen("rewards-site-closed", () => refresh());

/** Vignette à gauche du titre, comme sur le site Rewards et dans l'app Xbox. */
function withThumb(imageUrl, ...children) {
  const text = h("div", { class: "item-text" }, ...children);
  return imageUrl
    ? h("div", { class: "item-head" }, h("img", { class: "thumb", src: imageUrl, alt: "", loading: "lazy" }), text)
    : text;
}

/** Badge de la nature d'une tâche (recherche Bing, quiz…), avec une explication au survol. */
const KIND_TAGS = {
  search: ["🔍 Recherche Bing", "Ouvrir la tâche lance la recherche sur Bing : le clic suffit."],
  searchManual: ["🔍 Recherche à faire", "Il faut faire soi-même une ou plusieurs recherches sur Bing."],
  quiz: ["❓ Quiz", "Quiz à jouer sur Bing : répondez aux questions."],
  puzzle: ["🧩 Puzzle", "Puzzle d'images à terminer sur Bing."],
  partner: ["Site partenaire", "La tâche se fait sur le site d'un partenaire."],
  edgeSetting: ["Réglage Edge", "La tâche se fait dans le navigateur Edge (barre de recherche, moteur par défaut…)."],
  rewardsApp: ["Application Rewards", "À faire dans l'application Microsoft Rewards pour Windows : ces activités n'y sont proposées qu'à elle."],
  redeem: ["Échange", "Mène au catalogue d'échange de points."],
  referral: ["Parrainage", "Points gagnés quand des amis invités font des recherches sur Bing."],
};

function kindTag(kind) {
  const tag = KIND_TAGS[kind];
  return tag ? h("span", { class: `tag kind-${kind}`, title: tag[1] }, tag[0]) : null;
}

const isSearch = (kind) => kind === "search" || kind === "searchManual";

/** Page du site où se trouve l'activité : tableau de bord pour celles du jour, sinon page Gagner. */
function activityPage(activity) {
  return state.summary?.dailySet.some((a) => a.id === activity.id) ? DAILY_URL : EARN_URL;
}

function activityItem(activity) {
  const title = activity.url
    ? h("a", { href: "#", onclick: (e) => { e.preventDefault(); openTask(activityPage(activity), activity.complete || activity.locked ? null : activity.url, activity.searchQuery, activity.title); } }, activity.title)
    : activity.title;
  return h("li", { class: activity.complete ? "done" : "" },
    h("span", { class: `state${activity.complete ? " done" : ""}`, "aria-label": activity.complete ? "Terminé" : "À faire" }),
    withThumb(activity.imageUrl,
      h("div", { class: "title" }, title, " ",
        activity.xbox ? h("span", { class: "tag" }, "Xbox") : null, " ",
        kindTag(activity.kind), " ",
        activity.locked ? h("span", { class: "tag lock" }, "Verrouillé") : null),
      activity.description ? h("div", { class: "desc" }, activity.description) : null,
      activity.note ? h("div", { class: "desc" }, activity.note) : null),
    h("span", { class: "pts" }, activity.points ? `${activity.points} pts` : ""));
}

/**
 * Niveau : il dépend des points gagnés chaque mois. Affiche les points du mois, ce qu'il
 * manque pour garder le niveau actuel (s'il n'est pas encore assuré) et pour le suivant.
 */
function renderLevel(summary) {
  const status = summary.levelStatus;
  const box = $("level");
  if (!status) {
    const parts = [];
    if (summary.level) parts.push(`Niveau : ${summary.level}`);
    if (summary.levelProgressMax) parts.push(`${numberFormat.format(summary.levelProgress ?? 0)} / ${numberFormat.format(summary.levelProgressMax)} points vers le niveau suivant`);
    box.replaceChildren(parts.join(" · "));
    return;
  }
  const now = new Date();
  const monthEnd = new Date(now.getFullYear(), now.getMonth() + 1, 0);
  const daysLeft = monthEnd.getDate() - now.getDate() + 1;
  const level = (n) => status.levels.find((l) => l.level === n);
  const current = level(status.current);
  const next = level(status.current + 1);
  const pts = (n) => numberFormat.format(n);
  const parts = [h("span", {}, `Niveau : ${status.name ?? summary.level ?? status.current}`), ` · ce mois-ci ${pts(status.monthPoints)} pts`];
  if (current?.points && status.monthPoints < current.points) {
    parts.push(" · ", h("b", { class: "level-risk" }, `encore ${pts(current.points - status.monthPoints)} pts pour garder le niveau ${current.title ?? ""}`));
  }
  if (next?.points) {
    const missing = Math.max(0, next.points - status.monthPoints);
    const activities = next.activities ? ` et ${next.activities} activités de montée en niveau` : "";
    parts.push(` · ${next.title ?? "niveau suivant"} : ${missing ? `encore ${pts(missing)} pts` : "points atteints"}${activities}`);
  }
  parts.push(` · ${daysLeft} jour${daysLeft > 1 ? "s" : ""} restant${daysLeft > 1 ? "s" : ""} dans le mois`);
  box.replaceChildren(...parts);
}

/** Lecture incomplète : le site a probablement changé ; proposer de le signaler. */
function renderSiteWarning(warnings) {
  $("site-warning").hidden = !warnings.length;
  if (!warnings.length) return;
  $("site-warning-text").textContent = `Le site Microsoft Rewards semble avoir changé : ${warnings.join(", ")} n'${warnings.length > 1 ? "ont" : "a"} pas pu être lu${warnings.length > 1 ? "s" : ""}. Ces informations peuvent manquer ici ; le site reste utilisable normalement.`;
}

$("site-warning-report").addEventListener("click", () => {
  const warnings = state.summary?.warnings ?? [];
  const version = $("current-version").textContent;
  const title = encodeURIComponent(`Lecture incomplète du site Rewards : ${warnings.join(", ")}`);
  const body = encodeURIComponent(`Version : ${version}\nÉléments non lus : ${warnings.join(", ")}\nDate : ${new Date().toISOString()}\n\nLa page lue est enregistrée dans le dossier des journaux (Paramètres › Ouvrir le dossier des journaux, fichier rewards-earn.html). Elle contient le nom du compte et le solde : à anonymiser avant de la joindre.`);
  openUrl(`https://github.com/powange/microsoft-rewards/issues/new?title=${title}&body=${body}`);
});

/** La date du jour change pendant que l'application reste des jours dans la zone de notification. */
function renderToday() {
  $("today-date").textContent = dateFormat.format(new Date());
}
document.addEventListener("visibilitychange", () => { if (!document.hidden) renderToday(); });
window.addEventListener("focus", renderToday);

function renderSummary(summary) {
  state.summary = summary;
  renderToday();
  $("points").textContent = numberFormat.format(summary.availablePoints);
  renderLevel(summary);
  const claim = $("claim");
  claim.hidden = false;
  claim.disabled = summary.claimablePoints <= 0;
  claim.classList.toggle("ready", summary.claimablePoints > 0);
  claim.textContent = summary.claimablePoints > 0
    ? `${numberFormat.format(summary.claimablePoints)} points prêts à réclamer · Réclamer ↗`
    : "Aucun point à réclamer";
  const every = state.settings?.refreshIntervalMinutes;
  $("fetched-at").textContent = `Actualisé à ${timeFormat.format(new Date(summary.fetchedAt))}`
    + (every ? ` · automatiquement toutes les ${every} min` : "");
  $("fetch-error").hidden = true;
  renderSiteWarning(summary.warnings ?? []);

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

  // « Explorer sur Bing » a sa propre section, comme sur le site : à faire, puis verrouillées, puis faites.
  const isExplore = (a) => a.id.toLowerCase().includes("exploreonbing");
  const explore = summary.more.filter(isExplore);
  const exploreRank = (a) => (a.complete ? 2 : a.locked ? 1 : 0);
  explore.sort((a, b) => exploreRank(a) - exploreRank(b));
  $("explore-panel").hidden = explore.length === 0;
  $("explore-count").textContent = `${explore.filter((a) => a.complete).length} / ${explore.length} faites`;
  $("explore").replaceChildren(...explore.map(activityItem));

  // À faire d'abord (Xbox puis les plus payantes), les activités terminées à la fin.
  const others = summary.more.filter((a) => !isExplore(a));
  const todo = others.filter((a) => !a.complete);
  const done = others.filter((a) => a.complete);
  todo.sort((a, b) => Number(b.xbox) - Number(a.xbox) || b.points - a.points);
  $("more").replaceChildren(...(others.length ? [...todo, ...done].map(activityItem)
    : [h("li", { class: "empty" }, "Aucune activité pour le moment.")]));
  $("more-done").textContent = todo.length || !done.length ? "" : "Toutes les activités de cette section sont terminées.";

  renderSearchesToDo(summary);

  const cards = summary.punchCards;
  $("punch-panel").hidden = cards.length === 0;
  $("punch-cards").replaceChildren(...cards.map((card) => questItem(card)));

  renderStreaks(summary.streaks);
  renderGames();
  renderFollowed();
  renderGoal();
  if (state.view === "history") loadHistory();
}

/**
 * « Recherches à faire aujourd'hui » : activités et étapes de quêtes non verrouillées qui
 * demandent une recherche Bing, celles où le clic suffit d'abord, plus les points de
 * recherche quotidiens restants.
 */
function renderSearchesToDo(summary) {
  const pending = (t) => !t.complete && !t.locked && isSearch(t.kind);
  const tasks = [
    ...[...summary.dailySet, ...summary.more].filter(pending).map((a) => ({
      kind: a.kind, title: a.title, context: a.description, points: a.points, imageUrl: a.imageUrl, url: activityPage(a), click: a.url, query: a.searchQuery,
    })),
    ...summary.punchCards.flatMap((card) => card.steps.filter(pending).map((step) => ({
      kind: step.kind, title: step.title, context: `Quête : ${card.title}`, points: 0, imageUrl: card.imageUrl, url: card.url ?? step.url, click: card.url ? step.url : null, query: step.searchQuery,
    }))),
  ];
  tasks.sort((a, b) => Number(a.kind !== "search") - Number(b.kind !== "search") || b.points - a.points);
  const leftOf = (mobile) => summary.searches
    .filter((c) => /mobile/i.test(c.label) === mobile)
    .reduce((sum, c) => sum + Math.max(0, c.max - c.progress), 0);
  const pcLeft = leftOf(false);
  const mobileLeft = leftOf(true);

  const info = [];
  if (tasks.length) info.push(`${tasks.length} tâche${tasks.length > 1 ? "s" : ""}`);
  if (pcLeft) info.push(`${pcLeft} pts de recherche sur PC`);
  if (mobileLeft) info.push(`${mobileLeft} pts sur mobile`);
  $("bing-todo-info").textContent = info.join(" · ");

  const items = tasks.map((t) => h("li", {},
    h("span", { class: "state", "aria-label": "À faire" }),
    withThumb(t.imageUrl,
      h("div", { class: "title" },
        t.url ? h("a", { href: "#", onclick: (e) => { e.preventDefault(); openTask(t.url, t.click, t.query, t.title); } }, t.title) : t.title,
        " ", kindTag(t.kind)),
      t.context ? h("div", { class: "desc" }, t.context) : null),
    h("span", { class: "pts" }, t.points ? `${t.points} pts` : "")));
  // Recherches libres : celles sur ordinateur se font ici, celles sur mobile dans l'application
  // Bing du téléphone. Une ligne chacune, pour ne pas promettre des points inaccessibles sur PC.
  for (const c of summary.searches.filter((c) => c.max > c.progress)) {
    const mobile = /mobile/i.test(c.label);
    items.push(h("li", {},
      h("span", { class: "state", "aria-label": "À faire" }),
      h("div", { class: "item-text" },
        h("div", { class: "title" },
          mobile ? "Recherches libres sur mobile"
            : h("a", { href: "#", onclick: (e) => { e.preventDefault(); openTask(BING_URL); } }, "Recherches libres sur Bing"),
          " ", kindTag("searchManual")),
        h("div", { class: "desc" }, mobile
          ? `${c.progress} / ${c.max} pts · à faire dans l'application Bing de votre téléphone, connectée au même compte`
          : `${c.label} : ${c.progress} / ${c.max} pts`),
        mobile ? h("details", { class: "qr-details" },
          h("summary", {}, "Pas encore l'application Bing ? Code QR"),
          h("div", { class: "qr-body" },
            h("img", { class: "qr", src: "bing-app-qr.svg", width: "132", height: "132", alt: "Code QR vers l'application Bing pour iPhone et Android" }),
            h("span", { class: "muted small" }, "Scannez-le avec l'appareil photo du téléphone, puis connectez l'application au même compte Microsoft."))) : null),
      h("span", { class: "pts" }, `${c.max - c.progress} pts`)));
  }
  $("bing-todo").replaceChildren(...(items.length ? items
    : [h("li", { class: "empty" }, "Toutes les recherches Bing du jour sont faites ✓")]));
}

// ---------- Session guidée (N7) ----------
// « Faire mes tâches » ouvre les tâches une par une dans la fenêtre Microsoft Rewards ; un
// bandeau y propose « Tâche suivante ». Chaque tâche reste cliquée à la demande de
// l'utilisateur : rien n'est fait sans lui.

const guided = { tasks: [], index: -1 };
const GUIDED_RANK = { search: 0, quiz: 1, puzzle: 2, searchManual: 3 };

function guidedTasks(summary) {
  const open = (t) => !t.complete && !t.locked && t.url;
  const activity = (a, daily) => ({
    key: `a#${a.id}`, title: a.title, page: daily ? DAILY_URL : EARN_URL, click: a.url, query: a.searchQuery,
    rank: daily ? -1 : GUIDED_RANK[a.kind] ?? 4,
  });
  const activities = [
    ...summary.dailySet.filter(open).map((a) => activity(a, true)),
    ...summary.more.filter((a) => open(a) && a.points > 0 && a.kind !== "edgeSetting").map((a) => activity(a, false)),
  ];
  const steps = summary.punchCards.filter((c) => c.url && !c.complete).flatMap((card) =>
    card.steps.filter(open).map((step) => ({
      key: `s#${card.id}#${step.title}`, title: step.title, page: card.url, click: step.url, query: step.searchQuery, rank: 5,
    })));
  return [...activities, ...steps].sort((a, b) => a.rank - b.rank);
}

/** Une tâche terminée entre-temps (actualisation après chaque tâche) est sautée. */
function guidedTaskDone(task) {
  const summary = state.summary;
  if (!summary) return false;
  return !guidedTasks(summary).some((t) => t.key === task.key);
}

function startGuided() {
  if (!state.summary) {
    toast("Connectez-vous d'abord à Microsoft Rewards.");
    return;
  }
  guided.tasks = guidedTasks(state.summary);
  if (!guided.tasks.length) {
    toast("Rien à faire pour l'instant : toutes les tâches accessibles sont terminées.");
    return;
  }
  guided.index = 0;
  openGuided();
}

function openGuided() {
  const task = guided.tasks[guided.index];
  const total = guided.tasks.length;
  $("guided-text").textContent = `Session guidée · tâche ${guided.index + 1} / ${total} : ${task.title}. Passez à la suivante depuis le bandeau de la fenêtre Microsoft Rewards.`;
  $("guided-banner").hidden = false;
  openTask(task.page, task.click, task.query, task.title, { index: guided.index, total, title: task.title });
}

function nextGuided() {
  if (guided.index < 0) return;
  do guided.index++;
  while (guided.index < guided.tasks.length && guidedTaskDone(guided.tasks[guided.index]));
  if (guided.index >= guided.tasks.length) endGuided();
  else openGuided();
}

function endGuided({ quiet = false } = {}) {
  if (guided.index < 0) return;
  guided.index = -1;
  $("guided-banner").hidden = true;
  if (!quiet) {
    invoke("stop_guided").catch(() => {});
    toast("Session guidée terminée.");
  }
}

$("guided-btn").addEventListener("click", startGuided);
$("guided-stop").addEventListener("click", () => endGuided());
listen("guided-start", () => { showView("today"); startGuided(); });
listen("guided-next", nextGuided);
listen("guided-end", () => endGuided());
listen("show-view", (event) => showView(String(event.payload)));

/** « dans 2 h 15 min » jusqu'à une date ISO. */
function timeUntil(iso) {
  const minutes = Math.max(0, Math.round((new Date(iso) - Date.now()) / 60000));
  if (minutes < 60) return `dans ${minutes} min`;
  if (minutes >= 48 * 60) return `dans ${Math.round(minutes / 1440)} j`;
  const hours = Math.floor(minutes / 60);
  return `dans ${hours} h${minutes % 60 ? ` ${minutes % 60} min` : ""}`;
}

function streakCard(streak) {
  // Le jour en cours est le dernier validé s'il est fait, sinon le suivant.
  const today = streak.todayDone ? streak.completedDays - 1 : streak.completedDays;
  const days = Array.from({ length: streak.totalDays }, (_, i) => {
    const points = streak.dailyPoints[i];
    const classes = ["day", i < streak.completedDays ? "done" : "", i === today ? "today" : "", i === streak.totalDays - 1 ? "bonus" : ""];
    return h("span", { class: classes.filter(Boolean).join(" "), title: `Jour ${i + 1}${points ? ` : ${points} pts` : ""}` },
      i < streak.completedDays ? "✓" : "", i === today || i === streak.totalDays - 1 ? (points ?? "") : "");
  });
  return h("div", { class: `streak${streak.todayDone ? "" : " todo"}` },
    streak.imageUrl ? h("img", { src: streak.imageUrl, alt: "", loading: "lazy" }) : null,
    h("b", {}, streak.title),
    h("div", { class: "days", "aria-label": `${streak.completedDays} jours sur ${streak.totalDays}` }, days),
    h("div", { class: "streak-foot" },
      h("span", { class: streak.todayDone ? "ok" : "warn" }, streak.todayDone ? `✓ ${streak.todayLabel}` : `À faire : ${streak.todayLabel}`),
      streak.url ? h("button", { class: "btn small", type: "button", onclick: () => openTask(streak.url) }, "Y aller ↗") : null));
}

function renderStreaks(data) {
  const streaks = data?.streaks ?? [];
  $("streaks-panel").hidden = streaks.length === 0 && !data?.stamps;
  const info = [];
  if (data?.counter != null) info.push(`Série en cours : ${data.counter} jour${data.counter > 1 ? "s" : ""}`);
  if (data?.protectionOn != null) info.push(`protection ${data.protectionOn ? "activée" : "désactivée"}`);
  if (data?.resetAt) info.push(`réinitialisation ${timeUntil(data.resetAt)}`);
  $("streaks-info").textContent = info.join(" · ");

  const cards = [];
  if (data?.stamps) {
    const { collected, total, bonusPoints } = data.stamps;
    cards.push(h("div", { class: "streak stamps" },
      h("b", { class: "bonus-title" }, `${numberFormat.format(bonusPoints)} points bonus`),
      h("span", { class: "muted small" }, `Gagnez ${total} tampons : chaque série de 7 jours terminée en donne un.`),
      h("div", { class: "stamp-grid" }, Array.from({ length: total }, (_, i) => h("span", { class: i < collected ? "stamp on" : "stamp" }))),
      h("span", { class: "small" }, `${collected} / ${total} tampons`)));
  }
  cards.push(...streaks.map(streakCard));
  $("streaks").replaceChildren(...cards);
}

let refreshing = false;

/**
 * Actualise tout : points, activités, séries, quêtes, et quêtes Xbox si un compte est connecté.
 * `force` (boutons Actualiser) relit aussi les pages de quête et le service Xbox, que les
 * actualisations automatiques reprennent de leur dernière lecture quand rien n'a changé.
 */
async function refresh({ force = false } = {}) {
  if (refreshing) return;
  refreshing = true;
  clearTimeout(taskRefreshTimer);
  clearInterval(refreshCountdown);
  const buttons = [$("refresh-btn"), $("side-refresh")];
  for (const button of buttons) button.disabled = true;
  $("side-refresh").textContent = "Actualisation…";
  try {
    const summary = await invoke("refresh_rewards", { force });
    if (isNewSummary(summary)) renderSummary(summary);
    setLoggedIn(true);
  } catch (err) {
    if (String(err) === "not_logged_in") {
      setLoggedIn(false);
    } else {
      $("fetch-error").textContent = String(err);
      $("fetch-error").hidden = false;
    }
  } finally {
    if (xboxLive.status.connected) await loadXbox(force);
    for (const button of buttons) button.disabled = false;
    $("side-refresh").textContent = "↻ Actualiser";
    refreshing = false;
  }
}

$("refresh-btn").addEventListener("click", () => refresh({ force: true }));
// L'encart « Prêt à réclamer » est en haut du tableau de bord Rewards.
$("claim").addEventListener("click", () => openTask("https://rewards.bing.com/dashboard"));
$("side-refresh").addEventListener("click", () => refresh({ force: true }));
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
    Object.assign(xboxLive, { status: { connected: false, gamertag: null }, rewards: null, error: null });
    renderXboxLive();
    toast("Comptes Microsoft et Xbox Live déconnectés.");
  } catch (err) {
    toast(String(err));
  }
});

// La commande d'actualisation et cet événement portent la même lecture : elle n'est affichée qu'une fois.
const isNewSummary = (summary) => state.summary?.fetchedAt !== summary.fetchedAt;

listen("rewards-updated", (event) => {
  if (isNewSummary(event.payload)) renderSummary(event.payload);
  setLoggedIn(true);
  $("login-hint").hidden = true;
});
listen("rewards-logged-out", () => setLoggedIn(false));

// ---------- Quêtes Xbox ----------



// Barèmes du Rewards Hub de l'app Xbox (novembre 2026), par formule :
// [Tout le monde, Essential, Premium, Ultimate].
const XBOX_TIER = { none: 0, essential: 1, premium: 2, pcGamePass: 2, ultimate: 3 };
const XBOX_DAILY = [
  { act: "pc", device: "xboxPc", title: "Jouer à un jeu PC", desc: "15 minutes sur PC", pts: [5, 10, 10, 20] },
  { act: "jewel", device: "xboxMobile", title: "Jouer à Jewel", desc: "Un niveau dans l'application mobile Xbox", pts: [5, 5, 5, 10] },
  { act: "console", device: "xboxConsole", title: "Jouer sur la console", desc: "15 minutes sur console", pts: [5, 10, 10, 20] },
  { act: "gamePass", gamePass: true, title: "Jouer à un jeu Game Pass", desc: "15 minutes, Premium et Ultimate", pts: [0, 0, 10, 20] },
];
const XBOX_WEEKLY_BONUS = [110, 110, 130, 260];
const XBOX_MOBILE_DAYS = [2, 6, 10, 14, 18];

function xboxTier() {
  return XBOX_TIER[state.settings?.subscription] ?? 0;
}

const hasGamePassQuests = () => xboxTier() >= 2;

async function setXboxActivity(date, activity, done) {
  renderXbox(await invoke("set_xbox_activity", { date, activity, done }));
}

/** Sept pastilles du lundi au dimanche, cochables pour une activité donnée. */
function weekChips(progress, act, label) {
  const dayName = new Intl.DateTimeFormat("fr-FR", { weekday: "long", day: "numeric", month: "long" });
  return h("div", { class: "week-days", role: "group", "aria-label": label },
    progress.week.map((d) => {
      const name = dayName.format(dayDate(d.date));
      const done = d.play[act];
      return h("button", {
        class: "day", type: "button", "aria-pressed": done ? "true" : "false", disabled: d.future,
        title: d.future ? `${name} : à venir` : `${name} : ${done ? "fait (cliquer pour retirer)" : "pas fait (cliquer pour cocher)"}`,
        onclick: () => setXboxActivity(d.date, act, !done),
      }, name.charAt(0).toUpperCase());
    }));
}

function seriesCard(title, desc, status, chips, extra) {
  return h("div", { class: "xq-card" },
    h("div", { class: "xq-head" }, h("b", {}, title), h("span", { class: "xq-status" }, status)),
    h("div", { class: "muted small" }, desc),
    chips, extra ?? null);
}

function renderXbox(progress) {
  state.xbox = progress;
  const settings = state.settings ?? {};
  const tier = xboxTier();
  const today = progress.week.find((d) => !d.future && d.date === isoDay(new Date()))?.date ?? isoDay(new Date());
  const shown = (q) => (q.gamePass ? hasGamePassQuests() : settings[q.device] !== false);

  // Quêtes du jour
  $("xq-daily").replaceChildren(...XBOX_DAILY.filter(shown).map((q) => {
    const done = progress.today[q.act];
    return h("button", {
      class: `xq-daily-card${done ? " done" : ""}`, type: "button", "aria-pressed": done ? "true" : "false",
      title: done ? "Fait aujourd'hui (cliquer pour retirer)" : "Cliquer quand c'est fait",
      onclick: () => setXboxActivity(today, q.act, !done),
    },
      h("span", { class: "state" + (done ? " done" : "") }),
      h("span", { class: "xq-text" }, h("b", {}, q.title), h("span", { class: "muted small" }, q.desc)),
      h("span", { class: "pts" }, `+${q.pts[tier]}`));
  }));

  // Séries de la semaine
  const weekly = [];
  if (settings.xboxPc !== false) {
    weekly.push(seriesCard("Bonus hebdomadaire sur PC", "Jouez 5 jours cette semaine sur PC pendant 15 minutes.",
      `${Math.min(progress.pcDays, 5)}/5 · +${XBOX_WEEKLY_BONUS[tier]}`, weekChips(progress, "pc", "Jours joués sur PC")));
  }
  if (settings.xboxConsole) {
    weekly.push(seriesCard("Bonus console hebdomadaire", "Jouez 5 jours cette semaine sur console.",
      `${Math.min(progress.consoleDays, 5)}/5 · +${XBOX_WEEKLY_BONUS[tier]}`, weekChips(progress, "console", "Jours joués sur console")));
  }
  if (settings.xboxMobile !== false) {
    const streak = progress.mobileStreak;
    const next = XBOX_MOBILE_DAYS[Math.min(streak, 4)];
    const status = progress.today.mobileApp
      ? `Jour ${Math.min(streak, 5)} fait`
      : `Jour ${Math.min(streak + 1, 5)} à faire · +${next}`;
    weekly.push(seriesCard("Application mobile Xbox", `Connectez-vous chaque jour à l'application mobile : +${XBOX_MOBILE_DAYS.join(", +")} (barème de base, plus selon la formule).`,
      status, weekChips(progress, "mobileApp", "Jours de connexion à l'application mobile")));
  }
  if (hasGamePassQuests()) {
    const mult = tier === 3 ? 2 : 1;
    weekly.push(seriesCard("Série hebdomadaire Game Pass", `Jouez à un jeu Game Pass au moins 5 jours : 5 j +${40 * mult} et semaine suivante ×2, 6 j +${10 * mult}, 7 j +${20 * mult}.`,
      `${progress.gamePassDays}/7`, weekChips(progress, "gamePass", "Jours joués à un jeu Game Pass")));
  }
  $("xq-weekly").replaceChildren(...weekly);

  // Packs du mois
  $("xq-monthly-wrap").hidden = !hasGamePassQuests();
  const ultimate = tier === 3;
  const count = progress.monthCount;
  const pack = (n, pts) => h("span", { class: `xq-pack${count >= n ? " done" : ""}` }, `Pack ${n} jeux : ${Math.min(count, n)}/${n} · +${pts}`);
  const addInput = h("input", { id: "month-game-input", list: "known-games", placeholder: "Ajouter un jeu joué ce mois-ci", autocomplete: "off" });
  const addGame = async () => {
    const game = addInput.value.trim();
    if (!game) return;
    renderXbox(await invoke("set_month_game", { game, add: true }));
  };
  addInput.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); addGame(); } });
  const setExtra = async (n) => renderXbox(await invoke("set_month_extra", { extra: Math.max(0, n) }));
  $("xq-monthly").replaceChildren(h("div", { class: "xq-card" },
    h("div", { class: "xq-head" }, h("b", {}, "Jeux Game Pass joués ce mois-ci"), h("span", { class: "xq-status" }, `${count} jeu${count > 1 ? "x" : ""}`)),
    h("div", { class: "xq-packs" }, pack(4, ultimate ? 80 : 40), pack(8, ultimate ? 560 : 280)),
    h("div", { class: "chips" }, progress.monthGames.map((g) =>
      h("span", { class: "chip" }, g, h("button", { class: "chip-x", type: "button", "aria-label": `Retirer ${g}`, title: "Retirer de ce mois-ci", onclick: async () => renderXbox(await invoke("set_month_game", { game: g, add: false })) }, "×")))),
    h("div", { class: "inline" }, addInput, h("button", { class: "btn small", type: "button", onclick: addGame }, "Ajouter")),
    h("div", { class: "xq-extra" },
      h("span", { class: "muted small" }, "Jeux joués sans les nommer :"),
      h("button", { class: "btn small", type: "button", "aria-label": "Un de moins", disabled: progress.monthExtra <= 0, onclick: () => setExtra(progress.monthExtra - 1) }, "−"),
      h("b", {}, String(progress.monthExtra)),
      h("button", { class: "btn small", type: "button", "aria-label": "Un de plus", onclick: () => setExtra(progress.monthExtra + 1) }, "+"))));

  $("known-games").replaceChildren(...progress.knownGames.map((g) => h("option", { value: g })));
  renderGames();
}

$("open-xbox").addEventListener("click", () => invoke("open_xbox_app").catch((err) => toast(String(err))));

// ---------- Quêtes Xbox réelles (connexion Xbox Live, expérimental) ----------

const xboxLive = { status: { connected: false, gamertag: null }, rewards: null, error: null, loading: false };

function missionIcon(m) {
  if (m.locked) return lockIcon();
  if (m.expired) return h("span", { class: "state failed", "aria-label": "Expirée" });
  return h("span", { class: `state${m.claimed || m.claimable ? " done" : ""}`, "aria-label": m.claimed ? "Réclamée" : m.claimable ? "À réclamer" : "À faire" });
}

function missionItem(m) {
  const meta = [];
  if (m.statusLabel) meta.push(m.statusLabel);
  if (m.kind === "single" && m.target) meta.push(`${m.count ?? 0}/${m.target}`);
  if (m.kind !== "single" && m.target) meta.push(`${m.count}/${m.target}`);
  if (m.resetDate && !m.claimed) meta.push(`réinitialisation ${timeUntil(m.resetDate)}`);
  const days = m.steps.length
    ? h("div", { class: "days" }, m.steps.map((step, i) => h("span", {
        class: ["day", step.done ? "done" : "", i === m.current ? "today" : "", step.points ? "bonus" : ""].filter(Boolean).join(" "),
        title: [step.label, step.points ? `+${step.points} pts` : "", step.note ?? ""].filter(Boolean).join(" · "),
      }, step.done ? "✓" : step.points ? `+${step.points}` : "")))
    : null;
  return h("li", { class: m.claimed ? "done" : m.locked ? "locked" : "" },
    missionIcon(m),
    h("div", {},
      withThumb(m.imageUrl,
        h("div", { class: "title" }, m.title, " ", m.claimable ? h("span", { class: "tag" }, "À réclamer") : null),
        meta.length ? h("div", { class: "desc" }, meta.join(" · ")) : null,
        m.description ? h("div", { class: "desc" }, m.description) : null,
        m.requirement ? h("div", { class: "desc" }, `Pour débloquer : ${m.requirement}`) : null),
      days),
    h("span", { class: "pts" }, m.points ? `+${m.points} pts` : ""));
}

function renderXboxLive() {
  const box = $("xbox-live");
  const { status, rewards, error, loading } = xboxLive;
  $("xbox-sync-note").textContent = status.connected ? status.playedSync ?? "" : "";
  const refused = error?.startsWith("xbox_refused");
  // Le suivi manuel reste affiché tant que les vraies données ne sont pas disponibles.
  $("xbox-manual").hidden = Boolean(status.connected && rewards && !error);

  if (!status.connected) {
    box.replaceChildren(h("div", { class: "xbox-connect" },
      h("div", {},
        h("b", {}, "Afficher vos vraies quêtes Xbox "), h("span", { class: "tag" }, "Expérimental"),
        h("div", { class: "muted small" }, "Connectez votre compte Xbox Live pour lire le Rewards Hub de l'app Xbox : quêtes du jour, bonus hebdomadaires PC et console, application mobile, packs et points à réclamer."),
        error && !refused ? h("div", { class: "error-text small" }, String(error)) : null),
      h("button", { class: "btn primary", type: "button", onclick: xboxLogin }, "Connecter Xbox Live")));
    return;
  }

  const head = h("div", { class: "xbox-live-head" },
    h("span", {}, "Connecté à Xbox Live", status.gamertag ? h("b", {}, ` · ${status.gamertag}`) : null,
      rewards?.tier ? h("span", { class: "muted small" }, ` · ${rewards.tier}${rewards.tierMax ? ` (${rewards.tierProgress ?? 0}/${rewards.tierMax})` : ""}`) : null),
    h("span", { class: "btnrow" },
      h("button", { class: "btn small", type: "button", disabled: loading, onclick: () => loadXbox(true) }, loading ? "Actualisation…" : "Actualiser"),
      h("button", { class: "btn small ghost", type: "button", onclick: xboxLogout }, "Se déconnecter")));

  // Refus du service : l'encart se replie, le suivi manuel prend la place.
  if (refused) {
    box.replaceChildren(h("details", { class: "xbox-refused" },
      h("summary", {}, "Quêtes Xbox réelles indisponibles ", h("span", { class: "tag" }, "Expérimental")),
      h("p", { class: "muted small" }, `Connecté${status.gamertag ? ` en tant que ${status.gamertag}` : ""}, mais le service des quêtes de Microsoft n'accepte que l'app Xbox elle-même. Le suivi manuel ci-dessous le remplace ; un nouvel essai a lieu une fois par jour.`),
      h("span", { class: "btnrow" },
        h("button", { class: "btn small", type: "button", disabled: loading, onclick: () => loadXbox(true) }, loading ? "Essai…" : "Réessayer"),
        h("button", { class: "btn small ghost", type: "button", onclick: xboxLogout }, "Se déconnecter de Xbox Live"))));
    return;
  }
  const parts = [head];
  if (error) {
    parts.push(h("div", { class: "banner danger" }, h("span", {}, `Quêtes Xbox illisibles : ${error}`)));
  } else if (!rewards) {
    parts.push(h("p", { class: "muted" }, "Chargement des quêtes Xbox…"));
  } else {
    if (rewards.unclaimed > 0) {
      parts.push(h("div", { class: "banner info" },
        h("span", {}, h("b", {}, `${numberFormat.format(rewards.unclaimed)} points à réclamer`), " dans le Rewards Hub de l'app Xbox."),
        h("button", { class: "btn primary", type: "button", onclick: () => invoke("open_xbox_app").catch((err) => toast(String(err))) }, "Ouvrir l'app Xbox")));
    }
    const groups = [
      ["Quêtes du jour", rewards.missions.filter((m) => m.kind === "single")],
      ["Séries et bonus", rewards.missions.filter((m) => m.kind === "streak")],
      ["Packs", rewards.missions.filter((m) => m.kind === "pack")],
    ];
    for (const [title, missions] of groups) {
      if (!missions.length) continue;
      parts.push(h("h3", { class: "xbox-group" }, title), h("ul", { class: "list" }, missions.map(missionItem)));
    }
    if (!rewards.missions.length) parts.push(h("p", { class: "muted" }, "Aucune quête renvoyée par le service Xbox pour ce compte."));
    parts.push(h("p", { class: "muted small" }, `Lu à ${timeFormat.format(new Date(rewards.fetchedAt))} depuis le Rewards Hub de l'app Xbox.`));
  }
  box.replaceChildren(...parts);
}

async function loadXbox(force) {
  xboxLive.status = await invoke("xbox_status");
  if (!xboxLive.status.connected) {
    xboxLive.rewards = null;
    renderXboxLive();
    return;
  }
  xboxLive.loading = true;
  renderXboxLive();
  try {
    xboxLive.rewards = await invoke("xbox_rewards", { force });
    xboxLive.error = null;
    if (xboxLive.rewards.gamertag) xboxLive.status.gamertag = xboxLive.rewards.gamertag;
  } catch (err) {
    if (String(err) === "xbox_not_connected") xboxLive.status = { connected: false, gamertag: null };
    else xboxLive.error = String(err);
  } finally {
    xboxLive.loading = false;
    renderXboxLive();
  }
}

async function xboxLogin() {
  xboxLive.error = null;
  try {
    await invoke("xbox_login");
    toast("Terminez la connexion dans la fenêtre Microsoft.");
  } catch (err) {
    toast(String(err));
  }
}

async function xboxLogout() {
  await invoke("xbox_logout");
  Object.assign(xboxLive, { status: { connected: false, gamertag: null }, rewards: null, error: null });
  renderXboxLive();
  toast("Compte Xbox Live déconnecté.");
}

listen("xbox-connected", (event) => {
  xboxLive.status = event.payload;
  xboxLive.error = null;
  xboxLive.loading = true;
  renderXboxLive();
});
listen("xbox-rewards-updated", (event) => {
  xboxLive.rewards = event.payload;
  xboxLive.error = null;
  xboxLive.loading = false;
  xboxLive.status = { connected: true, gamertag: event.payload.gamertag ?? xboxLive.status.gamertag };
  renderXboxLive();
});
listen("xbox-error", (event) => {
  xboxLive.error = String(event.payload);
  xboxLive.loading = false;
  renderXboxLive();
});

// ---------- Historique des points (N1) ----------

const shortDate = new Intl.DateTimeFormat("fr-FR", { day: "numeric", month: "short" });
const DAY_MS = 86400e3;
/** « 2026-11-11 » → date locale à midi (évite un décalage d'un jour selon le fuseau). */
const dayDate = (iso) => new Date(`${iso}T12:00:00`);
const isoDay = (d) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;

/**
 * Gains par jour, d'après le solde relevé chaque jour : la différence avec le dernier relevé
 * précédent. Une baisse correspond à un échange de points.
 */
function dailyGains(history) {
  const days = [];
  for (let i = 1; i < history.length; i++) {
    const delta = history[i].balance - history[i - 1].balance;
    days.push({ date: history[i].date, gain: Math.max(0, delta), spent: Math.max(0, -delta) });
  }
  return days;
}

/** Moyenne des points gagnés par jour sur les 30 derniers jours relevés (null sans données). */
function averageGain(history) {
  const recent = dailyGains(history).slice(-30);
  if (!recent.length) return null;
  return recent.reduce((sum, d) => sum + d.gain, 0) / recent.length;
}

async function loadHistory() {
  try {
    state.history = await invoke("points_history");
  } catch (err) {
    state.history = [];
  }
  renderHistory();
  renderGoal();
}

function tile(value, label) {
  return h("div", { class: "tile" }, h("b", {}, value), h("span", {}, label));
}

function renderHistory() {
  const history = state.history ?? [];
  const gains = dailyGains(history);
  const today = isoDay(new Date());
  const todayGain = gains.find((d) => d.date === today);
  const last7 = gains.filter((d) => Date.now() - dayDate(d.date) < 7 * DAY_MS);
  const last30 = gains.slice(-30);
  const avg = averageGain(history);
  const fmt = (n) => numberFormat.format(Math.round(n));
  $("history-tiles").replaceChildren(
    tile(todayGain ? `+${fmt(todayGain.gain)}` : "—", "gagnés aujourd'hui"),
    tile(last7.length ? `+${fmt(last7.reduce((s, d) => s + d.gain, 0))}` : "—", "sur les 7 derniers jours"),
    tile(avg === null ? "—" : fmt(avg), "par jour en moyenne (30 j)"),
    tile(last30.length ? fmt(last30.reduce((s, d) => s + d.spent, 0)) : "—", "points échangés (30 j)"));

  if (history.length) {
    $("history-range").textContent = `depuis le ${shortDate.format(dayDate(history[0].date))}`;
  }
  $("history-note").textContent = gains.length < 2
    ? "L'historique se remplit à chaque actualisation : le solde de chaque jour est relevé, et le gain du jour est la différence avec la veille. Revenez demain pour voir le premier gain."
    : "Gain du jour = solde du jour moins le dernier solde relevé avant lui. Une baisse est comptée comme un échange de points.";
  renderChart(gains.slice(-30));
}

/** Histogramme des 30 derniers jours : gains en vert, échanges en orange. */
function renderChart(days) {
  const box = $("history-chart");
  if (!days.length) {
    box.replaceChildren(h("p", { class: "muted" }, "Pas encore assez de relevés pour tracer le graphique."));
    return;
  }
  const ns = "http://www.w3.org/2000/svg";
  const W = 720, H = 220, left = 40, right = 8, top = 16, bottom = 28;
  const max = Math.max(10, ...days.map((d) => Math.max(d.gain, d.spent)));
  const step = Math.pow(10, Math.floor(Math.log10(max)));
  const niceMax = Math.ceil(max / step) * step;
  const y = (v) => top + (H - top - bottom) * (1 - v / niceMax);
  const slot = (W - left - right) / days.length;
  const bar = Math.max(2, Math.min(18, slot * 0.6));
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", "Points gagnés et échangés par jour, 30 derniers jours");
  const el = (tag, attrs, text) => {
    const n = document.createElementNS(ns, tag);
    for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
    if (text !== undefined) n.textContent = text;
    svg.append(n);
    return n;
  };
  for (const v of [0, niceMax / 2, niceMax]) {
    el("line", { class: "grid-line", x1: left, x2: W - right, y1: y(v), y2: y(v) });
    el("text", { class: "axis", x: left - 6, y: y(v) + 4, "text-anchor": "end" }, numberFormat.format(v));
  }
  const today = isoDay(new Date());
  days.forEach((d, i) => {
    const cx = left + slot * i + slot / 2;
    if (d.gain > 0) {
      const r = el("rect", { class: d.date === today ? "today" : "gain", x: cx - bar / 2, y: y(d.gain), width: bar, height: y(0) - y(d.gain), rx: 2 });
      r.append(Object.assign(document.createElementNS(ns, "title"), { textContent: `${shortDate.format(dayDate(d.date))} : +${d.gain} pts` }));
    }
    if (d.spent > 0) {
      const r = el("rect", { class: "spent", x: cx - bar / 2, y: y(d.spent), width: bar / 3, height: y(0) - y(d.spent), rx: 1 });
      r.append(Object.assign(document.createElementNS(ns, "title"), { textContent: `${shortDate.format(dayDate(d.date))} : ${d.spent} pts échangés` }));
    }
    const labelEvery = Math.ceil(days.length / 8);
    if (i % labelEvery === 0 || i === days.length - 1) {
      el("text", { class: "axis", x: cx, y: H - 8, "text-anchor": "middle" }, shortDate.format(dayDate(d.date)));
    }
  });
  box.replaceChildren(svg, h("div", { class: "legend" },
    h("span", {}, h("i", { style: "background:var(--accent)" }), "Points gagnés"),
    h("span", {}, h("i", { style: "background:var(--warn)" }), "Points échangés")));
}

// ---------- Objectif d'échange (N2) ----------

async function setGoal(item) {
  try {
    state.goal = await invoke("set_goal", {
      goal: item ? { id: item.id, title: item.title, price: item.price, url: item.url ?? null, imageUrl: item.imageUrl ?? null, notified: false } : null,
    });
    toast(item ? `Objectif : ${item.title}` : "Objectif retiré.");
  } catch (err) {
    toast(String(err));
  }
  renderGoal();
  renderGameCatalog();
}

/** Carte de l'objectif en haut d'« Aujourd'hui » : points qui manquent et date estimée. */
function renderGoal() {
  const goal = state.goal;
  const balance = state.summary?.availablePoints;
  $("goal-panel").hidden = !goal || balance === undefined;
  if (!goal || balance === undefined) return;
  const missing = Math.max(0, goal.price - balance);
  const pct = Math.min(100, (balance / goal.price) * 100);
  const avg = averageGain(state.history ?? []);
  let eta;
  if (!missing) eta = "Vous avez assez de points pour l'échanger.";
  else if (avg && avg > 0) {
    const days = Math.ceil(missing / avg);
    const date = new Date(Date.now() + days * DAY_MS);
    eta = `Encore ${numberFormat.format(missing)} pts · environ ${days} jour${days > 1 ? "s" : ""} au rythme actuel (vers le ${shortDate.format(date)})`;
  } else eta = `Encore ${numberFormat.format(missing)} pts · la date estimée apparaîtra après quelques jours d'historique`;
  $("goal").replaceChildren(h("div", { class: "goal-card" },
    goal.imageUrl ? h("img", { src: goal.imageUrl, alt: "" }) : h("span", { class: "img" }),
    h("div", {},
      h("p", { class: "eyebrow" }, "Objectif d'échange"),
      h("div", { class: "goal-title" }, goal.title),
      h("div", { class: "counter-top" }, h("span", { class: "muted small" }, eta), h("b", {}, `${numberFormat.format(Math.min(balance, goal.price))} / ${numberFormat.format(goal.price)} pts`)),
      h("div", { class: "bar" }, h("i", { style: `width:${pct}%` }))),
    h("div", { class: "goal-actions" },
      goal.url ? h("button", { class: `btn small${missing ? "" : " primary"}`, type: "button", onclick: () => openTask(goal.url) }, missing ? "Voir l'article ↗" : "Échanger ↗") : null,
      h("button", { class: "btn small ghost", type: "button", onclick: () => setGoal(null) }, "Retirer"))));
}

// ---------- Récompenses par jeu ----------

const games = { query: "", catalog: null, loading: false };

/** « Sea of Thieves » et « SeaofThieves » doivent se correspondre : on compare sans accents, espaces ni ponctuation. */
const normalize = (text) => (text ?? "").normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase().replace(/[^a-z0-9]/g, "");

function matches(query, ...texts) {
  const q = normalize(query);
  return !q || texts.some((t) => normalize(t).includes(q));
}

const expiryFormat = new Intl.DateTimeFormat("fr-FR", { day: "numeric", month: "long" });

/** Cadenas des étapes verrouillées (même dessin que sur le site Rewards). */
function lockIcon() {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 16 16");
  svg.setAttribute("class", "lock");
  svg.setAttribute("aria-label", "Verrouillée");
  const path = document.createElementNS(ns, "path");
  path.setAttribute("fill", "currentColor");
  path.setAttribute("d", "M5 3.5a3 3 0 0 1 6 0V4h.5A2.5 2.5 0 0 1 14 6.5v5a2.5 2.5 0 0 1-2.5 2.5h-7A2.5 2.5 0 0 1 2 11.5v-5A2.5 2.5 0 0 1 4.5 4H5zm3-2a2 2 0 0 0-2 2V4h4v-.5a2 2 0 0 0-2-2M8 10a1 1 0 1 0 0-2 1 1 0 0 0 0 2");
  svg.append(path);
  return svg;
}

function stepItem(step, card) {
  const icon = step.locked
    ? lockIcon()
    : h("span", { class: `state${step.complete ? " done" : ""}`, "aria-label": step.complete ? "Terminée" : "À faire" });
  const button = step.url && step.linkText && !step.complete
    ? h("button", { class: "btn small", type: "button", disabled: step.locked, onclick: () => openTask(card?.url ?? step.url, card?.url ? step.url : null, step.searchQuery, step.title) }, `${step.linkText} ↗`)
    : null;
  return h("li", { class: step.complete ? "done" : step.locked ? "locked" : "" },
    icon,
    h("div", {},
      h("div", { class: "step-title" }, step.title, " ", kindTag(step.kind)),
      step.description ? h("div", { class: "desc" }, step.description) : null,
      button));
}

function questItem(card, { open = false } = {}) {
  const meta = [];
  if (card.total) meta.push(`${card.done} / ${card.total} tâches`);
  if (card.expiresAt) meta.push(`expire le ${expiryFormat.format(new Date(card.expiresAt))}`);
  const hoursLeft = card.expiresAt ? (new Date(card.expiresAt) - Date.now()) / 3600e3 : Infinity;
  const expiring = !card.complete && hoursLeft > 0 && hoursLeft <= 48
    ? h("span", { class: "tag expiring", title: "La quête se termine bientôt et n'est pas finie" }, `Expire dans ${Math.max(1, Math.round(hoursLeft))} h`)
    : null;
  const hasDetails = card.description || card.steps.length || card.imageUrl;
  const details = hasDetails
    ? h("details", { class: "quest-details", open },
        h("summary", {}, "Voir le détail"),
        h("div", { class: card.imageUrl ? "quest-body" : "quest-body no-image" },
          card.imageUrl ? h("img", { src: card.imageUrl, alt: "", loading: "lazy" }) : null,
          h("div", { class: "quest-text" },
            card.description ? h("p", { class: "desc" }, card.description) : null,
            card.steps.length ? h("ol", { class: "quest-steps" }, card.steps.map((step) => stepItem(step, card))) : null,
            card.url ? h("button", { class: "btn small ghost", type: "button", onclick: () => openTask(card.url) }, "Ouvrir la quête sur Microsoft Rewards ↗") : null)))
    : null;
  return h("li", { class: card.complete ? "done" : "" },
    h("span", { class: `state${card.complete ? " done" : ""}` }),
    h("div", {},
      withThumb(card.imageUrl,
        h("div", { class: "title" },
          card.url ? h("a", { href: "#", onclick: (e) => { e.preventDefault(); openTask(card.url); } }, card.title) : card.title,
          " ", card.xbox ? h("span", { class: "tag" }, "Xbox") : null, " ", expiring),
        meta.length ? h("div", { class: "desc" }, meta.join(" · ")) : null),
      details),
    h("div", { class: "quest-side" },
      starButton(card),
      card.points ? h("span", { class: "pts" }, `${card.points} pts`) : null));
}

// ---------- Quêtes suivies ----------

const isFollowed = (id) => state.followed.some((q) => q.id === id);

function starButton(quest) {
  const followed = isFollowed(quest.id);
  return h("button", {
    class: `star${followed ? " on" : ""}`,
    type: "button",
    "aria-pressed": followed ? "true" : "false",
    title: followed ? "Ne plus suivre cette quête" : "Suivre cette quête",
    onclick: () => toggleFollow(quest, !followed),
  }, followed ? "★" : "☆");
}

async function toggleFollow(quest, follow) {
  try {
    state.followed = await invoke("follow_quest", { id: quest.id, title: quest.title, url: quest.url ?? null, imageUrl: quest.imageUrl ?? null, follow });
  } catch (err) {
    toast(String(err));
    return;
  }
  toast(follow ? `« ${quest.title} » ajoutée aux quêtes suivies.` : `« ${quest.title} » n'est plus suivie.`);
  if (state.summary) renderSummary(state.summary);
  else renderFollowed();
}

function renderFollowed() {
  const count = state.followed.length;
  $("followed-count").textContent = count;
  $("followed-count").hidden = count === 0;

  const cards = new Map((state.summary?.punchCards ?? []).map((c) => [c.id, c]));
  // Les quêtes en cours d'abord, puis les terminées, puis celles que Microsoft ne propose plus.
  const rank = (q) => {
    const card = cards.get(q.id);
    return !card ? 2 : card.complete ? 1 : 0;
  };
  const items = [...state.followed].sort((a, b) => rank(a) - rank(b)).map((quest) => {
    const card = cards.get(quest.id);
    if (card) return questItem(card, { open: true });
    const reason = state.summary
      ? "Microsoft ne propose plus cette quête : elle est terminée ou expirée."
      : "Connectez-vous pour voir l'avancement de cette quête.";
    return h("li", {},
      h("span", { class: "state" }),
      withThumb(quest.imageUrl,
        h("div", { class: "title" },
          quest.url ? h("a", { href: "#", onclick: (e) => { e.preventDefault(); openTask(quest.url); } }, quest.title) : quest.title),
        h("div", { class: "desc" }, `${reason} Suivie depuis le ${expiryFormat.format(new Date(quest.followedOn))}.`)),
      h("div", { class: "quest-side" }, starButton(quest)));
  });
  $("followed-list").replaceChildren(...(items.length ? items
    : [h("li", { class: "empty" }, "Aucune quête suivie pour l'instant. Cliquez sur l'étoile ☆ d'une quête pour la retrouver ici.")]));
}

function renderGameQuests() {
  const list = $("game-quests");
  const summary = state.summary;
  const q = games.query;
  if (!summary) {
    list.replaceChildren(h("li", { class: "empty" },
      state.loggedIn === false ? "Connectez-vous pour voir les quêtes en cours sur votre compte." : "Chargement…"));
    return;
  }
  const cards = summary.punchCards.filter((c) => matches(q, c.title, c.description, c.id, ...c.steps.map((s) => s.title)));
  // Sans recherche, seules les activités Xbox sont montrées à côté des cartes à poinçonner.
  const activities = [...summary.dailySet, ...summary.more]
    .filter((a) => !a.complete && (q ? matches(q, a.title, a.description, a.id) : a.xbox));
  const items = [...cards.map((card) => questItem(card)), ...activities.map(activityItem)];
  list.replaceChildren(...(items.length ? items : [h("li", { class: "empty" }, q
    ? `Aucune quête en cours pour « ${q} ». Les quêtes liées à un jeu apparaissent lors d'événements : revenez plus tard.`
    : "Aucune quête de jeu en cours sur votre compte.")]));
}

/** Bouton « Viser » : choisit l'article comme objectif d'échange, ou le retire. */
function goalButton(item) {
  const active = state.goal?.id === item.id;
  return h("button", {
    class: "btn small goal-btn", type: "button", "aria-pressed": active ? "true" : "false",
    title: active ? "Retirer cet objectif" : "Suivre les points qui manquent pour cet article",
    onclick: () => setGoal(active ? null : item),
  }, active ? "🎯 Objectif" : "Viser");
}

function catalogItem(item) {
  const balance = state.summary?.availablePoints;
  let status = null;
  if (item.disabled) status = h("div", { class: "missing" }, "Indisponible pour le moment");
  else if (balance !== undefined) status = item.price <= balance
    ? h("div", { class: "afford" }, "Vous avez assez de points")
    : h("div", { class: "missing" }, `Encore ${numberFormat.format(item.price - balance)} pts`);
  return h("li", {},
    item.imageUrl ? h("img", { src: item.imageUrl, alt: "", loading: "lazy" }) : h("span", { class: "img" }),
    h("div", {},
      h("div", { class: "title" }, h("a", { href: "#", onclick: (e) => { e.preventDefault(); openTask(item.url); } }, item.title)),
      h("div", { class: "price" }, `${numberFormat.format(item.price)} pts`,
        item.originalPrice > item.price ? h("span", { class: "old" }, `${numberFormat.format(item.originalPrice)} pts`) : null),
      status,
      goalButton(item)));
}

function renderGameCatalog() {
  const list = $("game-catalog");
  if (!games.catalog) {
    list.replaceChildren();
    return;
  }
  const items = games.catalog.items.filter((i) => matches(games.query, i.title, i.description));
  $("catalog-title").textContent = `Échanger vos points (${items.length})`;
  list.replaceChildren(...(items.length ? items.map(catalogItem)
    : [h("li", { class: "empty muted" }, `Aucun article du catalogue Rewards pour « ${games.query} ».`)]));
}

function renderGameChips() {
  const names = state.xbox?.knownGames ?? [];
  $("game-chips").replaceChildren(...names.map((name) => h("button", {
    class: "chip",
    type: "button",
    "aria-pressed": normalize(name) === normalize(games.query) ? "true" : "false",
    onclick: () => setGameQuery(normalize(name) === normalize(games.query) ? "" : name),
  }, name)));
}

function renderGames() {
  renderGameChips();
  renderGameQuests();
  renderGameCatalog();
}

function setGameQuery(query) {
  games.query = query.trim();
  $("game-search").value = query;
  renderGames();
}

async function loadCatalog(force) {
  games.loading = true;
  $("catalog-status").textContent = "Chargement du catalogue…";
  $("catalog-refresh").disabled = true;
  try {
    games.catalog = await invoke("redeem_catalog", { force });
    $("catalog-status").textContent = "";
  } catch (err) {
    $("catalog-status").textContent = `Catalogue indisponible : ${err}`;
  } finally {
    games.loading = false;
    $("catalog-refresh").disabled = false;
    renderGameCatalog();
  }
}

$("game-search").addEventListener("input", (e) => {
  games.query = e.target.value.trim();
  renderGames();
});
$("game-search-form").addEventListener("submit", (e) => e.preventDefault());
$("catalog-refresh").addEventListener("click", () => loadCatalog(true));

// ---------- Paramètres ----------

/** « Enregistré ✓ » quelques instants à côté du réglage modifié. */
function flashSaved(control) {
  const anchor = control?.closest("label, .inline-field") ?? control;
  if (!anchor) return;
  anchor.parentElement.querySelector(":scope > .saved")?.remove();
  const mark = h("span", { class: "saved", role: "status" }, "Enregistré ✓");
  anchor.after(mark);
  setTimeout(() => mark.remove(), 1800);
}

async function saveSettings(control) {
  state.settings = {
    // Seul le mode « Rappels » existe : rien n'est lancé sans l'utilisateur.
    mode: "reminders",
    reminderTime: $("reminder-time").value || "19:00",
    remindBing: $("remind-bing").checked,
    remindXbox: $("remind-xbox").checked,
    remindStreaks: $("remind-streaks").checked,
    notifyUnlocks: $("notify-unlocks").checked,
    notifyExpiring: $("notify-expiring").checked,
    notifyNewSearches: $("notify-new-searches").checked,
    remindStreaksEvening: $("remind-streaks-evening").checked,
    notifyLevel: $("notify-level").checked,
    weeklySummary: $("weekly-summary").checked,
    autoXboxDays: $("auto-xbox-days").checked,
    xboxPc: $("xbox-pc").checked,
    xboxConsole: $("xbox-console").checked,
    xboxMobile: $("xbox-mobile").checked,
    // Seuls le mois et le jour sont gardés.
    birthday: $("birthday").value ? $("birthday").value.slice(5) : "",
    streakReminderTime: $("streak-reminder-time").value || "22:00",
    subscription: $("subscription").value,
    autoCheckUpdates: $("auto-update").checked,
    updateCheckIntervalMinutes: clampMinutesInput("update-interval", 1, 10080),
    autoInstallUpdates: $("auto-install").checked,
    refreshIntervalMinutes: refreshMinutesInput(),
  };
  try {
    await invoke("save_settings", { settings: state.settings });
    flashSaved(control);
  } catch (err) {
    toast(`Réglage non enregistré : ${err}`);
    return;
  }
  renderXbox(await invoke("xbox_progress"));
}

function renderSettings(settings) {
  state.settings = settings;
  $("reminder-time").value = settings.reminderTime;
  $("remind-bing").checked = settings.remindBing;
  $("remind-xbox").checked = settings.remindXbox;
  $("remind-streaks").checked = settings.remindStreaks;
  $("notify-unlocks").checked = settings.notifyUnlocks;
  $("notify-expiring").checked = settings.notifyExpiring;
  $("notify-new-searches").checked = settings.notifyNewSearches;
  $("remind-streaks-evening").checked = settings.remindStreaksEvening;
  $("notify-level").checked = settings.notifyLevel;
  $("weekly-summary").checked = settings.weeklySummary;
  $("auto-xbox-days").checked = settings.autoXboxDays;
  $("xbox-pc").checked = settings.xboxPc;
  $("xbox-console").checked = settings.xboxConsole;
  $("xbox-mobile").checked = settings.xboxMobile;
  $("birthday").value = settings.birthday ? `2000-${settings.birthday}` : "";
  $("streak-reminder-time").value = settings.streakReminderTime;
  $("subscription").value = settings.subscription;
  $("auto-update").checked = settings.autoCheckUpdates;
  $("update-interval").value = settings.updateCheckIntervalMinutes;
  $("auto-install").checked = settings.autoInstallUpdates;
  renderUpdateControls();
  $("refresh-interval").value = settings.refreshIntervalMinutes;
}

/** Valeur entière d'un champ, ramenée entre `min` et `max`. */
function clampMinutesInput(id, min, max) {
  const input = $(id);
  let minutes = Math.round(Number(input.value));
  if (!Number.isFinite(minutes)) minutes = min;
  minutes = Math.min(Math.max(minutes, min), max);
  input.value = minutes;
  return minutes;
}

/** Délai et installation automatique n'ont de sens que si la recherche automatique est active. */
function renderUpdateControls() {
  const enabled = $("auto-update").checked;
  $("update-interval").disabled = !enabled;
  $("auto-install").disabled = !enabled;
}

/** Délai saisi, ramené au minimum de 5 minutes (0 désactive l'actualisation automatique). */
function refreshMinutesInput() {
  const input = $("refresh-interval");
  let minutes = Math.round(Number(input.value));
  if (!Number.isFinite(minutes) || minutes < 0) minutes = 0;
  if (minutes > 0 && minutes < 5) minutes = 5;
  minutes = Math.min(minutes, 1440);
  input.value = minutes;
  return minutes;
}

for (const id of ["reminder-time", "remind-bing", "remind-xbox", "remind-streaks", "notify-unlocks", "notify-expiring", "notify-new-searches", "remind-streaks-evening", "streak-reminder-time", "notify-level", "weekly-summary", "auto-xbox-days", "xbox-pc", "xbox-console", "xbox-mobile", "birthday", "subscription", "auto-update", "update-interval", "auto-install", "refresh-interval"]) {
  $(id).addEventListener("change", () => saveSettings($(id)));
}

$("auto-update").addEventListener("change", renderUpdateControls);

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

// ---------- Jeux Game Pass gratuits ----------

const freeGames = { data: null, platform: "all", loading: false };

/** « Sea of Thieves » et « Sea of Thieves: 2026 Edition » : même jeu pour la liste du mois. */
const playedThisMonth = (title) => {
  const t = normalize(title);
  return (state.xbox?.monthGames ?? []).some((g) => {
    const n = normalize(g);
    return n && (n === t || n.startsWith(t) || t.startsWith(n));
  });
};

function renderFreeGames() {
  const list = $("free-games-list");
  const data = freeGames.data;
  for (const b of document.querySelectorAll("#free-games-dialog [data-platform]")) {
    b.setAttribute("aria-pressed", b.dataset.platform === freeGames.platform ? "true" : "false");
  }
  if (!data) {
    list.replaceChildren(h("li", { class: "empty muted" }, freeGames.loading
      ? "Lecture du catalogue Game Pass sur le Store Microsoft (une vingtaine de secondes la première fois)…"
      : "Catalogue indisponible pour le moment."));
    return;
  }
  const games = data.games.filter((g) => freeGames.platform === "all" || g.platforms.includes(freeGames.platform));
  const packs = hasGamePassQuests();
  list.replaceChildren(...games.map((g) => {
    const played = playedThisMonth(g.title);
    return h("li", { class: played ? "played" : "" },
      g.imageUrl ? h("img", { src: g.imageUrl, alt: "", loading: "lazy" }) : h("span", { class: "img" }),
      h("div", { class: "free-game-text" },
        h("b", {}, g.title),
        h("span", { class: "muted small" }, g.platforms.join(" · ")),
        played ? h("span", { class: "tag" }, "Joué ce mois-ci ✓") : null),
      h("div", { class: "free-game-actions" },
        packs && !played ? h("button", { class: "btn small", type: "button", title: "Ajouter aux jeux du mois des packs Game Pass",
          onclick: async () => { renderXbox(await invoke("set_month_game", { game: g.title, add: true })); renderFreeGames(); } }, "Ajouter au mois") : null,
        h("button", { class: "btn small ghost", type: "button", onclick: () => openUrl(g.url) }, "Store ↗")));
  }));
  $("free-games-note").textContent = `${games.length} jeu${games.length > 1 ? "x" : ""} sur ${data.checked} jeux du catalogue Game Pass (France). Prix illisible pour ${data.unknown} jeux : la liste peut en oublier quelques-uns.`;
}

async function openFreeGames() {
  $("free-games-dialog").showModal();
  if (freeGames.data) {
    renderFreeGames();
    return;
  }
  freeGames.loading = true;
  renderFreeGames();
  try {
    freeGames.data = await invoke("free_game_pass_games", { force: false });
  } catch (err) {
    toast(String(err));
  } finally {
    freeGames.loading = false;
    renderFreeGames();
  }
}

$("free-games-btn").addEventListener("click", openFreeGames);
$("free-games-close").addEventListener("click", () => $("free-games-dialog").close());
for (const b of document.querySelectorAll("#free-games-dialog [data-platform]")) {
  b.addEventListener("click", () => { freeGames.platform = b.dataset.platform; renderFreeGames(); });
}

// ---------- Formule Game Pass (N4) ----------

const checkedFormat = new Intl.DateTimeFormat("fr-FR", { day: "numeric", month: "long" });

async function renderSubscriptionStatus() {
  const checked = await invoke("subscription_checked").catch(() => null);
  $("subscription-status").textContent = checked ? `Détectée le ${checkedFormat.format(dayDate(checked))}` : "";
}

$("detect-subscription").addEventListener("click", async () => {
  const button = $("detect-subscription");
  button.disabled = true;
  $("subscription-status").textContent = "Lecture de la page de votre compte…";
  try {
    renderSettings(await invoke("detect_subscription"));
    renderXbox(await invoke("xbox_progress"));
    await renderSubscriptionStatus();
    toast(`Formule détectée : ${$("subscription").selectedOptions[0].textContent}`);
  } catch (err) {
    $("subscription-status").textContent = `Non détectée : ${err}`;
  } finally {
    button.disabled = false;
  }
});

listen("settings-updated", (event) => {
  renderSettings(event.payload);
  renderSubscriptionStatus();
});

// ---------- Sauvegarde (N10) ----------

$("export-backup").addEventListener("click", async () => {
  try {
    const path = await invoke("export_backup");
    if (path) toast(`Sauvegarde enregistrée : ${path}`);
  } catch (err) {
    toast(`Sauvegarde impossible : ${err}`);
  }
});

$("import-backup").addEventListener("click", async () => {
  try {
    if (await invoke("import_backup")) {
      toast("Sauvegarde restaurée.");
      // Tout l'état a changé : l'interface repart de zéro.
      setTimeout(() => location.reload(), 800);
    }
  } catch (err) {
    toast(`Restauration impossible : ${err}`);
  }
});

// ---------- Jours joués détectés (N5) ----------

listen("xbox-progress-updated", (event) => renderXbox(event.payload));

$("open-logs").addEventListener("click", () => invoke("open_logs").catch((err) => toast(String(err))));

// ---------- Mises à jour ----------

function showUpdate(info) {
  $("update-title").textContent = `Version ${info.version} disponible (vous avez la ${info.currentVersion})`;
  const release = `https://github.com/powange/microsoft-rewards/releases/tag/v${info.version}`;
  // Les anciennes versions publiaient l'adresse de la release à la place des nouveautés.
  const notes = info.notes && !info.notes.startsWith("https://") ? info.notes : "";
  $("update-notes").replaceChildren(
    ...(notes ? [notes, " · "] : []),
    h("a", { href: "#", onclick: (e) => { e.preventDefault(); openUrl(release); } }, "Voir la release ↗"));
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
  renderToday();
  const [info, settings, autostart, xbox, cached, followed, goal, history] = await Promise.all([
    invoke("app_info"),
    invoke("get_settings"),
    invoke("autostart_enabled"),
    invoke("xbox_progress"),
    invoke("cached_rewards"),
    invoke("followed_quests"),
    invoke("get_goal"),
    invoke("points_history"),
  ]);
  state.followed = followed;
  state.goal = goal;
  state.history = history;
  $("version-line").textContent = `Version ${info.version}`;
  $("current-version").textContent = info.version;
  renderSettings(settings);
  renderSubscriptionStatus();
  $("autostart").checked = autostart;
  renderXbox(xbox);
  renderFollowed();
  loadXbox(false);
  if (cached) {
    renderSummary(cached);
    setLoggedIn(true);
  } else {
    showView("today");
  }
  refresh();
})();
