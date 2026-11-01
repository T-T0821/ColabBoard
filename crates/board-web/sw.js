// グラボード の Service Worker。アプリ本体(HTML・JS・WASM・CSS・フォント)を端末に置き、
// ネットワークがなくても開けるようにする。ボードの中身は IndexedDB に別に保存する。
//
// - ページ(HTML): ネットワーク優先。つながらなければ前回のものを使う
// - trunk がハッシュを付けたファイル(中身が変われば名前も変わる): 端末にあればそれを使う
// - それ以外の同じオリジンのファイル: ネットワーク優先で、取れたら端末にも置く
// - WebSocket(/ws/…)、API(/api/…)、別オリジンへのリクエストには関わらない

const CACHE = 'colabboard-v1';
const HASHED = /-[0-9a-f]{16}(_bg)?\.(js|wasm|css)$/;

// 初回の表示ではページが Service Worker より先に読み込まれるので、ページが使うファイルを
// インストール時に洗い出して置いておく(HTML の href/src と、JS が読み込む snippets)。
async function precache() {
  const cache = await caches.open(CACHE);
  const base = new URL('./', self.location);
  const res = await fetch(base, { cache: 'no-cache' });
  if (!res.ok) return;
  const html = await res.clone().text();
  await cache.put(base.href, res);
  const urls = new Set([new URL('fonts/NotoSansJP-Regular.ttf', base).href]);
  for (const m of html.matchAll(/(?:href|src)="([^"]+)"/g)) {
    const u = new URL(m[1], base);
    if (u.origin === base.origin && u.pathname !== base.pathname) urls.add(u.href);
  }
  for (const u of [...urls]) {
    if (!u.endsWith('.js')) continue;
    const js = await fetch(u).then((r) => (r.ok ? r.text() : '')).catch(() => '');
    for (const m of js.matchAll(/['"](\.?\/?snippets\/[^'"]+\.js)['"]/g)) {
      urls.add(new URL(m[1], u).href);
    }
  }
  await Promise.all(
    [...urls].map((u) =>
      fetch(u)
        .then((r) => (r.ok ? cache.put(u, r) : undefined))
        .catch(() => {}),
    ),
  );
}

self.addEventListener('install', (event) => {
  event.waitUntil(
    precache()
      .catch(() => {})
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      // ブラウザ内 AI のモデル(colabboard-local-ai-*)は大きいので、アプリの更新では消さない。
      .then((keys) =>
        Promise.all(
          keys
            .filter((k) => k !== CACHE && !k.startsWith('colabboard-local-ai'))
            .map((k) => caches.delete(k)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});

async function networkFirst(request, cacheKey) {
  const cache = await caches.open(CACHE);
  try {
    const res = await fetch(request);
    if (res.ok) cache.put(cacheKey ?? request, res.clone());
    return res;
  } catch (e) {
    const hit = await cache.match(cacheKey ?? request, { ignoreSearch: true });
    if (hit) return hit;
    throw e;
  }
}

async function cacheFirst(request) {
  const cache = await caches.open(CACHE);
  const hit = await cache.match(request);
  if (hit) return hit;
  const res = await fetch(request);
  if (res.ok) cache.put(request, res.clone());
  return res;
}

self.addEventListener('fetch', (event) => {
  const req = event.request;
  if (req.method !== 'GET') return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  if (url.pathname.startsWith('/ws') || url.pathname.startsWith('/api')) return;
  if (req.mode === 'navigate') {
    // `?room=…` が違っても同じ HTML なので、クエリなしの 1 つにまとめて置く。
    event.respondWith(networkFirst(req, new URL('./', self.location).href));
  } else if (HASHED.test(url.pathname)) {
    event.respondWith(cacheFirst(req));
  } else {
    event.respondWith(networkFirst(req));
  }
});
