//! Trusted publisher state. This is not an HTTP API or caller authentication.
//!
//! Only the operator may create namespaces or replace the approved policy. A
//! signed transport verifies key possession; the approved policy determines
//! authority. Direct trusted-code methods must never accept caller-claimed keys.
//! Keep this directory inaccessible to agents and serve no files from it.
//!
//! Every operation reloads policy under a cross-process lock. No authorization
//! tokens are issued: future deployment mutations must check policy again at
//! commit, under this same transaction boundary, not rely on an earlier check.

use super::artifact::{self, Artifact};
use crate::resource::{CellIdentity, ResourceId, ResourceOperation, ResourceRecord};
pub mod resources;
use super::{NamespaceId, PublicationReceipt, PublishScope, SiteId};
use crate::prim::SiteName;
use crate::prim::Url;
use crate::{CellId, Grant, Hash, Key, RequestId, Tree};
use resources::CleanupReceipt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum RegistryError {
    Io(io::Error),
    InvalidState(String),
    InvalidPolicy(String),
    Denied,
    UnknownNamespace,
    UnknownSite,
    SiteExists,
    Unauthenticated,
    Replay,
    Busy,
    InvalidArtifact(String),
}
impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "publisher registry: {e}"),
            Self::InvalidState(e) => write!(f, "invalid publisher state: {e}"),
            Self::InvalidPolicy(e) => write!(f, "invalid publishing policy: {e}"),
            Self::Denied => f.write_str("publishing authority denied"),
            Self::UnknownNamespace => f.write_str("unknown publishing namespace"),
            Self::UnknownSite => f.write_str("unknown publication site"),
            Self::SiteExists => f.write_str("publication site already exists"),
            Self::Unauthenticated => f.write_str("invalid publisher request"),
            Self::Replay => f.write_str("publisher request already used"),
            Self::InvalidArtifact(e) => write!(f, "invalid artifact: {e}"),
            Self::Busy => f.write_str("publisher replay capacity reached"),
        }
    }
}
impl std::error::Error for RegistryError {}
impl From<io::Error> for RegistryError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
type Result<T> = std::result::Result<T, RegistryError>;

/// All these actions require the same site-management authority. Status is
/// private metadata even though a published site's content is public.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteAction {
    Publish,
    Rollback,
    Unpublish,
    Status,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Namespace {
    owner: Key,
    // Retain the reservation after unpublishing. Unpublishing must never let
    // another cell claim the site's identity or URL.
    sites: BTreeSet<SiteName>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Authority {
    key: Key,
    scopes: BTreeSet<PublishScope>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    format: u32,
    audience: RequestId,
    /// Successful request IDs, bound to caller key, retained until expiration.
    used_requests: BTreeMap<Hash, u64>,
    namespaces: BTreeMap<NamespaceId, Namespace>,
    authorities: Vec<Authority>,
    #[serde(default)]
    public_base: Option<Url>,
    #[serde(default)]
    publications: BTreeMap<Hash, History>,
    resources: BTreeMap<ResourceId, ResourceRecord>,
    resource_operations: BTreeMap<RequestId, ResourceOperation>,
    frozen: Vec<CellIdentity>,
    cleanups: BTreeMap<RequestId, CleanupReceipt>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    site: SiteId,
    versions: BTreeMap<RequestId, PublicationReceipt>,
    active: Option<RequestId>,
}
// The pre-transport registry had no audience or accepted network requests.
// Preserve its ownership/policy on upgrade; never reconstruct either from names.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyState {
    format: u32,
    namespaces: BTreeMap<NamespaceId, Namespace>,
    authorities: Vec<Authority>,
}

