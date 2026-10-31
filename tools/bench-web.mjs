#!/usr/bin/env node
// ブラウザでの描画ベンチマーク(Playwright)。
//
//   (cd crates/board-web && trunk build --release)
//   npm i -D playwright      # 未導入なら
//   node tools/bench-web.mjs --sizes 10000,100000,1000000 [--headed] [--webgpu] [--nocull] [--viewport 1600x900] [--out bench-web.json]
//
// ビルド済みの `crates/board-web/dist` を手元の HTTP サーバーで配り、`?bench&n=…` を開いて
// `window.__benchResult` が出るのを待つ。カメラの動きは決まっているので、同じマシンなら同じ条件で測れる。
//
// 注意: ヘッドレスの Chromium は多くの環境で GPU を使わず、WebGL2 を SwiftShader(CPU)で描く。
// ヘッドレスの WebGPU は画面に何も描かないことがあるので、ヘッドレスでは既定で WebGL2 を使う。
// GPU の実力を測るときは `--headed --webgpu` で実機のブラウザを使うか、`--chrome /path/to/chrome` を指定する。
// どの環境で測ったかは結果の `backend` / `adapter` / `user_agent` に残る。
// `--nocull` を付けるとブロック単位のカリングを切って測る(効果の比較用)。

import { createServer } from 'node:http';
import { readFile, writeFile } from 'node:fs/promises';
import { extname, join, normalize, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

// CommonJS の解決を使うので、グローバルに入れた Playwright(NODE_PATH)でも動く。
const { chromium } = createRequire(import.meta.url)('playwright');

const args = process.argv.slice(2);
const opt = (name, def) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : def;
};
const flag = (name) => args.includes(name);

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const dist = normalize(opt('--dist', join(root, 'crates/board-web/dist')));
const sizes = opt('--sizes', '10000,100000,1000000').split(',').map(Number);
const out = opt('--out', null);
const timeoutMs = Number(opt('--timeout', '180000'));

const types = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript',
  '.wasm': 'application/wasm',
  '.css': 'text/css',
  '.ttf': 'font/ttf',
  '.json': 'application/json',
};

const server = createServer(async (req, res) => {
  const path = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  const file = normalize(join(dist, path === '/' ? 'index.html' : path));
  if (!file.startsWith(dist)) {
    res.writeHead(403).end();
    return;
  }
  try {
    const body = await readFile(file);
    res.writeHead(200, { 'content-type': types[extname(file)] ?? 'application/octet-stream' });
    res.end(body);
  } catch {
    res.writeHead(404).end();
  }
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/`;

const browser = await chromium.launch({
  headless: !flag('--headed'),
  executablePath: opt('--chrome', undefined),
  args: ['--enable-unsafe-webgpu', '--ignore-gpu-blocklist', '--disable-gpu-vsync', '--disable-frame-rate-limit'],
});

const results = [];
try {
  for (const n of sizes) {
    const [width, height] = opt('--viewport', '1600x900').split('x').map(Number);
    const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: 1 });
    page.on('pageerror', (e) => console.error(`[page] ${e.message}`));
    const webgl = flag('--webgl') || (!flag('--headed') && !flag('--webgpu'));
    const url = `${base}?bench&n=${n}${webgl ? '&webgl' : ''}${flag('--nocull') ? '&nocull' : ''}`;
    const started = Date.now();
    await page.goto(url);
    await page.waitForFunction(() => window.__benchResult, null, { timeout: timeoutMs, polling: 250 });
    const result = JSON.parse(await page.evaluate(() => window.__benchResult));
    result.wall_ms = Date.now() - started;
    results.push(result);
    await page.close();

    const p = result.phases;
    const t = result.timings_ms;
    console.log(
      `${String(n).padStart(8)} 要素  ${result.backend.padEnd(6)}  初回描画 ${t.first_frame?.toFixed(0) ?? '-'} ms  ` +
        Object.entries(p)
          .map(([k, v]) => `${k}: ${v.fps.toFixed(0)} fps (p95 ${v.frame_ms.p95.toFixed(1)} ms, 描画 ${v.drawn_instances})`)
          .join('  '),
    );
  }
} finally {
  await browser.close();
  server.close();
}

if (out) {
  await writeFile(out, JSON.stringify(results, null, 2));
  console.log(`${out} に保存しました`);
}
