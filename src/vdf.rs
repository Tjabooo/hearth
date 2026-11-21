//! Minimal parser for Valve's text KeyValues (.vdf / .acf).

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Vdf {
    Str(String),
    Map(BTreeMap<String, Vdf>),
}

impl Vdf {
    /// Case-insensitive child lookup.
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Vdf::Map(m) => m
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Vdf::Str(_) => None,
        }
    }
    pub fn path(&self, keys: &[&str]) -> Option<&Vdf> {
        keys.iter().try_fold(self, |v, k| v.get(k))
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s),
            Vdf::Map(_) => None,
        }
    }
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(|v| v.str())
    }
    pub fn map(&self) -> Option<&BTreeMap<String, Vdf>> {
        match self {
            Vdf::Map(m) => Some(m),
            Vdf::Str(_) => None,
        }
    }
}

pub fn parse(text: &str) -> Vdf {
    let tokens = tokenize(text);
    let mut i = 0;
    Vdf::Map(parse_map(&tokens, &mut i))
}

#[derive(Debug)]
enum Tok {
    S(String),
    Open,
    Close,
}

fn tokenize(t: &str) -> Vec<Tok> {
    let mut out = vec![];
    let mut it = t.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                let mut s = String::new();
                while let Some(c) = it.next() {
                    match c {
                        '\\' => {
                            if let Some(n) = it.next() {
                                s.push(match n {
                                    'n' => '\n',
                                    't' => '\t',
                                    o => o,
                                });
                            }
                        }
                        '"' => break,
                        o => s.push(o),
                    }
                }
                out.push(Tok::S(s));
            }
            '{' => out.push(Tok::Open),
            '}' => out.push(Tok::Close),
            '/' if it.peek() == Some(&'/') => {
                for c in it.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => {}
            c => {
                // Unquoted token.
                let mut s = String::from(c);
                while let Some(&n) = it.peek() {
                    if n.is_whitespace() || n == '{' || n == '}' || n == '"' {
                        break;
                    }
                    s.push(n);
                    it.next();
                }
                out.push(Tok::S(s));
            }
        }
    }
    out
}

fn parse_map(t: &[Tok], i: &mut usize) -> BTreeMap<String, Vdf> {
    let mut m = BTreeMap::new();
    while *i < t.len() {
        match &t[*i] {
            Tok::Close => {
                *i += 1;
                return m;
            }
            Tok::Open => {
                *i += 1;
            }
            Tok::S(k) => {
                let k = k.clone();
                *i += 1;
                match t.get(*i) {
                    Some(Tok::S(v)) => {
                        m.insert(k, Vdf::Str(v.clone()));
                        *i += 1;
                    }
                    Some(Tok::Open) => {
                        *i += 1;
                        let sub = parse_map(t, i);
                        m.insert(k, Vdf::Map(sub));
                    }
                    _ => {}
                }
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses() {
        let v = super::parse(
            "\"AppState\"\n{\n\t\"appid\"\t\t\"730\"\n\t\"name\" \"Counter-Strike 2\" // c\n\t\"sub\" { \"a\" \"b\" }\n}",
        );
        assert_eq!(
            v.path(&["appstate", "appid"]).and_then(|v| v.str()),
            Some("730")
        );
        assert_eq!(
            v.path(&["AppState", "sub", "a"]).and_then(|v| v.str()),
            Some("b")
        );
    }
}
