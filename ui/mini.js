// Mini-fenêtre de la zone de notification : un coup d'œil sur la journée.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const numberFormat = new Intl.NumberFormat("fr-FR");

function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") el.className = value;
    else el.setAttribute(key, value);
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}

const row = (label, value, ok = false) =>
  h("div", { class: ok ? "mini-row ok" : "mini-row" }, h("span", {}, label), h("b", {}, value));

const isSearch = (kind) => kind === "search" || kind === "searchManual";

function render(summary, goal) {
  const body = $("m-body");
  if (!summary) {
    $("m-points").textContent = "—";
    $("m-level").textContent = "";
    body.replaceChildren(h("p", { class: "muted" }, "Connectez-vous à Microsoft Rewards depuis l'application pour voir votre journée."));
    return;
  }
  $("m-points").textContent = numberFormat.format(summary.availablePoints);
  $("m-level").textContent = summary.level ?? "";

  const pending = (t) => !t.complete && !t.locked;
  const daily = summary.dailySet.filter(pending).length;
  const searches = [...summary.dailySet, ...summary.more].filter((a) => pending(a) && isSearch(a.kind)).length
    + summary.punchCards.flatMap((c) => c.steps).filter((s) => pending(s) && isSearch(s.kind)).length;
  const pc = summary.searches.filter((c) => !/mobile/i.test(c.label)).reduce((n, c) => n + Math.max(0, c.max - c.progress), 0);
  const streaks = summary.streaks?.streaks ?? [];

  const parts = [h("div", { class: "mini-section" }, "Aujourd'hui")];
  parts.push(row("Activités du jour", daily ? `${daily} à faire` : "faites ✓", !daily));
  parts.push(row("Recherches à faire", searches ? `${searches} à faire` : "faites ✓", !searches));
  parts.push(row("Points de recherche PC", pc ? `${pc} pts` : "complets ✓", !pc));
  if (summary.claimablePoints > 0) parts.push(row("Prêts à réclamer", `${numberFormat.format(summary.claimablePoints)} pts`));
  if (streaks.length) {
    parts.push(h("div", { class: "mini-section" }, "Séries"));
    for (const s of streaks) parts.push(row(s.title, s.todayDone ? "faite ✓" : s.todayLabel || "à faire", s.todayDone));
  }
  if (goal) {
    const missing = Math.max(0, goal.price - summary.availablePoints);
    parts.push(h("div", { class: "mini-section" }, "Objectif"));
    parts.push(row(goal.title, missing ? `encore ${numberFormat.format(missing)} pts` : "atteint ✓", !missing));
    // Largeur réglée par script : la politique de sécurité peut bloquer l'attribut style.
    const fill = h("i");
    fill.style.width = `${Math.min(100, (summary.availablePoints / goal.price) * 100)}%`;
    parts.push(h("div", { class: "bar" }, fill));
  }
  body.replaceChildren(...parts);
}

async function load() {
  const [summary, goal] = await Promise.all([invoke("cached_rewards"), invoke("get_goal")]);
  render(summary, goal);
}

$("m-guided").addEventListener("click", () => invoke("start_guided"));
$("m-open").addEventListener("click", () => invoke("open_main", { view: "today" }));
$("m-refresh").addEventListener("click", async () => {
  $("m-refresh").disabled = true;
  try {
    await invoke("refresh_rewards", { force: true });
  } catch (err) {
    // L'erreur est affichée dans la fenêtre principale ; ici on garde les dernières données.
  }
  $("m-refresh").disabled = false;
  load();
});
document.addEventListener("keydown", (e) => { if (e.key === "Escape") invoke("hide_mini"); });

listen("rewards-updated", load);
listen("rewards-logged-out", () => render(null, null));
listen("mini-shown", load);
window.addEventListener("focus", load);
load();
