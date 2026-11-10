/* breadstudy session client. Works against stub GET /api/skills.
 * Expected skill shape (see API_CONTRACT.md):
 * [{ id, strand, name, vault_note }]
 * Session phases follow spec/session.md.
 */
const DEFAULT_PLAN = [
  { phase: "prime", planned_seconds: 0 },
  { phase: "recall", planned_seconds: 2 * 60 },
  { phase: "practice", planned_seconds: 10 * 60 },
  { phase: "explain", planned_seconds: 5 * 60 },
  { phase: "reflect", planned_seconds: 3 * 60 },
  { phase: "break", planned_seconds: 0 },
  { phase: "end", planned_seconds: 0 }
];
const TEMPLATE_20 = { recall: 2 * 60, practice: 10 * 60, explain: 5 * 60, reflect: 3 * 60 };
const CONTENT_PHASES = new Set(["recall", "practice", "explain"]);

const ACTIVE_SESSION_KEY = "breadstudy-active-session";
const SESSION_CACHE_KEY = "breadstudy-session-cache";
const ATTEMPT_QUEUE_KEY = "breadstudy-attempt-queue";
const BOARD_CACHE_KEY = "breadstudy-board-blocks";
const SAVED_EXPLANATIONS_KEY = "breadstudy-saved-explanations";

function readStoredJson(key, fallback) {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return fallback;
    return JSON.parse(raw);
  } catch (e) {
    return fallback;
  }
}

function writeStoredJson(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
    return true;
  } catch (e) {
    return false;
  }
}

function cacheSessionView(view) {
  writeStoredJson(SESSION_CACHE_KEY, { at: new Date().toISOString(), view });
}

function readCachedSessionView() {
  const cached = readStoredJson(SESSION_CACHE_KEY, null);
  return cached && cached.view ? cached : null;
}

function readAttemptQueue() {
  const queue = readStoredJson(ATTEMPT_QUEUE_KEY, []);
  return Array.isArray(queue) ? queue : [];
}

function writeAttemptQueue(queue) {
  return writeStoredJson(ATTEMPT_QUEUE_KEY, queue);
}

function queueAttempt(payload) {
  const queue = readAttemptQueue();
  queue.push({ id: Date.now() + "-" + Math.random().toString(16).slice(2), at: new Date().toISOString(), payload });
  return writeAttemptQueue(queue);
}


let sessionState = {
  phaseIndex: 0,
  remaining: TEMPLATE_20.recall,
  timerId: null,
  hintsUsed: 0,
  hintsUnlocked: 0,
  hintTotal: 0,
  phaseTotal: 0,
  phaseRemaining: 0,
  item: null,
  sessionId: null,
  template: null,
  plan: null,
  serverMode: false,
  phaseExhausted: false,
  idle: true,
  pollId: null,
  syncInFlight: false,
  offline: false,
  flushInFlight: false,
  marking: false
};

function readStoredSessionId() {
  try {
    return localStorage.getItem(ACTIVE_SESSION_KEY) || null;
  } catch (e) {
    return null;
  }
}

function urlSessionId() {
  try {
    return new URL(window.location.href).searchParams.get("session");
  } catch (e) {
    return null;
  }
}

function persistSessionId(id) {
  try {
    if (id) localStorage.setItem(ACTIVE_SESSION_KEY, id);
    else localStorage.removeItem(ACTIVE_SESSION_KEY);
  } catch (e) { /* private mode: the URL still carries the id */ }
  try {
    const url = new URL(window.location.href);
    if (id) url.searchParams.set("session", id);
    else url.searchParams.delete("session");
    window.history.replaceState({ session: id || null }, "", url.toString());
  } catch (e) { /* replaceState can be blocked in an embedded view */ }
}

function setStartState(active) {
  ["start-20-btn", "start-40-btn"].forEach((id) => {
    const button = document.getElementById(id);
    if (!button) return;
    button.disabled = Boolean(active);
    button.setAttribute("aria-disabled", active ? "true" : "false");
  });
}