impl State {
    fn policy(&self, tree: &Tree, pins: &BTreeMap<CellId, Key>) -> Result<Vec<Authority>> {
        let mut authorities = vec![];
        let mut keys = BTreeSet::new();
        for cell in tree.iter() {
            let scopes: BTreeSet<_> = cell
                .grants
                .iter()
                .filter_map(|g| match g {
                    Grant::Publish(scope) => Some(scope.clone()),
                    _ => None,
                })
                .collect();
            let key = *pins.get(&cell.id).ok_or_else(|| {
                RegistryError::InvalidPolicy(format!("missing pin for {}", cell.id))
            })?;
            if !keys.insert(key.to_string()) {
                return Err(RegistryError::InvalidPolicy(
                    "cells must have distinct pinned identities".into(),
                ));
            }
            if scopes.is_empty() {
                continue;
            }
            if self.frozen.contains(&CellIdentity(key)) {
                return Err(RegistryError::InvalidPolicy(
                    "cannot restore grants to a deleted/frozen identity".into(),
                ));
            }
            for scope in &scopes {
                self.check_scope(scope)?;
            }
            authorities.push(Authority { key, scopes });
        }
        Ok(authorities)
    }
    fn reserve(&mut self, caller: Key, site: &SiteId) -> Result<()> {
        if !self.allows(caller, &PublishScope::Namespace(site.namespace.clone())) {
            return Err(RegistryError::Denied);
        }
        let namespace = self
            .namespaces
            .get_mut(&site.namespace)
            .ok_or(RegistryError::UnknownNamespace)?;
        if namespace.sites.len() >= 64 && !namespace.sites.contains(&site.name) {
            return Err(RegistryError::Busy);
        }
        if self.frozen.contains(&CellIdentity(namespace.owner)) {
            return Err(RegistryError::Denied);
        }
        if !namespace.sites.insert(site.name.clone()) {
            return Err(RegistryError::SiteExists);
        }
        self.add_site_resource(site);
        Ok(())
    }
    fn validate(&self) -> Result<()> {
        if self.format != 3 {
            return Err(RegistryError::InvalidState("unsupported format".into()));
        }
        self.validate_resources()?;
        for (key, history) in &self.publications {
            if *key != Hash::of(&crate::canon::bytes(&history.site)) || history.versions.len() > 32
            {
                return Err(RegistryError::InvalidState(
                    "invalid publication history".into(),
                ));
            }
            self.check_scope(&PublishScope::Site(history.site.clone()))?;
            if history
                .active
                .is_some_and(|v| !history.versions.contains_key(&v))
            {
                return Err(RegistryError::InvalidState(
                    "active publication is missing".into(),
                ));
            }
            for (version, receipt) in &history.versions {
                if version != &receipt.version || receipt.site != history.site {
                    return Err(RegistryError::InvalidState(
                        "publication identity mismatch".into(),
                    ));
                }
            }
        }
        let mut keys = BTreeSet::new();
        for authority in &self.authorities {
            if !keys.insert(authority.key.to_string()) {
                return Err(RegistryError::InvalidState(
                    "duplicate authority key".into(),
                ));
            }
            for scope in &authority.scopes {
                self.check_scope(scope)?;
            }
        }
        Ok(())
    }
    fn check_scope(&self, scope: &PublishScope) -> Result<()> {
        let id = match scope {
            PublishScope::Namespace(id) => id,
            PublishScope::Site(site) => &site.namespace,
        };
        let namespace = self
            .namespaces
            .get(id)
            .ok_or(RegistryError::UnknownNamespace)?;
        if let PublishScope::Site(site) = scope {
            if !namespace.sites.contains(&site.name) {
                return Err(RegistryError::UnknownSite);
            }
        }
        Ok(())
    }
    fn allows(&self, caller: Key, scope: &PublishScope) -> bool {
        !self.frozen.contains(&CellIdentity(caller))
            && self
                .authorities
                .iter()
                .any(|a| a.key == caller && a.scopes.iter().any(|s| s.covers(scope)))
    }
    fn check_site(&self, caller: Key, site: &SiteId) -> Result<()> {
        let scope = PublishScope::Site(site.clone());
        // Check authority before revealing site existence.
        if !self.allows(caller, &scope) {
            return Err(RegistryError::Denied);
        }
        self.check_scope(&scope)?;
        if !self.effective_site_access(caller, site) {
            return Err(RegistryError::Denied);
        }
        Ok(())
    }
}

