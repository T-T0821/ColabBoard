#!/usr/bin/env python3
"""Whisper のエンコーダを ONNX にする(ブラウザ内 AI を NPU / GPU で動かすため)。

ブラウザ内 AI は、重い「エンコーダ」(音声 30 秒分を一度に読む部分)だけを WebNN / WebGPU で
動かし、デコーダは今までどおり candle(wasm)で動かす。このスクリプトは Hugging Face 形式の
`model.safetensors` から、そのエンコーダだけの ONNX を作る。形はすべて固定
(入力 [1, n_mels, 3000] → 出力 [1, 1500, d_model])なので、NPU でもそのまま動く。

ふつうは Hugging Face の `onnx-community/whisper-{tiny,base}` の `onnx/encoder_model*.onnx` を
使えばよく、このスクリプトはモデルを社内に置くとき(`LOCAL_AI_MODELS_DIR`)向け:

    pip install numpy onnx safetensors
    python3 tools/whisper-onnx/export_encoder.py models/whisper-base

`models/whisper-base/` の `config.json` と `model.safetensors` を読み、同じ場所に
`encoder_model.onnx`(float32)と `encoder_model_fp16.onnx`(重みを float16 にしたもの。入出力は
float32 のまま。NPU 向け)を書く。`--check` を付けると onnxruntime(Python)で numpy の素直な
実装と結果を比べる。
"""

import argparse
import json
import math
import os
import sys

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper
from safetensors.numpy import load_file

OPSET = 17


def load(model_dir):
    with open(os.path.join(model_dir, "config.json"), encoding="utf-8") as f:
        cfg = json.load(f)
    w = load_file(os.path.join(model_dir, "model.safetensors"))
    w = {k.removeprefix("model."): v.astype(np.float32) for k, v in w.items() if "encoder." in k}
    return cfg, w


class Graph:
    def __init__(self, dtype):
        self.nodes, self.inits, self.n = [], [], 0
        self.np_dtype = np.float16 if dtype == "fp16" else np.float32
        self.onnx_dtype = TensorProto.FLOAT16 if dtype == "fp16" else TensorProto.FLOAT

    def name(self, base):
        self.n += 1
        return f"{base}_{self.n}"

    def const(self, arr, name=None, keep=False):
        name = name or self.name("c")
        a = np.asarray(arr)
        if not keep and a.dtype.kind == "f":
            a = a.astype(self.np_dtype)
        self.inits.append(numpy_helper.from_array(a, name))
        return name

    def op(self, kind, inputs, **attrs):
        out = self.name(kind.lower())
        self.nodes.append(helper.make_node(kind, inputs, [out], **attrs))
        return out

    def linear(self, x, w, b=None):
        y = self.op("MatMul", [x, self.const(w.T)])
        return self.op("Add", [y, self.const(b)]) if b is not None else y

    def gelu(self, x):
        # 0.5 * x * (1 + erf(x / sqrt(2)))(PyTorch の既定と同じ厳密な GELU)
        e = self.op("Erf", [self.op("Mul", [x, self.const(np.array(1 / math.sqrt(2)))])])
        y = self.op("Add", [e, self.const(np.array(1.0))])
        return self.op("Mul", [self.op("Mul", [x, y]), self.const(np.array(0.5))])

    def layer_norm(self, x, g, b):
        return self.op(
            "LayerNormalization", [x, self.const(g), self.const(b)], axis=-1, epsilon=1e-5
        )


def build(cfg, w, dtype):
    g = Graph(dtype)
    n_mels, d = cfg["num_mel_bins"], cfg["d_model"]
    heads, layers = cfg["encoder_attention_heads"], cfg["encoder_layers"]
    hd = d // heads
    T = cfg.get("max_source_positions", 1500)
    p = "encoder."

    x = "input_features"
    if dtype == "fp16":
        x = g.op("Cast", [x], to=TensorProto.FLOAT16)
    x = g.op("Conv", [x, g.const(w[p + "conv1.weight"]), g.const(w[p + "conv1.bias"])],
             kernel_shape=[3], pads=[1, 1])
    x = g.gelu(x)
    x = g.op("Conv", [x, g.const(w[p + "conv2.weight"]), g.const(w[p + "conv2.bias"])],
             kernel_shape=[3], pads=[1, 1], strides=[2])
    x = g.gelu(x)
    x = g.op("Transpose", [x], perm=[0, 2, 1])  # [1, T, d]
    x = g.op("Add", [x, g.const(w[p + "embed_positions.weight"][:T])])

    split = g.const(np.array([1, T, heads, hd], dtype=np.int64), keep=True)
    merge = g.const(np.array([1, T, d], dtype=np.int64), keep=True)
    scale = hd ** -0.25
    for i in range(layers):
        lp = f"{p}layers.{i}."
        h = g.layer_norm(x, w[lp + "self_attn_layer_norm.weight"], w[lp + "self_attn_layer_norm.bias"])
        a = lp + "self_attn."
        # q・k の両方に hd^-0.25 を掛ける(HF は q に hd^-0.5。重みに畳み込んで同じ結果にする)
        q = g.linear(h, w[a + "q_proj.weight"] * scale, w[a + "q_proj.bias"] * scale)
        k = g.linear(h, w[a + "k_proj.weight"] * scale)
        v = g.linear(h, w[a + "v_proj.weight"], w[a + "v_proj.bias"])
        q = g.op("Transpose", [g.op("Reshape", [q, split])], perm=[0, 2, 1, 3])  # [1, H, T, hd]
        k = g.op("Transpose", [g.op("Reshape", [k, split])], perm=[0, 2, 3, 1])  # [1, H, hd, T]
        v = g.op("Transpose", [g.op("Reshape", [v, split])], perm=[0, 2, 1, 3])
        s = g.op("Softmax", [g.op("MatMul", [q, k])], axis=-1)
        o = g.op("MatMul", [s, v])
        o = g.op("Reshape", [g.op("Transpose", [o], perm=[0, 2, 1, 3]), merge])
        x = g.op("Add", [x, g.linear(o, w[a + "out_proj.weight"], w[a + "out_proj.bias"])])
        h = g.layer_norm(x, w[lp + "final_layer_norm.weight"], w[lp + "final_layer_norm.bias"])
        h = g.gelu(g.linear(h, w[lp + "fc1.weight"], w[lp + "fc1.bias"]))
        x = g.op("Add", [x, g.linear(h, w[lp + "fc2.weight"], w[lp + "fc2.bias"])])
    x = g.layer_norm(x, w[p + "layer_norm.weight"], w[p + "layer_norm.bias"])
    if dtype == "fp16":
        x = g.op("Cast", [x], to=TensorProto.FLOAT)
    g.nodes.append(helper.make_node("Identity", [x], ["last_hidden_state"]))

    graph = helper.make_graph(
        g.nodes,
        "whisper_encoder",
        [helper.make_tensor_value_info("input_features", TensorProto.FLOAT, [1, n_mels, 2 * T])],
        [helper.make_tensor_value_info("last_hidden_state", TensorProto.FLOAT, [1, T, d])],
        g.inits,
    )
    model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", OPSET)],
                              producer_name="colabboard")
    model.ir_version = 8
    onnx.checker.check_model(model)
    return model


