/* Zipper-Rust service worker — cache-first shell + wasm so the PWA boots offline.
 * Game assets are NOT precached: they live in IndexedDB, provided by the user's
 * own Zipper.pdx (BYOA). */
const CACHE = 'zipper-rust-v0.39o';
const PRECACHE = [
  './',
  './index.html',
  './main.js',
  './style.css',
  './manifest.webmanifest',
  './apple-touch-icon.png',
  './favicon.png',
  './icon-180.png',
  './icon-192.png',
  './icon-512.png',
  './pkg/zipper_wasm.js',
  './pkg/zipper_wasm_bg.wasm',
  './lib/pdz.js',
  './lib/zmap.js',
  './lib/worldmap_from_luac.js',
  './lib/dialogs_from_luac.js',
  './lib/fflate.js',
  './lib/asset-manifest.js',
  './lib/asset-cache.js',
  './lib/pdx-ingest.js',
  './lib/byoa-boot.js',
];

function toAbsolute(path) {
  return new URL(path, self.location.href).href;
}

async function precacheAll() {
  const cache = await caches.open(CACHE);
  // A missing file must not fail the whole install (assets may still be staging).
  await Promise.all(PRECACHE.map(async (path) => {
    const url = toAbsolute(path);
    try {
      const res = await fetch(url, { cache: 'reload' });
      if (res && res.ok) {
        await cache.put(url, res.clone());
        await cache.put(path, res.clone());
      } else {
        console.warn('[Zipper-Rust SW] precache skip (HTTP)', path, res && res.status);
      }
    } catch (err) {
      console.warn('[Zipper-Rust SW] precache skip', path, err);
    }
  }));
}

self.addEventListener('install', (event) => {
  event.waitUntil(precacheAll().then(() => self.skipWaiting()));
});

self.addEventListener('message', (event) => {
  const data = event.data || {};
  if (data.type === 'SKIP_WAITING') {
    self.skipWaiting();
  }
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)))
    ).then(() => self.clients.claim())
  );
});

function isSameOrigin(url) {
  try {
    return new URL(url, self.location.href).origin === self.location.origin;
  } catch {
    return false;
  }
}

async function matchCache(req) {
  // ignoreSearch: ?v=… cache-bust stamps must still hit the precache.
  return (
    (await caches.match(req, { ignoreSearch: true })) ||
    (await caches.match(req.url, { ignoreSearch: true }))
  );
}

async function cachedShell() {
  return (
    (await caches.match('./index.html', { ignoreSearch: true })) ||
    (await caches.match(toAbsolute('./index.html'), { ignoreSearch: true })) ||
    (await caches.match('./', { ignoreSearch: true })) ||
    (await caches.match(toAbsolute('./'), { ignoreSearch: true })) ||
    null
  );
}

async function putInCache(reqOrUrl, res) {
  if (!res || !res.ok) return;
  try {
    const cache = await caches.open(CACHE);
    await cache.put(reqOrUrl, res.clone());
  } catch (err) {
    console.warn('[Zipper-Rust SW] cache put failed', err);
  }
}

function isNavigation(req) {
  return req.mode === 'navigate' || req.destination === 'document';
}

// Background refresh only when the network answers quickly.
// A dead LAN server must not hang the device for ages.
function revalidateInBackground(req) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 2500);
  fetch(req, { signal: ctrl.signal })
    .then(async (res) => {
      clearTimeout(timer);
      if (!res || !res.ok) return;
      await putInCache(req, res);
      if (isNavigation(req)) {
        await putInCache('./index.html', res);
        await putInCache(toAbsolute('./index.html'), res);
        await putInCache('./', res);
        await putInCache(toAbsolute('./'), res);
      }
    })
    .catch(() => {
      clearTimeout(timer);
    });
}

self.addEventListener('fetch', (event) => {
  const req = event.request;
  if (req.method !== 'GET') return;
  if (!isSameOrigin(req.url)) return;

  event.respondWith((async () => {
    const cached = await matchCache(req);
    if (cached) {
      revalidateInBackground(req);
      return cached;
    }

    if (isNavigation(req)) {
      const shell = await cachedShell();
      if (shell) {
        revalidateInBackground(req);
        return shell;
      }
    }

    try {
      const ctrl = new AbortController();
      const timer = setTimeout(() => ctrl.abort(), 4000);
      const res = await fetch(req, { signal: ctrl.signal });
      clearTimeout(timer);
      await putInCache(req, res);
      if (isNavigation(req)) {
        await putInCache('./index.html', res);
        await putInCache(toAbsolute('./index.html'), res);
      }
      return res;
    } catch {
      if (isNavigation(req)) {
        const shell = await cachedShell();
        if (shell) return shell;
      }
      return new Response('Offline — Zipper-Rust resource unavailable', {
        status: 503,
        statusText: 'Offline',
        headers: { 'Content-Type': 'text/plain; charset=utf-8' }
      });
    }
  })());
});
