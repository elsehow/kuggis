//! What a cell says, signed. One envelope, [`Statement`], and one enum of
//! things it can hold, [`Said`]. The envelope's contents are private: the
//! only way to read them is [`Statement::open`] with the cell's pinned
//! public key, so verifying is not a step anyone can skip.

use crate::canon;
use crate::grant::Harness;
use crate::prim::{hex_bytes, hex_string, Commit, Hash, Invalid, Name, Repo, Url};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// Where a session is. The four in the middle page you.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Starting,
    Working,
    Idle,
    Question,
    Permission,
    Blocked,
    Done,
    Stopped,
}

impl State {
    pub fn pages(&self) -> bool {
        matches!(
            self,
            State::Question | State::Permission | State::Blocked | State::Done
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Said {
    /// A session changed state, and what it says, when it has something
    /// to say: the question, the permission asked for, what blocked it.
    Session {
        name: Name,
        harness: Harness,
        state: State,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// Text that wants to cross into another cell. Waits for you.
    Brief {
        to: Name,
        session: Name,
        text: String,
    },
    /// A page the cell wants read. Waits for you unless you vouched for it.
    Fetch { url: Url },
    /// A review verdict on an exact head. The parent cell says this; the
    /// repo's rules accept it only from the parent's key.
    Verdict {
        repo: Repo,
        head: Commit,
        pass: bool,
    },
    /// The wall check from inside, and which cell the box is running.
    Checked {
        cell: Hash,
        ok: bool,
        report: String,
    },
    /// An answer to a statement that waited, bound to its hash. You say
    /// this over ssh; a parent says it through the door.
    Reply {
        of: Hash,
        body: Result<String, String>,
    },
}

impl Said {
    pub fn reply_to(statement: &Statement, body: Result<String, String>) -> Said {
        Said::Reply {
            of: statement.hash(),
            body,
        }
    }

    pub fn answers(&self, statement: &Statement) -> bool {
        matches!(self, Said::Reply { of, .. } if *of == statement.hash())
    }
}

/// The signing half, generated inside the root and never leaving it.
#[derive(Clone)]
pub struct Secret(SigningKey);

/// The verifying half, pinned by the laptop and registered where verdicts
/// are checked.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Key(VerifyingKey);

#[derive(Clone, Copy, PartialEq, Eq)]
struct Sig(Signature);

impl Secret {
    pub fn generate() -> Secret {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).expect("the OS random source");
        Secret(SigningKey::from_bytes(&seed))
    }

    pub fn public(&self) -> Key {
        Key(self.0.verifying_key())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(for {})", self.public())
    }
}

impl FromStr for Secret {
    type Err = Invalid;
    fn from_str(s: &str) -> Result<Self, Invalid> {
        hex_bytes::<32>(s.trim())
            .map(|b| Secret(SigningKey::from_bytes(&b)))
            .map_err(|why| Invalid {
                what: "secret",
                why,
            })
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex_string(&self.0.to_bytes()))
    }
}

impl FromStr for Key {
    type Err = Invalid;
    fn from_str(s: &str) -> Result<Self, Invalid> {
        let b = hex_bytes::<32>(s.trim()).map_err(|why| Invalid { what: "key", why })?;
        VerifyingKey::from_bytes(&b).map(Key).map_err(|_| Invalid {
            what: "key",
            why: "not a point on the curve",
        })
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex_string(self.0.as_bytes()))
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Key({self})")
    }
}

impl Serialize for Key {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl fmt::Debug for Sig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sig({}..)", &hex_string(&self.0.to_bytes())[..16])
    }
}

impl Serialize for Sig {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex_string(&self.0.to_bytes()))
    }
}

impl<'de> Deserialize<'de> for Sig {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let b = hex_bytes::<64>(&s).map_err(serde::de::Error::custom)?;
        Ok(Sig(Signature::from_bytes(&b)))
    }
}