function prefersReducedMotion() {
  return Boolean(window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
}

function phaseLabel(phase) {
  if (!phase) return "Ready";
  return phase.charAt(0).toUpperCase() + phase.slice(1);
}

function setSessionStatus(message) {
  const status = document.getElementById("session-status");
  if (status) status.textContent = message || "";
}

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

async function postJson(path, body) {
  const res = await fetch(path, {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json" },
    body: JSON.stringify(body || {})
  });
  if (!res.ok) throw new Error(path + " -> " + res.status);
  return res.json();
}

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

const PHASE_INDEX = { prime: 0, recall: 1, practice: 2, explain: 3, reflect: 4, break: 5, end: 6 };

function applyServerView(view, options) {
  const offline = Boolean(options && options.offline);
  sessionState.offline = offline;
  sessionState.idle = false;
  sessionState.sessionId = view.id;
  sessionState.template = view.template || sessionState.template;
  sessionState.plan = Array.isArray(view.plan) && view.plan.length > 0 ? view.plan : null;
  if (view.id !== sessionStatsId) {
    sessionStatsId = view.id;
    sessionStats = loadSessionStats(view.id);
  }
  sessionState.serverMode = !offline;
  const index = typeof view.phase_index === "number" ? view.phase_index : PHASE_INDEX[view.phase];
  const nextIndex = index !== undefined ? index : 0;
  const phaseChanged = nextIndex !== sessionState.phaseIndex;
  sessionState.phaseIndex = nextIndex;
  if (phaseChanged) lastTimerAnnouncement = null;
  const phaseInfo = Array.isArray(view.phase_items) ? view.phase_items[nextIndex] : null;
  sessionState.phaseTotal = Number(phaseInfo && phaseInfo.total) || 0;
  sessionState.phaseRemaining = Number(phaseInfo && phaseInfo.remaining) || 0;
  sessionState.remaining = view.phase_remaining_seconds || 0;
  const previous = sessionState.item;
  const sameItem = Boolean(previous && view.item && previous.id === view.item.id);
  if (phaseChanged) sessionState.phaseExhausted = false;
  if (!sameItem) sessionState.phaseExhausted = false;
  if (view.item) {
    const incoming = view.item.hints || [];
    const total = Number(view.item.hint_count) || incoming.length;
    let ladder = incoming.slice();
    if (previous && previous.id === view.item.id && Array.isArray(previous.hints)) {
      if (previous.hints.length > ladder.length) ladder = previous.hints.slice();
      incoming.forEach((hint, idx) => { ladder[idx] = hint; });
    }
    const unlocked = Number.isFinite(view.item.hint_unlocked)
      ? Number(view.item.hint_unlocked)
      : (Number.isFinite(view.hints_used) ? Number(view.hints_used) : 0);
    sessionState.item = {
      id: view.item.id,
      skill_id: view.item.skill_id,
      strand: view.item.strand,
      prompt: view.item.prompt,
      hints: ladder,
      hint_count: Math.max(total, ladder.length),
      hint_unlocked: unlocked
    };
    sessionState.hintsUnlocked = unlocked;
    sessionState.hintTotal = Math.max(total, ladder.length);
  } else {
    sessionState.item = null;
    sessionState.hintsUnlocked = 0;
    sessionState.hintTotal = 0;
  }
  sessionState.hintsUsed = sessionState.hintsUnlocked;
  if (!sessionState.item || !previous || previous.id !== sessionState.item.id) {
    resetAttemptState();
  }
  const explainItem = view.phase === "explain" ? practisedExplainItem() : null;
  if (explainItem) {
    sessionState.item = {
      id: explainItem.id,
      skill_id: explainItem.skill_id,
      strand: explainItem.strand,
      prompt: explainItem.prompt,
      hints: (explainItem.hints || []).slice(),
      hint_count: explainItem.hint_count,
      hint_unlocked: explainItem.hint_unlocked
    };
    sessionState.hintsUnlocked = explainItem.hint_unlocked || 0;
    sessionState.hintTotal = explainItem.hint_count || sessionState.item.hints.length;
    sessionState.hintsUsed = sessionState.hintsUnlocked;
  }
  persistSessionId(view.id);
  if (offline) {
    sessionState.serverMode = false;
    renderOfflineBanner();
  } else {
    sessionState.serverMode = true;
    cacheSessionView(view);
  }
  renderPhases("phase-row");
  renderTimer();
  renderHints();
  renderItem();
  renderChrome();
  if (view.phase === "explain") openFold("teach-details");
  const finished = view.phase === "end" || Boolean(view.ended_at);
  setStartState(!finished);
  if (finished) {
    stopTimer();
    stopPolling();
    persistSessionId(null);
    setSessionStatus("Session finished.");
  } else {
    if (!offline) startPolling();
    if (!sessionState.timerId) startTimer();
  }
}

function marksLine() {
  return "Method " + sessionStats.method + "/" + sessionStats.methodTotal +
    ", final answer " + sessionStats.final + "/" + sessionStats.finalTotal + ".";
}

function phaseContent(phase) {
  if (phase === "reflect") {
    return "Reflection (no question to answer).\n" +
      "1. Which step felt automatic today?\n" +
      "2. Where did you lose a mark, and what will you check first next time?\n" +
      "3. What one thing should the next session start with?\n\n" +
      "Think it through, then press Next phase.";
  }
  if (phase === "end") {
    return "Session complete.\n" +
      "Questions marked this session: " + sessionStats.graded + ".\n" +
      marksLine() + "\n" +
      "Hints revealed: " + sessionStats.hints + ".\n\n" +
      "Nothing is blocked. Start a new session when you are ready.";
  }
  if (phase === "prime") {
    return "Warm-up (no question to answer).\n" +
      "Write down one formula you will need today, and one thing you remember from last session.\n" +
      "Then press Next phase when you feel warm.";
  }
  return null;
}

function renderItem() {
  const prompt = document.getElementById("item-prompt");
  if (!prompt) return;
  if (sessionState.item) {
    prompt.textContent = sessionState.item.prompt;
    const strand = document.getElementById("item-strand");
    if (strand) {
      const explaining = currentPhase() === "explain" && lastPractised && lastPractised.item &&
        lastPractised.item.id === sessionState.item.id;
      strand.textContent = explaining
        ? "Explain phase: the question you just practised" + (lastPractised.answer ? " - your answer: " + lastPractised.answer : "")
        : (sessionState.item.strand || "");
    }
  } else if (sessionState.sessionId) {
    const phase = currentPhase();
    const content = phaseContent(phase);
    prompt.textContent = content || "This phase has no question attached. Use the board, or move to the next phase.";
    const strand = document.getElementById("item-strand");
    if (strand) {
      strand.textContent = phase === "end"
        ? "Session summary"
        : phase === "reflect"
          ? "Reflection"
          : phase === "prime"
            ? "Warm-up"
            : "";
    }
  } else {
    prompt.textContent = "";
    const strand = document.getElementById("item-strand");
    if (strand) strand.textContent = "";
  }
}

function openFold(id) {
  const fold = document.getElementById(id);
  if (fold && "open" in fold && !fold.open) fold.open = true;
}

function itemCounterText() {
  if (sessionState.idle) return "No session";
  const phase = currentPhase();
  if (phase === "end") return "Session finished";
  if (!sessionState.item) return phaseLabel(phase) + " phase";
  if (phase === "explain") return "Explain";
  if (sessionState.phaseTotal > 0 && sessionState.phaseRemaining > 0) {
    const number = Math.max(1, sessionState.phaseTotal - sessionState.phaseRemaining + 1);
    return "Item " + number + "/" + sessionState.phaseTotal;
  }
  const fallback = (sessionStats && Array.isArray(sessionStats.items) ? sessionStats.items.length : 0) + 1;
  return "Item " + fallback;
}

function primaryActionState() {
  if (sessionState.idle) return { text: "Start 20 min", disabled: false, action: "start" };
  if (currentPhase() === "end") return { text: "Session finished", disabled: true, action: "none" };
  const phase = currentPhase();
  if (phase === "explain") return { text: "Next phase", disabled: false, action: "advance" };
  if (CONTENT_PHASES.has(phase) && sessionState.item && !sessionState.phaseExhausted) {
    return { text: "Next question", disabled: false, action: "next" };
  }
  return { text: "Next phase", disabled: false, action: "advance" };
}

function updateActionButtons() {
  const state = primaryActionState();
  const shortText = state.text === "Next question" || state.text === "Next phase" ? "Next" : state.text;
  const top = document.getElementById("next-question-btn");
  if (top) {
    top.textContent = state.text;
    top.disabled = state.disabled;
    top.hidden = state.action === "none";
  }
  const bottom = document.getElementById("bottom-next-btn");
  if (bottom) {
    bottom.textContent = shortText;
    bottom.disabled = state.disabled;
  }
}

function renderChrome() {
  const idle = sessionState.idle;
  const ended = !idle && currentPhase() === "end";
  document.body.classList.toggle("session-active", !idle);
  document.body.classList.toggle("session-idle", idle);
  const bar = document.getElementById("sessionbar");
  if (bar) bar.hidden = idle;
  const idleCard = document.getElementById("idle-card");
  if (idleCard) idleCard.hidden = !idle;
  const question = document.getElementById("question-card");
  if (question) question.hidden = idle;
  const answer = document.getElementById("answer-card");
  if (answer) answer.hidden = idle || !sessionState.item;
  const actions = document.getElementById("action-bar");
  if (actions) actions.hidden = idle || ended;
  const finish = document.getElementById("finish-actions");
  if (finish) finish.hidden = !ended;
  const hints = document.getElementById("hint-details");
  if (hints) hints.hidden = idle || ended;
  const teach = document.getElementById("teach-details");
  if (teach) teach.hidden = idle;
  const controls = document.getElementById("controls-details");
  if (controls) controls.hidden = idle || ended;
  const submit = document.getElementById("submit-btn");
  if (submit) submit.disabled = idle || ended || !sessionState.item || sessionState.marking;
  const label = document.getElementById("phase-label");
  if (label) label.textContent = idle ? "Ready" : phaseLabel(currentPhase());
  const counter = document.getElementById("item-counter");
  if (counter) counter.textContent = itemCounterText();
  updateActionButtons();
}

async function startSession(templateKey) {
  const template = templateKey === "weekend_40" ? "weekend_40" : "school_day_20";
  if (sessionState.sessionId && currentPhase() !== "end") {
    setSessionStatus("A session is already running. Finish or end it before starting another.");
    setStartState(true);
    return;
  }
  sessionState.idle = false;
  try {
    const view = await postJson("/api/session", { template });
    applyServerView(view);
    startTimer();
    setSessionStatus("Session started. The server owns the phase timer.");
    logLocal("session-start", { template, session: view.id });
  } catch (e) {
    const cached = readCachedSessionView();
    if (cached) {
      applyServerView(cached.view, { offline: true });
      stopPolling();
      startTimer();
      setSessionStatus("Offline: showing the session cached on this device at " +
        new Date(cached.at).toLocaleString() + ". Answers queue until you reconnect.");
      logLocal("session-start-cached", { at: cached.at });
      return;
    }
    setIdleState();
    setSessionStatus("Could not start a session: the server is unreachable and nothing is cached on this device yet.");
    logLocal("session-start-failed", { error: String(e) });
  }
}

async function restoreSession() {
  const stored = urlSessionId() || readStoredSessionId();
  if (!stored) {
    setIdleState();
    return false;
  }
  try {
    const res = await fetch("/api/session/" + encodeURIComponent(stored), {
      headers: { accept: "application/json" }
    });
    if (!res.ok) throw new Error("session " + stored + " -> " + res.status);
    const view = await res.json();
    applyServerView(view);
    if (view.phase !== "end" && !view.ended_at) {
      startTimer();
      startPolling();
      setSessionStatus("Restored the running session.");
    } else {
      stopTimer();
      stopPolling();
      setSessionStatus("The stored session has finished.");
    }
    return true;
  } catch (e) {
    const cached = readCachedSessionView();
    if (cached) {
      applyServerView(cached.view, { offline: true });
      stopPolling();
      startTimer();
      setSessionStatus("Server unreachable. Showing the session cached on this device at " +
        new Date(cached.at).toLocaleString() + ".");
      logLocal("session-restore-cached", { session: stored, at: cached.at });
      return true;
    }
    setIdleState();
    persistSessionId(null);
    updateStartLink();
    setStartState(false);
    setSessionStatus("The stored session is no longer on the server. Press Start for a fresh one.");
    logLocal("session-restore-failed", { session: stored, error: String(e) });
    return false;
  }
}

let teachHistory = [];

function sanitizeSvgClient(svg) {
  const doc = new DOMParser().parseFromString(svg, "image/svg+xml");
  const root = doc && doc.documentElement;
  if (!root || root.nodeName.toLowerCase() !== "svg") return null;
  const allowed = new Set(["svg","g","path","line","rect","circle","ellipse","polygon","polyline","text","tspan","defs","marker"]);
  const walk = (node) => {
    [...node.children].forEach((child) => {
      if (!allowed.has(child.nodeName.toLowerCase())) { child.remove(); return; }
      [...child.attributes].forEach((attr) => {
        const name = attr.name.toLowerCase();
        if (name.startsWith("on") || name === "href" || name === "xlink:href" || name === "style") {
          child.removeAttribute(attr.name);
        }
      });
      walk(child);
    });
  };
  walk(root);
  return root.outerHTML;
}

let boardSections = [];

function addBlock(block, target) {
  const board = target || document.getElementById("teach-board");
  if (!board) return;
  if (block.kind === "text") {
    const p = document.createElement("p");
    p.textContent = block.text;
    board.appendChild(p);
  } else if (block.kind === "latex") {
    const div = document.createElement("div");
    div.className = "board-latex";
    if (window.katex) {
      try { katex.render(block.latex, div, { displayMode: true, throwOnError: false }); }
      catch (e) { div.textContent = block.latex; }
    } else {
      div.textContent = block.latex;
    }
    board.appendChild(div);
  } else if (block.kind === "steps") {
    const ol = document.createElement("ol");
    (block.items || []).forEach((text) => {
      const li = document.createElement("li");
      li.textContent = text;
      ol.appendChild(li);
    });
    board.appendChild(ol);
  } else if (block.kind === "svg") {
    const clean = sanitizeSvgClient(block.svg || "");
    if (clean) {
      const wrap = document.createElement("div");
      wrap.className = "board-svg";
      wrap.innerHTML = clean;
      board.appendChild(wrap);
    }
  }
}

function setTeachStatus(message) {
  const status = document.getElementById("teach-status");
  if (status) status.textContent = message || "";
}

function saveExplanation(entry) {
  const saved = readStoredJson(SAVED_EXPLANATIONS_KEY, []);
  const list = Array.isArray(saved) ? saved : [];
  list.push(entry);
  const ok = writeStoredJson(SAVED_EXPLANATIONS_KEY, list.slice(-50));
  if (ok) {
    setTeachStatus("Explanation saved on this device. Vault write-back lands later.");
    logLocal("explanation-saved", { question: entry.question });
  } else {
    setTeachStatus("Could not save the explanation on this device: storage is full.");
  }
  return ok;
}

function addBoardSection(question, blocks, meta) {
  const board = document.getElementById("teach-board");
  if (!board) return null;
  const options = meta || {};
  const section = document.createElement("section");
  section.className = "board-section";
  const heading = document.createElement("h3");
  heading.textContent = question || "Explanation";
  section.appendChild(heading);

  const body = document.createElement("div");
  body.className = "board-body";
  section.appendChild(body);
  const reduce = prefersReducedMotion();
  (blocks || []).forEach((block, index) => {
    if (options.immediate || reduce) addBlock(block, body);
    else setTimeout(() => addBlock(block, body), index * 120);
  });

  const save = document.createElement("button");
  save.type = "button";
  save.className = "save-explanation";
  save.textContent = "Save explanation";
  save.title = "Saves on this device. Vault write-back lands later.";
  save.addEventListener("click", () => {
    const ok = saveExplanation({
      question: question || "Explanation",
      blocks: blocks || [],
      item_id: options.item_id || (sessionState.item ? sessionState.item.id : null),
      at: new Date().toISOString()
    });
    if (ok) {
      save.disabled = true;
      save.textContent = "Saved on this device";
    }
  });
  section.appendChild(save);

  board.appendChild(section);
  if (section.scrollIntoView) {
    try {
      section.scrollIntoView({ behavior: prefersReducedMotion() ? "auto" : "smooth", block: "nearest" });
    } catch (e) { /* older webview */ }
  }
  return section;
}

async function teach() {
  const input = document.getElementById("teach-input");
  const status = document.getElementById("teach-status");
  const btn = document.getElementById("teach-btn");
  const question = ((input && input.value) || "").trim();
  if (!question) {
    if (status) status.textContent = "Type a question first.";
    return;
  }
  if (btn) btn.disabled = true;
  if (status) status.textContent = "Thinking...";
  try {
    const activeItem = sessionState.item;
    const answerText = lastPractised && activeItem && lastPractised.item.id === activeItem.id
      ? lastPractised.answer
      : lastAnswerText;
    const worked = answerText ? question + "\n\nMy answer was: " + answerText : question;
    const reply = await postJson("/api/teach", {
      question: worked,
      item_id: activeItem ? activeItem.id : null,
      history: teachHistory
    });
    const entry = {
      question,
      blocks: reply.blocks,
      at: new Date().toISOString(),
      item_id: activeItem ? activeItem.id : null
    };
    addBoardSection(entry.question, entry.blocks, entry);
    boardSections.push(entry);
    boardSections = boardSections.slice(-20);
    writeStoredJson(BOARD_CACHE_KEY, { at: entry.at, sections: boardSections });
    teachHistory.push({ role: "user", content: worked });
    teachHistory.push({
      role: "assistant",
      content: reply.blocks.map((b) => b.text || b.latex || (b.items || []).join("; ")).join("\n")
    });
    teachHistory = teachHistory.slice(-8);
    if (input) input.value = "";
    if (status) status.textContent = "";
  } catch (e) {
    if (status) status.textContent = "Tutor unavailable: " + e.message + ". The last board is still shown.";
  } finally {
    if (btn) btn.disabled = false;
  }
}

function fileToBase64(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = String(reader.result || "");
      resolve(result.includes(",") ? result.split(",")[1] : result);
    };
    reader.onerror = () => reject(new Error("could not read the photo"));
    reader.readAsDataURL(file);
  });
}

