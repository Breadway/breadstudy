/* breadstudy progress page. Reads GET /api/progress and nothing else.
 * Shapes come from server/src/progress.rs:
 * { generated_at, attempt_limit, attempts_total,
 *   recent_attempts: [{attempt_id,item_id,prompt,skill_id,skill_name,verdict,
 *     correct,method_marks,method_marks_total,final_marks,final_marks_total,
 *     error_tags,hints_used,input_kind,created_at}],
 *   skills: [{skill_id,name,strand,mastery,accuracy,due_at,last_review,reviews,lapses}],
 *   error_patterns: [{skill_id,skill_name,tag,count,first_seen,last_seen}] }
 */

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function parseDate(value) {
  if (!value) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date;
}

function formatDate(value) {
  const date = parseDate(value);
  return date ? date.toLocaleString() : "unknown time";
}

function formatDue(value) {
  const date = parseDate(value);
  if (!date) return "not scheduled";
  const now = new Date();
  const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const startOfDue = new Date(date.getFullYear(), date.getMonth(), date.getDate());
  const days = Math.round((startOfDue - startOfToday) / 86400000);
  if (days <= 0) return days === 0 ? "due today" : "overdue by " + Math.abs(days) + " day" + (days === -1 ? "" : "s");
  if (days === 1) return "due tomorrow";
  return "due in " + days + " days (" + date.toLocaleDateString() + ")";
}

function verdictLabel(verdict) {
  if (verdict === "correct") return "Correct with working";
  if (verdict === "partial") return "Right answer, thin working";
  return "Incorrect";
}

function renderTopPattern(patterns) {
  const target = document.getElementById("top-pattern");
  if (!target) return;
  target.innerHTML = "";
  if (!Array.isArray(patterns) || patterns.length === 0) {
    target.textContent = "No error tags recorded yet. Tags appear after a marked attempt gets something wrong.";
    return;
  }
  const top = patterns[0];
  const line = el("p", "pattern-line");
  line.appendChild(el("strong", null, top.tag));
  line.appendChild(document.createTextNode(
    " x" + top.count + " in " + top.skill_name +
    " (last seen " + formatDate(top.last_seen) + ")"
  ));
  target.appendChild(line);
  if (patterns.length > 1) {
    const rest = patterns.slice(1, 4).map(function (pattern) {
      return pattern.tag + " x" + pattern.count + " (" + pattern.skill_name + ")";
    });
    target.appendChild(el("p", "muted", "Also recurring: " + rest.join(", ") + "."));
  }
}

function renderAttempts(attempts, total) {
  const list = document.getElementById("attempt-list");
  const summary = document.getElementById("attempt-summary");
  if (!list) return;
  list.innerHTML = "";
  if (summary) {
    summary.textContent = total === 0
      ? "No marked attempts yet. Finish a session and submit an answer."
      : "Showing the latest " + attempts.length + " of " + total + " marked attempt" +
        (total === 1 ? "" : "s") + ".";
  }
  if (!Array.isArray(attempts) || attempts.length === 0) {
    list.appendChild(el("li", "muted", "No attempts recorded on the server yet."));
    return;
  }
  attempts.forEach(function (attempt) {
    const item = el("li", "attempt");
    const head = el("div", "attempt-head");
    const badge = el("span", "verdict verdict-" + attempt.verdict, verdictLabel(attempt.verdict));
    head.appendChild(badge);
    head.appendChild(el("span", "attempt-skill", attempt.skill_name));
    item.appendChild(head);
    item.appendChild(el("p", "attempt-prompt", attempt.prompt));

    const marks = el("p", "muted attempt-marks");
    marks.textContent = "Method " + attempt.method_marks + "/" + attempt.method_marks_total +
      " - final " + attempt.final_marks + "/" + attempt.final_marks_total +
      (attempt.hints_used > 0 ? " - " + attempt.hints_used + " hint" + (attempt.hints_used === 1 ? "" : "s") : "");
    item.appendChild(marks);

    if (Array.isArray(attempt.error_tags) && attempt.error_tags.length > 0) {
      const tags = el("p", "tag-row");
      attempt.error_tags.forEach(function (tag) {
        tags.appendChild(el("span", "tag", tag));
      });
      item.appendChild(tags);
    }
    item.appendChild(el("p", "muted attempt-time", formatDate(attempt.created_at)));
    list.appendChild(item);
  });
}

function renderMastery(skills) {
  const list = document.getElementById("mastery-list");
  if (!list) return;
  list.innerHTML = "";
  if (!Array.isArray(skills) || skills.length === 0) {
    list.appendChild(el("li", "muted", "No skills indexed yet."));
    return;
  }
  skills.forEach(function (skill) {
    const item = el("li", "mastery");
    const head = el("div", "mastery-head");
    head.appendChild(el("strong", null, skill.name));
    head.appendChild(el("span", "muted", skill.strand || ""));
    item.appendChild(head);

    const pct = Math.round((Number(skill.mastery) || 0) * 100);
    const bar = el("div", "mastery-bar");
    bar.setAttribute("role", "img");
    bar.setAttribute("aria-label", pct + " percent mastery");
    const fill = el("div", "mastery-fill");
    fill.style.width = Math.max(0, Math.min(100, pct)) + "%";
    bar.appendChild(fill);
    item.appendChild(bar);

    const meta = el("p", "muted");
    meta.textContent = pct + "% mastery - " + formatDue(skill.due_at) +
      " - " + skill.reviews + " review" + (skill.reviews === 1 ? "" : "s") +
      (skill.lapses > 0 ? ", " + skill.lapses + " lapse" + (skill.lapses === 1 ? "" : "s") : "");
    item.appendChild(meta);
    list.appendChild(item);
  });
}

function renderOffline() {
  const banner = document.getElementById("offline-banner");
  if (!banner) return;
  const online = !(typeof navigator !== "undefined" && navigator.onLine === false);
  banner.hidden = online;
  if (!online) banner.textContent = "Offline: progress is served by the server and is unavailable until you reconnect.";
}

async function loadProgress() {
  renderOffline();
  const topPattern = document.getElementById("top-pattern");
  try {
    const response = await fetch("/api/progress", { headers: { accept: "application/json" } });
    if (!response.ok) throw new Error("GET /api/progress -> " + response.status);
    const data = await response.json();
    renderTopPattern(data.error_patterns);
    renderAttempts(data.recent_attempts, Number(data.attempts_total) || 0);
    renderMastery(data.skills);
  } catch (error) {
    if (topPattern) topPattern.textContent = "Progress is unavailable right now. This is not an empty result.";
    const list = document.getElementById("attempt-list");
    if (list) {
      list.innerHTML = "";
      list.appendChild(el("li", "muted", "Could not load attempts from the server."));
    }
    const mastery = document.getElementById("mastery-list");
    if (mastery) {
      mastery.innerHTML = "";
      mastery.appendChild(el("li", "muted", "Could not load mastery from the server."));
    }
    const summary = document.getElementById("attempt-summary");
    if (summary) summary.textContent = String(error);
  }
}

document.addEventListener("DOMContentLoaded", loadProgress);
if (window.addEventListener) {
  window.addEventListener("online", loadProgress);
  window.addEventListener("offline", renderOffline);
}

window.BreadstudyProgress = { loadProgress, formatDue, verdictLabel };
