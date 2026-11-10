/* Behavioural checks for the teaching board frame player (STUDY-126).
 *
 * The player is deliberately timer-free, so this harness loads teach.js in a
 * VM with just enough browser surface to reach window.BreadstudyTeach and then
 * drives playback by hand. No third party dependencies.
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("../teach.js", import.meta.url), "utf8");
const noop = () => {};
const storage = new Map();

const sandbox = {
  console,
  setTimeout,
  clearTimeout,
  URL,
  localStorage: {
    getItem: (key) => (storage.has(key) ? storage.get(key) : null),
    setItem: (key, value) => storage.set(key, value),
    removeItem: (key) => storage.delete(key)
  },
  document: {
    readyState: "loading",
    addEventListener: noop,
    getElementById: () => null,
    querySelectorAll: () => []
  }
};
sandbox.window = sandbox;

vm.createContext(sandbox);
new vm.Script(source, { filename: "teach.js" }).runInContext(sandbox);

const teach = sandbox.window.BreadstudyTeach;
assert.ok(teach, "teach.js should expose window.BreadstudyTeach");

const svg = (width) => `<svg viewBox="0 0 10 10"><rect width="${width}" height="10"/></svg>`;
let checks = 0;
const ok = (condition, message) => {
  assert.ok(condition, message);
  checks += 1;
};

/* 1. Frames are capped, sanitised and duration-clamped. */
const block = {
  kind: "animation",
  frames: [
    { svg: svg(2), caption: " start ", duration_ms: 10 },
    { svg: svg(4), caption: "", duration_ms: 900 },
    { svg: "", caption: "no picture" },
    { svg: svg(6), duration_ms: 9000 },
    ...Array.from({ length: 8 }, () => ({ svg: svg(8), duration_ms: 500 }))
  ]
};
const frames = teach.animationFrames(block);
ok(frames.length === teach.ANIMATION_MAX_FRAMES, "caps frames at 8");
ok(frames[0].duration === teach.ANIMATION_MIN_MS, "clamps short durations up");
ok(frames[1].duration === 900, "keeps a sane duration");
ok(frames[0].caption === "start", "trims captions");
ok(frames.every((frame) => frame.svg.startsWith("<svg")), "drops frames with no svg");
ok(teach.clampFrameDuration(undefined) === teach.ANIMATION_DEFAULT_MS, "defaults to 900ms");
ok(teach.clampFrameDuration(99999) === teach.ANIMATION_MAX_MS, "clamps long durations down");

/* 2. Playback advances in order and loops while spoken. */
const list = teach.animationFrames({
  kind: "animation",
  frames: [
    { svg: svg(2), caption: "one", duration_ms: 400 },
    { svg: svg(4), caption: "two", duration_ms: 400 },
    { svg: svg(6), caption: "three", duration_ms: 400 }
  ]
});
let player = teach.createFramePlayer(list, { loop: true });
ok(player.index === 0 && !player.playing, "starts on the first frame, paused");
player.play();
ok(player.playing, "play starts the sequence");
player.consume(399);
ok(player.index === 0, "does not advance early");
player.consume(1);
ok(player.index === 1, "advances after the frame duration");
player.consume(800);
ok(player.index === 0, "loops back to the first frame while spoken");
ok(player.current.caption === "one", "current frame tracks the index");

/* 3. Pause holds the frame and the remaining time; resume continues it. */
player.pause();
ok(!player.playing, "pause stops playback");
player.consume(5000);
ok(player.index === 0, "a paused player does not advance");
player.play();
player.consume(200);
player.pause();
player.play();
player.consume(199);
ok(player.index === 0, "resume keeps the unused part of the frame");
player.consume(1);
ok(player.index === 1, "resume advances at the right time");

/* 4. Stepping, scrubbing and replay are independent of play state. */
player.pause();
player.next();
ok(player.index === 2, "step forward works while paused");
player.next();
ok(player.index === 2, "step forward stops at the last frame");
player.previous();
ok(player.index === 1, "step back works");
player.seek(0);
ok(player.index === 0 && player.remainingMs === 400, "scrub resets the frame timer");
player.replay();
ok(player.index === 0, "replay returns to the first frame");

/* 5. Without loop, playback ends on the final frame. */
player = teach.createFramePlayer(list, { loop: false });
player.play();
player.consume(3000);
ok(player.index === 2 && !player.playing && player.finished, "ends on the final frame");

/* 6. Reduced motion starts on the final frame, not playing, and still steps. */
player = teach.createFramePlayer(list, { loop: true, reducedMotion: true });
ok(player.index === 2 && !player.playing, "reduced motion shows the final frame");
player.previous();
ok(player.index === 1, "reduced motion can still step back");
player.next();
ok(player.index === 2, "reduced motion can step forward");
player.play();
ok(player.playing, "reduced motion can still play on request");

/* 7. An animation block contributes captions to the spoken text. */
const speech = teach.segmentsForBlock({
  kind: "animation",
  frames: [
    { svg: svg(2), caption: "Unroll the label" },
    { svg: svg(6), caption: "The rectangle appears" }
  ]
});
ok(speech.length === 1 && speech[0].includes("Unroll the label"), "speaks the frame captions");
ok(speech[0].includes("rectangle appears"), "speaks every caption");

console.log(`check_animation: ${checks} assertions passed`);