function renderGrade(attempt) {
  const box = document.getElementById("grade-feedback");
  if (!box) return;
  box.innerHTML = "";
  const verdict = document.createElement("p");
  const finalFull = attempt.final_marks_total > 0 && attempt.final_marks >= attempt.final_marks_total;
  const methodFull = attempt.method_marks_total > 0 && attempt.method_marks >= attempt.method_marks_total;
  let verdictText = "Not quite.";
  let verdictClass = "grade bad";
  if (attempt.correct && methodFull) {
    verdictText = "Correct, with working.";
    verdictClass = "grade ok";
  } else if (attempt.correct || finalFull) {
    verdictText = "Answer is right, but the working needed more.";
    verdictClass = "grade partial";
  }
  verdict.className = verdictClass;
  verdict.textContent = verdictText;
  box.appendChild(verdict);

  const marks = document.createElement("p");
  marks.className = "muted";
  marks.textContent =
    "Method " + attempt.method_marks + "/" + attempt.method_marks_total +
    " - final answer " + attempt.final_marks + "/" + attempt.final_marks_total +
    (attempt.degraded ? " (graded by a local fallback model, lower confidence)" : "");
  box.appendChild(marks);

  if (attempt.feedback) {
    const feedback = document.createElement("p");
    feedback.textContent = attempt.feedback;
    box.appendChild(feedback);
  }

  if (Array.isArray(attempt.error_tags) && attempt.error_tags.length > 0) {
    const tags = document.createElement("p");
    tags.className = "muted";
    tags.textContent = "Tags: " + attempt.error_tags.join(", ");
    box.appendChild(tags);
  }
}

