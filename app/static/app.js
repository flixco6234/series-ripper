// Series Ripper web UI. Plain fetch()-based SPA, no build step.

const $ = (sel, root) => (root || document).querySelector(sel);
const $$ = (sel, root) => Array.from((root || document).querySelectorAll(sel));

// Disc labels, title names and MakeMKV messages come from the disc - never
// insert them as HTML.
function esc(value) {
  return String(value ?? "").replace(/[&<>"']/g, c => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

function fmtSize(bytes) {
  if (!bytes) return "";
  return bytes >= 1e9 ? (bytes / 1e9).toFixed(1) + " GB" : Math.round(bytes / 1e6) + " MB";
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
  if (!res.ok) {
    const msg = (data && data.error) ? data.error : `HTTP ${res.status}`;
    throw new Error(msg);
  }
  return data;
}

function fmtTime(ts) {
  return new Date(ts * 1000).toLocaleTimeString();
}

function alertError(err) {
  console.error(err);
  alert(err.message || String(err));
}

// ---------------------------------------------------------------- tabs --

$$(".tab-btn").forEach(btn => {
  btn.addEventListener("click", () => {
    $$(".tab-btn").forEach(b => b.classList.remove("active"));
    $$(".view").forEach(v => v.classList.remove("active"));
    btn.classList.add("active");
    $("#view-" + btn.dataset.view).classList.add("active");
    if (btn.dataset.view === "review") loadReviewQueue();
    if (btn.dataset.view === "settings") loadSettings();
  });
});

// -------------------------------------------------------- rows table ----

// Renders an editable rows table into `container`. Returns a function that
// reads the current UI state back out as a rows array (for POSTing).
function renderRowsTable(container, rows) {
  const table = document.createElement("table");
  table.className = "rows-table";
  table.innerHTML = `<thead><tr>
      <th>Rip?</th><th>Title #</th><th>Duration</th><th>Size</th><th>Episode #</th>
      <th>Special?</th><th>Episode Title</th><th>Notes</th>
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
      <td>${esc(row.title_index)}</td>
      <td class="dur">${esc(row.duration_text)}</td>
      <td class="dur">${esc(fmtSize(row.size_bytes))}</td>
      <td><input type="number" class="f-episode" min="1" value="${esc(row.episode ?? "")}"></td>
      <td><input type="checkbox" class="f-special" ${row.special ? "checked" : ""}></td>
      <td><input type="text" class="f-episode-title" value="${esc(row.episode_title || "")}"></td>
      <td class="notes">${(row.warnings || []).map(esc).join(" | ")}</td>
    `;
    tbody.appendChild(tr);
  });

  container.innerHTML = "";
  container.appendChild(table);

  function renumber() {
    let ep = null;
    // Use the smallest currently-set episode number among included, non-special rows as the base.
    Array.from(tbody.children).forEach(tr => {
      const included = tr.querySelector(".f-include").checked;
      const special = tr.querySelector(".f-special").checked;
      if (included && !special) {
        const v = parseInt(tr.querySelector(".f-episode").value, 10);
        if (!isNaN(v) && (ep === null || v < ep)) ep = v;
      }
    });
    if (ep === null) ep = 1;
    Array.from(tbody.children).forEach(tr => {
      const included = tr.querySelector(".f-include").checked;
      const special = tr.querySelector(".f-special").checked;
      const epInput = tr.querySelector(".f-episode");
      if (included && !special) {
        epInput.value = ep;
        ep += 1;
      } else if (included && special) {
        if (!epInput.value) epInput.value = 1;
      } else {
        epInput.value = "";
      }
    });
  }

  tbody.addEventListener("change", (e) => {
    if (e.target.classList.contains("f-include") || e.target.classList.contains("f-special")) {
      renumber();
    }
  });

  return function getRows() {
    return Array.from(tbody.children).map((tr, i) => {
      const orig = rows[i];
      const epVal = tr.querySelector(".f-episode").value;
      return {
        ...orig,
        include: tr.querySelector(".f-include").checked,
        special: tr.querySelector(".f-special").checked,
        episode: epVal === "" ? null : parseInt(epVal, 10),
        episode_title: tr.querySelector(".f-episode-title").value,
      };
    });
  };
}

function renderConflictBanner(container, conflict, onApply, onKeep) {
  if (!conflict) { container.innerHTML = ""; return; }
  const examples = conflict.examples.map(e =>
    esc(`  Title ${e.title_index}: '${e.source}' -> Episode ${e.episode} (${e.confidence} confidence)`)
  ).join("\n");
  const div = document.createElement("div");
  div.className = "banner";
  let msg = `Disc title names suggest episode numbers for ${conflict.hinted_count} of ${conflict.candidate_count} title(s).`;
  if (conflict.conflict) msg += " These don't match the default disc-order numbering.";
  if (conflict.partial) msg += " Some titles have no detected number.";
  div.innerHTML = `<div>${msg}</div><div class="examples">${examples}</div>`;
  const btnRow = document.createElement("div");
  btnRow.className = "btn-row";
  const applyBtn = document.createElement("button");
  applyBtn.className = "btn small";
  applyBtn.textContent = "Use the detected names";
  applyBtn.onclick = onApply;
  const keepBtn = document.createElement("button");
  keepBtn.className = "btn secondary small";
  keepBtn.textContent = "Use disc order (default)";
  keepBtn.onclick = onKeep;
  btnRow.appendChild(applyBtn);
  btnRow.appendChild(keepBtn);
  div.appendChild(btnRow);
  container.innerHTML = "";
  container.appendChild(div);
}

// ------------------------------------------------------------- manual --

let manualGetRows = null;
let manualDriveIndex = 0;

async function refreshDrives() {
  const sel = $("#driveSelect");
  sel.innerHTML = "<option>Loading...</option>";
  try {
    const data = await api("GET", "/api/drives");
    sel.innerHTML = "";
    data.drives.forEach(d => {
      const opt = document.createElement("option");
      opt.value = d.index;
      opt.textContent = `${d.index}: ${d.drive_name || "(drive)"}${d.has_disc ? " - " + d.disc_name : " (empty)"}${d.busy ? " [busy]" : ""}`;
      sel.appendChild(opt);
    });
  } catch (e) {
    sel.innerHTML = "<option>No drives found</option>";
  }
}
$("#refreshDrivesBtn").addEventListener("click", refreshDrives);

$("#scanBtn").addEventListener("click", async () => {
  const driveIndex = parseInt($("#driveSelect").value || "0", 10);
  manualDriveIndex = driveIndex;
  const btn = $("#scanBtn");
  btn.disabled = true;
  btn.textContent = "Scanning...";
  $("#scanHint").style.display = "";
  try {
    await api("POST", "/api/scan", { drive_index: driveIndex });
    let scan;
    do {
      await sleep(2000);
      scan = await api("GET", "/api/scan");
    } while (scan.running);
    if (scan.error) throw new Error(scan.error);
    const data = scan.result;
    if (!data) throw new Error("Scan finished without a result - see the log.");
    $("#manualResultsCard").style.display = "";
    $("#manualDiscInfo").textContent =
      `${data.disc_type || "Disc"} "${data.disc_name || "(no label)"}" - ${data.rows.length} title(s)`;
    manualGetRows = renderRowsTable($("#manualTableWrap"), data.rows);
    renderConflictBanner($("#manualConflictBanner"), data.conflict,
      async () => {
        const d = await api("POST", "/api/scan/apply_names");
        manualGetRows = renderRowsTable($("#manualTableWrap"), d.rows);
        $("#manualConflictBanner").innerHTML = "";
      },
      () => { $("#manualConflictBanner").innerHTML = ""; }
    );
  } catch (e) {
    alertError(e);
  } finally {
    btn.disabled = false;
    btn.textContent = "Scan Disc";
    $("#scanHint").style.display = "none";
  }
});

$("#ripBtn").addEventListener("click", async () => {
  if (!manualGetRows) return;
  const rows = manualGetRows();
  try {
    const data = await api("POST", "/api/rip", { drive_index: manualDriveIndex, rows });
    alert(`Started ripping ${data.count} title(s).`);
  } catch (e) {
    alertError(e);
  }
});

// ------------------------------------------------------------- review --

async function loadReviewQueue() {
  const data = await api("GET", "/api/review");
  const wrap = $("#reviewList");
  const badge = $("#reviewBadge");
  if (!data.items.length) {
    wrap.innerHTML = '<div class="empty">Nothing needs review right now.</div>';
    badge.style.display = "none";
    return;
  }
  badge.style.display = "";
  badge.textContent = data.items.length;
  badge.className = "badge pending";
  wrap.innerHTML = "";
  data.items.forEach(item => {
    const card = document.createElement("div");
    card.className = "card";
    card.style.marginBottom = "14px";
    card.innerHTML = `<h2>Drive ${esc(item.drive_index)} - ${esc(item.disc_label || "(unlabeled disc)")} - #${esc(item.id)}</h2>
      <p class="hint">${esc(item.reason)}</p>
      <div class="tableWrap"></div>
      <div class="btn-row">
        <button class="btn small resolve-btn">Rip These</button>
        <button class="btn secondary small dismiss-btn">Dismiss</button>
      </div>`;
    const getRows = renderRowsTable(card.querySelector(".tableWrap"), item.rows);
    card.querySelector(".resolve-btn").addEventListener("click", async () => {
      try {
        await api("POST", `/api/review/${item.id}/resolve`, { rows: getRows() });
        alert("Ripping started.");
        loadReviewQueue();
      } catch (e) { alertError(e); }
    });
    card.querySelector(".dismiss-btn").addEventListener("click", async () => {
      if (!confirm("Dismiss this disc without ripping?")) return;
      await api("POST", `/api/review/${item.id}/dismiss`);
      loadReviewQueue();
    });
    wrap.appendChild(card);
  });
}

// ----------------------------------------------------------- settings --

async function loadSettings() {
  const cfg = await api("GET", "/api/config");
  $("#cfgShowName").value = cfg.show_name || "";
  $("#cfgSeason").value = cfg.season;
  $("#cfgNextEpisode").value = cfg.next_episode;
  $("#cfgOutputDir").value = cfg.output_dir;
  $("#cfgMakemkvPath").value = cfg.makemkv_path || "";
  $("#cfgPollInterval").value = cfg.poll_interval_sec;
  $("#cfgAutoEject").checked = !!cfg.auto_eject;
  $("#cfgAutomaticMode").checked = !!cfg.automatic_mode;
  $("#cfgDetectLengthFilter").checked = !!cfg.detect_length_filter;
  $("#cfgMinLengthMin").value = Math.round((cfg.min_length_sec || 0) / 60);
  $("#cfgDetectLongWarn").checked = !!cfg.detect_long_warn;
  $("#cfgDetectCluster").checked = !!cfg.detect_cluster;
  $("#cfgDetectNames").checked = !!cfg.detect_names;
  $("#cfgDetectPlayAll").checked = !!cfg.detect_play_all;
  $("#cfgFolderLayout").value = cfg.folder_layout || "show_season";
  $("#cfgPadSeason").value = cfg.pad_season_folder ? "1" : "0";
}

$("#saveSettingsBtn").addEventListener("click", async () => {
  const patch = {
    show_name: $("#cfgShowName").value.trim(),
    season: parseInt($("#cfgSeason").value || "1", 10),
    next_episode: parseInt($("#cfgNextEpisode").value || "1", 10),
    output_dir: $("#cfgOutputDir").value.trim(),
    makemkv_path: $("#cfgMakemkvPath").value.trim(),
    poll_interval_sec: parseInt($("#cfgPollInterval").value || "15", 10),
    auto_eject: $("#cfgAutoEject").checked,
    automatic_mode: $("#cfgAutomaticMode").checked,
    detect_length_filter: $("#cfgDetectLengthFilter").checked,
    min_length_sec: parseInt($("#cfgMinLengthMin").value || "0", 10) * 60,
    detect_long_warn: $("#cfgDetectLongWarn").checked,
    detect_cluster: $("#cfgDetectCluster").checked,
    detect_names: $("#cfgDetectNames").checked,
    detect_play_all: $("#cfgDetectPlayAll").checked,
    folder_layout: $("#cfgFolderLayout").value,
    pad_season_folder: $("#cfgPadSeason").value === "1",
  };
  try {
    await api("POST", "/api/config", patch);
    const msg = $("#settingsSavedMsg");
    msg.style.display = "";
    setTimeout(() => msg.style.display = "none", 2000);
  } catch (e) { alertError(e); }
});

// ---------------------------------------------------------- dashboard --

async function pollStatus() {
  try {
    const st = await api("GET", "/api/status");
    const dot = $("#statusDot");
    dot.className = "dot " + st.status;
    $("#statusText").textContent = st.status[0].toUpperCase() + st.status.slice(1);

    $("#makemkvInfo").textContent = `${st.makemkv_version || "MakeMKV version unknown"} - ` +
      `license: ${(st.license && st.license.detail) || "checking..."} - temp folder: ${st.temp_dir}`;

    if (st.rip_progress) {
      const p = st.rip_progress;
      $("#dashStatus").textContent = `Ripping title ${p.title_index} -> ` +
        `S${String(p.season).padStart(2, "0")}E${String(p.episode).padStart(2, "0")} ` +
        `(${p.index + 1}/${p.total}) - ${p.percent}%${p.step ? " - " + p.step : ""}`;
      $("#dashProgress").style.width = p.percent + "%";
      $("#dashCancelBtn").style.display = "";
    } else if (st.status === "scanning") {
      $("#dashStatus").textContent = "Scanning disc...";
      $("#dashProgress").style.width = "0%";
      $("#dashCancelBtn").style.display = "none";
    } else {
      $("#dashStatus").textContent = st.automatic_mode
        ? "Idle - waiting for a disc (Automatic Mode is on)."
        : "Idle.";
      $("#dashProgress").style.width = "0%";
      $("#dashCancelBtn").style.display = "none";
    }
  } catch (e) { /* server may be mid-restart; ignore */ }
}
setInterval(pollStatus, 2000);
pollStatus();

$("#dashCancelBtn").addEventListener("click", async () => {
  if (!confirm("Cancel the current rip?")) return;
  await api("POST", "/api/cancel");
});

// --------------------------------------------------------------- logs --

function logLineEl(entry) {
  const div = document.createElement("div");
  div.className = "log-line level-" + entry.level;
  div.innerHTML = `<span class="ts">${fmtTime(entry.ts)}</span>${esc(entry.message)}`;
  return div;
}

// SSE carries only entries created after the client subscribes (no history
// replay - see main.py). The initial batch for both the dashboard's live
// tail and the full Action Log view comes from one GET /api/logs call so
// nothing shows up twice.
function connectLogStream() {
  const es = new EventSource("/api/logs/stream");
  es.onmessage = (ev) => {
    const entry = JSON.parse(ev.data);
    const dashLog = $("#dashLog");
    dashLog.appendChild(logLineEl(entry));
    dashLog.scrollTop = dashLog.scrollHeight;
    while (dashLog.children.length > 200) dashLog.removeChild(dashLog.firstChild);

    const fullLog = $("#fullLog");
    fullLog.appendChild(logLineEl(entry));
    fullLog.scrollTop = fullLog.scrollHeight;
  };
  es.onerror = () => { /* browser auto-reconnects EventSource */ };
}

let oldestLoadedId = null;
async function loadInitialLogs() {
  const data = await api("GET", "/api/logs");
  const dashLog = $("#dashLog");
  const fullLog = $("#fullLog");
  dashLog.innerHTML = "";
  fullLog.innerHTML = "";
  data.entries.forEach(e => {
    dashLog.appendChild(logLineEl(e));
    fullLog.appendChild(logLineEl(e));
  });
  if (data.entries.length) oldestLoadedId = data.entries[0].id;
  dashLog.scrollTop = dashLog.scrollHeight;
  fullLog.scrollTop = fullLog.scrollHeight;
  // Only start streaming live entries once the initial batch has landed,
  // so nothing can arrive out of order ahead of it.
  connectLogStream();
}
loadInitialLogs();

$("#loadMoreLogBtn").addEventListener("click", async () => {
  if (oldestLoadedId === null) return;
  const data = await api("GET", `/api/logs?before_id=${oldestLoadedId}&limit=200`);
  const fullLog = $("#fullLog");
  const frag = document.createDocumentFragment();
  data.entries.forEach(e => frag.appendChild(logLineEl(e)));
  fullLog.insertBefore(frag, fullLog.firstChild);
  if (data.entries.length) oldestLoadedId = data.entries[0].id;
});

// -------------------------------------------------------------- init ---

refreshDrives();
loadSettings();
