//! 画像・PDF・マークダウン・表の挿入(貼り付け・ドロップ・ファイル選択)。
//!
//! 重い処理(大きな画像のデコードと縮小、PDF のページの描画、巨大な CSV・Excel の読み取り)は
//! [`super::file_worker`] で複数の Web Worker に並行して任せ、画面のスレッドを止めない。
//! 数百ページの PDF は、先にページの大きさの枠を並べ、描けたページから順に画像に差し替える。
//! 保存した画像はボードの要素として同期される。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use board_core::{ElementId, ImageData, Shape, Vec2, pdf_pages};
use board_files::picture::prepare_image;
use web_sys::{DragEvent, File, HtmlInputElement, MouseEvent};

use super::file_worker::{self, Picture};
use super::{App, by_id, listen};

/// 置いたときの一辺の上限(ワールド単位)。
const MAX_WORLD: f32 = 480.0;

/// 画像を縮める(Worker で。Worker が使えなければこのスレッドで)。
async fn shrink_image(bytes: &[u8]) -> Result<Picture, String> {
    match file_worker::image(bytes).await {
        Ok(p) => Ok(p),
        Err(_) => prepare_image(bytes).map(|(data, w, h)| Picture { data, w, h }),
    }
}

/// PDF の取り込みの進み具合(ページごとの仕事で共有する)。
struct PdfJob {
    doc: u32,
    /// ページの枠の ID(ページ順)。
    ids: Vec<ElementId>,
    done: Cell<usize>,
    failed: Cell<usize>,
    /// PDF の全ページ数(取り込む上限を超えることがある)。
    total: usize,
    closed: Cell<bool>,
}

impl PdfJob {
    fn close(&self) {
        if !self.closed.replace(true) {
            file_worker::pdf_close(self.doc);
        }
    }
}

/// PDF を読み込み、ページの枠を先に並べてから、描けたページから順に画像にする。
async fn insert_pdf(app: &Rc<RefCell<App>>, bytes: &[u8], at: Vec2) {
    if !app.borrow().can_edit() {
        return;
    }
    let document = app.borrow().document.clone();
    super::ink_ui::toast(&document, "PDF を読み込んでいます…", "busy");
    let doc = file_worker::pdf_open(bytes);
    let (sizes, total) = match file_worker::pdf_info(doc).await {
        Ok(v) => v,
        Err(e) => {
            file_worker::pdf_close(doc);
            super::ink_ui::toast(&document, &e, "error");
            log::warn!("{e}");
            return;
        }
    };
    let ids = app.borrow_mut().place_pdf_frames(&sizes, at);
    if ids.is_empty() {
        file_worker::pdf_close(doc);
        return;
    }
    let pixels = board_files::page_pixels(ids.len());
    let job = Rc::new(PdfJob {
        doc,
        ids,
        done: Cell::new(0),
        failed: Cell::new(0),
        total,
        closed: Cell::new(false),
    });
    pdf_progress(&document, &job);
    // ページごとの仕事を全部並べる。Worker が空いた順に、前のページから描いていく。
    for page in 0..job.ids.len() {
        let app = app.clone();
        let job = job.clone();
        let document = document.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let id = job.ids[page];
            match file_worker::pdf_page(job.doc, page, pixels).await {
                Ok(pic) => {
                    let mut a = app.borrow_mut();
                    a.fill_pdf_page(id, pic);
                    // 取り込んだページが全部消された(Undo・削除)なら、残りのページは描かない。
                    if !job.ids.iter().any(|id| a.board.get(*id).is_some()) {
                        job.close();
                    }
                }
                Err(e) if e == file_worker::CANCELLED => {}
                Err(e) => {
                    log::warn!("PDF の {} ページ目: {e}", page + 1);
                    job.failed.set(job.failed.get() + 1);
                }
            }
            job.done.set(job.done.get() + 1);
            pdf_progress(&document, &job);
        });
    }
}

