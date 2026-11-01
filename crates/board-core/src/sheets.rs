//! 表計算・CSV のファイルを読んで行の一覧にする(並べるのは [`crate::tabular`])。
//! 機能 `spreadsheet` を有効にしたときだけ使える(Web・CLI・サーバーで共用)。

use crate::tabular::Sheet;

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

/// Excel の日付のシリアル値(1900 年基準)を `YYYY-MM-DD`(時刻があれば `HH:MM` も)にする。
fn excel_date(serial: f64) -> String {
    let days = serial.floor() as i64;
    // 1899-12-30 が 0(1900 年のうるう年の誤りを含めた Excel の数え方)。
    let z = days - 25569 + 719_468; // 1970-01-01 からの日数 → 0000-03-01 基準
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let minutes = ((serial - serial.floor()) * 1440.0).round() as i64;
    if minutes == 0 {
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}",
            minutes / 60,
            minutes % 60
        )
    }
}

fn cell_text(c: &calamine::Data) -> String {
    match c {
        calamine::Data::DateTime(d) if d.is_datetime() => excel_date(d.as_f64()),
        other => other.to_string(),
    }
}

/// 表計算ソフトのファイルを読んで、シートごとの行にする。
pub fn read_spreadsheet(bytes: Vec<u8>) -> Result<Vec<Sheet>, String> {
    use calamine::Reader;
    let mut wb = calamine::open_workbook_auto_from_rs(std::io::Cursor::new(bytes))
        .map_err(|e| format!("表計算のファイルを読めません: {e}"))?;
    let sheets = wb
        .worksheets()
        .into_iter()
        .map(|(name, range)| Sheet {
            name,
            rows: range
                .rows()
                .map(|r| r.iter().map(cell_text).collect())
                .collect(),
        })
        .collect();
    Ok(sheets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excel_dates() {
        assert_eq!(excel_date(45597.0), "2024-11-01");
        assert_eq!(excel_date(1.5), "1899-12-31 12:00");
    }

    #[test]
    fn shift_jis_csv() {
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("名前,年齢");
        assert_eq!(decode_text(&bytes), "名前,年齢");
    }
}
