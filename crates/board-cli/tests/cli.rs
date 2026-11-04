//! できあがった `graboard` を実際に動かす。

use std::process::Command;

fn graboard(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_graboard"))
        .args(args)
        .output()
        .expect("graboard を起動できません")
}

#[test]
fn new_convert_and_info() {
    let dir = std::env::temp_dir().join(format!("graboard-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = |name: &str| dir.join(name).to_string_lossy().to_string();

    let out = graboard(&["new", "kpt", "-o", &p("kpt.graboard")]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = graboard(&["info", &p("kpt.graboard")]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("frame: 3"), "{text}");

    std::fs::write(p("flow.mmd"), "graph TD\n A[開始] --> B[終了]").unwrap();
    let out = graboard(&[
        "convert",
        &p("kpt.graboard"),
        &p("flow.mmd"),
        "-o",
        &p("all.png"),
        "--scale",
        "1",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let img = image::open(p("all.png")).unwrap();
    assert!(img.width() > 1000);

    let out = graboard(&[
        "convert",
        &p("kpt.graboard"),
        "--frames",
        "-o",
        &p("kpt.pdf"),
    ]);
    assert!(out.status.success());
    let pdf = std::fs::read(p("kpt.pdf")).unwrap();
    assert!(String::from_utf8_lossy(&pdf).contains("/Count 3"));

    let out = graboard(&["convert", &p("flow.mmd"), "--to", "md", "-o", "-"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("開始"));

    // 間違いは終了コードで分かる。
    assert!(
        !graboard(&["new", "ないテンプレート", "-o", &p("x.png")])
            .status
            .success()
    );
    assert!(
        !graboard(&["convert", &p("flow.mmd"), "-o", &p("x.gif")])
            .status
            .success()
    );
    assert_eq!(graboard(&["--nope"]).status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}