/// PDF の進み具合をお知らせに出す。終わったら PDF を Worker から捨てる。
fn pdf_progress(document: &web_sys::Document, job: &PdfJob) {
    let (done, n) = (job.done.get(), job.ids.len());
    if done < n {
        if !job.closed.get() {
            let msg = format!("PDF のページを描いています {done} / {n}");
            super::ink_ui::toast(document, &msg, "busy");
        }
        return;
    }
    let cancelled = job.closed.get();
    job.close();
    if cancelled {
        super::ink_ui::toast(document, "PDF の取り込みをやめました", "done");
        return;
    }
    let failed = job.failed.get();
    let msg = if failed > 0 {
        format!(
            "PDF の {} ページを取り込みました({failed} ページは描けませんでした)",
            n - failed
        )
    } else if job.total > n {
        format!(
            "PDF の先頭 {n} ページを取り込みました(全 {} ページ)",
            job.total
        )
    } else {
        format!("PDF の {n} ページを取り込みました")
    };
    super::ink_ui::toast(document, &msg, if failed > 0 { "error" } else { "done" });
}

/// CSV・表計算のファイルを Worker で読んで並べる。
async fn insert_table(app: &Rc<RefCell<App>>, bytes: &[u8], csv: bool, at: Vec2) {
    if !app.borrow().can_edit() {
        return;
    }
    let keep = board_core::tabular::MAX_ROWS;
    let sheets = match file_worker::table(bytes, csv, keep).await {
        Ok(s) => s,
        Err(e) => {
            super::show_error(&e);
            return;
        }
    };
    let total = sheets.iter().map(|(_, t)| *t).max().unwrap_or(0);
    let sheets = sheets.into_iter().map(|(s, _)| s).collect();
    let mut a = app.borrow_mut();
    if !a.insert_sheets(sheets, at) {
        super::show_error("シートが空です");
        return;
    }
    if total > keep {
        let msg = format!("先頭の {keep} 行を取り込みました(全 {total} 行)");
        super::ink_ui::toast(&a.document, &msg, "done");
    }
}

impl App {
    /// PDF のページの枠(描き終わるまでの仮の四角形)を並べる。ページ順の ID を返す。
    /// 枠は 1 回の操作として置くので、Undo 1 回で PDF ごと取り消せる。
    fn place_pdf_frames(&mut self, sizes: &[(f32, f32)], at: Vec2) -> Vec<ElementId> {
        if !self.can_edit() {
            return Vec::new();
        }
        self.board.end_gesture();
        let ids = pdf_pages::place_frames(&mut self.board, sizes, at);
        self.board.end_gesture();
        // 画面に収まらなければ、PDF 全体が見えるまで引いて、ページが埋まっていく様子を見せる。
        if let Some(b) = self.board.bounds_of(&ids) {
            let (vmin, vmax) = self.camera.visible_world_rect();
            if b.min.x < vmin.x || b.min.y < vmin.y || b.max.x > vmax.x || b.max.y > vmax.y {
                self.camera.fit(b.min, b.max, 48.0);
            }
        }
        self.editor.set_selection(ids.clone());
        self.dirty = true;
        ids
    }

    /// 描けた PDF のページで、ページの枠を差し替える。
    fn fill_pdf_page(&mut self, id: ElementId, pic: Picture) {
        if pdf_pages::fill_page(&mut self.board, id, ImageData::new(pic.data)) {
            self.dirty = true;
        }
    }

    /// 縮めた画像をワールド座標 `at` を中心に置く。
    fn place_image(&mut self, pic: Picture, at: Vec2) {
        if !self.can_edit() || pic.w == 0 || pic.h == 0 {
            return;
        }
        let mut size = Vec2::new(pic.w as f32, pic.h as f32);
        let m = size.max_element();
        if m > MAX_WORLD {
            size *= MAX_WORLD / m;
        }
        let id = self.board.alloc_id();
        self.board.end_gesture();
        self.board.push(Shape::image(
            id,
            at - size * 0.5,
            size,
            ImageData::new(pic.data),
        ));
        self.board.end_gesture();
        self.editor.set_selection(vec![id]);
        self.dirty = true;
    }

