// アプリのロゴ(crates/board-web/assets/brand/logo.svg)から、ファビコン・PWA・Tauri 用の画像を作る。
//
//   node tools/brand/render-icons.js
//
// Playwright(Chromium)で SVG を描き、PNG にする。ICO / ICNS は Python の Pillow で作る。
// ロゴを変えたらこれを流して、できたファイルをコミットする。
const { chromium } = require('playwright');
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '../..');
const brand = path.join(root, 'crates/board-web/assets/brand');
const logo = fs.readFileSync(path.join(brand, 'logo.svg'), 'utf8');
const inner = logo.replace(/^[\s\S]*?<svg[^>]*>/, '').replace(/<\/svg>\s*$/, '').replace(/<title>.*?<\/title>/, '');
const defs = inner.match(/<defs>[\s\S]*?<\/defs>/)[0];
const art = inner.replace(defs, '').replace(/<rect width="512" height="512"[^>]*\/>/, '');

// 背景の形を変えた版を作る。scale は中身(付箋とカーソル)の縮小率。
function variant({ radius = 116, margin = 0, scale = 1 }) {
  const size = 512 - margin * 2;
  const c = 256;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">${defs}
<rect x="${margin}" y="${margin}" width="${size}" height="${size}" rx="${radius}" fill="url(#gb-bg)"/>
<g transform="translate(${c} ${c}) scale(${(scale * size) / 512}) translate(${-c} ${-c})">${art}</g></svg>`;
}

const variants = {
  logo, // 角丸のタイル(ヘッダー・ファビコン・Windows)
  square: variant({ radius: 0, scale: 0.95 }), // iOS は自分で角を丸める
  maskable: variant({ radius: 0, scale: 0.85 }), // Android の切り抜き(中央 80% の円)に収める
  mac: variant({ margin: 50, radius: 92 }), // macOS は周りに余白を取る
};

const outputs = [
  ['logo', 192, path.join(brand, 'icon-192.png')],
  ['logo', 512, path.join(brand, 'icon-512.png')],
  ['maskable', 512, path.join(brand, 'icon-maskable-512.png')],
  ['square', 180, path.join(brand, 'apple-touch-icon.png')],
  ['logo', 96, path.join(brand, 'logo-96.png')],
];
const ico = [16, 24, 32, 48, 64, 256];
const desktop = path.join(root, 'crates/board-desktop/icons');
const tauri = fs.existsSync(desktop)
  ? [
      ['logo', 32, path.join(desktop, '32x32.png')],
      ['logo', 128, path.join(desktop, '128x128.png')],
      ['logo', 256, path.join(desktop, '128x128@2x.png')],
      ['mac', 1024, path.join(desktop, 'icon.png')],
    ]
  : [];

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage();
  const tmp = fs.mkdtempSync(path.join(require('os').tmpdir(), 'gb-icons-'));
  const render = async (name, px, out) => {
    await page.setViewportSize({ width: px, height: px });
    const b64 = Buffer.from(variants[name]).toString('base64');
    await page.setContent(
      `<style>html,body{margin:0;background:transparent}</style><img src="data:image/svg+xml;base64,${b64}" width="${px}" height="${px}" style="display:block">`,
    );
    await page.screenshot({ path: out, omitBackground: true });
    console.log(path.relative(root, out));
  };
  for (const [name, px, out] of [...outputs, ...tauri]) await render(name, px, out);
  const icoSrc = [];
  for (const px of ico) {
    const f = path.join(tmp, `ico-${px}.png`);
    await render('logo', px, f);
    icoSrc.push(f);
  }
  const icnsSrc = path.join(tmp, 'mac-1024.png');
  await render('mac', 1024, icnsSrc);
  await browser.close();

  // ICO は大きさごとに描いた PNG をまとめる(縮小より 16px がくっきりする)。
  const py = `
import sys
from PIL import Image
out, icns_src, icns_out, *srcs = sys.argv[1:]
imgs = [Image.open(s).convert("RGBA") for s in srcs]
for o in out.split(","):
    imgs[-1].save(o, format="ICO", sizes=[i.size for i in imgs], append_images=imgs[:-1])
if icns_out:
    Image.open(icns_src).convert("RGBA").save(icns_out, format="ICNS")
`;
  const icoOuts = [path.join(brand, 'favicon.ico')];
  if (tauri.length) icoOuts.push(path.join(desktop, 'icon.ico'));
  execFileSync('python3', ['-I', '-c', py, icoOuts.join(','), icnsSrc, tauri.length ? path.join(desktop, 'icon.icns') : '', ...icoSrc], { stdio: 'inherit' });
  for (const o of icoOuts) console.log(path.relative(root, o));
  if (tauri.length) console.log(path.relative(root, path.join(desktop, 'icon.icns')));
  fs.rmSync(tmp, { recursive: true });
})();
