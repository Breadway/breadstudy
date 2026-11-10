/* breadstudy session client. Works against stub GET /api/skills.
 * Expected skill shape (see API_CONTRACT.md):
 * [{ id, strand, name, vault_note }]
 * Session phases follow spec/session.md.
 */
const PHASES = ["prime", "recall", "practice", "explain", "reflect", "break", "end"];
const TEMPLATE_20 = { recall: 2 * 60, practice: 10 * 60, explain: 5 * 60, reflect: 3 * 60 };
const ACTIVE_SESSION_KEY = "breadstudy-active-session";

let sessionState = {
  phaseIndex: 0,
  remaining: TEMPLATE_20.recall,
  timerId: null,
  hintsUsed: 0,
  hintsUnlocked: 0,
  hintTotal: 0,
  item: null,
  sessionId: null,
  serverMode: false,
  phaseExhausted: false
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
  const start = document.getElementById("start-btn");
  if (!start) return;
  start.disabled = Boolean(active);
  start.textContent = active ? "Session running" : "Start 20 min";
  start.setAttribute("aria-disabled", active ? "true" : "false");
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

function applyServerView(view) {
  sessionState.sessionId = view.id;
  sessionState.serverMode = true;
  const index = typeof view.phase_index === "number" ? view.phase_index : PHASE_INDEX[view.phase];
  const nextIndex = index !== undefined ? index : 0;
  const phaseChanged = nextIndex !== sessionState.phaseIndex;
  sessionState.phaseIndex = nextIndex;
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
  renderPhases("phase-row");
  renderTimer();
  renderHints();
  renderItem();
  const finished = view.phase === "end" || Boolean(view.ended_at);
  const nextQuestion = document.getElementById("next-question-btn");
  if (nextQuestion) nextQuestion.disabled = finished || !sessionState.item || Boolean(explainItem);
  setStartState(!finished);
  if (finished) persistSessionId(null);
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
  } else {
    prompt.textContent = sessionState.sessionId
      ? "This phase has no question attached. Use the board, or move to the next phase."
      : "Press Start to pull a due item from the planner.";
  }
}

async function startSession() {
  if (sessionState.sessionId) {
    setSessionStatus("A session is already running. Finish or end it before starting another.");
    setStartState(true);
    return;
  }
  try {
    const view = await postJson("/api/session", { template: "school_day_20" });
    applyServerView(view);
    startTimer();
    logLocal("session-start", { template: "school_day_20", session: view.id });
  } catch (e) {
    sessionState.serverMode = false;
    sessionState.sessionId = null;
    sessionState.item = SAMPLE_ITEM;
    sessionState.phaseIndex = 0;
    sessionState.remaining = TEMPLATE_20.recall;
    sessionState.hintsUsed = 0;
    renderPhases("phase-row");
    renderTimer();
    renderHints();
    renderItem();
    startTimer();
    setSessionStatus("Working offline with the sample item. Attempts will queue until the server is back.");
    logLocal("session-start", { template: "20min", offline: true });
  }
}