let pendingPhoto = null;
let lastAnswerText = "";
let lastPractised = null;
let sessionStats = blankSessionStats();
let sessionStatsId = null;

function blankSessionStats() {
  return { graded: 0, items: [], method: 0, methodTotal: 0, final: 0, finalTotal: 0, hints: 0 };
}

function sessionStatsKey(id) {
  return "breadstudy-stats-" + id;
}

function loadSessionStats(id) {
  const stored = readStoredJson(sessionStatsKey(id), null);
  return Object.assign(blankSessionStats(), stored || {});
}

function saveSessionStats() {
  if (!sessionStatsId) return;
  writeStoredJson(sessionStatsKey(sessionStatsId), sessionStats);
}

function practisedExplainItem() {
  if (!lastPractised || !lastPractised.item) return null;
  return lastPractised.item;
}

function setSubmitLabel(label) {
  const btn = document.getElementById("submit-btn");
  if (btn) btn.textContent = label;
}

function photoTargetSize(width, height, maxEdge) {
  const edge = maxEdge || 1600;
  const longest = Math.max(width || 0, height || 0);
  if (!longest) return { width: 0, height: 0 };
  const scale = Math.min(1, edge / longest);
  return {
    width: Math.max(1, Math.round(width * scale)),
    height: Math.max(1, Math.round(height * scale))
  };
}

function loadPhotoBitmap(file) {
  if (window.createImageBitmap) {
    return window.createImageBitmap(file).catch(() => loadPhotoImage(file));
  }
  return loadPhotoImage(file);
}

function loadPhotoImage(file) {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      URL.revokeObjectURL(url);
      resolve(img);
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error("could not decode the photo"));
    };
    img.src = url;
  });
}

function canvasToJpeg(canvas, quality) {
  return new Promise((resolve) => {
    if (canvas.toBlob) {
      canvas.toBlob((blob) => resolve(blob), "image/jpeg", quality);
      return;
    }
    try {
      const data = canvas.toDataURL("image/jpeg", quality);
      const raw = atob(data.split(",")[1] || "");
      const bytes = new Uint8Array(raw.length);
      for (let i = 0; i < raw.length; i += 1) bytes[i] = raw.charCodeAt(i);
      resolve(new Blob([bytes], { type: "image/jpeg" }));
    } catch (e) {
      resolve(null);
    }
  });
}

// Resize a phone photo to under 1 MB before upload. The long edge starts at
// 1600px and JPEG quality at 0.8; both step down until the blob fits.
async function resizePhoto(file, maxBytes) {
  const limit = maxBytes || 1000000;
  if (!file || !String(file.type || "").startsWith("image/")) {
    return { blob: file, mediaType: (file && file.type) || "image/jpeg", resized: false, error: "not an image" };
  }
  try {
    const bitmap = await loadPhotoBitmap(file);
    let width = bitmap.width || bitmap.naturalWidth;
    let height = bitmap.height || bitmap.naturalHeight;
    let size = photoTargetSize(width, height, 1600);
    let blob = null;
    for (let scalePass = 0; scalePass < 4; scalePass += 1) {
      const canvas = document.createElement("canvas");
      canvas.width = size.width;
      canvas.height = size.height;
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("canvas unavailable");
      ctx.drawImage(bitmap, 0, 0, size.width, size.height);
      for (let quality = 0.8; quality >= 0.5; quality -= 0.1) {
        blob = await canvasToJpeg(canvas, quality);
        if (blob && blob.size <= limit) break;
      }
      if (blob && blob.size <= limit) break;
      size = {
        width: Math.max(1, Math.round(size.width * 0.75)),
        height: Math.max(1, Math.round(size.height * 0.75))
      };
    }
    if (!blob) throw new Error("canvas produced no image");
    return { blob, mediaType: "image/jpeg", resized: true, width: size.width, height: size.height };
  } catch (e) {
    return {
      blob: file,
      mediaType: file.type || "image/jpeg",
      resized: false,
      error: String(e && e.message ? e.message : e)
    };
  }
}

function refreshAnswerPreview() {
  const latex = document.getElementById("latex-answer");
  const preview = document.getElementById("latex-preview");
  if (!latex || !preview) return;
  const raw = latex.value.trim();
  if (window.BreadstudyScratch && raw) {
    window.BreadstudyScratch.previewInto("latex-preview", raw);
  } else {
    preview.textContent = raw ? "Typed answer: " + raw : "Typed answer preview appears here.";
  }
}

function resetAttemptState() {
  pendingPhoto = null;
  lastAnswerText = "";
  setSubmitLabel("Submit answer");
}

