// 10 万要素のボードをパン・ズームする様子を録画し、GIF にする。
//
// 実機の GPU で描画させたいので、既定ではインストール済みの Google Chrome を
// 画面ありで起動する(ヘッドレスやソフトウェア描画だと fps が出ない)。
//
//   cd tools/perf-gif && npm install && npm run record
//
// 環境変数:
//   URL       開くページ(既定: http://localhost:8080/?n=100000)
//   CHANNEL   Playwright のブラウザ指定(既定: chrome。msedge も可。空にすると同梱 Chromium)
//   HEADLESS  1 でヘッドレス(既定: 0)
//   OUT       出力する GIF(既定: ../../docs/images/perf-100k.gif)
//   MP4       あわせて保存する MP4(既定: GIF と同じ名前の .mp4。空にすると作らない)
//   WIDTH / HEIGHT  録画サイズ(既定: 1280x720)
//   GIF_WIDTH GIF の横幅(既定: 720)
//   GIF_FPS   GIF のフレームレート(既定: 15)
import { chromium } from 'playwright';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const env = process.env;
const url = env.URL ?? 'http://localhost:8080/?n=100000';
const channel = env.CHANNEL ?? 'chrome';
const headless = env.HEADLESS === '1';
const out = resolve(here, env.OUT ?? '../../docs/images/perf-100k.gif');
const mp4 = env.MP4 === '' ? null : resolve(here, env.MP4 ?? out.replace(/\.gif$/, '.mp4'));
const width = Number(env.WIDTH ?? 1280);
const height = Number(env.HEIGHT ?? 720);
const gifWidth = Number(env.GIF_WIDTH ?? 720);
const gifFps = Number(env.GIF_FPS ?? 15);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const videoDir = mkdtempSync(join(tmpdir(), 'colabboard-video-'));

const browser = await chromium.launch({
  channel: channel || undefined,
  headless,
  args: ['--ignore-gpu-blocklist', '--enable-gpu-rasterization', `--window-size=${width},${height + 120}`],
});
const context = await browser.newContext({
  viewport: { width, height },
  recordVideo: { dir: videoDir, size: { width, height } },
});
const page = await context.newPage();
const started = Date.now();

await page.goto(url);
await page.waitForFunction(() => {
  const n = document.querySelector('#hud-count')?.textContent ?? '';
  return Number(n.replace(/[^0-9]/g, '')) > 0;
}, null, { timeout: 60_000 });
// fps を HUD に出す(常時再描画)
await page.check('#continuous');
await sleep(1500);

const cx = width / 2;
const cy = height / 2;
await page.mouse.move(cx, cy);

// ズーム: Ctrl+ホイールを少しずつ送り、合計で factor 倍にする
// (アプリ側は 1 回のホイールで exp(-deltaY * 0.01) 倍)
async function zoom(steps, factor, frameMs = 16) {
  const delta = -Math.log(factor) / 0.01 / steps;
  await page.keyboard.down('Control');
  for (let i = 0; i < steps; i++) {
    await page.mouse.wheel(0, delta);
    await sleep(frameMs);
  }
  await page.keyboard.up('Control');
}
// パン: 修飾キーなしのホイール
async function pan(steps, dx, dy, frameMs = 16) {
  for (let i = 0; i < steps; i++) {
    await page.mouse.wheel(dx, dy);
    await sleep(frameMs);
  }
}

const recStart = (Date.now() - started) / 1000;
await zoom(90, 6);              // 全体表示 → 寄る
await pan(60, 18, 0);           // 右へ
await pan(45, 0, 18);           // 下へ
await pan(60, -18, -6);         // 左上へ
await zoom(60, 3);              // さらに寄る(輪郭がくっきりしたまま)
await pan(50, 14, 10);
await zoom(120, 1 / 18);        // 引いて全体へ
await sleep(800);
const recEnd = (Date.now() - started) / 1000;

console.log('HUD:', await page.textContent('#hud-fps'));
await context.close();
await browser.close();

const [video] = readdirSync(videoDir).filter((f) => f.endsWith('.webm'));
const src = join(videoDir, video);
const filter =
  `fps=${gifFps},scale=${gifWidth}:-1:flags=lanczos,split[a][b];` +
  `[a]palettegen=max_colors=96:stats_mode=diff[p];[b][p]paletteuse=dither=none:diff_mode=rectangle`;
execFileSync('ffmpeg', [
  '-y', '-v', 'error',
  '-ss', String(Math.max(0, recStart - 0.3)), '-to', String(recEnd),
  '-i', src, '-vf', filter, out,
], { stdio: 'inherit' });
if (mp4) {
  execFileSync('ffmpeg', [
    '-y', '-v', 'error',
    '-ss', String(Math.max(0, recStart - 0.3)), '-to', String(recEnd),
    '-i', src, '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-crf', '23', '-movflags', '+faststart', mp4,
  ], { stdio: 'inherit' });
}
rmSync(videoDir, { recursive: true, force: true });
const mb = statSync(out).size / 1e6;
console.log(`保存しました: ${out} (${mb.toFixed(1)} MB)`);
if (mp4) console.log('保存しました:', mp4);
if (mb > 10) console.log('GIF が 10 MB を超えています。GIF_WIDTH や GIF_FPS を下げると小さくなります。');