/// Local trusted-code interface. Possessing a Registry does not authenticate a
/// network caller. Operator-only methods must never be exposed to cell RPCs.
/// Instances may coexist: the separate lock inode survives state replacement.
pub struct Registry {
    directory: PathBuf,
}
impl Registry {
    /// Initialize a NEW private directory. Never resets existing state.
    pub fn create(directory: &Path) -> Result<Self> {
        fs::DirBuilder::new().mode(0o700).create(directory)?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("lock"))?
            .sync_all()?;
        let registry = Self {
            directory: directory.into(),
        };
        let state = State {
            format: 3,
            audience: RequestId::generate(),
            used_requests: BTreeMap::new(),
            namespaces: BTreeMap::new(),
            authorities: vec![],
            public_base: None,
            publications: BTreeMap::new(),
            resources: BTreeMap::new(),
            resource_operations: BTreeMap::new(),
            frozen: vec![],
            cleanups: BTreeMap::new(),
        };
        registry.store(&state)?;
        // Persist creation of the registry directory as well as its contents.
        File::open(
            directory
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        Ok(registry)
    }
    /// Open existing state; missing/corrupt state fails closed, never resets.
    pub fn open(directory: &Path) -> Result<Self> {
        let registry = Self {
            directory: directory.into(),
        };
        registry.transaction(|_| Ok(((), false)))?;
        Ok(registry)
    }
    fn transaction<T>(&self, run: impl FnOnce(&mut State) -> Result<(T, bool)>) -> Result<T> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.directory.join("lock"))?;
        lock.lock()?;
        let bytes = fs::read(self.directory.join("state.json"))?;
        let mut value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| RegistryError::InvalidState(e.to_string()))?;
        let legacy = value.get("format").and_then(|f| f.as_u64());
        let migrated = matches!(legacy, Some(1 | 2));
        if legacy == Some(2) {
            let object = value.as_object_mut().expect("format in object");
            for field in ["resources", "resource_operations", "cleanups"] {
                if object.contains_key(field) {
                    return Err(RegistryError::InvalidState(
                        "unexpected resource fields in format 2".into(),
                    ));
                }
                object.insert(field.into(), serde_json::json!({}));
            }
            if object.contains_key("frozen") {
                return Err(RegistryError::InvalidState(
                    "unexpected frozen identities in format 2".into(),
                ));
            }
            object.insert("frozen".into(), serde_json::json!([]));
        }
        let mut state: State = if legacy == Some(1) {
            let old: LegacyState = serde_json::from_value(value)
                .map_err(|e| RegistryError::InvalidState(e.to_string()))?;
            debug_assert_eq!(old.format, 1);
            State {
                format: 2,
                audience: RequestId::generate(),
                used_requests: BTreeMap::new(),
                namespaces: old.namespaces,
                authorities: old.authorities,
                public_base: None,
                publications: BTreeMap::new(),
                resources: BTreeMap::new(),
                resource_operations: BTreeMap::new(),
                frozen: vec![],
                cleanups: BTreeMap::new(),
            }
        } else {
            serde_json::from_value(value).map_err(|e| RegistryError::InvalidState(e.to_string()))?
        };
        if state.format == 2 {
            state.format = 3;
            state.migrate_resources();
        }
        state.validate()?;
        let (result, changed) = run(&mut state)?;
        if changed || migrated {
            state.sync_resource_states();
            state.validate()?;
            self.store(&state)?;
        }
        Ok(result) // dropping the file releases the lock, including on error
    }
    fn store(&self, state: &State) -> Result<()> {
        let temporary = self
            .directory
            .join(format!(".state-{}", RequestId::generate()));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(
                &serde_json::to_vec(state)
                    .map_err(|e| RegistryError::InvalidState(e.to_string()))?,
            )?;
            file.sync_all()?;
            fs::rename(&temporary, self.directory.join("state.json"))?;
            File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    /// OPERATOR ONLY: allocate a namespace bound permanently to this identity.
    /// Allocation alone does not grant publishing access, even to its owner.
    pub fn allocate_namespace(&self, owner: Key) -> Result<NamespaceId> {
        self.transaction(|state| {
            if state.frozen.contains(&CellIdentity(owner)) {
                return Err(RegistryError::Denied);
            }
            let id = loop {
                let candidate = NamespaceId(Hash::of(RequestId::generate().to_string().as_bytes()));
                if !state.namespaces.contains_key(&candidate) {
                    break candidate;
                }
            };
            state.namespaces.insert(
                id.clone(),
                Namespace {
                    owner,
                    sites: BTreeSet::new(),
                },
            );
            Ok((id, true))
        })
    }

    /// OPERATOR ONLY: replace the COMPLETE approved publishing policy atomically.
    /// Tree construction has already enforced parent/child grant containment.
    /// Pins must come from verified operator state, not request-supplied names.
    /// Removing a grant/key revokes it on the next operation, across restarts.
    /// Operator approval may explicitly delegate an owner's namespace or site;
    /// it never changes ownership. No automatic key rotation/name inheritance.
    pub fn approve_tree(&self, tree: &Tree, pins: &BTreeMap<CellId, Key>) -> Result<()> {
        self.transaction(|state| {
            state.authorities = state.policy(tree, pins)?;
            state.refresh_attachments();
            Ok(((), true))
        })
    }

    /// Public identifier, NOT a credential. Distribute through trusted operator
    /// configuration so a signer knows which publisher it is addressing.
    pub fn audience(&self) -> Result<RequestId> {
        self.transaction(|state| Ok((state.audience, false)))
    }

    /// Preview the full policy, including registry identity and the policy it
    /// replaces. Nothing is changed. Invalid policies cannot be approved.
    pub fn plan_policy(
        &self,
        tree: &Tree,
        pins: &BTreeMap<CellId, Key>,
    ) -> Result<super::operator::PolicyPlan> {
        self.transaction(|state| {
            state.policy(tree, pins)?;
            let plan = super::operator::PolicyPlan::new(
                state.audience,
                Hash::of(&crate::canon::bytes(&(
                    &state.authorities,
                    &state.public_base,
                ))),
                tree,
                pins,
            )?;
            Ok((plan, false))
        })
    }

    /// OPERATOR ONLY: replan and compare the approval under the same lock used
    /// for policy replacement. Changes to pins, topology, grants, registry, or
    /// the previous policy invalidate the approval.
    pub fn apply_policy(
        &self,
        tree: &Tree,
        pins: &BTreeMap<CellId, Key>,
        approval: &Hash,
    ) -> Result<()> {
        self.transaction(|state| {
            let next = state.policy(tree, pins)?;
            let plan = super::operator::PolicyPlan::new(
                state.audience,
                Hash::of(&crate::canon::bytes(&(
                    &state.authorities,
                    &state.public_base,
                ))),
                tree,
                pins,
            )?;
            if &plan.hash() != approval {
                return Err(RegistryError::InvalidPolicy(
                    "approval no longer matches publisher plan".into(),
                ));
            }
            state.authorities = next;
            state.refresh_attachments();
            Ok(((), true))
        })
    }

    /// The only entry point used by the cell-facing transport. Signature,
    /// audience, expiry, current grants, reservation, and replay recording are
    /// all checked before acknowledging success. The replay marker and mutation
    /// commit in one durable state replacement.
    pub fn execute(
        &self,
        request: &super::transport::SignedRequest,
    ) -> Result<super::transport::Reply> {
        self.commit(request, None)
    }
    pub fn execute_upload(
        &self,
        request: &super::transport::SignedRequest,
        artifact: &Artifact,
    ) -> Result<super::transport::Reply> {
        self.commit(request, Some(artifact))
    }
    fn commit(
        &self,
        request: &super::transport::SignedRequest,
        upload: Option<&Artifact>,
    ) -> Result<super::transport::Reply> {
        use super::transport::{Command, Reply, MAX_LIFETIME_SECS};
        let verified = request.verify()?;
        self.transaction(|state| {
            let now = super::transport::unix_time()?;
            if verified.audience != state.audience
                || verified.expires <= now
                || verified.expires.saturating_sub(now) > MAX_LIFETIME_SECS
            {
                return Err(RegistryError::Unauthenticated);
            }
            let replay_key = Hash::of(&crate::canon::bytes(&(verified.caller, verified.request)));
            state.used_requests.retain(|_, expires| *expires > now);
            if state.used_requests.contains_key(&replay_key) {
                return Err(RegistryError::Replay);
            }
            if state.used_requests.len() >= 10_000 {
                return Err(RegistryError::Busy);
            }
            if upload.is_some() && !matches!(verified.command, Command::Publish { .. }) {
                return Err(RegistryError::InvalidArtifact("unexpected upload".into()));
            }
            let site = verified.command.site();
            let site_key = Hash::of(&crate::canon::bytes(site));
            let reply = match &verified.command {
                Command::ReserveSite { site } => {
                    state.reserve(verified.caller, site)?;
                    Reply::Reserved
                }
                Command::Check { site, .. } => {
                    state.check_site(verified.caller, site)?;
                    Reply::Authorized
                }
                Command::Publish {
                    site,
                    artifact: expected,
                } => {
                    if state
                        .check_scope(&PublishScope::Site(site.clone()))
                        .is_err()
                    {
                        state.reserve(verified.caller, site)?;
                    }
                    state.check_site(verified.caller, site)?;
                    let base = state.public_base.as_ref().ok_or_else(|| {
                        RegistryError::InvalidArtifact(
                            "operator must configure public URL first".into(),
                        )
                    })?;
                    let artifact = upload
                        .ok_or_else(|| RegistryError::InvalidArtifact("missing upload".into()))?;
                    let digest = artifact
                        .hash()
                        .map_err(|e| RegistryError::InvalidArtifact(e.to_string()))?;
                    if &digest != expected {
                        return Err(RegistryError::InvalidArtifact(
                            "upload does not match signed digest".into(),
                        ));
                    }
                    let history = state
                        .publications
                        .entry(site_key)
                        .or_insert_with(|| History {
                            site: site.clone(),
                            versions: BTreeMap::new(),
                            active: None,
                        });
                    if history.versions.len() >= 32 {
                        return Err(RegistryError::Busy);
                    }
                    if history.versions.contains_key(&verified.request) {
                        return Err(RegistryError::Replay);
                    }
                    let receipt = PublicationReceipt {
                        site: site.clone(),
                        publisher: verified.caller,
                        version: verified.request,
                        artifact: digest,
                        url: format!(
                            "{}/sites/{}/{}/",
                            base.as_str().trim_end_matches('/'),
                            site.namespace.0,
                            site.name
                        )
                        .parse()
                        .map_err(|e: crate::Invalid| {
                            RegistryError::InvalidArtifact(e.to_string())
                        })?,
                    };
                    // The immutable blob is durable before the atomic state switch.
                    // A crash can leave an unreferenced blob, never a partial site.
                    artifact::store(&self.directory, artifact)?;
                    history.versions.insert(receipt.version, receipt.clone());
                    history.active = Some(receipt.version);
                    Reply::Published(receipt)
                }
                Command::Status { site } => {
                    state.check_site(verified.caller, site)?;
                    let history = state.publications.get(&site_key);
                    Reply::Status {
                        site: site.clone(),
                        active: history
                            .and_then(|h| h.active.and_then(|v| h.versions.get(&v).cloned())),
                        versions: history
                            .map(|h| h.versions.values().cloned().collect())
                            .unwrap_or_default(),
                    }
                }
                Command::Rollback { site, version } => {
                    state.check_site(verified.caller, site)?;
                    let history = state
                        .publications
                        .get_mut(&site_key)
                        .ok_or(RegistryError::UnknownSite)?;
                    let receipt = history
                        .versions
                        .get(version)
                        .ok_or(RegistryError::UnknownSite)?
                        .clone();
                    artifact::load(&self.directory, receipt.artifact)?;
                    history.active = Some(*version);
                    Reply::RolledBack(receipt)
                }
                Command::Unpublish { site } => {
                    state.check_site(verified.caller, site)?;
                    if let Some(history) = state.publications.get_mut(&site_key) {
                        history.active = None;
                    }
                    Reply::Unpublished
                }
            };
            state.used_requests.insert(replay_key, verified.expires);
            Ok((reply, true))
        })
    }

    /// OPERATOR ONLY. Fix the public HTTPS origin before publishing anything.
    /// Published URLs cannot silently move to another operator-selected origin.
    pub fn configure_public_base(&self, base: Url) -> Result<()> {
        let rest = &base.as_str()["https://".len()..];
        if rest.trim_end_matches('/').contains(['/', '?', '#']) {
            return Err(RegistryError::InvalidArtifact(
                "public URL must be an HTTPS origin".into(),
            ));
        }
        self.transaction(|state| {
            if !state.publications.is_empty() && state.public_base.as_ref() != Some(&base) {
                return Err(RegistryError::InvalidPolicy(
                    "cannot change origin after publishing".into(),
                ));
            }
            state.public_base = Some(base);
            Ok(((), true))
        })
    }

    /// Public content lookup: only the currently selected immutable artifact.
    /// There is no route to historical/unpublished blobs, private state, or logs.
    pub fn public_file(&self, site: &SiteId, path: &str) -> Result<Vec<u8>> {
        artifact::check_path(path).map_err(|_| RegistryError::UnknownSite)?;
        self.transaction(|state| {
            let history = state
                .publications
                .get(&Hash::of(&crate::canon::bytes(site)))
                .ok_or(RegistryError::UnknownSite)?;
            let receipt = history
                .active
                .and_then(|v| history.versions.get(&v))
                .ok_or(RegistryError::UnknownSite)?;
            let artifact = artifact::load(&self.directory, receipt.artifact)?;
            let file = artifact
                .files
                .into_iter()
                .find(|f| f.path == path)
                .ok_or(RegistryError::UnknownSite)?;
            Ok((file.bytes, false))
        })
    }

    /// OPERATOR ONLY: namespace allocation provenance. Site ownership is in
    /// the generic resource record and can differ after an explicit transfer.
    pub fn owner(&self, namespace: &NamespaceId) -> Result<Key> {
        self.transaction(|state| {
            let record = state
                .namespaces
                .get(namespace)
                .ok_or(RegistryError::UnknownNamespace)?;
            Ok((record.owner, false))
        })
    }

    /// Reserve a new site. Only namespace authority may create; a site delegate
    /// can only manage an existing reservation. The owner remains the namespace
    /// owner even when a namespace delegate creates the site.
    pub fn create_site(&self, authenticated_caller: Key, site: &SiteId) -> Result<()> {
        self.transaction(|state| {
            state.reserve(authenticated_caller, site)?;
            Ok(((), true))
        })
    }

    /// Check current authority only. This does not publish, roll back, unpublish,
    /// or return a reusable permit. Future commit operations must recheck inside
    /// their transaction to avoid a revocation race.
    pub fn authorize(
        &self,
        authenticated_caller: Key,
        site: &SiteId,
        _action: SiteAction,
    ) -> Result<()> {
        self.transaction(|state| {
            state.check_site(authenticated_caller, site)?;
            Ok(((), false))
        })
    }
}