async function submitAnswer() {
  const status = document.getElementById("grade-feedback");
  const item = sessionState.item;
  if (status) status.textContent = "";
  if (!item) {
    if (status) {
      status.textContent = !sessionState.sessionId
        ? "Start a session first so there is an item to mark."
        : currentPhase() === "end"
          ? "This is the session summary. There is no question to mark here."
          : currentPhase() === "reflect"
            ? "Reflection has no right answer to mark. Type a note if you like, then press Next phase."
            : "This phase has no question to mark.";
    }
    return;
  }
  const latexEl = document.getElementById("latex-answer");
  const latex = ((latexEl && latexEl.value) || "").trim();
  const photoInput = document.getElementById("photo-input");
  const file = photoInput && photoInput.files && photoInput.files[0];

  if (!latex && !file && !pendingPhoto) {
    if (status) status.textContent = "Type an answer or attach a photo first.";
    return;
  }

  const btn = document.getElementById("submit-btn");
  if (btn) btn.disabled = true;
  sessionState.marking = true;
  if (status) status.textContent = "Marking...";

  let payload = null;
  try {
    if (pendingPhoto) {
      payload = {
        item_id: item.id,
        input_kind: "photo",
        photo_base64: pendingPhoto.base64,
        photo_media_type: pendingPhoto.mediaType,
        confirmed: true,
        confirmed_text: latex || pendingPhoto.transcription
      };
    } else if (file) {
      const resized = await resizePhoto(file);
      const photoStatus = document.getElementById("photo-status");
      if (resized.resized) {
        if (photoStatus) {
          photoStatus.textContent =
            "Photo resized to " + Math.round(resized.blob.size / 1024) + " KB (" +
            resized.width + "x" + resized.height + "). The original stays on the device.";
        }
      } else if (photoStatus) {
        photoStatus.textContent = "Could not resize this photo; sending it as-is (" +
          Math.round(file.size / 1024) + " KB).";
      }
      payload = {
        item_id: item.id,
        input_kind: "photo",
        photo_base64: await fileToBase64(resized.blob),
        photo_media_type: resized.mediaType
      };
      lastAnswerText = "";
    } else {
      payload = { item_id: item.id, input_kind: "latex", latex };
      lastAnswerText = latex;
    }

    const reply = await postJson("/api/attempts", payload);

    if (reply.status === "needs_confirmation") {
      const transcription = reply.transcription || "";
      if (payload.input_kind === "photo" && payload.photo_base64) {
        pendingPhoto = {
          base64: payload.photo_base64,
          mediaType: payload.photo_media_type || "image/jpeg",
          transcription
        };
      }
      if (latexEl) latexEl.value = transcription;
      refreshAnswerPreview();
      setSubmitLabel("Confirm and mark");
      if (status) {
        status.textContent =
          'I read "' + transcription + '" at ' + Math.round((reply.confidence || 0) * 100) +
          "% confidence. Fix the transcription in the answer box if needed, then press Confirm and mark.";
      }
      if (photoInput) photoInput.value = "";
      return;
    }

    pendingPhoto = null;
    setSubmitLabel("Submit answer");
    lastAnswerText = (reply.attempt && reply.attempt.given) || latex;
    lastPractised = {
      item: {
        id: item.id,
        skill_id: item.skill_id,
        strand: item.strand,
        prompt: item.prompt,
        hints: (item.hints || []).slice(),
        hint_count: item.hint_count,
        hint_unlocked: sessionState.hintsUnlocked
      },
      answer: lastAnswerText
    };
    if (latexEl && payload.input_kind === "latex") latexEl.value = "";
    renderGrade(reply.attempt);
    sessionStats.graded += 1;
    if (!sessionStats.items.includes(item.id)) sessionStats.items.push(item.id);
    sessionStats.method += Number(reply.attempt.method_marks) || 0;
    sessionStats.methodTotal += Number(reply.attempt.method_marks_total) || 0;
    sessionStats.final += Number(reply.attempt.final_marks) || 0;
    sessionStats.finalTotal += Number(reply.attempt.final_marks_total) || 0;
    saveSessionStats();
    renderChrome();
    logLocal("graded", { item: item.id, correct: reply.attempt.correct });
  } catch (e) {
    const networkError = e && (
      e.name === "TypeError" ||
      /Failed to fetch|NetworkError|Load failed|network/i.test(String(e.message || ""))
    );
    if (networkError && payload) {
      if (queueAttempt(payload)) {
        if (status) {
          status.textContent = "Saved on this device. The answer will be sent when the server is back.";
        }
        setSessionStatus("Offline: " + readAttemptQueue().length + " answer(s) waiting to send.");
        logLocal("attempt-queued", { item: item.id, kind: payload.input_kind });
      } else if (status) {
        status.textContent = "Could not mark that and could not save it offline. Free some storage and retry.";
      }
    } else if (status) {
      status.textContent = "Could not mark that: " + e.message +
        (pendingPhoto ? " The photo is still attached, so you can retry." : "");
    }
  } finally {
    sessionState.marking = false;
    if (btn) btn.disabled = false;
    renderChrome();
  }
}

function activePlan() {
  return Array.isArray(sessionState.plan) && sessionState.plan.length > 0
    ? sessionState.plan
    : DEFAULT_PLAN;
}

function currentPhase() {
  const plan = activePlan();
  const entry = plan[sessionState.phaseIndex];
  return (entry && entry.phase) || "end";
}

function renderPhases(rootId) {
  const root = document.getElementById(rootId);
  if (!root) return;
  root.innerHTML = "";
  const label = document.getElementById("phase-label");
  if (sessionState.idle) {
    if (label) label.textContent = "Ready";
    return;
  }
  const plan = activePlan();
  plan.forEach((entry, i) => {
    const phase = entry.phase;
    // Zero-duration phases are skipped by the server; do not draw a chip
    // that claims they were done. Always show the active phase, including end.
    if (Number(entry.planned_seconds) === 0 && i !== sessionState.phaseIndex) return;
    const el = document.createElement("span");
    const done = Boolean(entry.done) || i < sessionState.phaseIndex;
    el.className = "phase" + (i === sessionState.phaseIndex ? " active" : done ? " done" : "");
    el.textContent = phase;
    root.appendChild(el);
  });
  if (label) label.textContent = phaseLabel(currentPhase());
}

function fmtClock(totalSeconds) {
  const m = Math.floor(totalSeconds / 60);
  const s = totalSeconds % 60;
  return String(m).padStart(2, "0") + ":" + String(s).padStart(2, "0");
}

let lastTimerAnnouncement = null;

function renderTimer() {
  const el = document.getElementById("session-timer");
  if (!el) return;
  if (sessionState.idle) {
    el.textContent = "--:--";
    el.classList.remove("low");
    lastTimerAnnouncement = null;
    return;
  }
  const remaining = Math.max(0, sessionState.remaining);
  el.textContent = fmtClock(remaining);
  el.classList.toggle("low", remaining <= 60);
  // The visible clock ticks every second; announce only at meaningful points
  // so a screen reader is not reading a new number every second.
  const live = document.getElementById("timer-announce");
  if (!live) return;
  let message = null;
  if (remaining === 0) message = "Phase time is up.";
  else if (remaining <= 10) message = remaining + (remaining === 1 ? " second remaining." : " seconds remaining.");
  else if (remaining % 60 === 0) {
    const minutes = Math.round(remaining / 60);
    message = minutes + (minutes === 1 ? " minute remaining." : " minutes remaining.");
  }
  if (message && message !== lastTimerAnnouncement) {
    live.textContent = message;
    lastTimerAnnouncement = message;
  }
}

function startTimer() {
  stopTimer();
  if (sessionState.idle) return;
  sessionState.timerId = setInterval(() => {
    if (sessionState.remaining > 0) {
      sessionState.remaining -= 1;
      renderTimer();
      return;
    }
    // The server owns phase transitions. At zero, mirror it once instead of
    // calling /advance every second.
    stopTimer();
    renderTimer();
    syncSession();
  }, 1000);
}

function stopTimer() {
  if (sessionState.timerId) clearInterval(sessionState.timerId);
  sessionState.timerId = null;
}

function startPolling() {
  if (sessionState.pollId || sessionState.idle) return;
  sessionState.pollId = setInterval(syncSession, 12000);
}

function stopPolling() {
  if (sessionState.pollId) clearInterval(sessionState.pollId);
  sessionState.pollId = null;
}

async function syncSession() {
  if (!sessionState.serverMode || !sessionState.sessionId || sessionState.syncInFlight) return;
  sessionState.syncInFlight = true;
  try {
    const res = await fetch("/api/session/" + encodeURIComponent(sessionState.sessionId), {
      headers: { accept: "application/json" }
    });
    if (!res.ok) throw new Error("session " + sessionState.sessionId + " -> " + res.status);
    const view = await res.json();
    applyServerView(view);
  } catch (e) {
    logLocal("sync-failed", { error: String(e) });
  } finally {
    sessionState.syncInFlight = false;
  }
}

function setIdleState() {
  stopTimer();
  stopPolling();
  sessionState.idle = true;
  sessionState.serverMode = false;
  sessionState.sessionId = null;
  sessionState.plan = null;
  sessionState.item = null;
  sessionState.remaining = 0;
  sessionState.hintsUnlocked = 0;
  sessionState.hintTotal = 0;
  sessionState.phaseTotal = 0;
  sessionState.phaseRemaining = 0;
  sessionState.hintsUsed = 0;
  sessionState.phaseExhausted = false;
  lastPractised = null;
  renderPhases("phase-row");
  renderTimer();
  renderHints();
  renderItem();
  renderChrome();
  setStartState(false);
  setSessionStatus("");
}

