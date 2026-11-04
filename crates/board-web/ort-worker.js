// ブラウザ内 AI: Whisper のエンコーダを NPU / GPU で動かす Worker(ONNX Runtime Web)。
//
// 本体(src/local_ai.rs)が `new Worker('ort-worker.js', { type: 'module' })` で起こす。
// ONNX Runtime Web(assets/ort/、tools/fetch-ort.sh で取り込む)は WebNN を呼ぶときに
// `new Function` を使うので、`unsafe-eval` を許していないページの中では動かない。Worker は
// 自分のスクリプトの応答の Content-Security-Policy に従う(サーバーは JS には付けない)ので、
// ここで動かす。推論も画面のスレッドの外で走る。
//
// やりとり(`postMessage`、どれも `id` を付けて送り、同じ `id` で返す):
// - `{ type: 'probe' }` → `{ kinds: ['npu', 'gpu', 'webgpu'] }`(このブラウザが受け付けたもの)
// - `{ type: 'load', kind, bytes: Uint8Array, nMels, frames }` → `{ ms }`(1 回動かした時間)
// - `{ type: 'run', mel: Float32Array }` → `{ features: Float32Array }`
// - `{ type: 'drop' }` → `{}`
// - 失敗したら `{ error }`

let ort = null;
let enc = null;
// 1 つのセッションを同時に動かさないように、順に処理する。
let queue = Promise.resolve();

async function loadOrt() {
  if (ort) return ort;
  const base = new URL('ort/', self.location.href).href;
  try {
    ort = await import(base + 'ort.min.mjs');
  } catch (e) {
    throw 'NPU / GPU を使う部品(ONNX Runtime Web)がサーバーにありません';
  }
  // スレッドは使わない(SharedArrayBuffer が要るため)。WebNN / WebGPU の計算は別のところで走る。
  ort.env.wasm.numThreads = 1;
  ort.env.wasm.wasmPaths = base;
  return ort;
}

async function probe() {
  const kinds = [];
  if (self.navigator.ml && self.navigator.ml.createContext) {
    for (const deviceType of ['npu', 'gpu']) {
      try {
        await self.navigator.ml.createContext({ deviceType });
        kinds.push(deviceType);
      } catch (e) {}
    }
  }
  try {
    if (self.navigator.gpu && (await self.navigator.gpu.requestAdapter())) kinds.push('webgpu');
  } catch (e) {}
  return { kinds };
}

function f32ToF16(src) {
  const out = new Uint16Array(src.length);
  const f = new Float32Array(1);
  const u = new Uint32Array(f.buffer);
  for (let i = 0; i < src.length; i++) {
    f[0] = src[i];
    const x = u[0];
    const sign = (x >>> 16) & 0x8000;
    const e = ((x >>> 23) & 0xff) - 112;
    const m = x & 0x7fffff;
    if (e <= 0) out[i] = e < -10 ? sign : sign | ((m | 0x800000) >> (14 - e));
    else if (e >= 31) out[i] = sign | 0x7c00 | (((x >>> 23) & 0xff) === 0xff && m ? 0x200 : 0);
    else out[i] = sign | Math.min(0x7c00, (e << 10) + ((m + 0x1000) >> 13));
  }
  return out;
}

function f16ToF32(src) {
  const out = new Float32Array(src.length);
  for (let i = 0; i < src.length; i++) {
    const h = src[i];
    const s = h & 0x8000 ? -1 : 1;
    const e = (h >> 10) & 0x1f;
    const m = h & 0x3ff;
    out[i] =
      e === 0 ? s * m * 5.960464477539063e-8 : e === 31 ? (m ? NaN : s * Infinity) : s * (1 + m / 1024) * 2 ** (e - 15);
  }
  return out;
}

async function run(mel) {
  if (!enc) throw 'エンコーダを読み込んでいません';
  const dims = [1, enc.nMels, enc.frames];
  const input = enc.f16 ? new ort.Tensor('float16', f32ToF16(mel), dims) : new ort.Tensor('float32', mel, dims);
  const out = await enc.session.run({ [enc.input]: input });
  const t = out[enc.output];
  const data = t.location && t.location !== 'cpu' ? await t.getData(true) : t.data;
  const features = t.type === 'float16' ? (data instanceof Uint16Array ? f16ToF32(data) : Float32Array.from(data)) : data;
  for (const k in out) if (out[k].dispose) out[k].dispose();
  return features instanceof Float32Array ? features : Float32Array.from(features);
}

async function load(kind, bytes, nMels, frames) {
  drop();
  await loadOrt();
  const ep = kind === 'webgpu' ? 'webgpu' : { name: 'webnn', deviceType: kind, powerPreference: 'default' };
  const session = await ort.InferenceSession.create(bytes, {
    executionProviders: [ep],
    freeDimensionOverrides: { batch_size: 1 },
    graphOptimizationLevel: 'all',
  });
  const meta = (session.inputMetadata && session.inputMetadata[0]) || {};
  enc = {
    session,
    kind,
    nMels,
    frames,
    f16: meta.type === 'float16',
    input: session.inputNames[0],
    output: session.outputNames[0],
  };
  // 一度動かしておく(NPU / GPU はここでモデルを作り替えるので、1 回目だけ遅い)。
  const t0 = performance.now();
  await run(new Float32Array(nMels * frames));
  return { ms: performance.now() - t0 };
}

function drop() {
  if (enc) {
    try {
      enc.session.release();
    } catch (e) {}
  }
  enc = null;
  return {};
}

async function handle(d) {
  switch (d.type) {
    case 'probe':
      return probe();
    case 'load':
      return load(d.kind, d.bytes, d.nMels, d.frames);
    case 'run': {
      const features = await run(d.mel);
      return { features, transfer: [features.buffer] };
    }
    case 'drop':
      return drop();
    default:
      throw '知らない指示です: ' + d.type;
  }
}

self.onmessage = (e) => {
  const d = e.data || {};
  const job = queue.then(() => handle(d));
  queue = job.catch(() => {});
  job.then(
    (r) => {
      const transfer = (r && r.transfer) || [];
      if (r) delete r.transfer;
      self.postMessage({ ...r, id: d.id }, transfer);
    },
    (err) => self.postMessage({ id: d.id, error: String((err && err.message) || err) }),
  );
};
