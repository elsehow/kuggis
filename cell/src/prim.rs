//! Parsed primitives. Each has exactly one constructor, its parser, so a
//! value of one of these types can only exist if it passed the check at the
//! edge. Deserializing goes through the same parser, so a cell file, a
//! statement or a reply with a bad value fails to load rather than loading
//! wrong.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

/// Why a string was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub what: &'static str,
    pub why: &'static str,
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.what, self.why)
    }
}

impl std::error::Error for Invalid {}

/// Words the `cell` binary uses as subcommands. A cell or session may not
/// take one of these as its name.
pub const RESERVED: &[&str] = &[
    "cell", "key", "say", "open", "allow", "tree", "plan", "observe", "proxy", "help",
];

macro_rules! text {
    ($(#[$meta:meta])* $t:ident, $what:literal, $check:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $t(String);

        impl $t {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $t {
            type Err = Invalid;
            fn from_str(s: &str) -> Result<Self, Invalid> {
                $check(s).map($t).map_err(|why| Invalid { what: $what, why })
            }
        }

        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($what, "({:?})"), self.0)
            }
        }

        impl AsRef<str> for $t {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Serialize for $t {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

text!(
    /// A cell, session or integration name: `[a-z0-9][a-z0-9-]*[a-z0-9]`, at
    /// most 63 bytes, not a command word. Doubles as a DNS label.
    Name, "name", name
);
text!(
    /// A hostname the proxy may tunnel to: lowercase DNS labels, no port, no
    /// path, no trailing dot, never an address.
    Host, "host", host
);
text!(
    /// A GitHub repository, `owner/name`, lowercased for comparison.
    Repo, "repo", repo
);
text!(
    /// An https URL with a named host, no userinfo, no control bytes.
    Url, "url", url
);

fn name(s: &str) -> Result<String, &'static str> {
    let b = s.as_bytes();
    if b.is_empty() {
        return Err("empty");
    }
    if b.len() > 63 {
        return Err("longer than 63");
    }
    if !b
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
    {
        return Err("only a-z, 0-9 and -");
    }
    if b[0] == b'-' || b[b.len() - 1] == b'-' {
        return Err("starts or ends with -");
    }
    if RESERVED.contains(&s) {
        return Err("a command word");
    }
    Ok(s.to_string())
}

fn host(s: &str) -> Result<String, &'static str> {
    if s.is_empty() {
        return Err("empty");
    }
    if s.len() > 253 {
        return Err("longer than 253");
    }
    if s.ends_with('.') {
        return Err("trailing dot");
    }
    for label in s.split('.') {
        let b = label.as_bytes();
        if b.is_empty() {
            return Err("empty label");
        }
        if b.len() > 63 {
            return Err("label longer than 63");
        }
        if !b
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        {
            return Err("only a-z, 0-9, - and .");
        }
        if b[0] == b'-' || b[b.len() - 1] == b'-' {
            return Err("label starts or ends with -");
        }
    }
    if s.split('.').all(|l| l.bytes().all(|c| c.is_ascii_digit())) {
        return Err("an address, not a name");
    }
    Ok(s.to_string())
}

fn repo(s: &str) -> Result<String, &'static str> {
    let (owner, name) = s.split_once('/').ok_or("needs owner/name")?;
    for part in [owner, name] {
        if part.is_empty() {
            return Err("empty owner or name");
        }
        if part.len() > 100 {
            return Err("part longer than 100");
        }
        if part.contains('/') {
            return Err("more than one /");
        }
        if !part
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        {
            return Err("only letters, digits, _ . and -");
        }
        if part == "." || part == ".." {
            return Err("dot name");
        }
    }
    Ok(s.to_ascii_lowercase())
}

fn url(s: &str) -> Result<String, &'static str> {
    if s.len() > 2048 {
        return Err("longer than 2048");
    }
    if s.bytes().any(|c| c <= 0x20 || c >= 0x7f) {
        return Err("control, space or non-ascii byte");
    }
    let rest = s.strip_prefix("https://").ok_or("not https")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.contains('@') {
        return Err("userinfo");
    }
    if authority.starts_with('[') {
        return Err("address literal");
    }
    let (h, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    let h = h.to_ascii_lowercase();
    host(&h)?;
    let port = match port {
        None => String::new(),
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n != 0 && p.bytes().all(|c| c.is_ascii_digit()) => format!(":{n}"),
            _ => return Err("bad port"),
        },
    };
    Ok(format!("https://{h}{port}{tail}"))
}