    /// 取り込んだマークダウンの画像の仮置きを、読み込んだ画像に差し替える。
    fn replace_placeholder(&mut self, placeholder: ElementId, pic: Picture) {
        let Some(rect) = self.board.get(placeholder).map(|e| e.bounds()) else {
            return;
        };
        if pic.w == 0 {
            return;
        }
        let width = rect.size().x;
        let size = Vec2::new(width, width * pic.h as f32 / pic.w as f32);
        let id = self.board.alloc_id();
        self.board.end_gesture();
        self.board.remove(&[placeholder]);
        self.board
            .push(Shape::image(id, rect.min, size, ImageData::new(pic.data)));
        self.board.end_gesture();
        let mut sel = self.editor.selection().to_vec();
        if let Some(p) = sel.iter_mut().find(|p| **p == placeholder) {
            *p = id;
            self.editor.set_selection(sel);
        }
        self.dirty = true;
    }

    /// 画面の中央(ワールド座標)。
    fn media_center(&self) -> Vec2 {
        self.camera.screen_to_world(self.camera.viewport * 0.5)
    }
}

/// 画像を縮めてから置く。
async fn insert_image(app: &Rc<RefCell<App>>, bytes: &[u8], at: Vec2) {
    if !app.borrow().can_edit() {
        return;
    }
    match shrink_image(bytes).await {
        Ok(pic) => app.borrow_mut().place_image(pic, at),
        Err(e) => {
            log::warn!("画像を読めません: {e}");
            super::show_error(&format!("画像を読めません: {e}"));
        }
    }
}

/// ファイルを読み込んで置く(複数なら少しずつずらす)。重いファイルは並行して処理する。
pub fn insert_files(app: &Rc<RefCell<App>>, files: Vec<File>, at: Option<Vec2>) {
    let files: Vec<File> = files.into_iter().filter(is_supported).collect();
    for (i, f) in files.into_iter().enumerate() {
        let app = app.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let Ok(buf) = wasm_bindgen_futures::JsFuture::from(f.array_buffer()).await else {
                return;
            };
            let bytes = js_sys::Uint8Array::new(&buf).to_vec();
            let base = {
                let a = app.borrow();
                at.unwrap_or_else(|| a.cursor_world.unwrap_or_else(|| a.media_center()))
            };
            let base = base + Vec2::splat(32.0 * i as f32);
            if super::imports::is_diagram_file(&f.name()) {
                let text = super::imports::decode_text(&bytes);
                if !app.borrow_mut().insert_diagram_text(&text) {
                    super::show_error(
                        "Mermaid / PlantUML / draw.io / Miro の図として読めませんでした",
                    );
                }
            } else if super::imports::is_csv(&f.name(), &f.type_()) {
                insert_table(&app, &bytes, true, base).await;
            } else if super::imports::is_spreadsheet(&f.name()) {
                insert_table(&app, &bytes, false, base).await;
            } else if is_markdown(&f) {
                insert_markdown(&app, &String::from_utf8_lossy(&bytes), Some(base));
            } else if is_pdf(&f) {
                insert_pdf(&app, &bytes, base).await;
            } else {
                insert_image(&app, &bytes, base).await;
            }
        });
    }
}

fn is_pdf(f: &File) -> bool {
    f.type_() == "application/pdf" || f.name().to_ascii_lowercase().ends_with(".pdf")
}

fn is_markdown(f: &File) -> bool {
    let name = f.name().to_ascii_lowercase();
    f.type_() == "text/markdown" || name.ends_with(".md") || name.ends_with(".markdown")
}

fn is_supported(f: &File) -> bool {
    f.type_().starts_with("image/")
        || is_pdf(f)
        || is_markdown(f)
        || super::imports::is_csv(&f.name(), &f.type_())
        || super::imports::is_spreadsheet(&f.name())
        || super::imports::is_diagram_file(&f.name())
}

