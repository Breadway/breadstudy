/* breadstudy PWA shell. No build step. */
const CACHE = "breadstudy-v12";
const SHELL = [
  "./index.html",
  "./session.html",
  "./styles.css",
  "./app.js",
  "./scratchpad.js",
  "./manifest.webmanifest"
];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(CACHE).then((cache) => cache.addAll(SHELL)).then(() => self.skipWaiting())
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim())
  );
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (event.request.method !== "GET") return;
  // API passes through, never cached as shell.
  if (url.pathname.startsWith("/api/")) {
    // Cache the last successful session read so an offline reload can still
    // show the current item and hints. Other API reads pass straight through.
    event.respondWith(
      fetch(event.request).then((res) => {
        if (res.ok && url.pathname.startsWith("/api/session/")) {
          const copy = res.clone();
          caches.open(CACHE).then((cache) => cache.put(event.request, copy));
        }
        return res;
      }).catch(() => caches.match(event.request).then((hit) => {
        if (hit) return hit;
        return new Response(JSON.stringify({ error: "offline", cached: false }), {
          status: 503,
          headers: { "content-type": "application/json" }
        });
      }))
    );
    return;
  }
  event.respondWith(
    caches.match(event.request, { ignoreSearch: true }).then((hit) => {
      return hit || fetch(event.request).then((res) => {
        const copy = res.clone();
        caches.open(CACHE).then((cache) => cache.put(event.request, copy));
        return res;
      }).catch(() => caches.match("./index.html"));
    })
  );
});
