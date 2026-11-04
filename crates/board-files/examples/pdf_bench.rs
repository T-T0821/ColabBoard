//! PDF のページを描く速さを測る: `cargo run --release -p board-files --example pdf_bench -- file.pdf`
fn main() {
    let path = std::env::args().nth(1).expect("PDF のパス");
    let bytes = std::fs::read(path).unwrap();
    let t = std::time::Instant::now();
    let pdf = board_files::pdf::open(bytes).unwrap();
    let (sizes, total) = board_files::pdf::page_sizes(&pdf).unwrap();
    println!("開く: {:?}({total} ページ)", t.elapsed());
    let pixels = board_files::page_pixels(sizes.len());
    let t = std::time::Instant::now();
    let n = sizes.len().min(30);
    for i in 0..n {
        board_files::pdf::render_page(&pdf, i, pixels).unwrap();
    }
    println!(
        "1 ページ平均: {:?}(長辺 {pixels}px)",
        t.elapsed() / n as u32
    );
}