async function nextQuestion() {
  if (!sessionState.serverMode || !sessionState.sessionId) {
    setSessionStatus("Start a session to work through its cached questions.");
    return;
  }
  if (currentPhase() === "end") {
    setSessionStatus("The session has ended. There is no next question.");
    return;
  }
  if (currentPhase() === "explain" && lastPractised) {
    setSessionStatus("Explain stays on the question you just practised. Ask the tutor on the board.");
    return;
  }
  const btn = document.getElementById("next-question-btn");
  if (btn) btn.disabled = true;
  const bottom = document.getElementById("bottom-next-btn");
  if (bottom) bottom.disabled = true;
  const before = sessionState.item ? sessionState.item.id : null;
  try {
    const view = await postJson("/api/session/" + sessionState.sessionId + "/next-item", {});
    applyServerView(view);
    const after = sessionState.item ? sessionState.item.id : null;
    if (!after || after === before) {
      sessionState.phaseExhausted = true;
      sessionState.remaining = view.phase_remaining_seconds || 0;
      setSessionStatus("No more cached questions in this phase. Use Next phase when you are ready.");
    } else {
      sessionState.phaseExhausted = false;
      setSessionStatus("Loaded the next cached question.");
    }
    logLocal("next-item", { phase: currentPhase(), item: after, changed: after !== before });
  } catch (e) {
    setSessionStatus("Could not load the next question: " + e.message);
    logLocal("next-item-failed", { error: String(e) });
  } finally {
    updateActionButtons();
  }
}

async function advancePhase() {
  if (sessionState.idle) {
    setSessionStatus("Start a session before moving phases.");
    return;
  }
  if (currentPhase() === "end") {
    setSessionStatus("The session has ended. Advancing is a no-op.");
    return;
  }
  if (sessionState.serverMode && sessionState.sessionId) {
    if (currentPhase() === "end") {
      setSessionStatus("The session has ended.");
      return;
    }
    if (!sessionState.phaseExhausted && CONTENT_PHASES.has(currentPhase()) && sessionState.item &&
        !(currentPhase() === "explain" && lastPractised)) {
      const before = sessionState.item.id;
      try {
        const view = await postJson("/api/session/" + sessionState.sessionId + "/next-item", {});
        const after = view.item ? view.item.id : null;
        if (after && after !== before) {
          applyServerView(view);
          setSessionStatus("A cached question was still waiting in this phase. Work it, then press Next phase again.");
          return;
        }
        sessionState.phaseExhausted = true;
      } catch (e) {
        logLocal("next-item-failed", { error: String(e) });
      }
    }
    try {
      const view = await postJson("/api/session/" + sessionState.sessionId + "/advance", {});
      applyServerView(view);
      return;
    } catch (e) {
      logLocal("advance-failed", { error: String(e) });
    }
  }
  const plan = activePlan();
  let nextIndex = sessionState.phaseIndex + 1;
  while (nextIndex < plan.length && Number(plan[nextIndex].planned_seconds) === 0) nextIndex += 1;
  if (nextIndex < plan.length) {
    sessionState.phaseIndex = nextIndex;
    const phase = currentPhase();
    sessionState.remaining = Number(plan[nextIndex].planned_seconds) || TEMPLATE_20[phase] || 60;
    renderPhases("phase-row");
    renderTimer();
    renderChrome();
    logLocal("phase", { phase });
  } else {
    stopTimer();
  }
}

function renderHints() {
  const root = document.getElementById("hint-list");
  if (!root) return;
  root.innerHTML = "";
  const item = sessionState.item || (sessionState.sessionId ? null : SAMPLE_ITEM);
  const btn = document.getElementById("hint-btn");
  const meta = document.getElementById("hint-meta");
  if (sessionState.idle) {
    if (meta) meta.textContent = "Start a session to load the hint ladder.";
    if (btn) btn.disabled = true;
    return;
  }
  if (!item) {
    if (meta) meta.textContent = sessionState.sessionId
      ? "No hints in this phase."
      : "Start a session to load the hint ladder.";
    if (btn) btn.disabled = true;
    return;
  }
  const ladder = Array.isArray(item.hints) ? item.hints : [];
  const total = Math.max(Number(item.hint_count) || 0, Number(sessionState.hintTotal) || 0, ladder.length);
  const unlocked = Math.max(0, Math.min(
    Number.isFinite(sessionState.hintsUnlocked) ? sessionState.hintsUnlocked : sessionState.hintsUsed,
    total
  ));
  if (total === 0) {
    const empty = document.createElement("p");
    empty.className = "muted";
    empty.textContent = "This item has no hint ladder. Ask the tutor on the board instead.";
    root.appendChild(empty);
  }
  for (let idx = 0; idx < total; idx += 1) {
    const div = document.createElement("div");
    const isUnlocked = idx < unlocked;
    div.className = "hint" + (isUnlocked ? "" : " locked");
    if (isUnlocked) {
      const text = ladder[idx];
      div.textContent = text
        ? "Hint " + (idx + 1) + ": " + text
        : "Hint " + (idx + 1) + ": revealed. Reload the session to fetch its text.";
    } else {
      div.textContent = "Hint " + (idx + 1) + " of " + total + ": locked";
    }
    root.appendChild(div);
  }
  if (btn) btn.disabled = total === 0 || unlocked >= total;
  if (meta) meta.textContent = "Hints used: " + unlocked + " of " + total;
}

async function useHint() {
  if (sessionState.serverMode && sessionState.sessionId) {
    try {
      const outcome = await postJson(
        "/api/session/" + sessionState.sessionId + "/hint",
        {}
      );
      if (outcome.session) applyServerView(outcome.session);
      const unlocked = outcome.session && outcome.session.item && Number.isFinite(outcome.session.item.hint_unlocked)
        ? Number(outcome.session.item.hint_unlocked)
        : Number(outcome.hints_used);
      if (Number.isFinite(unlocked)) {
        sessionState.hintsUnlocked = unlocked;
        sessionState.hintsUsed = unlocked;
      }
      if (outcome.hint && sessionState.item) {
        const idx = Math.max(0, (Number(outcome.hints_used) || 1) - 1);
        if (!sessionState.item.hints) sessionState.item.hints = [];
        sessionState.item.hints[idx] = outcome.hint;
        sessionState.item.hint_count = Math.max(
          Number(sessionState.item.hint_count) || 0,
          sessionState.item.hints.length
        );
      }
      renderHints();
      sessionStats.hints += 1;
      saveSessionStats();
      logLocal("hint", { session: sessionState.sessionId, count: sessionState.hintsUnlocked, source: "server" });
      return;
    } catch (e) {
      logLocal("hint-failed", { error: String(e) });
    }
  }
  const item = sessionState.item || SAMPLE_ITEM;
  const total = Math.max(item.hints.length, sessionState.hintTotal || 0);
  if (sessionState.hintsUnlocked < total) {
    sessionState.hintsUnlocked += 1;
    sessionState.hintsUsed = sessionState.hintsUnlocked;
    renderHints();
    sessionStats.hints += 1;
    saveSessionStats();
    logLocal("hint", { item: item.id, count: sessionState.hintsUnlocked });
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

function insertIntoAnswer(text, cursorBack) {
  const latex = document.getElementById("latex-answer");
  if (!latex) return;
  const value = latex.value || "";
  const start = Number.isFinite(latex.selectionStart) ? latex.selectionStart : value.length;
  const end = Number.isFinite(latex.selectionEnd) ? latex.selectionEnd : value.length;
  latex.value = value.slice(0, start) + text + value.slice(end);
  const caret = start + text.length - (Number(cursorBack) || 0);
  try {
    latex.setSelectionRange(caret, caret);
  } catch (e) { /* some embedded webviews do not expose selection */ }
  latex.focus();
  refreshAnswerPreview();
}

const ANSWER_SYMBOLS = {
  "sym-pi": { text: "\\pi " },
  "sym-sqrt": { text: "\\sqrt{}", cursorBack: 1 },
  "sym-power": { text: "^{2}" },
  "sym-times": { text: "\\times " },
  "sym-divide": { text: "\\div " },
  "sym-frac": { text: "\\frac{}{}", cursorBack: 3 },
  "sym-leq": { text: "\\leq " },
  "sym-geq": { text: "\\geq " },
  "sym-deg": { text: "^{\\circ}" }
};

function bindAnswerInputs() {
  const latex = document.getElementById("latex-answer");
  if (latex) latex.addEventListener("input", refreshAnswerPreview);
  Object.keys(ANSWER_SYMBOLS).forEach((id) => {
    const btn = document.getElementById(id);
    if (!btn) return;
    const symbol = ANSWER_SYMBOLS[id];
    btn.addEventListener("click", () => insertIntoAnswer(symbol.text, symbol.cursorBack));
  });
  const photo = document.getElementById("photo-input");
  const photoStatus = document.getElementById("photo-status");
  if (photo) {
    photo.addEventListener("change", () => {
      pendingPhoto = null;
      setSubmitLabel("Submit answer");
      if (photo.files && photo.files.length > 0) {
        if (photoStatus) photoStatus.textContent = "Photo attached: " + photo.files[0].name + ". It will be resized before upload.";
        logLocal("photo-input", { name: photo.files[0].name, bytes: photo.files[0].size });
      }
    });
  }
}

function primaryAction() {
  const state = primaryActionState();
  if (state.action === "start") return startSession("school_day_20");
  if (state.action === "next") return nextQuestion();
  if (state.action === "advance") return advancePhase();
  return undefined;
}

function bindSessionControls() {
  const start20 = document.getElementById("start-20-btn");
  if (start20) start20.addEventListener("click", () => startSession("school_day_20"));
  const start40 = document.getElementById("start-40-btn");
  if (start40) start40.addEventListener("click", () => startSession("weekend_40"));
  const next = document.getElementById("next-phase-btn");
  if (next) next.addEventListener("click", advancePhase);
  ["next-question-btn", "bottom-next-btn"].forEach((id) => {
    const button = document.getElementById(id);
    if (button) button.addEventListener("click", primaryAction);
  });
  const again20 = document.getElementById("again-20-btn");
  if (again20) again20.addEventListener("click", () => startSession("school_day_20"));
  const again40 = document.getElementById("again-40-btn");
  if (again40) again40.addEventListener("click", () => startSession("weekend_40"));
  const hintBtn = document.getElementById("hint-btn");
  if (hintBtn) hintBtn.addEventListener("click", useHint);
  const teachBtn = document.getElementById("teach-btn");
  if (teachBtn) teachBtn.addEventListener("click", teach);
  const submitBtn = document.getElementById("submit-btn");
  if (submitBtn) submitBtn.addEventListener("click", submitAnswer);
  const teachInput = document.getElementById("teach-input");
  if (teachInput) {
    teachInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
        event.preventDefault();
        teach();
      }
    });
  }
  const skip = document.getElementById("skip-btn");
  if (skip) skip.addEventListener("click", skipItem);
}