/// The bytes that are signed: cell, sequence and content, canonical.
#[derive(Serialize)]
struct Body<'a> {
    cell: &'a Name,
    seq: u64,
    said: &'a Said,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    pub cell: Name,
    pub seq: u64,
    said: Said,
    sig: Sig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unverified;

impl fmt::Display for Unverified {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the signature does not verify under that key")
    }
}

impl std::error::Error for Unverified {}

impl Statement {
    pub fn sign(secret: &Secret, cell: Name, seq: u64, said: Said) -> Statement {
        let bytes = canon::bytes(&Body {
            cell: &cell,
            seq,
            said: &said,
        });
        let sig = Sig(secret.0.sign(&bytes));
        Statement {
            cell,
            seq,
            said,
            sig,
        }
    }

    /// The only accessor. A statement that does not verify under `key`
    /// yields nothing, however well formed it looks.
    pub fn open(&self, key: &Key) -> Result<&Said, Unverified> {
        let bytes = canon::bytes(&Body {
            cell: &self.cell,
            seq: self.seq,
            said: &self.said,
        });
        key.0
            .verify_strict(&bytes, &self.sig.0)
            .map(|()| &self.said)
            .map_err(|_| Unverified)
    }

    /// Identity of this exact statement, signature included. Replies bind to it.
    pub fn hash(&self) -> Hash {
        Hash::of(&canon::bytes(self))
    }