/// マークダウンを読んで並べる(中心は `at`、なければカーソルか画面中央)。
/// 画像のリンクは仮の四角形で置き、読み込めたものから画像に差し替える。
pub fn insert_markdown(app: &Rc<RefCell<App>>, md: &str, at: Option<Vec2>) -> bool {
    let images = {
        let mut a = app.borrow_mut();
        if !a.can_edit() {
            return false;
        }
        let center = at.unwrap_or_else(|| a.cursor_world.unwrap_or_else(|| a.media_center()));
        let sticky = a.editor.sticky_color;
        a.board.end_gesture();
        let Some(r) = board_core::markdown::insert(&mut a.board, md, center, sticky) else {
            return false;
        };
        a.board.end_gesture();
        a.editor.set_selection(r.ids);
        a.dirty = true;
        r.images
    };
    for img in images {
        let app = app.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let Some(bytes) = fetch_bytes(&img.url).await else {
                return;
            };
            let Ok(pic) = shrink_image(&bytes).await else {
                return;
            };
            app.borrow_mut().replace_placeholder(img.placeholder, pic);
        });
    }
    true
}

/// URL の中身を読む(http(s) と data: だけ。相対パスは元のファイルの場所が分からないので読まない)。
async fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
    use wasm_bindgen::JsCast;
    let lower = url.to_ascii_lowercase();
    if !["http://", "https://", "data:image/"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return None;
    }
    let window = web_sys::window()?;
    let res = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url))
        .await
        .ok()?;
    let res: web_sys::Response = res.dyn_into().ok()?;
    if !res.ok() {
        return None;
    }
    let buf = wasm_bindgen_futures::JsFuture::from(res.array_buffer().ok()?)
        .await
        .ok()?;
    Some(js_sys::Uint8Array::new(&buf).to_vec())
}

fn file_list(list: Option<web_sys::FileList>) -> Vec<File> {
    let Some(list) = list else {
        return Vec::new();
    };
    (0..list.length()).filter_map(|i| list.get(i)).collect()
}

/// 貼り付け・ドロップされたデータに画像・PDF・マークダウンのファイルがあれば取り出す。
pub fn image_files(dt: Option<web_sys::DataTransfer>) -> Vec<File> {
    file_list(dt.and_then(|d| d.files()))
        .into_iter()
        .filter(is_supported)
        .collect()
}

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let (doc, canvas) = {
        let a = app.borrow();
        (a.document.clone(), a.canvas.clone())
    };

    // ファイルを選んで挿入。
    let input: HtmlInputElement = by_id(&doc, "image-file")?;
    {
        let input2 = input.clone();
        let button = by_id::<web_sys::Element>(&doc, "insert-image")?;
        listen(button.as_ref(), "click", false, move |_: MouseEvent| {
            input2.set_value("");
            input2.click();
        })?;
    }
    {
        let app = app.clone();
        let input2 = input.clone();
        listen(input.as_ref(), "change", false, move |_: web_sys::Event| {
            let files = file_list(input2.files());
            let at = Some(app.borrow().media_center());
            insert_files(&app, files, at);
        })?;
    }

    // キャンバスへのドロップ。
    listen(canvas.as_ref(), "dragover", false, |e: DragEvent| {
        e.prevent_default();
    })?;
    {
        let app = app.clone();
        listen(canvas.as_ref(), "drop", false, move |e: DragEvent| {
            let at = {
                let a = app.borrow();
                let p = a.css_to_physical(e.client_x() as f64, e.client_y() as f64);
                a.camera.screen_to_world(p)
            };
            let files = image_files(e.data_transfer());
            if files.is_empty() {
                // ほかのアプリから引きずってきたマークダウンの文字列。
                let text = e
                    .data_transfer()
                    .and_then(|d| d.get_data("text/plain").ok())
                    .unwrap_or_default();
                if board_core::markdown::looks_like_markdown(&text) {
                    e.prevent_default();
                    insert_markdown(&app, &text, Some(at));
                }
                return;
            }
            e.prevent_default();
            insert_files(&app, files, Some(at));
        })?;
    }
    Ok(())
}