impl Url {
    /// The host part, already validated.
    pub fn host(&self) -> Host {
        let rest = &self.0["https://".len()..];
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        let h = authority
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(authority);
        h.parse().expect("validated at construction")
    }
}

macro_rules! digest {
    ($(#[$meta:meta])* $t:ident, $what:literal, $n:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $t([u8; $n]);

        impl $t {
            pub fn as_bytes(&self) -> &[u8; $n] {
                &self.0
            }
        }

        impl From<[u8; $n]> for $t {
            fn from(b: [u8; $n]) -> Self {
                $t(b)
            }
        }

        impl FromStr for $t {
            type Err = Invalid;
            fn from_str(s: &str) -> Result<Self, Invalid> {
                hex_bytes::<$n>(s).map($t).map_err(|why| Invalid { what: $what, why })
            }
        }

        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                for b in &self.0 {
                    write!(f, "{b:02x}")?;
                }
                Ok(())
            }
        }

        impl fmt::Debug for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($what, "({})"), self)
            }
        }

        impl Serialize for $t {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

digest!(
    /// A git commit id: 40 lowercase hex digits.
    Commit, "commit", 20
);
digest!(
    /// A SHA-256 digest: 64 lowercase hex digits. Binds approvals to plans and
    /// replies to statements.
    Hash, "hash", 32
);

impl Hash {
    pub const ZERO: Hash = Hash([0; 32]);

    pub fn of(bytes: &[u8]) -> Hash {
        Hash(Sha256::digest(bytes).into())
    }
}

pub(crate) fn hex_bytes<const N: usize>(s: &str) -> Result<[u8; N], &'static str> {
    if s.len() != 2 * N {
        return Err("wrong length");
    }
    let mut out = [0u8; N];
    for (i, pair) in s.as_bytes().chunks(2).enumerate() {
        let nib = |c: u8| match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            _ => Err("only lowercase hex"),
        };
        out[i] = (nib(pair[0])? << 4) | nib(pair[1])?;
    }
    Ok(out)
}

