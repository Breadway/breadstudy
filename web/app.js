/* breadstudy session client. Works against stub GET /api/skills.
 * Expected skill shape (see API_CONTRACT.md):
 * [{ id, strand, name, vault_note }]
 * Session phases follow spec/session.md.
 */
const PHASES = ["prime", "recall", "practice", "explain", "reflect", "break", "end"];
const TEMPLATE_20 = { recall: 2 * 60, practice: 10 * 60, explain: 5 * 60, reflect: 3 * 60 };

let sessionState = {
  phaseIndex: 0,
  remaining: TEMPLATE_20.recall,
  timerId: null,
  hintsUsed: 0,
  item: null
};

const SAMPLE_ITEM = {
  id: "sample-volume-cylinder",
  skill_id: "volume-prism-cylinder",
  prompt: "A cylinder has radius 3 cm and height 7 cm. Find its volume.",
  latex: "V = \\pi r^2 h = \\pi \\times 3^2 \\times 7 = 63\\pi \\approx 197.9\\ \\text{cm}^3",
  hints: [
    "Write the volume formula for a cylinder first.",
    "Substitute r = 3 and h = 7, then compute r squared before multiplying.",
    "Final step only: 63 pi cubic cm, about 197.9 cm cubed. Check units."
  ]
};

async function fetchSkills() {
  try {
    const res = await fetch("/api/skills", { headers: { accept: "application/json" } });
    const text = await res.text();
    const data = JSON.parse(text);
    if (Array.isArray(data)) return data;
    return [];
  } catch (e) {
    return [];
  }
}

function currentPhase() {
  return PHASES[sessionState.phaseIndex] || "end";
}

function renderPhases(rootId) {
  const root = document.getElementById(rootId);
  if (!root) return;
  root.innerHTML = "";
  PHASES.forEach((p, i) => {
    const el = document.createElement("span");
    el.className = "phase" + (i === sessionState.phaseIndex ? " active" : i < sessionState.phaseIndex ? " done" : "");
    el.textContent = p;
    root.appendChild(el);
  });
  const label = document.getElementById("phase-label");
  if (label) label.textContent = "Phase: " + currentPhase();
}

function fmtClock(totalSeconds) {
  const m = Math.floor(totalSeconds / 60);
  const s = totalSeconds % 60;
  return String(m).padStart(2, "0") + ":" + String(s).padStart(2, "0");
}

function renderTimer() {
  const el = document.getElementById("session-timer");
  if (el) el.textContent = fmtClock(Math.max(0, sessionState.remaining));
}

function startTimer() {
  stopTimer();
  sessionState.timerId = setInterval(() => {
    if (sessionState.remaining > 0) {
      sessionState.remaining -= 1;
      renderTimer();
    } else {
      advancePhase();
    }
  }, 1000);
}

function stopTimer() {
  if (sessionState.timerId) clearInterval(sessionState.timerId);
  sessionState.timerId = null;
}

function advancePhase() {
  if (sessionState.phaseIndex < PHASES.length - 1) {
    sessionState.phaseIndex += 1;
    const phase = currentPhase();
    sessionState.remaining = TEMPLATE_20[phase] || 60;
    renderPhases("phase-row");
    renderTimer();
    logLocal("phase", { phase });
  } else {
    stopTimer();
  }
}

function renderHints() {
  const root = document.getElementById("hint-list");
  if (!root) return;
  root.innerHTML = "";
  const item = sessionState.item || SAMPLE_ITEM;
  item.hints.forEach((hint, idx) => {
    const div = document.createElement("div");
    const unlocked = idx < sessionState.hintsUsed;
    div.className = "hint" + (unlocked ? "" : " locked");
    div.textContent = unlocked ? "Hint " + (idx + 1) + ": " + hint : "Hint " + (idx + 1) + ": locked";
    root.appendChild(div);
  });
  const btn = document.getElementById("hint-btn");
  if (btn) btn.disabled = sessionState.hintsUsed >= item.hints.length;
  const meta = document.getElementById("hint-meta");
  if (meta) meta.textContent = "Hints used: " + sessionState.hintsUsed + " of " + item.hints.length;
}

function useHint() {
  const item = sessionState.item || SAMPLE_ITEM;
  if (sessionState.hintsUsed < item.hints.length) {
    sessionState.hintsUsed += 1;
    renderHints();
    logLocal("hint", { item: item.id, count: sessionState.hintsUsed });
  }
}