    /// Sequence discipline: the first statement a cell makes is 1, and
    /// every later one is the previous plus one. Gaps and repeats are
    /// refused at the laptop.
    pub fn follows(&self, last: Option<u64>) -> bool {
        last.map_or(Some(1), |n| n.checked_add(1)) == Some(self.seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said() -> Said {
        Said::Verdict {
            repo: "elsehow/airo".parse().unwrap(),
            head: "a".repeat(40).parse().unwrap(),
            pass: true,
        }
    }

    #[test]
    fn round_trip() {
        let k = Secret::generate();
        let s = Statement::sign(&k, "airo".parse().unwrap(), 1, said());
        assert_eq!(s.open(&k.public()).unwrap(), &said());
        let json = serde_json::to_string(&s).unwrap();
        let back: Statement = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.open(&k.public()).unwrap(), &said());
        assert_eq!(back.hash(), s.hash());
    }

    #[test]
    fn wrong_key_tampered_body_and_borrowed_signature_all_fail() {
        let k = Secret::generate();
        let other = Secret::generate();
        let s = Statement::sign(&k, "airo".parse().unwrap(), 1, said());
        assert_eq!(s.open(&other.public()), Err(Unverified));

        let mut json: serde_json::Value = serde_json::to_value(&s).unwrap();
        json["said"]["verdict"]["pass"] = serde_json::Value::Bool(false);
        let t: Statement = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(t.open(&k.public()), Err(Unverified));

        json["said"]["verdict"]["pass"] = serde_json::Value::Bool(true);
        json["cell"] = serde_json::Value::String("forecasting".into());
        let t: Statement = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(t.open(&k.public()), Err(Unverified));

        json["cell"] = serde_json::Value::String("airo".into());
        json["seq"] = serde_json::Value::from(2);
        let t: Statement = serde_json::from_value(json).unwrap();
        assert_eq!(t.open(&k.public()), Err(Unverified));
    }

    #[test]
    fn malformed_statements_never_load() {
        let k = Secret::generate();
        let s = Statement::sign(&k, "airo".parse().unwrap(), 1, said());
        let mut json: serde_json::Value = serde_json::to_value(&s).unwrap();
        json["extra"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<Statement>(json.clone()).is_err());
        json.as_object_mut().unwrap().remove("extra");
        json["sig"] = serde_json::Value::String("zz".repeat(64));
        assert!(serde_json::from_value::<Statement>(json.clone()).is_err());
        json["sig"] = serde_json::Value::String("ab".repeat(63));
        assert!(serde_json::from_value::<Statement>(json.clone()).is_err());
        json["sig"] = serde_json::Value::String("ab".repeat(64));
        json["said"] = serde_json::json!({"fetch": {"url": "http://example.com"}});
        assert!(serde_json::from_value::<Statement>(json.clone()).is_err());
        json["said"] = serde_json::json!({"fetch": {"url": "https://example.com", "and": 1}});
        assert!(serde_json::from_value::<Statement>(json).is_err());
    }

    #[test]
    fn replies_bind_to_the_exact_statement() {
        let k = Secret::generate();
        let a = Statement::sign(
            &k,
            "t".parse().unwrap(),
            1,
            Said::Fetch {
                url: "https://example.com/a".parse().unwrap(),
            },
        );
        let b = Statement::sign(
            &k,
            "t".parse().unwrap(),
            2,
            Said::Fetch {
                url: "https://example.com/a".parse().unwrap(),
            },
        );
        let r = Said::reply_to(&a, Ok("page".into()));
        assert!(r.answers(&a));
        assert!(!r.answers(&b));
        assert!(!said().answers(&a), "only a reply answers");
        // a reply is a said like any other: it signs, and it round-trips
        let s = Statement::sign(&k, "p".parse().unwrap(), 1, r.clone());
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""reply":{"of":""#), "{json}");
        let back: Statement = serde_json::from_str(&json).unwrap();
        assert_eq!(back.open(&k.public()).unwrap(), &r);
    }

    #[test]
    fn sequence_discipline() {
        let k = Secret::generate();
        let s = |n| Statement::sign(&k, "t".parse().unwrap(), n, said());
        assert!(s(1).follows(None));
        assert!(!s(0).follows(None));
        assert!(!s(2).follows(None));
        assert!(s(5).follows(Some(4)));
        assert!(!s(4).follows(Some(4)));
        assert!(!s(7).follows(Some(4)));
        assert!(!s(u64::MAX).follows(Some(u64::MAX)));
    }

    #[test]
    fn keys_parse_strictly() {
        let k = Secret::generate();
        let hex = k.public().to_string();
        assert_eq!(hex.parse::<Key>().unwrap(), k.public());
        assert!(hex.to_uppercase().parse::<Key>().is_err());
        assert!("00".repeat(31).parse::<Key>().is_err());
        assert_eq!(
            k.to_string().parse::<Secret>().unwrap().public(),
            k.public()
        );
        assert!(!format!("{k:?}").contains(&k.to_string()));
    }

    #[test]
    fn signed_bytes_are_stable_across_field_order() {
        // A statement rebuilt from JSON with keys in another order signs identically.
        let k = Secret::generate();
        let s = Statement::sign(
            &k,
            "t".parse().unwrap(),
            3,
            Said::Session {
                name: "x".parse().unwrap(),
                harness: Harness::Pi,
                state: State::Idle,
                text: None,
            },
        );
        let shuffled = r#"{"sig":"SIG","said":{"session":{"state":"idle","harness":"pi","name":"x"}},"seq":3,"cell":"t"}"#;
        let json: serde_json::Value = serde_json::to_value(&s).unwrap();
        assert!(
            !json["said"]["session"]
                .as_object()
                .unwrap()
                .contains_key("text"),
            "a session with nothing to say carries no text"
        );
        let shuffled = shuffled.replace("SIG", json["sig"].as_str().unwrap());
        let back: Statement = serde_json::from_str(&shuffled).unwrap();
        assert!(back.open(&k.public()).is_ok());
        assert_eq!(back.hash(), s.hash());
    }

    #[test]
    fn a_question_carries_its_text() {
        let k = Secret::generate();
        let s = Statement::sign(
            &k,
            "t".parse().unwrap(),
            1,
            Said::Session {
                name: "x".parse().unwrap(),
                harness: Harness::Claude,
                state: State::Question,
                text: Some("merge or rebase?".into()),
            },
        );
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""text":"merge or rebase?""#));
        let back: Statement = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.open(&k.public()).unwrap(),
            &s.open(&k.public()).unwrap().clone()
        );
        assert!(serde_json::from_str::<Said>(
            r#"{"session":{"name":"x","harness":"claude","state":"question","text":null}}"#
        )
        .is_ok());
    }
}