async function skipItem() {
  const reason = window.prompt("One-line skip reason (required):", "");
  if (!reason || !reason.trim()) {
    setSessionStatus("Skip needs a one-line reason. Nothing was skipped.");
    return;
  }
  if (!sessionState.serverMode || !sessionState.sessionId) {
    setSessionStatus("Skip needs the session server. Nothing was skipped.");
    return;
  }
  const trimmed = reason.trim();
  const beforePhase = currentPhase();
  const beforeItem = sessionState.item ? sessionState.item.id : null;
  const btn = document.getElementById("skip-btn");
  if (btn) btn.disabled = true;
  try {
    const outcome = await postJson("/api/session/" + sessionState.sessionId + "/skip", { reason: trimmed });
    if (outcome.session) applyServerView(outcome.session);
    const samePhase = currentPhase() === beforePhase;
    const sameItem = sessionState.item ? sessionState.item.id : null;
    if (samePhase && sameItem === beforeItem) {
      // Older servers advanced the phase without moving within it. Pull the
      // next cached item so a skip still skips one question.
      const view = await postJson("/api/session/" + sessionState.sessionId + "/next-item", {});
      applyServerView(view);
    }
    const weekly = Number.isFinite(outcome.weekly_skip_count)
      ? " Weekly skips: " + outcome.weekly_skip_count + "."
      : "";
    setSessionStatus(
      "Skipped: " + trimmed + "." + weekly +
      (samePhase ? "" : " The server moved the session to " + currentPhase() + ".")
    );
    logLocal("skip", { reason: trimmed, weekly_skip_count: outcome.weekly_skip_count, phase: currentPhase() });
  } catch (e) {
    setSessionStatus("Could not record the skip: " + e.message);
    logLocal("skip-failed", { reason: trimmed, error: String(e) });
  } finally {
    if (btn) btn.disabled = false;
  }
}

function localDayKey(date) {
  const pad = (n) => String(n).padStart(2, "0");
  return date.getFullYear() + "-" + pad(date.getMonth() + 1) + "-" + pad(date.getDate());
}

function computeSessionStreak(log) {
  const starts = (Array.isArray(log) ? log : []).filter((entry) => entry.kind === "session-start" && entry.at);
  if (starts.length === 0) return 0;
  const days = new Set(starts.map((entry) => localDayKey(new Date(entry.at))));
  const cursor = new Date();
  if (!days.has(localDayKey(cursor))) {
    cursor.setDate(cursor.getDate() - 1);
    if (!days.has(localDayKey(cursor))) return 0;
  }
  let streak = 0;
  while (days.has(localDayKey(cursor))) {
    streak += 1;
    cursor.setDate(cursor.getDate() - 1);
  }
  return streak;
}

function updateStartLink() {
  const startLink = document.getElementById("start-session-link");
  if (!startLink) return;
  const active = readStoredSessionId() || urlSessionId();
  startLink.textContent = active ? "Resume session" : "Start 20 min";
  startLink.setAttribute("aria-label", active ? "Resume the session already in progress" : "Start a new 20 minute session");
}

function weakRow(skill) {
  const row = document.createElement("a");
  row.className = "weak-row";
  row.href = "./skill.html?skill=" + encodeURIComponent(skill.skill_id);
  row.title = skill.name + " - " + Math.round((Number(skill.mastery) || 0) * 100) + "% mastery";
  const top = document.createElement("span");
  top.className = "weak-top";
  const name = document.createElement("span");
  name.className = "weak-name";
  name.textContent = skill.name;
  const pct = Math.round((Number(skill.mastery) || 0) * 100);
  const value = document.createElement("span");
  value.className = "weak-pct";
  value.textContent = pct + "%";
  top.appendChild(name);
  top.appendChild(value);
  row.appendChild(top);

  const bar = document.createElement("span");
  bar.className = "mastery-bar";
  bar.setAttribute("role", "progressbar");
  bar.setAttribute("aria-valuemin", "0");
  bar.setAttribute("aria-valuemax", "100");
  bar.setAttribute("aria-valuenow", String(pct));
  bar.setAttribute("aria-label", skill.name + " mastery");
  const fill = document.createElement("span");
  fill.className = "mastery-fill";
  fill.style.width = Math.max(0, Math.min(100, pct)) + "%";
  bar.appendChild(fill);
  row.appendChild(bar);
  return row;
}

