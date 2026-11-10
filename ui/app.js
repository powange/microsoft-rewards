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
  $("view-games").hidden = view !== "games";
  $("view-followed").hidden = view !== "followed";
  $("view-settings").hidden = view !== "settings";
  if (view === "games" && !games.catalog && !games.loading) loadCatalog(false);
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
  if (summary.level) level.push(`Niveau : ${summary.level}`);
  if (summary.levelProgressMax) level.push(`${numberFormat.format(summary.levelProgress ?? 0)} / ${numberFormat.format(summary.levelProgressMax)} points vers le niveau suivant`);
  $("level").textContent = level.join(" · ");
  const every = state.settings?.refreshIntervalMinutes;
  $("fetched-at").textContent = `Actualisé à ${timeFormat.format(new Date(summary.fetchedAt))}`
    + (every ? ` · automatiquement toutes les ${every} min` : "");
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
  $("punch-cards").replaceChildren(...cards.map((card) => questItem(card)));

  renderStreaks(summary.streaks);
  renderGames();
  renderFollowed();
}

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
      streak.url ? h("button", { class: "btn small", type: "button", onclick: () => openUrl(streak.url) }, "Y aller ↗") : null));
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
  state.xbox = progress;
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
  renderGames();
}

$("xbox-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const game = $("xbox-game").value.trim();
  renderXbox(await invoke("set_xbox_today", { game }));
  $("xbox-game").value = "";
});
$("xbox-undo").addEventListener("click", async () => renderXbox(await invoke("set_xbox_today", { game: null })));
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
      h("div", { class: "title" }, m.title, " ", m.claimable ? h("span", { class: "tag" }, "À réclamer") : null),
      meta.length ? h("div", { class: "desc" }, meta.join(" · ")) : null,
      m.description ? h("div", { class: "desc" }, m.description) : null,
      m.requirement ? h("div", { class: "desc" }, `Pour débloquer : ${m.requirement}`) : null,
      days),
    h("span", { class: "pts" }, m.points ? `+${m.points} pts` : ""));
}

function renderXboxLive() {
  const box = $("xbox-live");
  const { status, rewards, error, loading } = xboxLive;
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

  const parts = [head];
  if (refused) {
    parts.push(h("div", { class: "banner danger" },
      h("span", {}, "Le service Xbox des quêtes a refusé la connexion de Rewards Autoplay : il n'accepte sans doute que l'app Xbox elle-même. Le suivi manuel ci-dessous reste disponible.")));
  } else if (error) {
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

function stepItem(step) {
  const icon = step.locked
    ? lockIcon()
    : h("span", { class: `state${step.complete ? " done" : ""}`, "aria-label": step.complete ? "Terminée" : "À faire" });
  const button = step.url && step.linkText && !step.complete
    ? h("button", { class: "btn small", type: "button", disabled: step.locked, onclick: () => openUrl(step.url) }, `${step.linkText} ↗`)
    : null;
  return h("li", { class: step.complete ? "done" : step.locked ? "locked" : "" },
    icon,
    h("div", {},
      h("div", { class: "step-title" }, step.title),
      step.description ? h("div", { class: "desc" }, step.description) : null,
      button));
}

function questItem(card, { open = false } = {}) {
  const meta = [];
  if (card.total) meta.push(`${card.done} / ${card.total} tâches`);
  if (card.expiresAt) meta.push(`expire le ${expiryFormat.format(new Date(card.expiresAt))}`);
  const hasDetails = card.description || card.steps.length || card.imageUrl;
  const details = hasDetails
    ? h("details", { class: "quest-details", open },
        h("summary", {}, "Voir le détail"),
        h("div", { class: card.imageUrl ? "quest-body" : "quest-body no-image" },
          card.imageUrl ? h("img", { src: card.imageUrl, alt: "", loading: "lazy" }) : null,
          h("div", { class: "quest-text" },
            card.description ? h("p", { class: "desc" }, card.description) : null,
            card.steps.length ? h("ol", { class: "quest-steps" }, card.steps.map(stepItem)) : null,
            card.url ? h("button", { class: "btn small ghost", type: "button", onclick: () => openUrl(card.url) }, "Ouvrir la quête sur Microsoft Rewards ↗") : null)))
    : null;
  return h("li", { class: card.complete ? "done" : "" },
    h("span", { class: `state${card.complete ? " done" : ""}` }),
    h("div", {},
      h("div", { class: "title" },
        card.url ? h("a", { href: "#", onclick: (e) => { e.preventDefault(); openUrl(card.url); } }, card.title) : card.title,
        " ", card.xbox ? h("span", { class: "tag" }, "Xbox") : null),
      meta.length ? h("div", { class: "desc" }, meta.join(" · ")) : null,
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
    state.followed = await invoke("follow_quest", { id: quest.id, title: quest.title, url: quest.url ?? null, follow });
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
      h("div", {},
        h("div", { class: "title" },
          quest.url ? h("a", { href: "#", onclick: (e) => { e.preventDefault(); openUrl(quest.url); } }, quest.title) : quest.title),
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
      h("div", { class: "title" }, h("a", { href: "#", onclick: (e) => { e.preventDefault(); openUrl(item.url); } }, item.title)),
      h("div", { class: "price" }, `${numberFormat.format(item.price)} pts`,
        item.originalPrice > item.price ? h("span", { class: "old" }, `${numberFormat.format(item.originalPrice)} pts`) : null),
      status));
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

async function saveSettings() {
  state.settings = {
    mode: document.querySelector('input[name="mode"]:checked')?.value ?? "reminders",
    reminderTime: $("reminder-time").value || "19:00",
    remindBing: $("remind-bing").checked,
    remindXbox: $("remind-xbox").checked,
    remindStreaks: $("remind-streaks").checked,
    subscription: $("subscription").value,
    autoCheckUpdates: $("auto-update").checked,
    updateCheckIntervalMinutes: clampMinutesInput("update-interval", 15, 10080),
    autoInstallUpdates: $("auto-install").checked,
    refreshIntervalMinutes: refreshMinutesInput(),
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
  $("remind-streaks").checked = settings.remindStreaks;
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

for (const id of ["reminder-time", "remind-bing", "remind-xbox", "remind-streaks", "subscription", "auto-update", "update-interval", "auto-install", "refresh-interval", "mode-reminders"]) {
  $(id).addEventListener("change", saveSettings);
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
  const [info, settings, autostart, xbox, cached, followed] = await Promise.all([
    invoke("app_info"),
    invoke("get_settings"),
    invoke("autostart_enabled"),
    invoke("xbox_progress"),
    invoke("cached_rewards"),
    invoke("followed_quests"),
  ]);
  state.followed = followed;
  $("version-line").textContent = `Version ${info.version}`;
  $("current-version").textContent = info.version;
  renderSettings(settings);
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