function logLocal(kind, payload) {
  try {
    const key = "breadstudy-log";
    const existing = JSON.parse(localStorage.getItem(key) || "[]");
    existing.push({ at: new Date().toISOString(), kind, payload });
    localStorage.setItem(key, JSON.stringify(existing));
  } catch (e) { /* storage full or blocked, session continues */ }
}

function bindAnswerInputs() {
  const latex = document.getElementById("latex-answer");
  const preview = document.getElementById("latex-preview");
  if (latex && preview) {
    latex.addEventListener("input", () => {
      const raw = latex.value.trim();
      preview.textContent = raw ? "LaTeX: " + raw : "Typed answer preview appears here.";
      if (window.BreadstudyScratch && raw) {
        window.BreadstudyScratch.previewInto("latex-preview", raw);
      }
    });
  }
  const voiceBtn = document.getElementById("voice-btn");
  const voiceStatus = document.getElementById("voice-status");
  if (voiceBtn) {
    voiceBtn.addEventListener("click", async () => {
      if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
        if (voiceStatus) voiceStatus.textContent = "Mic not available on this device.";
        return;
      }
      try {
        const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
        if (voiceStatus) voiceStatus.textContent = "Mic ready. Recording is handled by the session service.";
        stream.getTracks().forEach((t) => t.stop());
        logLocal("voice-input", { ok: true });
      } catch (e) {
        if (voiceStatus) voiceStatus.textContent = "Mic blocked. You can still type or take a photo.";
        logLocal("voice-input", { ok: false });
      }
    });
  }
  const photo = document.getElementById("photo-input");
  const photoStatus = document.getElementById("photo-status");
  if (photo) {
    photo.addEventListener("change", () => {
      if (photo.files && photo.files.length > 0) {
        if (photoStatus) photoStatus.textContent = "Photo attached: " + photo.files[0].name;
        logLocal("photo-input", { name: photo.files[0].name });
      }
    });
  }
}

function bindSessionControls() {
  const start = document.getElementById("start-btn");
  if (start) start.addEventListener("click", () => {
    sessionState.item = SAMPLE_ITEM;
    sessionState.phaseIndex = 0;
    sessionState.remaining = TEMPLATE_20.recall;
    sessionState.hintsUsed = 0;
    renderPhases("phase-row");
    renderTimer();
    renderHints();
    renderSamplePrompt();
    startTimer();
    logLocal("session-start", { template: "20min" });
  });
  const next = document.getElementById("next-phase-btn");
  if (next) next.addEventListener("click", advancePhase);
  const hintBtn = document.getElementById("hint-btn");
  if (hintBtn) hintBtn.addEventListener("click", useHint);
  const skip = document.getElementById("skip-btn");
  if (skip) skip.addEventListener("click", () => {
    const reason = prompt("One-line skip reason (required):", "");
    if (reason && reason.trim()) {
      logLocal("skip", { reason: reason.trim() });
      advancePhase();
    }
  });
}

function renderSamplePrompt() {
  const prompt = document.getElementById("item-prompt");
  if (prompt) prompt.textContent = SAMPLE_ITEM.prompt;
  if (window.BreadstudyScratch) {
    window.BreadstudyScratch.previewInto("scratch-preview", SAMPLE_ITEM.latex);
    window.BreadstudyScratch.drawCylinder("scratch-diagram");
  }
}

async function renderDashboard() {
  const skills = await fetchSkills();
  const el = document.getElementById("dash-skills");
  if (el) {
    el.textContent = skills.length > 0 ? skills.length + " skills loaded from /api/skills." : "Using cached sample skills (API stub returns empty).";
  }
  const today = document.getElementById("dash-today");
  if (today) today.textContent = "Today: 20 min school-day session, Test 6 Volume/SA focus.";
}

function registerSW() {
  if ("serviceWorker" in navigator) {
    navigator.serviceWorker.register("./sw.js").catch(() => {});
  }
}

document.addEventListener("DOMContentLoaded", () => {
  renderPhases("phase-row");
  renderTimer();
  renderHints();
  renderSamplePrompt();
  bindSessionControls();
  bindAnswerInputs();
  renderDashboard();
  registerSW();
});

window.BreadstudySession = { advancePhase, useHint, fetchSkills, SAMPLE_ITEM };