async function renderDashboard() {
  updateStartLink();
  const dateEl = document.getElementById("dash-date");
  if (dateEl) {
    dateEl.textContent = new Date().toLocaleDateString([], { weekday: "short", day: "numeric", month: "short" });
  }
  const skills = await fetchSkills();
  const el = document.getElementById("dash-skills");
  const skillList = document.getElementById("dash-skill-list");
  if (el) {
    if (skills.length === 0) {
      el.textContent = "No skills indexed yet. Run breadstudy-seed against the vault.";
      if (skillList) skillList.innerHTML = "";
    } else {
      const strands = [...new Set(skills.map((s) => s.strand).filter(Boolean))].sort();
      el.textContent = skills.length + " skills" + (strands.length ? " in " + strands.length + " strands" : "");
      if (skillList) {
        skillList.innerHTML = "";
        skills.forEach((skill) => {
          const li = document.createElement("li");
          const link = document.createElement("a");
          link.className = "skill-link";
          link.href = "./skill.html?skill=" + encodeURIComponent(skill.id);
          link.title = skill.name + (skill.strand ? " - " + skill.strand : "");
          const name = document.createElement("span");
          name.className = "skill-name";
          name.textContent = skill.name;
          const strand = document.createElement("span");
          strand.className = "skill-strand";
          strand.textContent = skill.strand || "";
          link.appendChild(name);
          link.appendChild(strand);
          li.appendChild(link);
          skillList.appendChild(li);
        });
      }
    }
  }

  let log = [];
  try {
    log = JSON.parse(localStorage.getItem("breadstudy-log") || "[]");
  } catch (e) { /* local history is best effort */ }
  if (!Array.isArray(log)) log = [];
  const starts = log.filter((entry) => entry.kind === "session-start");

  const streakEl = document.getElementById("dash-streak");
  if (streakEl) {
    const streak = computeSessionStreak(log);
    streakEl.textContent = streak + (streak === 1 ? " day" : " days");
    if (streak === 0) streakEl.title = "No consecutive session days recorded on this device yet.";
  }
  const recentEl = document.getElementById("dash-recent");
  if (recentEl) {
    recentEl.textContent = starts.length
      ? starts.length + " session" + (starts.length === 1 ? "" : "s") + " on this device"
      : "No sessions yet";
  }

  let due = null;
  try {
    const response = await fetch("/api/plan/due", { headers: { accept: "application/json" } });
    if (response.ok) {
      const parsed = await response.json();
      if (Array.isArray(parsed)) due = parsed;
    }
  } catch (e) { /* planner unavailable, fall back to local history */ }

  const today = document.getElementById("dash-today");
  if (today) {
    if (!due) {
      today.textContent = "The planner is unavailable right now. A session still uses cached content.";
    } else {
      const now = new Date();
      const reviewed = due.filter((skill) => Number(skill.reviews) > 0);
      const dueNow = reviewed.filter((skill) => skill.due_at && new Date(skill.due_at) <= now).length;
      if (dueNow > 0) {
        today.textContent = dueNow + " skill" + (dueNow === 1 ? " is" : "s are") +
          " due for review today.";
      } else if (reviewed.length > 0) {
        today.textContent = "No reviews are due right now. The planner will pick the next test-relevant skill.";
      } else {
        today.textContent = "No mastery yet. Start a session and weak spots appear here.";
      }
    }
  }

  const weak = document.getElementById("dash-weak");
  if (weak) {
    weak.innerHTML = "";
    if (!due) {
      weak.appendChild(elNode("li", "muted", "Mastery data is unavailable right now."));
    } else {
      const reviewed = due
        .filter((skill) => Number(skill.reviews) > 0)
        .sort((a, b) => (Number(a.mastery) || 0) - (Number(b.mastery) || 0));
      if (reviewed.length === 0) {
        weak.appendChild(elNode("li", "muted", "No graded reviews yet. Finish a session to see weak spots."));
      } else {
        reviewed.slice(0, 3).forEach((skill) => {
          const li = document.createElement("li");
          li.appendChild(weakRow(skill));
          weak.appendChild(li);
        });
      }
    }
  }

  const history = document.getElementById("dash-history");
  if (history) {
    history.innerHTML = "";
    if (starts.length > 0) {
      starts.slice(-3).reverse().forEach((entry) => {
        const li = document.createElement("li");
        const when = new Date(entry.at);
        const template = (entry.payload && entry.payload.template === "weekend_40") ? "40 min" : "20 min";
        li.textContent = when.toLocaleDateString([], { day: "numeric", month: "short" }) +
          ", " + when.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) +
          " - " + template;
        history.appendChild(li);
      });
    }
  }
}

function elNode(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function renderOfflineBanner() {
  const banner = document.getElementById("offline-banner");
  if (!banner) return;
  const online = !(typeof navigator !== "undefined" && navigator.onLine === false);
  const waiting = readAttemptQueue().length;
  banner.hidden = online;
  if (!online) {
    banner.textContent = "Offline: showing content cached on this device." +
      (waiting ? " " + waiting + " answer(s) will send when you reconnect." : "");
  }
}

function updateOnlineStatus() {
  renderOfflineBanner();
  const online = !(typeof navigator !== "undefined" && navigator.onLine === false);
  if (online) flushAttemptQueue();
}

function restoreCachedBoard() {
  const cached = readStoredJson(BOARD_CACHE_KEY, null);
  if (!cached) return;
  if (Array.isArray(cached.sections) && cached.sections.length > 0) {
    cached.sections.forEach((section) => {
      addBoardSection(section.question, section.blocks, {
        at: section.at,
        item_id: section.item_id,
        immediate: true
      });
      boardSections.push(section);
    });
  } else if (Array.isArray(cached.blocks) && cached.blocks.length > 0) {
    const section = { question: "Cached explanation", blocks: cached.blocks, at: cached.at };
    addBoardSection(section.question, section.blocks, { at: cached.at, immediate: true });
    boardSections.push(section);
  } else {
    return;
  }
  setTeachStatus("Showing the board cached at " + new Date(cached.at).toLocaleString() +
    ". Saved explanations stay on this device; vault write-back lands later.");
}

async function flushAttemptQueue() {
  if (sessionState.flushInFlight) return;
  const queue = readAttemptQueue();
  if (queue.length === 0) return;
  sessionState.flushInFlight = true;
  const remaining = [];
  let sent = 0;
  for (const entry of queue) {
    try {
      const reply = await postJson("/api/attempts", entry.payload);
      if (reply.status === "needs_confirmation") {
        const current = sessionState.item;
        if (!current || current.id !== entry.payload.item_id) {
          remaining.push(entry);
          continue;
        }
        const transcription = reply.transcription || "";
        if (entry.payload.input_kind === "photo" && entry.payload.photo_base64) {
          pendingPhoto = {
            base64: entry.payload.photo_base64,
            mediaType: entry.payload.photo_media_type || "image/jpeg",
            transcription
          };
        }
        const latexEl = document.getElementById("latex-answer");
        if (latexEl) latexEl.value = transcription;
        refreshAnswerPreview();
        setSubmitLabel("Confirm and mark");
        setSessionStatus("A saved photo needs confirmation. Check the transcription, then press Confirm and mark.");
        continue;
      }
      sent += 1;
      if (reply.attempt && sessionState.item && sessionState.item.id === entry.payload.item_id) {
        renderGrade(reply.attempt);
      }
      logLocal("graded", { item: entry.payload.item_id, correct: reply.attempt.correct, queued: true });
    } catch (e) {
      remaining.push(entry);
    }
  }
  writeAttemptQueue(remaining);
  sessionState.flushInFlight = false;
  renderOfflineBanner();
  if (sent > 0) {
    setSessionStatus("Sent " + sent + " saved answer(s)." +
      (remaining.length ? " " + remaining.length + " still waiting." : ""));
  }
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
  renderItem();
  renderChrome();
  setStartState(false);
  bindSessionControls();
  bindAnswerInputs();
  renderDashboard();
  registerSW();
  restoreSession();
  updateOnlineStatus();
  if (typeof navigator !== "undefined" && navigator.onLine === false) restoreCachedBoard();
  if (window.addEventListener) {
    window.addEventListener("online", updateOnlineStatus);
    window.addEventListener("offline", renderOfflineBanner);
  }
});

window.BreadstudySession = {
  advancePhase,
  useHint,
  fetchSkills,
  SAMPLE_ITEM,
  restoreSession,
  persistSessionId,
  submitAnswer,
  resizePhoto,
  photoTargetSize,
  nextQuestion,
  primaryAction,
  primaryActionState,
  renderChrome,
  startSession,
  syncSession,
  setIdleState,
  skipItem,
  updateOnlineStatus,
  flushAttemptQueue,
  readAttemptQueue,
  restoreCachedBoard,
  computeSessionStreak,
  renderDashboard
};