async function restoreSession() {
  const stored = urlSessionId() || readStoredSessionId();
  if (!stored) return false;
  try {
    const res = await fetch("/api/session/" + encodeURIComponent(stored), {
      headers: { accept: "application/json" }
    });
    if (!res.ok) throw new Error("session " + stored + " -> " + res.status);
    const view = await res.json();
    applyServerView(view);
    if (view.phase !== "end" && !view.ended_at) {
      startTimer();
      setSessionStatus("Restored the running session.");
    } else {
      stopTimer();
      setSessionStatus("The stored session has finished.");
    }
    return true;
  } catch (e) {
    persistSessionId(null);
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

function addBlock(block) {
  const board = document.getElementById("teach-board");
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

async function teach() {
  const input = document.getElementById("teach-input");
  const status = document.getElementById("teach-status");
  const btn = document.getElementById("teach-btn");
  const board = document.getElementById("teach-board");
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
    if (board) board.innerHTML = "";
    reply.blocks.forEach((block, index) => setTimeout(() => addBlock(block), index * 140));
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
    if (status) status.textContent = "Start a session first so there is an item to mark.";
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
  if (status) status.textContent = "Marking...";

  try {
    let payload;
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
    logLocal("graded", { item: item.id, correct: reply.attempt.correct });
  } catch (e) {
    if (status) {
      status.textContent = "Could not mark that: " + e.message +
        (pendingPhoto ? " The photo is still attached, so you can retry." : "");
    }
  } finally {
    if (btn) btn.disabled = false;
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
    const current = document.getElementById("next-question-btn");
    if (current) current.disabled = currentPhase() === "end" || !sessionState.item;
  }
}

const CONTENT_PHASES = new Set(["recall", "practice", "explain"]);

async function advancePhase() {
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
  const item = sessionState.item || (sessionState.sessionId ? null : SAMPLE_ITEM);
  const btn = document.getElementById("hint-btn");
  const meta = document.getElementById("hint-meta");
  if (!item) {
    if (meta) meta.textContent = "Start a session to load the hint ladder.";
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

function bindAnswerInputs() {
  const latex = document.getElementById("latex-answer");
  if (latex) latex.addEventListener("input", refreshAnswerPreview);
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
      pendingPhoto = null;
      setSubmitLabel("Submit answer");
      if (photo.files && photo.files.length > 0) {
        if (photoStatus) photoStatus.textContent = "Photo attached: " + photo.files[0].name + ". It will be resized before upload.";
        logLocal("photo-input", { name: photo.files[0].name, bytes: photo.files[0].size });
      }
    });
  }
}

function bindSessionControls() {
  const start = document.getElementById("start-btn");
  if (start) start.addEventListener("click", startSession);
  const next = document.getElementById("next-phase-btn");
  if (next) next.addEventListener("click", advancePhase);
  const nextQuestionBtn = document.getElementById("next-question-btn");
  if (nextQuestionBtn) nextQuestionBtn.addEventListener("click", nextQuestion);
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
  if (skip) skip.addEventListener("click", () => {
    const reason = prompt("One-line skip reason (required):", "");
    if (reason && reason.trim()) {
      logLocal("skip", { reason: reason.trim() });
      advancePhase();
    }
  });
}

async function renderDashboard() {
  const skills = await fetchSkills();
  const el = document.getElementById("dash-skills");
  if (el) {
    if (skills.length === 0) {
      el.textContent = "No skills indexed yet. Run breadstudy-seed against the vault.";
    } else {
      const strands = [...new Set(skills.map((s) => s.strand))].sort();
      el.textContent = skills.length + " skills indexed across " + strands.join(", ") + ".";
    }
  }

  const today = document.getElementById("dash-today");
  if (today) today.textContent = "Today: 20 min school-day session. Start to pull a due skill from the planner.";

  const weak = document.getElementById("dash-weak");
  if (weak) {
    try {
      const due = await fetch("/api/plan/due", { headers: { accept: "application/json" } }).then((r) => r.json());
      weak.innerHTML = "";
      if (Array.isArray(due) && due.length > 0) {
        due.slice(0, 5).forEach((skill) => {
          const li = document.createElement("li");
          const pct = Math.round((skill.mastery || 0) * 100);
          li.textContent = skill.name + " (" + pct + "% mastery)";
          weak.appendChild(li);
        });
      } else {
        const li = document.createElement("li");
        li.textContent = "Nothing due yet. Finish a session to start tracking mastery.";
        weak.appendChild(li);
      }
    } catch (e) {
      weak.innerHTML = "";
      const li = document.createElement("li");
      li.textContent = "Due list unavailable right now.";
      weak.appendChild(li);
    }
  }

  const history = document.getElementById("dash-history");
  if (history) {
    try {
      const log = JSON.parse(localStorage.getItem("breadstudy-log") || "[]");
      history.innerHTML = "";
      if (log.length === 0) {
        const li = document.createElement("li");
        li.textContent = "No sessions recorded on this device yet.";
        history.appendChild(li);
      } else {
        log.filter((entry) => entry.kind === "session-start").slice(-5).reverse().forEach((entry) => {
          const li = document.createElement("li");
          li.textContent = new Date(entry.at).toLocaleString() + " - " + (entry.payload.template || "session");
          history.appendChild(li);
        });
      }
    } catch (e) { /* local history is best effort */ }
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
  setStartState(false);
  bindSessionControls();
  bindAnswerInputs();
  renderDashboard();
  registerSW();
  restoreSession();
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
  nextQuestion
};
