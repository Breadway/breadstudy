/* Interactive teaching mode for breadstudy.
 *
 * The board contract is the same one used by the session page: text, latex,
 * steps and svg blocks. This file adds a topic picker, sequential reveal with
 * speech, sentence-level audio pipelining and one interrupt path shared by
 * every speech backend.
 */
(() => {
  "use strict";

  const KOKORO_VOICES = [
    { id: "bm_george", label: "George (Australian male)" },
    { id: "af_heart", label: "Heart (American female)" },
    { id: "bf_alice", label: "Alice (British female)" },
    { id: "af_bella", label: "Bella (American female)" },
    { id: "am_michael", label: "Michael (American male)" },
    { id: "bf_emma", label: "Emma (British female)" },
    { id: "bm_lewis", label: "Lewis (British male)" }
  ];

  const GEMINI_VOICES = [
    { id: "Kore", label: "Kore" },
    { id: "Puck", label: "Puck" },
    { id: "Charon", label: "Charon" },
    { id: "Fenrir", label: "Fenrir" },
    { id: "Aoede", label: "Aoede" },
    { id: "Leda", label: "Leda" },
    { id: "Orus", label: "Orus" },
    { id: "Zephyr", label: "Zephyr" }
  ];

  const BREEZE_VOICES = [{ id: "patient-coach", label: "Patient coach (Breeze GPU)" }];

  const SUGGESTED_GROUPS = [
    {
      title: "Volume and surface area",
      subtitle: "Prisms, cylinders, cones, spheres and surface area",
      ids: ["volume-prism-cylinder", "volume-cone-pyramid-sphere", "surface-area", "unit-conversion-scaling"],
      seed: "Teach me volume and surface area. Start with the core formulas, then work one example."
    },
    {
      title: "Trigonometry",
      subtitle: "SOH CAH TOA, right triangles and Pythagoras",
      ids: ["trigonometry", "pythagoras-3d", "angle-properties", "circle-theorems"],
      seed: "Teach me trigonometry from the start, with one worked example."
    },
    {
      title: "Quadratics",
      subtitle: "Expanding, factorising, solving and graphing",
      ids: ["quadratics", "nonlinear-graphs", "linear-relationships"],
      seed: "Teach me quadratics. Start with factorising and solving, then show one example."
    },
    {
      title: "Probability",
      subtitle: "Simple, conditional and two-way tables",
      ids: ["probability"],
      seed: "Teach me probability from the start, with one worked example."
    },
    {
      title: "Statistics",
      subtitle: "Bivariate data, association and boxplots",
      ids: ["statistics"],
      seed: "Teach me statistics from the start, with one worked example."
    }
  ];

  const STORAGE = {
    muted: "breadstudy.teach.muted",
    rate: "breadstudy.teach.rate",
    backend: "breadstudy.teach.backend",
    voices: "breadstudy.teach.voices.v2"
  };

  const STRAND_ORDER = ["Number", "Algebra", "Geometry", "Probability", "Statistics"];

  /* Animation blocks: frame dwell times are clamped by the server too, but
   * the client must survive an old or hand-written payload as well. */
  const ANIMATION_DEFAULT_MS = 900;
  const ANIMATION_MIN_MS = 200;
  const ANIMATION_MAX_MS = 3000;
  const ANIMATION_MAX_FRAMES = 8;

  const $ = (id) => document.getElementById(id);

  /** Block element to animation handle, so reveal can drive the right one. */
  const animationHandles = new WeakMap();

  function readJson(key, fallback) {
    try {
      const raw = localStorage.getItem(key);
      return raw === null ? fallback : JSON.parse(raw);
    } catch (error) {
      return fallback;
    }
  }

  function writeJson(key, value) {
    try {
      localStorage.setItem(key, JSON.stringify(value));
    } catch (error) {
      /* Storage can be full or disabled. Settings are still applied in memory. */
    }
  }

  const state = {
    skills: [],
    topic: null,
    turns: [],
    busy: false,
    revealToken: 0,
    teachAbort: null,
    muted: readJson(STORAGE.muted, false) === true,
    rate: clampRate(readJson(STORAGE.rate, 1)),
    backend: readJson(STORAGE.backend, "auto"),
    voices: readJson(STORAGE.voices, { kokoro: "bm_george", gemini: "Kore", browser: "" }),
    browserVoices: [],
    browserSpeech: typeof window !== "undefined" && "speechSynthesis" in window,
    lastSpoken: null,
    servedBackend: null,
    servedVoice: null,
    serverDownUntil: 0,
    urlOpened: false,
    audioEl: null,
    audioResolve: null,
    utteranceResolve: null,
    ttsControllers: new Set(),
    animations: new Set(),
    speechHooksBound: false
  };

  function clampRate(value) {
    const number = Number(value);
    if (!Number.isFinite(number)) return 1;
    return Math.min(2, Math.max(0.5, number));
  }

  function clampFrameDuration(value) {
    const number = Number(value);
    if (!Number.isFinite(number) || number <= 0) return ANIMATION_DEFAULT_MS;
    return Math.min(ANIMATION_MAX_MS, Math.max(ANIMATION_MIN_MS, Math.round(number)));
  }

  /** Normalise an animation block into at most 8 playable frames. */
  function animationFrames(block) {
    const raw = block && Array.isArray(block.frames) ? block.frames : [];
    return raw
      .map((frame) => ({
        svg: String((frame && frame.svg) || ""),
        caption: String((frame && frame.caption) || "").trim(),
        duration: clampFrameDuration(frame && frame.duration_ms)
      }))
      .filter((frame) => frame.svg)
      .slice(0, ANIMATION_MAX_FRAMES);
  }

  function prefersReducedMotion() {
    try {
      return typeof window.matchMedia === "function" &&
        window.matchMedia("(prefers-reduced-motion: reduce)").matches === true;
    } catch (error) {
      return false;
    }
  }

  /* Pure frame state machine. No timers and no DOM so the widget, the tests
   * and any future player can drive it by handing it elapsed milliseconds.
   * `loop` keeps motion going while a block is being spoken; without it the
   * sequence stops on the final frame. */
  function createFramePlayer(frames, options) {
    const opts = options || {};
    const list = frames.length
      ? frames
      : [{ svg: "", caption: "", duration: ANIMATION_DEFAULT_MS }];
    let index = opts.reducedMotion ? list.length - 1 : 0;
    let remaining = list[index].duration;
    let playing = false;
    let finished = false;
    const listeners = new Set();
    const notify = () => listeners.forEach((listener) => listener(api));

    const clampIndex = (value) => Math.min(list.length - 1, Math.max(0, value));

    const api = {
      get length() { return list.length; },
      get index() { return index; },
      get playing() { return playing; },
      get finished() { return finished; },
      get current() { return list[index]; },
      get remainingMs() { return remaining; },
      onFrame(listener) {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      play() {
        if (list.length < 2 || playing) return;
        finished = false;
        if (remaining <= 0) remaining = list[index].duration;
        playing = true;
        notify();
      },
      pause() {
        if (!playing) return;
        playing = false;
        notify();
      },
      /** Consume playback time. Returns true when the visible frame changed. */
      consume(ms) {
        if (!playing) return false;
        let moved = false;
        let budget = Math.max(0, Number(ms) || 0);
        while (budget > 0 && playing) {
          if (budget < remaining) {
            remaining -= budget;
            budget = 0;
            break;
          }
          budget -= remaining;
          moved = true;
          if (index < list.length - 1) {
            index += 1;
            remaining = list[index].duration;
          } else if (opts.loop) {
            index = 0;
            remaining = list[index].duration;
          } else {
            remaining = 0;
            playing = false;
            finished = true;
          }
        }
        if (moved) notify();
        return moved;
      },
      next() {
        index = clampIndex(index + 1);
        remaining = list[index].duration;
        finished = false;
        notify();
      },
      previous() {
        index = clampIndex(index - 1);
        remaining = list[index].duration;
        finished = false;
        notify();
      },
      seek(value) {
        index = clampIndex(Math.round(Number(value) || 0));
        remaining = list[index].duration;
        finished = false;
        notify();
      },
      replay() {
        index = 0;
        remaining = list[0].duration;
        finished = false;
        notify();
      },
      /** Stop on the last frame without clearing the sequence. */
      finish() {
        index = list.length - 1;
        remaining = list[index].duration;
        playing = false;
        finished = true;
        notify();
      }
    };
    return api;
  }

  function pauseAnimations() {
    state.animations.forEach((animation) => animation.pause());
  }

  function resumeAnimations() {
    state.animations.forEach((animation) => animation.resume());
  }

  function stopAnimations() {
    state.animations.forEach((animation) => animation.stop());
  }

  function delay(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  function setStatus(message) {
    const node = $("board-status");
    if (node) node.textContent = message || "";
  }

  function setPickerStatus(message) {
    const node = $("picker-status");
    if (node) node.textContent = message || "";
  }

  function setTtsStatus(message) {
    const node = $("tts-status");
    if (node) node.textContent = message || "";
  }

  function setAsking(asking) {
    const button = $("ask-btn");
    if (button) button.disabled = asking;
  }

  /* ------------------------------------------------------------------ */
  /* Speech backends                                                     */
  /* ------------------------------------------------------------------ */

  function updateTtsBadge() {
    const badge = $("tts-badge");
    if (!badge) return;
    if (state.servedBackend) {
      badge.textContent = state.servedVoice
        ? `Voice: ${state.servedBackend} (${state.servedVoice})`
        : `Voice: ${state.servedBackend}`;
      return;
    }
    badge.textContent = state.backend === "auto" ? "Voice: auto" : `Voice: ${state.backend}`;
  }

  function backendVoiceKey() {
    return state.backend === "auto" ? "kokoro" : state.backend;
  }

  function selectedServerVoice() {
    const key = backendVoiceKey();
    if (key === "breeze") return null;
    return state.voices[key] || null;
  }

  function findBrowserVoice() {
    if (!state.browserVoices.length) return null;
    const stored = state.voices.browser;
    let voice = stored ? state.browserVoices.find((item) => item.voiceURI === stored) : null;
    if (voice) return voice;
    const preferences = [/^en[-_]AU/i, /^en[-_]GB/i, /^en[-_]US/i, /^en/i];
    for (const pattern of preferences) {
      voice = state.browserVoices.find((item) => pattern.test(item.lang || ""));
      if (voice) return voice;
    }
    return state.browserVoices[0] || null;
  }

  function loadBrowserVoices() {
    if (!state.browserSpeech) {
      setTtsStatus("This browser has no speech synthesis. Server voices still work; if they are unavailable the board will reveal without sound.");
      return;
    }
    const refresh = () => {
      const voices = window.speechSynthesis.getVoices() || [];
      state.browserVoices = voices.slice().sort((a, b) => {
        const lang = String(a.lang || "").localeCompare(String(b.lang || ""));
        return lang !== 0 ? lang : String(a.name || "").localeCompare(String(b.name || ""));
      });
      if (state.backend === "browser" || state.backend === "auto") populateVoices();
      if (state.servedBackend && state.servedBackend !== "browser") return;
      if (state.browserVoices.length) {
        setTtsStatus("Browser speech is ready. Auto tries the server voices first.");
      } else {
        setTtsStatus("No system speech voices are available. The board will still write; install a voice to hear browser speech.");
      }
    };
    if (typeof window.speechSynthesis.addEventListener === "function") {
      window.speechSynthesis.addEventListener("voiceschanged", refresh);
      if (!state.speechHooksBound) {
        state.speechHooksBound = true;
        /* Keep diagrams in step with browser speech. */
        window.speechSynthesis.addEventListener("pause", pauseAnimations);
        window.speechSynthesis.addEventListener("resume", resumeAnimations);
      }
    }
    refresh();
  }

  function populateVoices() {
    const select = $("voice-select");
    if (!select) return;
    const key = backendVoiceKey();
    let options = KOKORO_VOICES;
    if (key === "gemini") options = GEMINI_VOICES;
    else if (key === "breeze") options = BREEZE_VOICES;
    else if (key === "browser") {
      options = state.browserVoices.map((voice) => ({
        id: voice.voiceURI,
        label: `${voice.name || "System voice"}${voice.lang ? ` (${voice.lang})` : ""}`
      }));
    }
    select.innerHTML = "";
    if (!options.length) {
      const option = document.createElement("option");
      option.textContent = "No voices available";
      option.value = "";
      option.disabled = true;
      select.appendChild(option);
      select.disabled = true;
      return;
    }
    select.disabled = false;
    for (const voice of options) {
      const option = document.createElement("option");
      option.value = voice.id;
      option.textContent = voice.label;
      select.appendChild(option);
    }
    const stored = state.voices[key];
    if (stored && options.some((voice) => voice.id === stored)) {
      select.value = stored;
    } else {
      select.value = options[0].id;
      state.voices[key] = options[0].id;
      writeJson(STORAGE.voices, state.voices);
    }
  }

  function applySettings() {
    const mute = $("mute-btn");
    const rate = $("rate-range");
    const output = $("rate-output");
    const backend = $("backend-select");
    if (mute) {
      mute.textContent = state.muted ? "Unmute" : "Mute";
      mute.setAttribute("aria-pressed", String(state.muted));
    }
    if (rate) rate.value = String(state.rate);
    if (output) output.textContent = `${state.rate.toFixed(2).replace(/0$/, "")}x`;
    if (backend) backend.value = state.backend;
    updateBackendNote();
    populateVoices();
    updateTtsBadge();
  }

  function updateBackendNote() {
    if (state.backend === "breeze") {
      setTtsStatus("Breeze uses the GPU. It only works when the breeze broker is already running, and it may pause or contend with the llama router. It never unloads the router automatically.");
    } else if (state.backend === "gemini") {
      setTtsStatus("Gemini is the quality option and needs a server key. If it is not configured, browser speech takes over.");
    } else if (state.backend === "kokoro") {
      setTtsStatus("Kokoro runs locally on the server CPU. The first sentence may wait while the model warms.");
    } else if (state.backend === "browser") {
      setTtsStatus(state.browserSpeech ? "Browser speech is always available and needs no server." : "This browser has no speech synthesis. The board will reveal without sound.");
    } else {
      setTtsStatus("Auto uses Kokoro, then Gemini, then Breeze, then the browser voice.");
    }
  }

  function cancelAudio() {
    stopAnimations();
    for (const controller of state.ttsControllers) {
      try { controller.abort(); } catch (error) { /* already finished */ }
    }
    state.ttsControllers.clear();
    if (state.audioEl) {
      try {
        state.audioEl.pause();
        state.audioEl.removeAttribute("src");
        state.audioEl.load();
      } catch (error) { /* element already gone */ }
      state.audioEl = null;
    }
    if (state.audioResolve) {
      const resolve = state.audioResolve;
      state.audioResolve = null;
      resolve();
    }
    if (state.browserSpeech) {
      try { window.speechSynthesis.cancel(); } catch (error) { /* no-op */ }
    }
    if (state.utteranceResolve) {
      const resolve = state.utteranceResolve;
      state.utteranceResolve = null;
      resolve();
    }
  }

  function clearSpeaking() {
    document.querySelectorAll(".board-block.speaking").forEach((node) => node.classList.remove("speaking"));
  }

  function noteServerFailure(error) {
    state.serverDownUntil = Date.now() + 30000;
    const message = error && error.message ? error.message : String(error);
    setTtsStatus(`Server speech unavailable (${message}). Using the browser voice.`);
    state.servedBackend = "browser";
    state.servedVoice = null;
    updateTtsBadge();
  }

  async function fetchTts(text) {
    const controller = new AbortController();
    state.ttsControllers.add(controller);
    const body = { input: text, speed: state.rate };
    if (state.backend !== "auto") body.backend = state.backend;
    const voice = selectedServerVoice();
    if (voice) body.voice = voice;
    try {
      const response = await fetch("/api/tts", {
        method: "POST",
        headers: { "Content-Type": "application/json", Accept: "audio/wav" },
        body: JSON.stringify(body),
        signal: controller.signal
      });
      if (!response.ok) {
        let message = `HTTP ${response.status}`;
        try {
          const payload = await response.json();
          if (payload && payload.error) message = payload.error;
        } catch (error) { /* not JSON */ }
        throw new Error(message);
      }
      const served = response.headers.get("x-breadstudy-tts-backend");
      if (served) {
        state.servedBackend = served;
        state.servedVoice = response.headers.get("x-breadstudy-tts-voice");
        updateTtsBadge();
        setTtsStatus(state.servedVoice ? `Server voice: ${served} (${state.servedVoice}).` : `Server voice: ${served}.`);
      }
      const blob = await response.blob();
      if (!blob.size) throw new Error("the server returned an empty clip");
      return blob;
    } finally {
      state.ttsControllers.delete(controller);
    }
  }

  function playAudioBlob(blob, token) {
    return new Promise((resolve, reject) => {
      const url = URL.createObjectURL(blob);
      const audio = new Audio(url);
      audio.preload = "auto";
      state.audioEl = audio;
      let settled = false;
      const cleanup = () => {
        if (settled) return;
        settled = true;
        audio.onended = null;
        audio.onerror = null;
        if (state.audioEl === audio) state.audioEl = null;
        if (state.audioResolve === finish) state.audioResolve = null;
        URL.revokeObjectURL(url);
      };
      const finish = () => {
        cleanup();
        resolve();
      };
      state.audioResolve = finish;
      audio.onended = finish;
      audio.onerror = () => {
        cleanup();
        reject(new Error("the browser could not play the speech clip"));
      };
      /* Audio pause happens when speech buffers or is interrupted; the frame
       * player follows the same pause/resume so motion and voice stay synced. */
      audio.addEventListener("pause", pauseAnimations);
      audio.addEventListener("playing", resumeAnimations);
      if (token !== state.revealToken) {
        cleanup();
        resolve();
        return;
      }
      audio.play().catch((error) => {
        cleanup();
        reject(error);
      });
    });
  }

  function speakUtterance(text, token) {
    return new Promise((resolve) => {
      if (!state.browserSpeech) {
        resolve();
        return;
      }
      const utterance = new SpeechSynthesisUtterance(text);
      utterance.rate = state.rate;
      utterance.pitch = 1;
      const voice = findBrowserVoice();
      if (voice) utterance.voice = voice;
      let settled = false;
      let watchdog = null;
      const finish = () => {
        if (settled) return;
        settled = true;
        if (watchdog) clearTimeout(watchdog);
        utterance.onend = null;
        utterance.onerror = null;
        if (state.utteranceResolve === finish) state.utteranceResolve = null;
        resolve();
      };
      state.utteranceResolve = finish;
      utterance.onend = finish;
      utterance.onerror = finish;
      const words = String(text || "").split(/\s+/).filter(Boolean).length;
      const estimateMs = Math.max(2500, (words / (2.6 * state.rate)) * 1000 + 1200);
      watchdog = setTimeout(finish, Math.min(30000, estimateMs * 2.2));
      if (token !== state.revealToken) {
        finish();
        return;
      }
      try {
        window.speechSynthesis.speak(utterance);
      } catch (error) {
        finish();
      }
    });
  }

  async function speakBrowserPipeline(segments, token) {
    const voice = findBrowserVoice();
    state.servedBackend = "browser";
    state.servedVoice = voice ? voice.name : "system";
    updateTtsBadge();
    if (!state.browserSpeech) {
      setTtsStatus("No speech synthesis is available. The board will reveal without sound.");
      for (const segment of segments) {
        if (token !== state.revealToken) return;
        await delay(Math.min(1600, 350 + segment.length * 10));
      }
      return;
    }
    if (!state.browserVoices.length) {
      setTtsStatus("No system speech voices are available. The board will reveal without sound.");
      for (const segment of segments) {
        if (token !== state.revealToken) return;
        await delay(Math.min(1600, 350 + segment.length * 10));
      }
      return;
    }
    for (const segment of segments) {
      if (token !== state.revealToken) return;
      await speakUtterance(segment, token);
    }
  }

  async function speakServerPipeline(segments, token) {
    let prefetch = fetchTts(segments[0]);
    try {
      for (let index = 0; index < segments.length; index += 1) {
        if (token !== state.revealToken) return true;
        const blob = await prefetch;
        if (token !== state.revealToken) return true;
        if (index + 1 < segments.length) {
          prefetch = fetchTts(segments[index + 1]);
          prefetch.catch(() => { /* handled when the loop reaches it */ });
        }
        await playAudioBlob(blob, token);
      }
      return true;
    } catch (error) {
      if (error && error.name === "AbortError") return true;
      if (token !== state.revealToken) return true;
      noteServerFailure(error);
      return false;
    }
  }

  async function speakBlock(block, token, options) {
    const opts = options || {};
    const segments = segmentsForBlock(block).slice(0, 16);
    if (!segments.length) {
      await delay(350);
      return;
    }
    if (state.muted && !opts.force) {
      await delay(Math.min(1800, 400 + segments.join(" ").length * 8));
      return;
    }
    if (opts.el) opts.el.classList.add("speaking");
    try {
      const serverDown = Date.now() < state.serverDownUntil;
      if (state.backend !== "browser" && !serverDown) {
        const served = await speakServerPipeline(segments, token);
        if (served) return;
      }
      await speakBrowserPipeline(segments, token);
    } finally {
      if (opts.el) opts.el.classList.remove("speaking");
    }
  }

  /* ------------------------------------------------------------------ */
  /* Speech text                                                         */
  /* ------------------------------------------------------------------ */

  function normalizeLatex(latex) {
    let text = String(latex || "");
    text = text
      .replace(/\\frac\s*\{([^{}]*)\}\s*\{([^{}]*)\}/g, "$1 over $2")
      .replace(/\\sqrt\s*\{([^{}]*)\}/g, " square root of $1 ")
      .replace(/\\sqrt/g, " square root ")
      .replace(/\\pi/g, " pi ")
      .replace(/\\theta/g, " theta ")
      .replace(/\\alpha/g, " alpha ")
      .replace(/\\beta/g, " beta ")
      .replace(/\\times|\\cdot/g, " times ")
      .replace(/\\div/g, " divided by ")
      .replace(/\\pm/g, " plus or minus ")
      .replace(/\\le|\\leq|<=/g, " is less than or equal to ")
      .replace(/\\ge|\\geq|>=/g, " is greater than or equal to ")
      .replace(/\^\\circ/g, " degrees ")
      .replace(/\^\s*2\b/g, " squared ")
      .replace(/\^\s*3\b/g, " cubed ")
      .replace(/\^\s*\{?([a-zA-Z0-9]+)\}?/g, " to the power of $1 ")
      .replace(/\\left|\\right/g, " ")
      .replace(/\\[a-zA-Z]+/g, " ")
      .replace(/[{}]/g, " ")
      .replace(/π/g, " pi ")
      .replace(/×/g, " times ")
      .replace(/÷/g, " divided by ")
      .replace(/²/g, " squared ")
      .replace(/³/g, " cubed ")
      .replace(/√/g, " square root of ")
      .replace(/∞/g, " infinity ")
      .replace(/≈/g, " approximately ")
      .replace(/[=]/g, " equals ")
      .replace(/[+]/g, " plus ")
      .replace(/(?<=\d)\s*-\s*(?=\d)/g, " minus ")
      .replace(/\*/g, " times ")
      .replace(/\//g, " over ");
    return text.replace(/\s+/g, " ").trim();
  }

  function normalizeSpeechText(text) {
    let value = String(text || "");
    value = value
      .replace(/\\\(|\\\)/g, " ")
      .replace(/\$/g, " ")
      .replace(/π/g, " pi ")
      .replace(/×/g, " times ")
      .replace(/÷/g, " divided by ")
      .replace(/²/g, " squared ")
      .replace(/³/g, " cubed ")
      .replace(/√/g, " square root of ")
      .replace(/∞/g, " infinity ")
      .replace(/≈/g, " approximately ");
    if (/\\[a-zA-Z]+/.test(value)) value = normalizeLatex(value);
    return value.replace(/\s+/g, " ").trim();
  }

  function splitSentences(text) {
    const clean = String(text || "").replace(/\s+/g, " ").trim();
    if (!clean) return [];
    const parts = clean.match(/[^.!?]+[.!?]+(?=\s|$)|[^.!?]+$/g) || [clean];
    return parts.map((part) => part.trim()).filter(Boolean);
  }

  function segmentsForBlock(block) {
    if (!block || !block.kind) return [];
    if (block.kind === "text") return splitSentences(normalizeSpeechText(block.text));
    if (block.kind === "latex") return splitSentences(normalizeLatex(block.latex));
    if (block.kind === "steps") {
      return (block.items || []).flatMap((item) => splitSentences(normalizeSpeechText(item)));
    }
    if (block.kind === "svg") {
      const labels = [];
      const pattern = /<text[^>]*>([\s\S]*?)<\/text>/gi;
      let match = pattern.exec(block.svg || "");
      while (match) {
        const label = match[1].replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();
        if (label) labels.push(label);
        match = pattern.exec(block.svg || "");
      }
      return [labels.length ? `Here is a diagram. Labels: ${labels.join(", ")}.` : "Here is a diagram for this idea."];
    }
    if (block.kind === "animation") {
      const frames = animationFrames(block);
      if (!frames.length) return [];
      const captions = frames.map((frame) => frame.caption).filter(Boolean);
      if (captions.length) {
        return [`Here is the animation: ${captions.join(". ")}.`];
      }
      const labels = [];
      const pattern = /<text[^>]*>([\s\S]*?)<\/text>/gi;
      frames.forEach((frame) => {
        let match = pattern.exec(frame.svg);
        while (match) {
          const label = match[1].replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();
          if (label && !labels.includes(label)) labels.push(label);
          match = pattern.exec(frame.svg);
        }
      });
      return [labels.length ? `Here is an animation. Labels: ${labels.join(", ")}.` : "Here is an animation for this idea."];
    }
    return [];
  }

  /* ------------------------------------------------------------------ */
  /* Board rendering                                                     */
  /* ------------------------------------------------------------------ */

  function sanitizeSvg(svg) {
    try {
      const doc = new DOMParser().parseFromString(String(svg || ""), "image/svg+xml");
      const root = doc && doc.documentElement;
      if (!root || root.nodeName.toLowerCase() !== "svg") return null;
      const allowed = new Set(["svg", "g", "path", "line", "rect", "circle", "ellipse", "polygon", "polyline", "text", "tspan", "defs", "marker"]);
      const walk = (node) => {
        [...node.children].forEach((child) => {
          if (!allowed.has(child.nodeName.toLowerCase())) {
            child.remove();
            return;
          }
          [...child.attributes].forEach((attribute) => {
            const name = attribute.name.toLowerCase();
            if (name.startsWith("on") || name === "href" || name === "xlink:href" || name === "style") {
              child.removeAttribute(attribute.name);
            }
          });
          walk(child);
        });
      };
      walk(root);
      return root.outerHTML;
    } catch (error) {
      return null;
    }
  }

  /* Fixed-height frame stage: every frame scales into the same box, so the
   * board never shifts when the picture changes. Controls stay usable with
   * mouse, touch and keyboard (native button and range semantics). */
  function buildAnimationWidget(block, host) {
    const frames = animationFrames(block);
    if (!frames.length) return null;
    const reducedMotion = prefersReducedMotion();
    const player = createFramePlayer(frames, { loop: true, reducedMotion });

    const wrapper = document.createElement("div");
    wrapper.className = "board-animation";
    wrapper.setAttribute("role", "group");
    wrapper.setAttribute("aria-label", "Animated diagram");

    const stage = document.createElement("div");
    stage.className = "board-animation-stage";
    const frameNode = document.createElement("div");
    frameNode.className = "board-svg board-animation-frame";
    stage.appendChild(frameNode);

    const caption = document.createElement("p");
    caption.className = "board-animation-caption";

    const controls = document.createElement("div");
    controls.className = "board-animation-controls";

    const makeButton = (label, text, extraClass) => {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = text;
      button.className = `ghost-btn animation-btn${extraClass ? ` ${extraClass}` : ""}`;
      button.setAttribute("aria-label", label);
      return button;
    };
    const prev = makeButton("Previous frame", "◀ Back");
    const playPause = makeButton("Pause animation", "Pause", "animation-play");
    const next = makeButton("Next frame", "Next ▶");
    const replay = makeButton("Replay animation", "Replay");

    const slider = document.createElement("input");
    slider.type = "range";
    slider.className = "animation-scrub";
    slider.min = "0";
    slider.max = String(frames.length - 1);
    slider.step = "1";
    slider.value = String(player.index);
    slider.setAttribute("aria-label", "Animation frame");
    slider.disabled = frames.length < 2;

    controls.appendChild(prev);
    controls.appendChild(playPause);
    controls.appendChild(next);
    controls.appendChild(replay);

    const controlRow = document.createElement("div");
    controlRow.className = "board-animation-scrub-row";
    controlRow.appendChild(slider);

    wrapper.appendChild(stage);
    wrapper.appendChild(caption);
    wrapper.appendChild(controls);
    wrapper.appendChild(controlRow);

    let timer = null;
    let oneShot = null;
    let tickedAt = 0;
    let speechPaused = false;
    let userPaused = reducedMotion;

    const clearTimer = () => {
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
    };
    const clearOneShot = () => {
      if (oneShot !== null) {
        clearTimeout(oneShot);
        oneShot = null;
      }
    };

    const schedule = () => {
      clearTimer();
      if (!player.playing) return;
      const wait = Math.max(40, player.remainingMs);
      tickedAt = Date.now();
      timer = setTimeout(() => {
        timer = null;
        const elapsed = Math.max(40, Date.now() - tickedAt);
        player.consume(elapsed);
        schedule();
      }, wait);
    };

    const render = () => {
      const frame = player.current;
      const clean = sanitizeSvg(frame.svg);
      if (clean) {
        frameNode.innerHTML = clean;
      } else {
        frameNode.textContent = "This animation frame could not be shown safely.";
      }
      const label = frame.caption || `Frame ${player.index + 1} of ${player.length}`;
      caption.textContent = label;
      const singleFrame = player.length < 2;
      slider.value = String(player.index);
      slider.setAttribute(
        "aria-valuetext",
        `${label} (frame ${player.index + 1} of ${player.length})`
      );
      playPause.textContent = player.playing ? "Pause" : "Play";
      playPause.setAttribute("aria-label", player.playing ? "Pause animation" : "Play animation");
      playPause.setAttribute("aria-pressed", String(player.playing));
      playPause.disabled = singleFrame;
      prev.disabled = singleFrame || player.index === 0;
      next.disabled = singleFrame || player.index === player.length - 1;
      replay.disabled = singleFrame;
    };

    player.onFrame(render);

    playPause.addEventListener("click", () => {
      if (player.playing) {
        userPaused = true;
        player.pause();
        clearTimer();
      } else {
        userPaused = false;
        player.play();
        schedule();
      }
    });
    prev.addEventListener("click", () => {
      userPaused = true;
      clearTimer();
      player.pause();
      player.previous();
    });
    next.addEventListener("click", () => {
      userPaused = true;
      clearTimer();
      player.pause();
      player.next();
    });
    replay.addEventListener("click", () => {
      userPaused = false;
      player.replay();
      player.play();
      schedule();
    });
    slider.addEventListener("input", () => {
      userPaused = true;
      player.seek(slider.value);
      if (player.playing) schedule();
    });

    const handle = {
      player,
      /** Total dwell for one full pass, so muted mode can show every frame. */
      get playbackMs() {
        return frames.reduce((sum, frame) => sum + frame.duration, 0);
      },
      begin(options) {
        const opts = options || {};
        clearTimer();
        clearOneShot();
        speechPaused = false;
        if (reducedMotion) {
          render();
          return;
        }
        userPaused = false;
        player.play();
        schedule();
        state.animations.add(handle);
        if (opts.speaking === false) {
          const total = frames.reduce((sum, frame) => sum + frame.duration, 0);
          oneShot = setTimeout(() => {
            oneShot = null;
            clearTimer();
            player.finish();
          }, total);
        }
      },
      /** Speech or the block ended: hold the final frame when it completed. */
      settle(completed) {
        clearTimer();
        clearOneShot();
        if (completed) player.finish();
        else player.pause();
        speechPaused = false;
        state.animations.delete(handle);
      },
      pause() {
        if (player.playing) {
          player.pause();
          clearTimer();
          speechPaused = true;
        }
      },
      resume() {
        if (!speechPaused || userPaused || reducedMotion) return;
        speechPaused = false;
        player.play();
        schedule();
      },
      stop() {
        clearTimer();
        clearOneShot();
        player.pause();
        state.animations.delete(handle);
      }
    };

    render();
    if (host) animationHandles.set(host, handle);
    return wrapper;
  }

  function renderBlock(block) {
    const wrapper = document.createElement("div");
    wrapper.className = `board-block board-block--${block.kind || "unknown"}`;
    if (block.kind === "text") {
      const paragraph = document.createElement("p");
      paragraph.textContent = block.text || "";
      wrapper.appendChild(paragraph);
    } else if (block.kind === "latex") {
      const latex = block.latex || block.text || "";
      const target = document.createElement("div");
      target.className = "board-latex";
      if (window.katex && typeof window.katex.render === "function") {
        try {
          window.katex.render(latex, target, { displayMode: true, throwOnError: false });
        } catch (error) {
          target.textContent = latex;
        }
      } else {
        const fallback = document.createElement("span");
        fallback.className = "katex-fallback";
        fallback.textContent = latex;
        target.appendChild(fallback);
      }
      wrapper.appendChild(target);
    } else if (block.kind === "steps") {
      const list = document.createElement("ol");
      (block.items || []).forEach((item) => {
        const entry = document.createElement("li");
        entry.textContent = item;
        list.appendChild(entry);
      });
      wrapper.appendChild(list);
    } else if (block.kind === "svg") {
      const clean = sanitizeSvg(block.svg || "");
      if (clean) {
        const holder = document.createElement("div");
        holder.className = "board-svg";
        holder.innerHTML = clean;
        wrapper.appendChild(holder);
      } else {
        const note = document.createElement("p");
        note.textContent = "The tutor sent a diagram that could not be shown safely.";
        wrapper.appendChild(note);
      }
    } else if (block.kind === "animation") {
      wrapper.classList.add("board-block--animation");
      const animation = buildAnimationWidget(block, wrapper);
      if (animation) {
        wrapper.appendChild(animation);
      } else {
        const note = document.createElement("p");
        note.textContent = "The tutor sent an animation that could not be shown safely.";
        wrapper.appendChild(note);
      }
    } else {
      const note = document.createElement("p");
      note.textContent = "The tutor sent a block this page does not understand.";
      wrapper.appendChild(note);
    }
    return wrapper;
  }

  function scrollBoardToEnd() {
    const board = $("teach-board");
    if (!board) return;
    const nearBottom = board.scrollHeight - board.scrollTop - board.clientHeight < 150;
    if (nearBottom || !board.dataset.started) {
      board.scrollTop = board.scrollHeight;
      board.dataset.started = "1";
    }
  }

  function createSection(question, heading) {
    const board = $("teach-board");
    const section = document.createElement("section");
    section.className = "board-section";
    const title = document.createElement("h2");
    title.textContent = heading || question;
    const body = document.createElement("div");
    body.className = "board-body";
    section.appendChild(title);
    section.appendChild(body);
    board.appendChild(section);
    scrollBoardToEnd();
    return { el: section, body, question, heading: heading || question };
  }

  function renderSectionError(body, question, error) {
    const box = document.createElement("div");
    box.className = "board-error";
    box.setAttribute("role", "alert");
    const message = document.createElement("p");
    const detail = error && error.message ? error.message : String(error);
    message.textContent = `The tutor could not answer: ${detail}`;
    const hint = document.createElement("p");
    hint.className = "muted";
    hint.textContent = "The board above is still yours. Try again, ask a smaller question, or press Stop and ask.";
    const retry = document.createElement("button");
    retry.type = "button";
    retry.textContent = "Try again";
    retry.addEventListener("click", () => {
      box.remove();
      askQuestion(question, { heading: state.topic ? state.topic.title : question });
    });
    box.appendChild(message);
    box.appendChild(hint);
    box.appendChild(retry);
    body.appendChild(box);
  }

  function blocksToPlain(blocks) {
    return (blocks || []).map((block) => {
      if (block.kind === "text") return block.text || "";
      if (block.kind === "latex") return block.latex || "";
      if (block.kind === "steps") return (block.items || []).join("; ");
      if (block.kind === "svg") return "[diagram]";
      if (block.kind === "animation") {
        const captions = animationFrames(block).map((frame) => frame.caption).filter(Boolean);
        return captions.length ? `[animation: ${captions.join("; ")}]` : "[animation]";
      }
      return "";
    }).filter(Boolean).join("\n");
  }

  async function waitForDwell(ms, token) {
    const end = Date.now() + Math.max(0, ms);
    while (Date.now() < end) {
      if (token !== state.revealToken) return false;
      await delay(80);
    }
    return token === state.revealToken;
  }

  async function revealBlocks(section, blocks, token) {
    const writing = section.body.querySelector(".writing");
    for (const block of blocks) {
      if (token !== state.revealToken) return;
      if (writing && writing.parentNode) writing.remove();
      const element = renderBlock(block);
      section.body.appendChild(element);
      state.lastSpoken = { block, el: element };
      scrollBoardToEnd();
      const animation = animationHandles.get(element);
      if (animation && state.muted) {
        /* No speech to follow: run one pass at the frame durations instead of
         * the short text dwell, so every frame is seen. */
        animation.begin({ speaking: false });
        await waitForDwell(animation.playbackMs, token);
        animation.settle(token === state.revealToken);
        continue;
      }
      if (animation) animation.begin({ speaking: true });
      try {
        await speakBlock(block, token, { el: element });
      } finally {
        if (animation) animation.settle(token === state.revealToken);
      }
    }
    if (token === state.revealToken) {
      const trailing = section.body.querySelector(".writing");
      if (trailing) trailing.remove();
      setStatus("");
    }
  }

  /* ------------------------------------------------------------------ */
  /* Teaching request                                                    */
  /* ------------------------------------------------------------------ */

  function buildTranscript() {
    return state.turns
      .map((turn) => `Student: ${turn.question}\nTutor: ${turn.answer}`)
      .join("\n\n")
      .slice(-2400);
  }

  async function askQuestion(question, options) {
    if (!state.topic) return;
    const opts = options || {};
    if (state.busy) interrupt();
    state.busy = true;
    setAsking(true);
    const section = createSection(question, opts.heading || question);
    const body = section.body;
    const writing = document.createElement("p");
    writing.className = "writing";
    writing.textContent = "Thinking. The tutor is writing on the board.";
    body.appendChild(writing);
    setStatus("Thinking...");
    const token = ++state.revealToken;
    const controller = new AbortController();
    state.teachAbort = controller;
    try {
      const response = await fetch("/api/teach", {
        method: "POST",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: JSON.stringify({
          question,
          skill_id: state.topic.skillId || null,
          transcript: buildTranscript()
        }),
        signal: controller.signal
      });
      if (token !== state.revealToken) return;
      if (!response.ok) {
        let message = `HTTP ${response.status}`;
        try {
          const payload = await response.json();
          if (payload && payload.error) message = payload.error;
        } catch (error) { /* not JSON */ }
        throw new Error(message);
      }
      const reply = await response.json();
      if (!reply || !Array.isArray(reply.blocks) || reply.blocks.length === 0) {
        throw new Error("the tutor returned an empty board");
      }
      if (reply.skill) {
        state.topic.skillName = reply.skill;
        const subtitle = $("topic-subtitle");
        if (subtitle) subtitle.textContent = `${state.topic.strand || ""} ${reply.skill}`.trim();
      }
      body.innerHTML = "";
      await revealBlocks(section, reply.blocks, token);
      if (token === state.revealToken) {
        state.turns.push({ question, answer: blocksToPlain(reply.blocks).slice(0, 800) });
        state.turns = state.turns.slice(-6);
      }
    } catch (error) {
      if (error && error.name === "AbortError") return;
      if (token !== state.revealToken) return;
      body.innerHTML = "";
      renderSectionError(body, question, error);
      setStatus("The tutor is unavailable. The last board is still shown.");
    } finally {
      if (state.teachAbort === controller) {
        state.teachAbort = null;
        state.busy = false;
        setAsking(false);
      }
    }
  }

  /* ------------------------------------------------------------------ */
  /* Interrupt and repeat                                                */
  /* ------------------------------------------------------------------ */

  function interrupt() {
    if (!state.topic) {
      const search = $("topic-search");
      if (search) search.focus();
      return;
    }
    state.revealToken += 1;
    if (state.teachAbort) {
      try { state.teachAbort.abort(); } catch (error) { /* already aborted */ }
      state.teachAbort = null;
    }
    state.busy = false;
    cancelAudio();
    clearSpeaking();
    setAsking(false);
    setStatus("Stopped. Ask what confused you, or press Repeat last.");
    setTtsStatus("Speech stopped. The board is still here.");
    const input = $("ask-input");
    if (input) input.focus();
  }

  async function repeatLast() {
    if (!state.lastSpoken) {
      setTtsStatus("Nothing has been spoken yet.");
      return;
    }
    state.revealToken += 1;
    if (state.teachAbort) {
      try { state.teachAbort.abort(); } catch (error) { /* already aborted */ }
      state.teachAbort = null;
    }
    state.busy = false;
    cancelAudio();
    clearSpeaking();
    setAsking(false);
    setStatus("Repeating the last block.");
    await speakBlock(state.lastSpoken.block, state.revealToken, {
      el: state.lastSpoken.el,
      force: true
    });
  }

  function cancelAll() {
    state.revealToken += 1;
    if (state.teachAbort) {
      try { state.teachAbort.abort(); } catch (error) { /* already aborted */ }
      state.teachAbort = null;
    }
    state.busy = false;
    cancelAudio();
    clearSpeaking();
    setAsking(false);
  }

  /* ------------------------------------------------------------------ */
  /* Topic picker                                                        */
  /* ------------------------------------------------------------------ */

  function skillMatches(skill, query) {
    if (!query) return true;
    const haystack = `${skill.name || ""} ${skill.strand || ""} ${skill.vault_note || ""}`.toLowerCase();
    return haystack.includes(query);
  }

  function groupSkill(group, skill) {
    return group.ids.includes(skill.id);
  }

  function primarySkillId(group) {
    return group.ids.find((id) => state.skills.some((skill) => skill.id === id)) || null;
  }

  function groupMatches(group, query) {
    if (!query) return true;
    const haystack = `${group.title} ${group.subtitle}`.toLowerCase();
    if (haystack.includes(query)) return true;
    return state.skills.some((skill) => groupSkill(group, skill) && skillMatches(skill, query));
  }

  function renderTopics() {
    const search = $("topic-search");
    const query = ((search && search.value) || "").trim().toLowerCase();
    const suggestedGrid = $("suggested-grid");
    const strandList = $("strand-list");
    const empty = $("picker-empty");
    if (!suggestedGrid || !strandList || !empty) return;
    suggestedGrid.innerHTML = "";
    strandList.innerHTML = "";

    let visible = 0;
    for (const group of SUGGESTED_GROUPS) {
      if (!groupMatches(group, query)) continue;
      visible += 1;
      const button = document.createElement("button");
      button.type = "button";
      button.className = "topic-card";
      const title = document.createElement("strong");
      title.textContent = group.title;
      const subtitle = document.createElement("span");
      subtitle.textContent = group.subtitle;
      button.appendChild(title);
      button.appendChild(subtitle);
      button.addEventListener("click", () => {
        const skillId = primarySkillId(group);
        openTopic({
          title: group.title,
          subtitle: "Suggested group",
          strand: "Suggested group",
          skillId,
          seed: group.seed
        });
      });
      suggestedGrid.appendChild(button);
    }

    const strands = [...new Set(state.skills.map((skill) => skill.strand))];
    strands.sort((a, b) => {
      const ai = STRAND_ORDER.indexOf(a);
      const bi = STRAND_ORDER.indexOf(b);
      if (ai !== -1 || bi !== -1) {
        if (ai === -1) return 1;
        if (bi === -1) return -1;
        return ai - bi;
      }
      return String(a).localeCompare(String(b));
    });

    for (const strand of strands) {
      const skills = state.skills.filter((skill) => skill.strand === strand && skillMatches(skill, query));
      if (!skills.length) continue;
      visible += skills.length;
      const block = document.createElement("section");
      block.className = "strand-block";
      const heading = document.createElement("h2");
      heading.textContent = strand;
      const grid = document.createElement("div");
      grid.className = "skill-grid";
      for (const skill of skills) {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "skill-card";
        const name = document.createElement("strong");
        name.textContent = skill.name;
        const note = document.createElement("span");
        note.textContent = skill.vault_note ? `Vault note: ${skill.vault_note}` : "Vault note not indexed";
        button.appendChild(name);
        button.appendChild(note);
        button.addEventListener("click", () => {
          openTopic({
            title: skill.name,
            subtitle: `${skill.strand || "Topic"} · ${skill.vault_note || "vault note"}`,
            strand: skill.strand,
            skillId: skill.id,
            seed: `Teach me ${skill.name} from the start, with one worked example.`
          });
        });
        grid.appendChild(button);
      }
      block.appendChild(heading);
      block.appendChild(grid);
      strandList.appendChild(block);
    }

    empty.hidden = visible > 0;
  }

  function defaultPrompts(topic) {
    return [
      `Teach me ${topic.title} from the start.`,
      `Show me a worked example for ${topic.title}.`,
      `What mistakes should I avoid in ${topic.title}?`
    ];
  }

  function renderSamplePrompts(prompts) {
    const holder = $("sample-prompts");
    if (!holder) return;
    holder.innerHTML = "";
    for (const prompt of prompts || []) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "prompt-chip";
      button.textContent = prompt;
      button.title = prompt;
      button.addEventListener("click", () => {
        if (state.busy) interrupt();
        const input = $("ask-input");
        if (input) input.value = prompt;
        const form = $("ask-form");
        if (form && typeof form.requestSubmit === "function") form.requestSubmit();
      });
      holder.appendChild(button);
    }
  }

  async function loadTeachContext(topic) {
    if (!topic.skillId) {
      renderSamplePrompts(defaultPrompts(topic));
      return;
    }
    try {
      const response = await fetch(`/api/skills/${encodeURIComponent(topic.skillId)}/teach-context`, {
        headers: { Accept: "application/json" }
      });
      if (!response.ok) {
        renderSamplePrompts(defaultPrompts(topic));
        return;
      }
      const context = await response.json();
      if (state.topic !== topic) return;
      const prompts = context && Array.isArray(context.sample_prompts) && context.sample_prompts.length
        ? context.sample_prompts
        : defaultPrompts(topic);
      renderSamplePrompts(prompts);
      if (context && context.vault_note) {
        const subtitle = $("topic-subtitle");
        if (subtitle) subtitle.textContent = `${context.strand || ""} · ${context.vault_note}`.trim();
      }
    } catch (error) {
      renderSamplePrompts(defaultPrompts(topic));
    }
  }

  function setTopicUrl(topic) {
    const params = new URLSearchParams();
    if (topic.skillId) params.set("skill", topic.skillId);
    params.set("topic", topic.title);
    try {
      history.replaceState(null, "", `teach.html?${params.toString()}`);
    } catch (error) { /* file URLs cannot replace state */ }
  }

  function openTopic(topic) {
    cancelAll();
    state.topic = topic;
    state.turns = [];
    state.lastSpoken = null;
    state.servedBackend = null;
    state.servedVoice = null;
    state.serverDownUntil = 0;
    const picker = $("picker-view");
    const board = $("board-view");
    const dock = $("control-dock");
    if (picker) picker.hidden = true;
    if (board) board.hidden = false;
    if (dock) dock.hidden = false;
    const title = $("topic-title");
    const subtitle = $("topic-subtitle");
    if (title) title.textContent = topic.title;
    if (subtitle) subtitle.textContent = topic.subtitle || topic.strand || "";
    const boardNode = $("teach-board");
    if (boardNode) {
      boardNode.innerHTML = "";
      boardNode.scrollTop = 0;
      delete boardNode.dataset.started;
    }
    const input = $("ask-input");
    if (input) input.value = "";
    setTopicUrl(topic);
    updateTtsBadge();
    renderSamplePrompts(defaultPrompts(topic));
    loadTeachContext(topic);
    askQuestion(topic.seed, { heading: topic.title, initial: true });
  }

  function backToPicker() {
    cancelAll();
    state.topic = null;
    state.turns = [];
    state.lastSpoken = null;
    const picker = $("picker-view");
    const board = $("board-view");
    const dock = $("control-dock");
    if (picker) picker.hidden = false;
    if (board) board.hidden = true;
    if (dock) dock.hidden = true;
    const boardNode = $("teach-board");
    if (boardNode) boardNode.innerHTML = "";
    try { history.replaceState(null, "", "teach.html"); } catch (error) { /* no-op */ }
    const search = $("topic-search");
    if (search) search.focus();
  }

  async function loadTopics() {
    setPickerStatus("Loading topics...");
    try {
      const response = await fetch("/api/skills", { headers: { Accept: "application/json" } });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const data = await response.json();
      if (!Array.isArray(data)) throw new Error("the skill list was not an array");
      state.skills = data.filter((skill) => skill && typeof skill.id === "string");
      renderTopics();
      setPickerStatus(state.skills.length ? "" : "No skills are indexed yet. The suggested groups still work.");
    } catch (error) {
      state.skills = [];
      renderTopics();
      setPickerStatus(`Could not load the skill list (${error.message}). The suggested groups still work.`);
    }
    openFromUrl();
  }

  function openFromUrl() {
    if (state.urlOpened) return;
    state.urlOpened = true;
    let params;
    try {
      params = new URLSearchParams(window.location.search);
    } catch (error) {
      return;
    }
    const skillId = params.get("skill");
    const topicName = params.get("topic");
    if (skillId) {
      const skill = state.skills.find((item) => item.id === skillId);
      if (skill) {
        openTopic({
          title: skill.name,
          subtitle: `${skill.strand || "Topic"} · ${skill.vault_note || "vault note"}`,
          strand: skill.strand,
          skillId: skill.id,
          seed: `Teach me ${skill.name} from the start, with one worked example.`
        });
        return;
      }
    }
    if (topicName) {
      const group = SUGGESTED_GROUPS.find((item) => item.title.toLowerCase() === topicName.toLowerCase());
      if (group) {
        openTopic({
          title: group.title,
          subtitle: "Suggested group",
          strand: "Suggested group",
          skillId: primarySkillId(group),
          seed: group.seed
        });
        return;
      }
      const skill = state.skills.find((item) => item.name.toLowerCase() === topicName.toLowerCase());
      if (skill) {
        openTopic({
          title: skill.name,
          subtitle: `${skill.strand || "Topic"} · ${skill.vault_note || "vault note"}`,
          strand: skill.strand,
          skillId: skill.id,
          seed: `Teach me ${skill.name} from the start, with one worked example.`
        });
        return;
      }
    }
    if (skillId && !state.skills.length) {
      setPickerStatus("That topic is not in the current index. Pick another topic.");
    }
  }

  /* ------------------------------------------------------------------ */
  /* Events                                                              */
  /* ------------------------------------------------------------------ */

  function isTypingTarget(target) {
    if (!target) return false;
    const tag = target.tagName;
    return target.isContentEditable || tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
  }

  function bindEvents() {
    const search = $("topic-search");
    if (search) search.addEventListener("input", renderTopics);

    const back = $("back-btn");
    if (back) back.addEventListener("click", backToPicker);

    const repeat = $("repeat-btn");
    if (repeat) repeat.addEventListener("click", repeatLast);

    const interruptBtn = $("interrupt-btn");
    if (interruptBtn) interruptBtn.addEventListener("click", interrupt);

    const mute = $("mute-btn");
    if (mute) {
      mute.addEventListener("click", () => {
        state.muted = !state.muted;
        writeJson(STORAGE.muted, state.muted);
        mute.textContent = state.muted ? "Unmute" : "Mute";
        mute.setAttribute("aria-pressed", String(state.muted));
        if (state.muted) {
          cancelAudio();
          clearSpeaking();
          setTtsStatus("Muted. The board still reveals block by block.");
        } else {
          setTtsStatus("Sound on. The next block will be spoken.");
        }
      });
    }

    const backend = $("backend-select");
    if (backend) {
      backend.addEventListener("change", () => {
        state.backend = backend.value;
        writeJson(STORAGE.backend, state.backend);
        state.serverDownUntil = 0;
        cancelAudio();
        clearSpeaking();
        updateBackendNote();
        populateVoices();
        updateTtsBadge();
      });
    }

    const voice = $("voice-select");
    if (voice) {
      voice.addEventListener("change", () => {
        const key = backendVoiceKey();
        state.voices[key] = voice.value;
        writeJson(STORAGE.voices, state.voices);
        const label = voice.options[voice.selectedIndex] ? voice.options[voice.selectedIndex].textContent : voice.value;
        setTtsStatus(`Voice set to ${label}.`);
      });
    }

    const rate = $("rate-range");
    if (rate) {
      rate.addEventListener("input", () => {
        state.rate = clampRate(rate.value);
        writeJson(STORAGE.rate, state.rate);
        const output = $("rate-output");
        if (output) output.textContent = `${state.rate.toFixed(2).replace(/0$/, "")}x`;
      });
    }

    const form = $("ask-form");
    if (form) {
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        const input = $("ask-input");
        const question = ((input && input.value) || "").trim();
        if (!question) {
          setTtsStatus("Type a question first.");
          if (input) input.focus();
          return;
        }
        if (!state.topic) {
          setTtsStatus("Pick a topic first.");
          return;
        }
        if (input) input.value = "";
        if (state.busy) interrupt();
        askQuestion(question, { heading: question });
      });
    }

    document.addEventListener("keydown", (event) => {
      const key = event.key;
      if (key !== " " && key !== "Escape" && event.code !== "Space") return;
      if (isTypingTarget(event.target)) {
        if (key === " " || event.code === "Space") return;
      }
      if ((key === " " || event.code === "Space") && event.target) {
        const tag = event.target.tagName;
        if (tag === "BUTTON" || tag === "A") return;
      }
      event.preventDefault();
      interrupt();
    });
  }

  function init() {
    bindEvents();
    applySettings();
    loadBrowserVoices();
    loadTopics();
  }

  window.BreadstudyTeach = {
    state,
    splitSentences,
    normalizeLatex,
    normalizeSpeechText,
    segmentsForBlock,
    animationFrames,
    clampFrameDuration,
    createFramePlayer,
    ANIMATION_DEFAULT_MS,
    ANIMATION_MIN_MS,
    ANIMATION_MAX_MS,
    ANIMATION_MAX_FRAMES,
    interrupt,
    repeatLast
  };

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
