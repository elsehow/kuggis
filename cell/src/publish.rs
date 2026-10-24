//! Publishing authority, not a deployment implementation.
//!
//! Namespace IDs are allocated by the operator/publisher and bound there to a
//! cell's pinned identity. They are not cell names, URLs, or bearer secrets.
//! The publisher must authenticate callers and load their approved grants; a
//! caller supplying any of these values proves neither ownership nor authority.
//! Ownership survives delegation and is never inferred from the latest receipt.

use crate::prim::{Hash, RequestId, SiteName, Url};
use crate::said::Key;
use serde::{Deserialize, Serialize};
pub mod artifact;
pub mod bridge;
pub mod client;
pub mod hosting;
pub mod operator;
pub mod private;
pub mod provision;
pub mod registry;
pub mod transport;
pub mod verify;

/// Opaque stable namespace identifier. Recreating a cell with the same name
/// must not automatically inherit an old namespace. Allocation and identity
/// binding are publisher responsibilities, not consequences of deserialization.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(transparent)]
pub struct NamespaceId(pub Hash);

/// A site is identified independently of its public hostname or current author.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct SiteId {
    pub namespace: NamespaceId,
    pub name: SiteName,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum PublishScope {
    /// Private sites in the caller’s own automatically allocated namespace.
    Private,
    /// Create and manage multiple sites in exactly this namespace.
    Namespace(NamespaceId),
    /// Manage an existing site only; does not confer namespace ownership or
    /// permission to create other sites.
    Site(SiteId),
}

impl PublishScope {
    /// No implicit tree/name prefix authority. To delegate a child's separate
    /// namespace, ancestors must explicitly hold authority for that namespace.
    pub fn covers(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Private, Self::Private) => true,
            (Self::Private, _) | (_, Self::Private) => false,
            (Self::Namespace(a), Self::Namespace(b)) => a == b,
            (Self::Namespace(a), Self::Site(b)) => a == &b.namespace,
            (Self::Site(a), Self::Site(b)) => a == b,
            (Self::Site(_), Self::Namespace(_)) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Private,
    #[default]
    Public,
}

impl Visibility {
    pub fn is_public(&self) -> bool {
        *self == Self::Public
    }
}