pub(crate) fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bad<T: FromStr<Err = Invalid> + fmt::Debug>(s: &str) -> &'static str {
        s.parse::<T>().expect_err("should be refused").why
    }

    #[test]
    fn names() {
        assert!("forecasting".parse::<Name>().is_ok());
        assert!("a".parse::<Name>().is_ok());
        assert!("fix-scoring-2".parse::<Name>().is_ok());
        assert_eq!(bad::<Name>(""), "empty");
        assert_eq!(bad::<Name>("Forecasting"), "only a-z, 0-9 and -");
        assert_eq!(bad::<Name>("-x"), "starts or ends with -");
        assert_eq!(bad::<Name>("x-"), "starts or ends with -");
        assert_eq!(bad::<Name>("a b"), "only a-z, 0-9 and -");
        assert_eq!(bad::<Name>("a/b"), "only a-z, 0-9 and -");
        assert_eq!(bad::<Name>("plan"), "a command word");
        assert_eq!(bad::<Name>(&"a".repeat(64)), "longer than 63");
        // confusables and invisibles never pass the byte check
        assert_eq!(bad::<Name>("forecаsting"), "only a-z, 0-9 and -"); // cyrillic а
        assert_eq!(bad::<Name>("fore\u{200b}casting"), "only a-z, 0-9 and -");
    }

    #[test]
    fn hosts() {
        assert!("api.anthropic.com".parse::<Host>().is_ok());
        assert!("localhost".parse::<Host>().is_ok());
        assert!("xn--80ak6aa92e.com".parse::<Host>().is_ok());
        assert_eq!(bad::<Host>("API.anthropic.com"), "only a-z, 0-9, - and .");
        assert_eq!(bad::<Host>("api.anthropic.com."), "trailing dot");
        assert_eq!(bad::<Host>("api..anthropic.com"), "empty label");
        assert_eq!(bad::<Host>("169.254.169.254"), "an address, not a name");
        assert_eq!(bad::<Host>("1234"), "an address, not a name");
        assert_eq!(bad::<Host>("[::1]"), "only a-z, 0-9, - and .");
        assert_eq!(bad::<Host>("::1"), "only a-z, 0-9, - and .");
        assert_eq!(
            bad::<Host>("api.anthropic.com:443"),
            "only a-z, 0-9, - and ."
        );
        assert_eq!(bad::<Host>("evil.com\r\nx"), "only a-z, 0-9, - and .");
        assert_eq!(bad::<Host>("a_b.com"), "only a-z, 0-9, - and .");
        assert_eq!(bad::<Host>("-a.com"), "label starts or ends with -");
        assert_eq!(
            bad::<Host>(&format!("{}.com", "a".repeat(64))),
            "label longer than 63"
        );
        let long = format!(
            "{}.com",
            [
                "a".repeat(60),
                "a".repeat(60),
                "a".repeat(60),
                "a".repeat(60),
                "a".repeat(60)
            ]
            .join(".")
        );
        assert_eq!(bad::<Host>(&long), "longer than 253");
    }

    #[test]
    fn repos() {
        assert_eq!(
            "elsehow/BigBrain".parse::<Repo>().unwrap().as_str(),
            "elsehow/bigbrain"
        );
        assert_eq!(bad::<Repo>("elsehow"), "needs owner/name");
        assert_eq!(bad::<Repo>("elsehow/"), "empty owner or name");
        assert_eq!(bad::<Repo>("a/b/c"), "more than one /");
        assert_eq!(bad::<Repo>("../x"), "dot name");
        assert_eq!(bad::<Repo>("a/b c"), "only letters, digits, _ . and -");
        assert_eq!(bad::<Repo>("a/b\n"), "only letters, digits, _ . and -");
    }

    #[test]
    fn urls() {
        let u: Url = "https://Docs.Python.org/3/library/json.html?x=1#y"
            .parse()
            .unwrap();
        assert_eq!(
            u.as_str(),
            "https://docs.python.org/3/library/json.html?x=1#y"
        );
        assert_eq!(u.host().as_str(), "docs.python.org");
        let u: Url = "https://example.com:8443/p".parse().unwrap();
        assert_eq!(u.host().as_str(), "example.com");
        assert_eq!(bad::<Url>("http://example.com/"), "not https");
        assert_eq!(bad::<Url>("https://user:pw@example.com/"), "userinfo");
        assert_eq!(
            bad::<Url>("https://169.254.169.254/latest"),
            "an address, not a name"
        );
        assert_eq!(bad::<Url>("https://[::1]/"), "address literal");
        assert_eq!(bad::<Url>("https://example.com:0/"), "bad port");
        assert_eq!(bad::<Url>("https://example.com:65536/"), "bad port");
        assert_eq!(bad::<Url>("https://example.com:+1/"), "bad port");
        assert_eq!(
            bad::<Url>("https://example.com/a b"),
            "control, space or non-ascii byte"
        );
        assert_eq!(
            bad::<Url>("https://example.com/\u{e9}"),
            "control, space or non-ascii byte"
        );
        assert!("https://example.com/%0d%0a".parse::<Url>().is_ok());
    }

    #[test]
    fn digests() {
        let h = Hash::of(b"");
        assert_eq!(
            h.to_string(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(h.to_string().parse::<Hash>().unwrap(), h);
        assert_eq!(
            bad::<Hash>(&h.to_string().to_uppercase()),
            "only lowercase hex"
        );
        assert_eq!(bad::<Hash>("abc"), "wrong length");
        assert_eq!(bad::<Commit>(&"a".repeat(64)), "wrong length");
        assert!("a".repeat(40).parse::<Commit>().is_ok());
        assert_ne!(Hash::ZERO, h);
    }

    #[test]
    fn serde_goes_through_the_parser() {
        assert!(serde_json::from_str::<Host>("\"169.254.169.254\"").is_err());
        assert!(serde_json::from_str::<Name>("\"Plan\"").is_err());
        let h: Host = serde_json::from_str("\"pypi.org\"").unwrap();
        assert_eq!(serde_json::to_string(&h).unwrap(), "\"pypi.org\"");
    }
}
