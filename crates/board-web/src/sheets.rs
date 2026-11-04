//! 表計算・CSV のファイルの見分けと文字コード(大きなファイルの読み取りは Worker 側の
//! `board_files::table`、並べるのは `board_core::tabular`)。

/// 拡張子(小文字、`.` なし)。
fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// CSV・TSV のファイルか。
pub fn is_csv(name: &str, mime: &str) -> bool {
    matches!(extension(name).as_str(), "csv" | "tsv")
        || mime == "text/csv"
        || mime == "text/tab-separated-values"
}

/// 表計算ソフトのファイルか。
pub fn is_spreadsheet(name: &str) -> bool {
    matches!(
        extension(name).as_str(),
        "xlsx" | "xlsm" | "xlsb" | "xls" | "ods"
    )
}

/// 文字コードを推測して文字列にする(UTF-8 でなければ Shift_JIS とみなす。Excel の日本語 CSV 向け)。
pub fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::SHIFT_JIS.decode(bytes).0.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_jis_csv() {
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("名前,年齢");
        assert_eq!(decode_text(&bytes), "名前,年齢");
    }
}
