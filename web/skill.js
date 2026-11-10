/* breadstudy skill topic view (STUDY-45).
 * Reads GET /api/skills/{id} and starts a skill-scoped session with
 * POST /api/session {template, skill_id}. Shapes come from
 * server/src/skills.rs.
 */

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function skillIdFromUrl() {
  try {
    return new URL(window.location.href).searchParams.get("skill");
  } catch (error) {
    return null;
  }
}

function setStatus(message) {
  const status = document.getElementById("skill-status");
  if (status) status.textContent = message || "";
}

function renderOffline() {
  const banner = document.getElementById("offline-banner");
  if (!banner) return;
  const online = !(typeof navigator !== "undefined" && navigator.onLine === false);
  banner.hidden = online;
  if (!online) banner.textContent = "Offline: the skill summary is served by the server and needs a connection.";
}

function renderDetail(detail) {
  const name = document.getElementById("skill-name");
  if (name) name.textContent = detail.name;
  const strand = document.getElementById("skill-strand");
  if (strand) strand.textContent = detail.strand;
  const vault = document.getElementById("skill-vault");
  if (vault) {
    vault.innerHTML = "";
    vault.appendChild(document.createTextNode("Vault note: "));
    vault.appendChild(el("strong", null, detail.vault_note));
    vault.appendChild(document.createTextNode(" (" + detail.vault_note_path + ")"));
  }
  const summary = document.getElementById("skill-summary");
  if (summary) summary.textContent = detail.summary;

  const notes = document.getElementById("skill-notes");
  if (notes) {
    notes.innerHTML = "";
    if (!Array.isArray(detail.note_excerpts) || detail.note_excerpts.length === 0) {
      notes.appendChild(el("li", "muted", "No note excerpts were stored with this skill's items yet. The vault note name above is the place to read."));
    } else {
      detail.note_excerpts.forEach(function (note) {
        notes.appendChild(el("li", "note", note));
      });
    }
  }

  const questions = document.getElementById("skill-questions");
  if (questions) {
    questions.innerHTML = "";
    if (!Array.isArray(detail.sample_items) || detail.sample_items.length === 0) {
      questions.appendChild(el("li", "muted", "No verified questions in this skill yet."));
    } else {
      detail.sample_items.forEach(function (item) {
        const row = el("li", "question", item.prompt);
        if (item.source_url) row.title = item.source_url;
        questions.appendChild(row);
      });
    }
  }

  const practise = document.getElementById("practise-btn");
  if (practise) practise.disabled = !(Number(detail.item_count) > 0);
}

async function startScopedSession(skillId) {
  const practise = document.getElementById("practise-btn");
  if (practise) {
    practise.disabled = true;
    practise.textContent = "Starting...";
  }
  setStatus("");
  try {
    const response = await fetch("/api/session", {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify({ template: "school_day_20", skill_id: skillId })
    });
    const data = await response.json().catch(function () { return {}; });
    if (!response.ok) {
      throw new Error(data.error || "/api/session -> " + response.status);
    }
    window.location.href = "./session.html?session=" + encodeURIComponent(data.id);
  } catch (error) {
    setStatus("Could not start a session for this skill: " + error.message);
    if (practise) {
      practise.disabled = false;
      practise.textContent = "Practise this skill";
    }
  }
}

async function loadSkill() {
  renderOffline();
  const skillId = skillIdFromUrl();
  if (!skillId) {
    setStatus("No skill was named in the link. Open a skill from the dashboard.");
    return;
  }
  try {
    const response = await fetch("/api/skills/" + encodeURIComponent(skillId), {
      headers: { accept: "application/json" }
    });
    if (!response.ok) {
      throw new Error("GET /api/skills/" + skillId + " -> " + response.status);
    }
    const detail = await response.json();
    renderDetail(detail);
    const practise = document.getElementById("practise-btn");
    if (practise) {
      practise.onclick = function () {
        startScopedSession(detail.id);
      };
    }
    document.title = "breadstudy - " + detail.name;
  } catch (error) {
    setStatus("This skill could not be loaded: " + error.message);
    const name = document.getElementById("skill-name");
    if (name) name.textContent = "Skill unavailable";
  }
}

document.addEventListener("DOMContentLoaded", loadSkill);
if (window.addEventListener) {
  window.addEventListener("online", loadSkill);
  window.addEventListener("offline", renderOffline);
}

window.BreadstudySkill = { loadSkill, startScopedSession };
