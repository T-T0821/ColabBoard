# ONNX Runtime Web(ブラウザ内 AI を NPU / GPU で動かす部品)

ブラウザ内 AI の音声認識は、使えるときは重いエンコーダを NPU / GPU(WebNN、なければ WebGPU)で動かします。
そのための [ONNX Runtime Web](https://github.com/microsoft/onnxruntime)(MIT ライセンス)をここに置きます。
約 30 MB あるのでリポジトリには含めません。

```sh
tools/fetch-ort.sh
```

で npm から決まった版を取り込みます(Docker イメージのビルドでは自動で取り込みます)。
取り込んでいなければ、音声認識は CPU(wasm)だけで動きます。
