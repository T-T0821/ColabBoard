//! コマンドライン引数(依存を増やさないよう自前で読む)。

use std::collections::HashMap;

/// 値を取るオプション(長い名前, 短い名前)。
const VALUED: &[(&str, Option<char>)] = &[
    ("output", Some('o')),
    ("to", None),
    ("from", None),
    ("scale", Some('s')),
    ("padding", None),
    ("background", None),
    ("frame", None),
    ("font", None),
    ("title", None),
    ("server", None),
    ("token", None),
    ("key", None),
    ("board", None),
];

/// 値を取らないオプション。
const FLAGS: &[(&str, Option<char>)] = &[
    ("frames", None),
    ("hide-private", None),
    ("help", Some('h')),
    ("version", Some('V')),
];

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub command: Option<String>,
    pub positional: Vec<String>,
    values: HashMap<String, String>,
    flags: Vec<String>,
}

impl Args {
    pub fn parse(it: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut a = Args::default();
        let mut it = it.into_iter();
        let mut only_positional = false;
        while let Some(arg) = it.next() {
            let name = if only_positional || arg == "-" || !arg.starts_with('-') {
                None
            } else if arg == "--" {
                only_positional = true;
                continue;
            } else if let Some(long) = arg.strip_prefix("--") {
                Some(long.to_string())
            } else {
                let c = arg[1..].chars().next().unwrap_or('-');
                let long = VALUED
                    .iter()
                    .chain(FLAGS)
                    .find(|(_, s)| *s == Some(c))
                    .map(|(l, _)| l.to_string())
                    .ok_or_else(|| format!("知らないオプションです: {arg}"))?;
                // `-ofile.png` のようにくっついた値。
                let rest = &arg[1 + c.len_utf8()..];
                if !rest.is_empty() {
                    a.values.insert(long, rest.to_string());
                    continue;
                }
                Some(long)
            };
            let Some(name) = name else {
                if a.command.is_none() {
                    a.command = Some(arg);
                } else {
                    a.positional.push(arg);
                }
                continue;
            };
            let (name, inline) = match name.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (name, None),
            };
            if VALUED.iter().any(|(l, _)| *l == name) {
                let v = match inline {
                    Some(v) => v,
                    None => it
                        .next()
                        .ok_or_else(|| format!("--{name} には値が必要です"))?,
                };
                a.values.insert(name, v);
            } else if FLAGS.iter().any(|(l, _)| *l == name) {
                a.flags.push(name);
            } else {
                return Err(format!("知らないオプションです: --{name}"));
            }
        }
        Ok(a)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Args, String> {
        Args::parse(s.split_whitespace().map(str::to_string))
    }

    #[test]
    fn reads_commands_and_options() {
        let a = parse("convert a.md b.csv -o out.png --scale=3 --frames --frame 2").unwrap();
        assert_eq!(a.command.as_deref(), Some("convert"));
        assert_eq!(a.positional, ["a.md", "b.csv"]);
        assert_eq!(a.value("output"), Some("out.png"));
        assert_eq!(a.value("scale"), Some("3"));
        assert_eq!(a.value("frame"), Some("2"));
        assert!(a.flag("frames"));
        assert!(!a.flag("help"));
    }

    #[test]
    fn short_and_stdin() {
        let a = parse("convert - -oout.pdf -h").unwrap();
        assert_eq!(a.positional, ["-"]);
        assert_eq!(a.value("output"), Some("out.pdf"));
        assert!(a.flag("help"));
    }

    #[test]
    fn errors() {
        assert!(parse("convert --nope").is_err());
        assert!(parse("convert -o").is_err());
    }
}
