// Series Ripper web UI. Plain JavaScript, no build step. Live state and log
// lines arrive over Server-Sent Events (/api/events).

const $ = (sel, root) => (root || document).querySelector(sel);
const $$ = (sel, root) => Array.from((root || document).querySelectorAll(sel));

// Disc labels, title names and MakeMKV messages come from the disc - never
// insert them as HTML unescaped.
function esc(value) {
  return String(value ?? "").replace(/[&<>"']/g, c => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

function fmtSize(bytes) {
  if (!bytes) return "";
  return bytes >= 1e9 ? (bytes / 1e9).toFixed(1) + " GB" : Math.round(bytes / 1e6) + " MB";
}

function fmtDuration(sec) {
  if (sec == null || !isFinite(sec)) return "";
  sec = Math.round(sec);
  const h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
  if (h) return `${h}h ${String(m).padStart(2, "0")}m`;
  if (m) return `${m}m ${String(s).padStart(2, "0")}s`;
  return `${s}s`;
}

function epCode(season, episode) {
  return `S${String(season).padStart(2, "0")}E${String(episode).padStart(2, "0")}`;
}

const sleep = ms => new Promise(r => setTimeout(r, ms));

async function api(method, path, body) {
  const opts = { method, headers: {} };
  if (body !== undefined) {
    opts.headers["Content-Type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  const res = await fetch(path, opts);
  let data = null;
  try { data = await res.json(); } catch (e) { /* empty body */ }
  if (!res.ok) throw new Error((data && data.error) ? data.error : `HTTP ${res.status}`);
  return data;
}

function fmtTime(ts) {
  const d = new Date(ts * 1000);
  const today = new Date().toDateString() === d.toDateString();
  return today ? d.toLocaleTimeString() : d.toLocaleString();
}

function alertError(err) {
  console.error(err);
  alert(err.message || String(err));
}

// --------------------------------------------------------------- state --

let state = { drives: [], review_pending: 0 };
let shows = [];

function showById(id) { return shows.find(s => s.id === id); }
function driveById(id) { return state.drives.find(d => d.id === id); }

async function loadShows() {
  try {
    shows = (await api("GET", "/api/shows")).shows;
  } catch (e) { return; }
  onShowsChanged();
}

function onShowsChanged() {
  renderDrives();
  fillShowSelect($("#ripShow"), $("#ripShow").value ? parseInt($("#ripShow").value, 10) : null);
  fillHistoryShows();
  updateRipShowInfo();
  $("#noShowsHint").style.display = shows.length ? "none" : "";
}

function fillShowSelect(sel, selectedId, allowNone) {
  if (document.activeElement === sel) return;
  const opts = [];
  if (allowNone || !shows.length) opts.push(`<option value="">${shows.length ? "(no show)" : "(add a show first)"}</option>`);
  shows.forEach(s => opts.push(`<option value="${s.id}">${esc(s.name)} (season ${s.season})</option>`));
  const html = opts.join("");
  if (sel.dataset.html !== html) { sel.innerHTML = html; sel.dataset.html = html; }
  sel.value = selectedId != null && showById(selectedId) ? String(selectedId) : (allowNone ? "" : (shows[0] ? String(shows[0].id) : ""));
}

// ---------------------------------------------------------------- tabs --

function openView(name) {
  $$(".tab-btn").forEach(b => b.classList.toggle("active", b.dataset.view === name));
  $$(".view").forEach(v => v.classList.toggle("active", v.id === "view-" + name));
  if (name === "review") loadReviewQueue();
  if (name === "settings") loadSettings();
  if (name === "shows") loadShowsTable();
  if (name === "history") loadHistory(true);
  if (name === "rip") onRipDriveChange();
}
$$(".tab-btn").forEach(btn => btn.addEventListener("click", () => openView(btn.dataset.view)));

// ------------------------------------------------------- drive cards ----

const TRAY_TEXT = {
  disc_ok: "Disc in", no_disc: "No disc", tray_open: "Tray open", not_ready: "Spinning up...", unknown: "",
};

function trayBadge(d) {
  if (!d.present) return ["offline", "Not connected"];
  if (d.activity !== "idle" || d.has_disc) {
    const label = d.disc_label ? `${d.disc_type ? d.disc_type + " " : ""}${d.disc_label}` : "Disc in";
    return ["disc", label];
  }
  if (d.ioctl === false) return ["empty", "No disc"];
  return [d.tray === "tray_open" ? "open" : "empty", TRAY_TEXT[d.tray] || "No disc"];
}

function progressText(d) {
  const p = d.progress;
  if (d.activity === "scanning") {
    return p ? `Scanning... ${Math.round(p.percent || 0)}%${p.step ? " - " + p.step : ""}` : "Scanning...";
  }
  if (d.activity === "ripping") {
    if (!p) return "Preparing rip...";
    const ep = p.special ? epCode(0, p.episode) : epCode(p.season, p.episode);
    const parts = [`Title ${p.title_index} -> ${ep} (${p.position}/${p.total})`, `${Math.round(p.percent || 0)}%`];
    if (p.bytes_per_sec) parts.push(`${(p.bytes_per_sec / 1e6).toFixed(1)} MB/s`);
    if (p.eta_sec != null) parts.push(`${fmtDuration(p.eta_sec)} left`);
    return parts.join(" - ");
  }
  return "Idle";
}

function driveCard(d) {
  const card = document.createElement("div");
  card.className = "card drive-card";
  card.dataset.id = d.id;
  card.innerHTML = `
    <div class="drive-head">
      <div class="drive-title-wrap">
        <div class="drive-title"><span class="d-label"></span>
          <button class="icon-btn d-rename" title="Rename this drive">&#9998;</button></div>
        <div class="hint d-model"></div>
      </div>
      <span class="tray-badge"></span>
    </div>
    <div class="grid-2 drive-controls">
      <div><label>Show</label><select class="d-show"></select></div>
      <div class="checkline align-end"><input type="checkbox" class="d-auto" id="auto-${d.id}">
        <label for="auto-${d.id}">Automatic Mode</label></div>
    </div>
    <div class="d-status"></div>
    <div class="progress-outer"><div class="progress-inner"></div></div>
    <div class="hint d-step"></div>
    <div class="btn-row">
      <button class="btn small d-scan">Scan</button>
      <button class="btn small d-review" style="display:none">Check titles</button>
      <button class="btn secondary small d-eject">Eject</button>
      <button class="btn secondary small d-close">Close tray</button>
      <button class="btn danger small d-cancel" style="display:none">Cancel</button>
    </div>
    <div class="hint d-last"></div>`;

  $(".d-show", card).addEventListener("change", async e => {
    const v = e.target.value;
    try { await api("POST", `/api/drives/${d.id}/settings`, { show_id: v ? parseInt(v, 10) : null }); }
    catch (err) { alertError(err); }
  });
  $(".d-auto", card).addEventListener("change", async e => {
    try { await api("POST", `/api/drives/${d.id}/settings`, { automatic: e.target.checked }); }
    catch (err) { e.target.checked = !e.target.checked; alertError(err); }
  });
  $(".d-rename", card).addEventListener("click", async () => {
    const cur = driveById(d.id);
    const name = prompt("Name for this drive (empty = default):", cur ? cur.nickname : "");
    if (name === null) return;
    try { await api("POST", `/api/drives/${d.id}/settings`, { nickname: name }); }
    catch (err) { alertError(err); }
  });
  $(".d-scan", card).addEventListener("click", () => {
    $("#ripDrive").value = String(d.id);
    openView("rip");
    startScan();
  });
  $(".d-review", card).addEventListener("click", () => {
    $("#ripDrive").value = String(d.id);
    openView("rip");
  });
  $(".d-eject", card).addEventListener("click", async () => {
    try { await api("POST", `/api/drives/${d.id}/eject`); } catch (err) { alertError(err); }
  });
  $(".d-close", card).addEventListener("click", async () => {
    try { await api("POST", `/api/drives/${d.id}/close`); } catch (err) { alertError(err); }
  });
  $(".d-cancel", card).addEventListener("click", async () => {
    const cur = driveById(d.id);
    if (!confirm(`Cancel the ${cur ? cur.activity : "current job"} on ${cur ? cur.label : "this drive"}?`)) return;
    try { await api("POST", `/api/drives/${d.id}/cancel`); } catch (err) { alertError(err); }
  });
  return card;
}

function updateDriveCard(card, d) {
  $(".d-label", card).textContent = d.label;
  $(".d-model", card).textContent = `${d.drive_name || "drive"}${d.device ? " - " + d.device : ""}`;
  const [cls, text] = trayBadge(d);
  const badge = $(".tray-badge", card);
  badge.className = "tray-badge " + cls;
  badge.textContent = text;
  fillShowSelect($(".d-show", card), d.show_id, true);
  const auto = $(".d-auto", card);
  if (document.activeElement !== auto) auto.checked = !!d.automatic;

  const status = $(".d-status", card);
  status.textContent = progressText(d);
  status.className = "d-status act-" + d.activity;
  // Scanning: the scan's own percentage. Ripping: progress through the whole
  // disc, so the bar doesn't jump back to 0 for every title.
  const p = d.progress;
  let pct = p ? p.percent || 0 : 0;
  if (d.activity === "ripping" && p && p.total) pct = ((p.position - 1) + pct / 100) / p.total * 100;
  $(".progress-inner", card).style.width = (d.activity === "idle" ? 0 : pct) + "%";
  $(".progress-inner", card).classList.toggle("scan", d.activity === "scanning");
  $(".d-step", card).textContent = d.activity === "ripping" && d.progress
    ? [d.progress.filename, d.progress.step].filter(Boolean).join(" - ") : "";
  $(".d-last", card).textContent = d.last_message || "";

  const idle = d.activity === "idle";
  $(".d-scan", card).disabled = !idle || !d.present;
  $(".d-review", card).style.display = d.has_staged && idle ? "" : "none";
  $(".d-cancel", card).style.display = idle ? "none" : "";
  $(".d-eject", card).disabled = !idle || !d.device;
  $(".d-close", card).disabled = !idle || !d.device;
  card.classList.toggle("busy", !idle);
  card.classList.toggle("offline", !d.present);
}

function renderDrives() {
  const wrap = $("#driveCards");
  const seen = new Set();
  state.drives.forEach(d => {
    seen.add(String(d.id));
    let card = wrap.querySelector(`.drive-card[data-id="${d.id}"]`);
    if (!card) { card = driveCard(d); wrap.appendChild(card); }
    updateDriveCard(card, d);
  });
  $$(".drive-card", wrap).forEach(c => { if (!seen.has(c.dataset.id)) c.remove(); });
  $("#noDrivesHint").style.display = state.drives.length ? "none" : "";
}

function renderHeader() {
  const busy = state.drives.filter(d => d.activity !== "idle");
  const ripping = busy.filter(d => d.activity === "ripping").length;
  const scanning = busy.length - ripping;
  const dot = $("#statusDot");
  dot.className = "dot " + (ripping ? "ripping" : scanning ? "scanning" : "idle");
  const parts = [];
  if (ripping) parts.push(`${ripping} ripping`);
  if (scanning) parts.push(`${scanning} scanning`);
  $("#statusText").textContent = parts.length ? parts.join(", ") : `Idle - ${state.drives.length} drive(s)`;
  const badge = $("#reviewBadge");
  badge.style.display = state.review_pending ? "" : "none";
  badge.textContent = state.review_pending || "";
  $("#makemkvInfo").textContent = `Series Ripper ${state.version || ""} - ${state.makemkv_version || "MakeMKV version unknown"}` +
    ` - license: ${(state.license && state.license.detail) || "checking..."} - temp folder: ${state.temp_dir || ""}`;
}

function fillRipDrives() {
  const sel = $("#ripDrive");
  if (document.activeElement === sel) return;
  const cur = sel.value;
  const html = state.drives.filter(d => d.present).map(d =>
    `<option value="${d.id}">${esc(d.label)} - ${esc(d.has_disc ? (d.disc_label || "disc in") : "no disc")}` +
    `${d.activity !== "idle" ? " [" + d.activity + "]" : ""}</option>`).join("") || "<option value=''>No drives</option>";
  if (sel.dataset.html !== html) {
    sel.innerHTML = html;
    sel.dataset.html = html;
    if (cur && sel.querySelector(`option[value="${cur}"]`)) sel.value = cur;
  }
}

let prevPending = null;
function applyState(s) {
  state = s;
  if (s.shows && JSON.stringify(s.shows) !== JSON.stringify(shows)) {
    shows = s.shows;
    onShowsChanged();
  }
  renderDrives();
  renderHeader();
  fillRipDrives();
  if (prevPending !== null && s.review_pending !== prevPending && $("#view-review").classList.contains("active")) {
    loadReviewQueue();
  }
  prevPending = s.review_pending;
  if ($("#view-rip").classList.contains("active")) refreshRipStatus();
}

$("#refreshDrivesBtn").addEventListener("click", async e => {
  e.target.disabled = true;
  try { const d = await api("POST", "/api/drives/refresh"); applyState({ ...state, drives: d.drives }); }
  catch (err) { alertError(err); }
  finally { e.target.disabled = false; }
});

// -------------------------------------------------------- rows table ----

// Renders an editable rows table. Returns a function that reads the
// current UI state back out as a rows array (for POSTing).
function renderRowsTable(container, rows, opts) {
  opts = opts || {};
  const season = opts.season ?? 1;
  const table = document.createElement("table");
  table.className = "rows-table";
  table.innerHTML = `<thead><tr>
      <th>Rip?</th><th>Title</th><th>Length</th><th>Size</th><th>Episode #</th>
      <th>Special?</th><th>Episode title (optional)</th><th>Notes</th>
    </tr></thead>`;
  const tbody = document.createElement("tbody");
  table.appendChild(tbody);

  rows.forEach((row, i) => {
    const tr = document.createElement("tr");
    tr.dataset.idx = i;
    const flags = row.flags || [];
    if (row.special) tr.classList.add("special");
    else if (flags.includes("play_all") || flags.includes("long")) tr.classList.add("long");
    else if (row.warnings && row.warnings.length) tr.classList.add("warn");
    tr.innerHTML = `
      <td><input type="checkbox" class="f-include" ${row.include ? "checked" : ""}></td>
      <td>${esc(row.title_index)}${row.name ? `<div class="hint">${esc(row.name)}</div>` : ""}</td>
      <td class="dur">${esc(row.duration_text)}</td>
      <td class="dur">${esc(fmtSize(row.size_bytes))}</td>
      <td><input type="number" class="f-episode" min="1" value="${esc(row.episode ?? "")}"><div class="hint f-code"></div></td>
      <td><input type="checkbox" class="f-special" ${row.special ? "checked" : ""}></td>
      <td><input type="text" class="f-episode-title" value="${esc(row.episode_title || "")}"></td>
      <td class="notes">${(row.warnings || []).map(esc).join("<br>")}</td>`;
    tbody.appendChild(tr);
  });
  container.innerHTML = "";
  container.appendChild(table);

  function updateCodes() {
    Array.from(tbody.children).forEach(tr => {
      const v = parseInt(tr.querySelector(".f-episode").value, 10);
      const sp = tr.querySelector(".f-special").checked;
      tr.querySelector(".f-code").textContent =
        tr.querySelector(".f-include").checked && !isNaN(v) ? epCode(sp ? 0 : season, v) : "";
    });
  }

  // `changed` is the row whose checkbox was just clicked, `field` which one.
  function renumber(changed, field) {
    const trs = Array.from(tbody.children);
    const get = tr => ({
      inc: tr.querySelector(".f-include").checked,
      sp: tr.querySelector(".f-special").checked,
      v: parseInt(tr.querySelector(".f-episode").value, 10),
    });
    // Keep the lowest number of each group as its start - including the
    // number the clicked row had before the click (unticking the first
    // episode must not move the rest up by one).
    let ep = null, sp = null;
    const take = (special, v) => {
      if (special) sp = sp === null ? v : Math.min(sp, v);
      else ep = ep === null ? v : Math.min(ep, v);
    };
    trs.forEach(tr => {
      const r = get(tr);
      if (isNaN(r.v)) return;
      if (tr === changed) {
        if (field === "f-include" && !r.inc) take(r.sp, r.v);            // was included
        else if (field === "f-special" && r.inc) take(!r.sp, r.v);       // was in the other group
        return;
      }
      if (r.inc) take(r.sp, r.v);
    });
    if (ep === null) ep = opts.nextEpisode || 1;
    if (sp === null) sp = opts.nextSpecial || 1;
    trs.forEach(tr => {
      const r = get(tr);
      const input = tr.querySelector(".f-episode");
      if (r.inc && !r.sp) input.value = ep++;
      else if (r.inc && r.sp) input.value = sp++;
      else input.value = "";
    });
    updateCodes();
  }

  tbody.addEventListener("change", e => {
    const field = ["f-include", "f-special"].find(c => e.target.classList.contains(c));
    if (field) renumber(e.target.closest("tr"), field);
    else updateCodes();
  });
  updateCodes();

  return function getRows() {
    return Array.from(tbody.children).map((tr, i) => {
      const epVal = tr.querySelector(".f-episode").value;
      return {
        ...rows[i],
        include: tr.querySelector(".f-include").checked,
        special: tr.querySelector(".f-special").checked,
        episode: epVal === "" ? null : parseInt(epVal, 10),
        episode_title: tr.querySelector(".f-episode-title").value,
      };
    });
  };
}

function renderConflictBanner(container, conflict, onApply) {
  if (!conflict) { container.innerHTML = ""; return; }
  const examples = conflict.examples.map(e =>
    esc(`Title ${e.title_index}: '${e.source}' -> Episode ${e.episode} (${e.confidence} confidence)`)).join("\n");
  let msg = `Disc title names suggest episode numbers for ${conflict.hinted_count} of ${conflict.candidate_count} title(s).`;
  if (conflict.conflict) msg += " These don't match the default disc-order numbering.";
  if (conflict.partial) msg += " Some titles have no detected number.";
  container.innerHTML = `<div class="banner"><div>${esc(msg)}</div><div class="examples">${examples}</div>
    <div class="btn-row"><button class="btn small b-apply">Use the detected numbers</button>
    <button class="btn secondary small b-keep">Keep disc order</button></div></div>`;
  $(".b-apply", container).onclick = onApply;
  $(".b-keep", container).onclick = () => { container.innerHTML = ""; };
}

// ---------------------------------------------------------------- rip --

let ripGetRows = null;
let ripShownScan = null; // scanned_ts of the table on screen
let scanPolling = false;

function selectedRipDrive() { return driveById(parseInt($("#ripDrive").value, 10)); }

function updateRipShowInfo() {
  const show = showById(parseInt($("#ripShow").value, 10));
  $("#ripShowInfo").textContent = show
    ? `Next disc starts at ${epCode(show.season, show.next_episode)}; next special ${epCode(0, show.next_special)}.`
    : "Add a show under Shows first.";
}

function onRipDriveChange() {
  fillRipDrives();
  const d = selectedRipDrive();
  fillShowSelect($("#ripShow"), d ? d.show_id : null);
  updateRipShowInfo();
  ripShownScan = null;
  loadScanResult();
}
$("#ripDrive").addEventListener("change", onRipDriveChange);
$("#ripShow").addEventListener("change", updateRipShowInfo);

async function loadScanResult() {
  const d = selectedRipDrive();
  if (!d) { $("#ripResultsCard").style.display = "none"; return; }
  let scan;
  try { scan = await api("GET", `/api/drives/${d.id}/scan`); } catch (e) { return; }
  showScan(d, scan);
}

function showScan(d, scan) {
  const hint = $("#scanHint");
  if (scan.running) {
    const p = scan.progress;
    hint.style.display = "";
    hint.textContent = `Scanning ${d.label}${p ? ` - ${Math.round(p.percent || 0)}%${p.step ? " - " + p.step : ""}` : ""}. ` +
      "DVDs take a minute or two, Blu-ray and 4K UHD discs several minutes.";
    return;
  }
  hint.style.display = scan.error ? "" : "none";
  hint.textContent = scan.error ? `Last scan: ${scan.error}` : "";
  const data = scan.result;
  if (!data) { $("#ripResultsCard").style.display = "none"; ripGetRows = null; ripShownScan = null; return; }
  if (ripShownScan === data.scanned_ts) return; // don't wipe the user's edits
  ripShownScan = data.scanned_ts;
  const show = scan.show || showById(data.show_id);
  if (show) $("#ripShow").value = String(show.id);
  updateRipShowInfo();
  $("#ripResultsCard").style.display = "";
  $("#ripDiscInfo").textContent = `${data.disc_type || "Disc"} "${data.disc_name || "(no label)"}" in ${d.label} - ` +
    `${data.rows.length} title(s) - numbered for ${show ? show.name : "?"}` +
    (data.notes && data.notes.length ? ` - ${data.notes.join(" ")}` : "");
  const opts = { season: show ? show.season : 1, nextEpisode: show ? show.next_episode : 1, nextSpecial: show ? show.next_special : 1 };
  ripGetRows = renderRowsTable($("#ripTableWrap"), data.rows, opts);
  renderConflictBanner($("#ripConflictBanner"), data.conflict, async () => {
    try {
      const r = await api("POST", `/api/drives/${d.id}/apply_names`);
      ripGetRows = renderRowsTable($("#ripTableWrap"), r.rows, opts);
      $("#ripConflictBanner").innerHTML = "";
    } catch (e) { alertError(e); }
  });
}

function refreshRipStatus() {
  const d = selectedRipDrive();
  if (!d) return;
  $("#scanBtn").disabled = d.activity !== "idle";
  $("#ripBtn").disabled = d.activity !== "idle";
  if (d.activity === "scanning" && !scanPolling) pollScan(d.id);
  if (d.activity === "idle" && !d.has_staged && ripShownScan !== null) loadScanResult();
}

async function pollScan(id) {
  scanPolling = true;
  try {
    for (;;) {
      const d = driveById(id);
      let scan;
      try { scan = await api("GET", `/api/drives/${id}/scan`); } catch (e) { break; }
      if (selectedRipDrive() && selectedRipDrive().id === id) showScan(d || { label: "drive" }, scan);
      if (!scan.running) break;
      await sleep(1500);
    }
  } finally { scanPolling = false; }
}

async function startScan() {
  const d = selectedRipDrive();
  if (!d) return alert("No drive selected.");
  const showId = parseInt($("#ripShow").value, 10);
  if (!showId) return alert("Pick a show first (add one under Shows).");
  try {
    await api("POST", `/api/drives/${d.id}/scan`, { show_id: showId });
    $("#ripResultsCard").style.display = "none";
    ripShownScan = null;
    pollScan(d.id);
  } catch (e) { alertError(e); }
}
$("#scanBtn").addEventListener("click", startScan);

$("#ripBtn").addEventListener("click", async () => {
  const d = selectedRipDrive();
  if (!ripGetRows || !d) return;
  const showId = parseInt($("#ripShow").value, 10);
  try {
    const r = await api("POST", `/api/drives/${d.id}/rip`, { rows: ripGetRows(), show_id: showId || null });
    $("#ripResultsCard").style.display = "none";
    ripGetRows = null;
    openView("dashboard");
    console.log(`Started ripping ${r.count} title(s).`);
  } catch (e) { alertError(e); }
});

// ------------------------------------------------------------- review --

async function loadReviewQueue() {
  let data;
  try { data = await api("GET", "/api/review"); } catch (e) { return; }
  const wrap = $("#reviewList");
  if (!data.items.length) {
    wrap.innerHTML = '<div class="empty">Nothing needs review right now.</div>';
    return;
  }
  wrap.innerHTML = "";
  data.items.forEach(item => {
    const card = document.createElement("div");
    card.className = "card inner";
    const show = showById(item.show_id);
    const drive = state.drives.find(d => d.key === item.drive_key) ||
      state.drives.find(d => d.index === item.drive_index && d.present);
    card.innerHTML = `<h3>#${esc(item.id)} - ${esc(item.disc_label || "(unlabeled disc)")}
        <span class="hint">${esc(new Date(item.created_ts * 1000).toLocaleString())}</span></h3>
      <p class="hint">${esc(item.reason)}</p>
      <div class="grid-3">
        <div><label>Rip from drive</label><select class="r-drive">${state.drives.filter(d => d.present).map(d =>
          `<option value="${d.id}">${esc(d.label)}</option>`).join("")}</select></div>
        <div><label>Show</label><select class="r-show"></select></div>
      </div>
      <div class="table-wrap"></div>
      <div class="btn-row">
        <button class="btn small resolve-btn">Rip These</button>
        <button class="btn secondary small dismiss-btn">Dismiss</button>
      </div>`;
    if (drive) $(".r-drive", card).value = String(drive.id);
    fillShowSelect($(".r-show", card), show ? show.id : null);
    const opts = { season: show ? show.season : 1, nextEpisode: show ? show.next_episode : 1, nextSpecial: show ? show.next_special : 1 };
    const getRows = renderRowsTable($(".table-wrap", card), item.rows, opts);
    $(".resolve-btn", card).addEventListener("click", async () => {
      try {
        await api("POST", `/api/review/${item.id}/resolve`, {
          rows: getRows(),
          drive_id: parseInt($(".r-drive", card).value, 10) || null,
          show_id: parseInt($(".r-show", card).value, 10) || null,
        });
        loadReviewQueue();
        openView("dashboard");
      } catch (e) { alertError(e); }
    });
    $(".dismiss-btn", card).addEventListener("click", async () => {
      if (!confirm("Dismiss this disc without ripping?")) return;
      try { await api("POST", `/api/review/${item.id}/dismiss`); loadReviewQueue(); } catch (e) { alertError(e); }
    });
    wrap.appendChild(card);
  });
}

// -------------------------------------------------------------- shows --

async function loadShowsTable() {
  await loadShows();
  const tbody = $("#showsTable tbody");
  tbody.innerHTML = shows.length ? "" : '<tr><td colspan="6" class="empty">No shows yet - add one below.</td></tr>';
  shows.forEach(s => {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td><input type="text" class="s-name" value="${esc(s.name)}"></td>
      <td><input type="number" class="s-season" min="0" value="${s.season}"></td>
      <td><input type="number" class="s-next" min="1" value="${s.next_episode}"></td>
      <td><input type="number" class="s-special" min="1" value="${s.next_special}"></td>
      <td><input type="text" class="s-dir" value="${esc(s.output_dir)}" placeholder="default library folder"></td>
      <td class="nowrap"><button class="btn small s-save">Save</button>
        <button class="btn danger small s-del">Delete</button></td>`;
    $(".s-save", tr).addEventListener("click", async () => {
      try {
        await api("POST", `/api/shows/${s.id}`, {
          name: $(".s-name", tr).value,
          season: parseInt($(".s-season", tr).value, 10),
          next_episode: parseInt($(".s-next", tr).value, 10),
          next_special: parseInt($(".s-special", tr).value, 10),
          output_dir: $(".s-dir", tr).value,
        });
        loadShowsTable();
      } catch (e) { alertError(e); }
    });
    $(".s-del", tr).addEventListener("click", async () => {
      if (!confirm(`Delete the show "${s.name}"? Ripped files are kept.`)) return;
      try { await api("DELETE", `/api/shows/${s.id}`); loadShowsTable(); } catch (e) { alertError(e); }
    });
    tbody.appendChild(tr);
  });
}

$("#addShowBtn").addEventListener("click", async () => {
  try {
    await api("POST", "/api/shows", {
      name: $("#newShowName").value,
      season: parseInt($("#newShowSeason").value || "1", 10),
      next_episode: parseInt($("#newShowNext").value || "1", 10),
    });
    $("#newShowName").value = "";
    loadShowsTable();
  } catch (e) { alertError(e); }
});

// ------------------------------------------------------------ history --

let historyOldest = null;

function fillHistoryShows() {
  const sel = $("#historyShow");
  const cur = sel.value;
  const html = `<option value="">All shows</option>` +
    shows.map(s => `<option value="${s.id}">${esc(s.name)}</option>`).join("");
  if (sel.dataset.html === html) return;
  sel.innerHTML = html;
  sel.dataset.html = html;
  sel.value = cur;
}
$("#historyShow").addEventListener("change", () => loadHistory(true));

async function loadHistory(reset) {
  if (reset) historyOldest = null;
  const q = new URLSearchParams({ limit: "100" });
  if ($("#historyShow").value) q.set("show_id", $("#historyShow").value);
  if (historyOldest) q.set("before_id", historyOldest);
  let data;
  try { data = await api("GET", "/api/history?" + q); } catch (e) { return; }
  const tbody = $("#historyTable tbody");
  if (reset) tbody.innerHTML = "";
  if (reset && !data.entries.length) tbody.innerHTML = '<tr><td colspan="8" class="empty">Nothing ripped yet.</td></tr>';
  data.entries.forEach(h => {
    const tr = document.createElement("tr");
    const speed = h.status === "ok" && h.elapsed_sec > 0 && h.size_bytes ? `${(h.size_bytes / h.elapsed_sec / 1e6).toFixed(1)} MB/s` : "";
    tr.className = h.status === "ok" ? "" : "long";
    tr.innerHTML = `<td class="nowrap">${esc(fmtTime(h.ts))}</td><td>${esc(h.show_name)}</td>
      <td title="${esc(h.path)}">${esc(epCode(h.season, h.episode))}<div class="hint">${esc(h.disc_label)} t${esc(h.title_index)}</div></td>
      <td class="dur">${esc(fmtSize(h.size_bytes))}</td><td class="dur">${esc(fmtDuration(h.duration_sec))}</td>
      <td class="dur">${esc(speed)}<div class="hint">${esc(fmtDuration(h.elapsed_sec))}</div></td>
      <td>${esc(h.drive)}</td>
      <td class="notes">${h.status === "ok" ? `<span class="ok">Saved</span><div class="hint path">${esc(h.path)}</div>` : esc(h.error || h.status)}</td>`;
    tbody.appendChild(tr);
  });
  if (data.entries.length) historyOldest = data.entries[data.entries.length - 1].id;
  $("#historyMoreBtn").style.display = data.entries.length < 100 ? "none" : "";
}
$("#historyMoreBtn").addEventListener("click", () => loadHistory(false));

// ----------------------------------------------------------- settings --

const SETTINGS = [
  // [element id, config key, kind]
  ["cfgOutputDir", "output_dir", "text"],
  ["cfgFolderLayout", "folder_layout", "text"],
  ["cfgPadSeason", "pad_season_folder", "boolsel"],
  ["cfgDetectLengthFilter", "detect_length_filter", "check"],
  ["cfgMinLengthMin", "min_length_sec", "minutes"],
  ["cfgLongWarnMin", "long_warn_sec", "minutes"],
  ["cfgDetectPlayAll", "detect_play_all", "check"],
  ["cfgDetectLongWarn", "detect_long_warn", "check"],
  ["cfgDetectCluster", "detect_cluster", "check"],
  ["cfgDetectNames", "detect_names", "check"],
  ["cfgAutoEject", "auto_eject", "check"],
  ["cfgSkipRipped", "skip_ripped_discs", "check"],
  ["cfgAutomaticDefault", "automatic_mode", "check"],
  ["cfgPollInterval", "poll_interval_sec", "int"],
  ["cfgNotifyFormat", "notify_format", "text"],
  ["cfgNotifyUrl", "notify_url", "text"],
  ["cfgNotifyDone", "notify_on_done", "check"],
  ["cfgNotifyReview", "notify_on_review", "check"],
  ["cfgNotifyError", "notify_on_error", "check"],
  ["cfgMakemkvPath", "makemkv_path", "text"],
  ["cfgExtraArgs", "makemkv_extra_args", "text"],
];

async function loadSettings() {
  let cfg;
  try { cfg = await api("GET", "/api/config"); } catch (e) { return; }
  SETTINGS.forEach(([id, key, kind]) => {
    const el = $("#" + id);
    const v = cfg[key];
    if (kind === "check") el.checked = !!v;
    else if (kind === "boolsel") el.value = v ? "1" : "0";
    else if (kind === "minutes") el.value = Math.round((v || 0) / 60);
    else el.value = v ?? "";
  });
}

function settingsPatch() {
  const patch = {};
  SETTINGS.forEach(([id, key, kind]) => {
    const el = $("#" + id);
    if (kind === "check") patch[key] = el.checked;
    else if (kind === "boolsel") patch[key] = el.value === "1";
    else if (kind === "minutes") patch[key] = (parseInt(el.value || "0", 10) || 0) * 60;
    else if (kind === "int") patch[key] = parseInt(el.value || "0", 10) || 0;
    else patch[key] = el.value.trim();
  });
  return patch;
}

$("#saveSettingsBtn").addEventListener("click", async () => {
  try {
    await api("POST", "/api/config", settingsPatch());
    await loadSettings();
    const msg = $("#settingsSavedMsg");
    msg.style.display = "";
    setTimeout(() => msg.style.display = "none", 2000);
  } catch (e) { alertError(e); }
});

$("#notifyTestBtn").addEventListener("click", async () => {
  try {
    await api("POST", "/api/config", settingsPatch());
    await api("POST", "/api/notify/test");
    alert("Test notification sent.");
  } catch (e) { alertError(e); }
});

// --------------------------------------------------------------- logs --

function logLineEl(entry) {
  const div = document.createElement("div");
  div.className = "log-line level-" + entry.level;
  div.innerHTML = `<span class="ts">${esc(fmtTime(entry.ts))}</span>` +
    (entry.drive ? `<span class="drv">${esc(entry.drive)}</span>` : "") + esc(entry.message);
  return div;
}

function appendLog(box, entry, max) {
  const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
  box.appendChild(logLineEl(entry));
  if (max) while (box.children.length > max) box.removeChild(box.firstChild);
  if (atBottom) box.scrollTop = box.scrollHeight;
}

$("#showDebug").addEventListener("change", e => $("#fullLog").classList.toggle("hide-debug", !e.target.checked));
$("#fullLog").classList.add("hide-debug");

let oldestLogId = null;
async function loadInitialLogs() {
  try {
    const data = await api("GET", "/api/logs?limit=300");
    const dash = $("#dashLog"), full = $("#fullLog");
    dash.innerHTML = "";
    full.innerHTML = "";
    data.entries.forEach(e => {
      if (e.level !== "debug") dash.appendChild(logLineEl(e));
      full.appendChild(logLineEl(e));
    });
    if (data.entries.length) oldestLogId = data.entries[0].id;
    dash.scrollTop = dash.scrollHeight;
    full.scrollTop = full.scrollHeight;
  } catch (e) { /* server restarting */ }
}

$("#loadMoreLogBtn").addEventListener("click", async () => {
  if (oldestLogId === null) return;
  const data = await api("GET", `/api/logs?before_id=${oldestLogId}&limit=300`);
  const full = $("#fullLog");
  const frag = document.createDocumentFragment();
  data.entries.forEach(e => frag.appendChild(logLineEl(e)));
  full.insertBefore(frag, full.firstChild);
  if (data.entries.length) oldestLogId = data.entries[0].id;
});

// ------------------------------------------------------- live events --

function connectEvents() {
  const es = new EventSource("/api/events");
  es.addEventListener("state", ev => applyState(JSON.parse(ev.data)));
  es.addEventListener("log", ev => {
    const entry = JSON.parse(ev.data);
    if (entry.level !== "debug") appendLog($("#dashLog"), entry, 300);
    appendLog($("#fullLog"), entry, 5000);
  });
  es.onopen = () => {
    $("#statusDot").classList.remove("offline");
  };
  es.onerror = () => {
    $("#statusDot").className = "dot offline";
    $("#statusText").textContent = "Reconnecting...";
  };
}

// --------------------------------------------------------------- init --

(async function init() {
  await loadShows();
  await loadInitialLogs();
  connectEvents(); // the first event is a full state snapshot
})();