def reference(cfg, w, mel):
    """numpy の素直な実装(確認用)。"""
    from math import erf
    gelu = np.vectorize(lambda v: 0.5 * v * (1 + erf(v / math.sqrt(2))), otypes=[np.float32])

    def conv(x, wt, b, stride):
        x = np.pad(x, ((0, 0), (1, 1)))
        n = (x.shape[1] - 3) // stride + 1
        cols = np.stack([x[:, i * stride:i * stride + 3] for i in range(n)], 0)  # n, cin, 3
        return np.einsum("nck,ock->on", cols, wt) + b[:, None]

    def ln(x, gm, bt):
        m = x.mean(-1, keepdims=True)
        v = ((x - m) ** 2).mean(-1, keepdims=True)
        return (x - m) / np.sqrt(v + 1e-5) * gm + bt

    p = "encoder."
    x = gelu(conv(mel[0], w[p + "conv1.weight"], w[p + "conv1.bias"], 1))
    x = gelu(conv(x, w[p + "conv2.weight"], w[p + "conv2.bias"], 2)).T
    x = x + w[p + "embed_positions.weight"][: x.shape[0]]
    H = cfg["encoder_attention_heads"]
    for i in range(cfg["encoder_layers"]):
        lp = f"{p}layers.{i}."
        a = lp + "self_attn."
        h = ln(x, w[lp + "self_attn_layer_norm.weight"], w[lp + "self_attn_layer_norm.bias"])
        q = h @ w[a + "q_proj.weight"].T + w[a + "q_proj.bias"]
        k = h @ w[a + "k_proj.weight"].T
        v = h @ w[a + "v_proj.weight"].T + w[a + "v_proj.bias"]
        T, d = q.shape
        hd = d // H
        q, k, v = (t.reshape(T, H, hd).transpose(1, 0, 2) for t in (q, k, v))
        s = q @ k.transpose(0, 2, 1) / math.sqrt(hd)
        s = np.exp(s - s.max(-1, keepdims=True))
        s /= s.sum(-1, keepdims=True)
        o = (s @ v).transpose(1, 0, 2).reshape(T, d)
        x = x + o @ w[a + "out_proj.weight"].T + w[a + "out_proj.bias"]
        h = ln(x, w[lp + "final_layer_norm.weight"], w[lp + "final_layer_norm.bias"])
        h = gelu(h @ w[lp + "fc1.weight"].T + w[lp + "fc1.bias"])
        x = x + h @ w[lp + "fc2.weight"].T + w[lp + "fc2.bias"]
    return ln(x, w[p + "layer_norm.weight"], w[p + "layer_norm.bias"])[None]


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("model_dir", help="config.json と model.safetensors のあるディレクトリ")
    ap.add_argument("--out", help="書き出す先(既定は model_dir)")
    ap.add_argument("--check", action="store_true", help="onnxruntime で numpy の実装と比べる")
    args = ap.parse_args()
    out = args.out or args.model_dir
    cfg, w = load(args.model_dir)
    for dtype, file in [("fp32", "encoder_model.onnx"), ("fp16", "encoder_model_fp16.onnx")]:
        model = build(cfg, w, dtype)
        path = os.path.join(out, file)
        onnx.save(model, path)
        print(f"{path}: {os.path.getsize(path) / 1e6:.1f} MB")
        if args.check:
            import onnxruntime as ort

            rng = np.random.default_rng(0)
            mel = rng.uniform(-1, 1, (1, cfg["num_mel_bins"], 3000)).astype(np.float32)
            got = ort.InferenceSession(path).run(None, {"input_features": mel})[0]
            want = reference(cfg, w, mel)
            # 平均の誤差を、出力の大きさ(標準偏差)との比で見る。
            err = np.abs(got - want).mean() / want.std()
            print(f"  誤差 {err:.2e}(出力の標準偏差比)")
            if err > (2e-2 if dtype == "fp16" else 1e-4):
                sys.exit(f"{file}: numpy の実装と合いません")


if __name__ == "__main__":
    main()