/// Data for a publisher-issued receipt. This value alone is not proof of a
/// deployment: it must be authenticated by the publisher/log transport.
/// The publishing cell's key can differ from the namespace owner's key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct PublicationReceipt {
    pub site: SiteId,
    #[ts(type = "string")]
    pub publisher: Key,
    // Unique publication event, distinct from the content digest (republishing
    // identical content can produce another version).
    pub version: RequestId,
    pub artifact: Hash,
    pub url: Url,
    // Missing on historical receipts, which were always public.
    #[serde(default, skip_serializing_if = "Visibility::is_public")]
    pub visibility: Visibility,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, Grant, Tree, TreeError};

    fn ns(seed: &str) -> NamespaceId {
        NamespaceId(Hash::of(seed.as_bytes()))
    }
    fn site(namespace: &str, name: &str) -> SiteId {
        SiteId {
            namespace: ns(namespace),
            name: name.parse().unwrap(),
        }
    }
    fn grant(scope: PublishScope) -> Grant {
        Grant::Publish(scope)
    }

    #[test]
    fn site_names_are_validated_on_parse_and_deserialization() {
        for valid in ["a", "news-viewer", "help", "123"] {
            assert!(valid.parse::<SiteName>().is_ok());
        }
        for bad in ["", "../news", "a/b", "A", "-a", "a-", "a.b", "a b", "é"] {
            assert!(bad.parse::<SiteName>().is_err());
            assert!(serde_json::from_value::<SiteName>(serde_json::json!(bad)).is_err());
        }
        assert!("a".repeat(63).parse::<SiteName>().is_ok());
        assert!("a".repeat(64).parse::<SiteName>().is_err());
        assert!(serde_json::from_str::<NamespaceId>("\"news\"").is_err());
    }

    #[test]
    fn authority_is_exact_and_directional() {
        let namespace = grant(PublishScope::Namespace(ns("owner")));
        let a = grant(PublishScope::Site(site("owner", "news")));
        let b = grant(PublishScope::Site(site("owner", "report")));
        let foreign = grant(PublishScope::Site(site("other", "news")));
        assert!(namespace.covers(&namespace));
        assert!(namespace.covers(&a));
        assert!(namespace.covers(&b));
        assert!(!namespace.covers(&foreign));
        assert!(!namespace.covers(&grant(PublishScope::Namespace(ns("other")))));
        assert!(a.covers(&a));
        assert!(!a.covers(&b));
        assert!(!a.covers(&foreign));
        assert!(!a.covers(&namespace));
        assert!(namespace.hosts().is_empty());
        assert!(!namespace.at_edge());
        let host = Grant::Host("example.com".parse().unwrap());
        assert!(!host.covers(&a));
        assert!(!a.covers(&host));
    }

    #[test]
    fn tree_enforces_delegation_without_transferring_ownership() {
        let owned_site = site("parent-namespace", "news");
        let parent = Cell::new(
            "parent".parse().unwrap(),
            [grant(PublishScope::Namespace(ns("parent-namespace")))],
        );
        let child = |scope| Cell::new("child".parse().unwrap(), [grant(scope)]);
        assert!(Tree::new([
            (parent.clone(), None),
            (
                child(PublishScope::Site(owned_site.clone())),
                Some(parent.name.clone())
            ),
        ])
        .is_ok());
        // The namespace stays the parent's, regardless of the publishing cell.
        assert_eq!(owned_site.namespace, ns("parent-namespace"));
        assert!(matches!(
            Tree::new([
                (parent.clone(), None),
                (
                    child(PublishScope::Namespace(ns("unapproved-child"))),
                    Some(parent.name.clone())
                ),
            ]),
            Err(TreeError::Wider { .. })
        ));
        let limited = Cell::new(parent.name.clone(), [grant(PublishScope::Site(owned_site))]);
        assert!(matches!(
            Tree::new([
                (limited, None),
                (
                    child(PublishScope::Namespace(ns("parent-namespace"))),
                    Some(parent.name)
                ),
            ]),
            Err(TreeError::Wider { .. })
        ));
    }

    #[test]
    fn wire_types_roundtrip_and_reject_extra_authority() {
        let namespace = grant(PublishScope::Namespace(ns("owner")));
        let json = serde_json::to_string(&namespace).unwrap();
        assert_eq!(serde_json::from_str::<Grant>(&json).unwrap(), namespace);
        assert!(serde_json::from_str::<Grant>(r#"{"publish":{"all":true}}"#).is_err());
        let site = site("owner", "news");
        let g = grant(PublishScope::Site(site.clone()));
        let value = serde_json::to_value(&g).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"publish": {"site": {
                "namespace": ns("owner"), "name": "news"
            }}})
        );
        assert_eq!(serde_json::from_value::<Grant>(value.clone()).unwrap(), g);
        let mut bad = value;
        bad["publish"]["site"]["owner"] = serde_json::json!("impostor");
        assert!(serde_json::from_value::<Grant>(bad).is_err());
        let receipt = PublicationReceipt {
            site,
            publisher: crate::Secret::generate().public(),
            version: RequestId::generate(),
            artifact: Hash::of(b"artifact"),
            url: "https://news.example.com/".parse().unwrap(),
            visibility: Visibility::Public,
        };
        let value = serde_json::to_value(&receipt).unwrap();
        assert_eq!(
            serde_json::from_value::<PublicationReceipt>(value.clone()).unwrap(),
            receipt
        );
        let mut bad = value;
        bad["url"] = serde_json::json!("http://news.example.com/");
        assert!(serde_json::from_value::<PublicationReceipt>(bad).is_err());
    }
}
