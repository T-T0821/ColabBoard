# 手書き文字認識の見本

`glyphs.bin` は端末内の手書き文字認識(`board_core::handwriting`)が比べる文字の見本(筆画の折れ線)。
`chars.txt` の文字を、次のデータから `tools/handwriting-data` で作る。

| 文字 | 元のデータ | ライセンス |
| --- | --- | --- |
| ひらがな・カタカナ・記号・小学校で習う漢字 | [KanjiVG](https://kanjivg.tagaini.net/)(© Ulrich Apel) | [CC BY-SA 3.0](https://creativecommons.org/licenses/by-sa/3.0/) |
| 英数字・記号 | Hershey フォント(`futural.jhf`、Dr. A. V. Hershey / U.S. National Bureau of Standards) | 出典を示せば自由に使える(Hershey フォントの利用条件) |

KanjiVG から作った `glyphs.bin` は CC BY-SA 3.0 で配布する(このファイルを変えて配るときも同じ条件)。
「」〜 は KanjiVG にないため、`tools/handwriting-data` の中で手で定義している。

作り直し:

```sh
# KanjiVG の kanji/ フォルダ(0xxxx.svg)と futural.jhf を用意して
cargo run -p handwriting-data --release -- --kanjivg kanjivg/kanji --hershey futural.jhf
```
