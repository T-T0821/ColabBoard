// グラボード の Service Worker。アプリ本体(HTML・JS・WASM・CSS・フォント)を端末に置き、
// ネットワークがなくても開けるようにする。ボードの中身は IndexedDB に別に保存する。
//
// - ページ(HTML): ネットワーク優先。つながらなければ前回のものを使う
// - trunk がハッシュを付けたファイル(中身が変われば名前も変わる): 端末にあればそれを使う
// - それ以外の同じオリジンのファイル: ネットワーク優先で、取れたら端末にも置く
// - WebSocket(/ws/…)、API(/api/…)、別オリジンへのリクエストには関わらない
// - プッシュ通知(メンション・返信・期限)を受け取って表示する(下の「プッシュ通知」)

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

// ---- プッシュ通知 ----
// サーバーは中身のない合図だけを送る(通知の中身を Google・Apple などのプッシュサービスに渡さないため)。
// 合図が来たら自分宛ての通知(/api/notifications)を取り直し、まだ見せていない未読のものを表示する。

const LAST_SHOWN = new URL('./__push-last-shown', self.location).href;

async function lastShown() {
  const hit = await (await caches.open(CACHE)).match(LAST_SHOWN);
  return hit ? Number(await hit.text()) || 0 : 0;
}

async function setLastShown(id) {
  await (await caches.open(CACHE)).put(LAST_SHOWN, new Response(String(id)));
}

function headline(n) {
  switch (n.kind) {
    case 'mention': return `${n.from_name} があなたをメンションしました`;
    case 'reply': return `${n.from_name} がスレッドに返信しました`;
    case 'due': return 'コメントの期限が近づいています';
    case 'overdue': return 'コメントの期限を過ぎました';
    default: return 'グラボード の通知';
  }
}

function notificationUrl(n) {
  const base = new URL('./', self.location);
  if (!n.board) return base.href;
  const q = new URLSearchParams({ room: n.board });
  if (n.key) q.set('key', n.key);
  q.set('comment', String(n.thread));
  return `${base.href}?${q}`;
}

async function showPush() {
  let list = null;
  try {
    const res = await fetch(new URL('api/notifications', self.registration.scope), {
      credentials: 'include',
      cache: 'no-store',
    });
    if (res.ok) list = await res.json();
  } catch (_) {}
  if (!Array.isArray(list)) {
    // ログインが切れているなど。中身は見せられないが、届いたことは知らせる。
    await self.registration.showNotification('グラボード', {
      body: '新しい通知があります。開いてログインすると内容を見られます。',
      tag: 'colabboard-generic',
      data: { url: new URL('./', self.location).href },
    });
    return;
  }
  const last = await lastShown();
  // 新しい順に並んでいる。まだ見せていない未読を古いものから(多すぎるときは 3 件まで)。
  let fresh = list.filter((n) => !n.read && n.id > last).slice(0, 3).reverse();
  if (fresh.length === 0) fresh = list.filter((n) => !n.read).slice(0, 1);
  if (fresh.length === 0) {
    await self.registration.showNotification('グラボード', {
      body: '新しい通知はすべて確認済みです。',
      tag: 'colabboard-generic',
      data: { url: new URL('./', self.location).href },
    });
    return;
  }
  for (const n of fresh) {
    await self.registration.showNotification(headline(n), {
      body: n.board_title ? `${n.board_title}\n${n.excerpt}` : n.excerpt,
      tag: `colabboard-${n.id}`,
      timestamp: n.created,
      data: { url: notificationUrl(n) },
    });
  }
  await setLastShown(Math.max(last, ...fresh.map((n) => n.id)));
}

self.addEventListener('push', (event) => {
  event.waitUntil(showPush());
});

self.addEventListener('notificationclick', (event) => {
  event.notification.close();
  const url = (event.notification.data && event.notification.data.url) || new URL('./', self.location).href;
  event.waitUntil(
    self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then((wins) => {
      const same = wins.find((w) => w.url === url);
      if (same) return same.focus();
      return self.clients.openWindow(url);
    }),
  );
});
