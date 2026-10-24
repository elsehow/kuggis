use super::*;
use crate::publish::provision::{Discovery, PolicyCell};
use crate::publish::{
    private,
    transport::{Command, SignedRequest},
    Visibility,
};
use crate::resource::ProviderRef;
impl State {
    pub(super) fn provision_private_namespaces(&mut self) -> Result<()> {
        for authority in &self.authorities {
            if authority.scopes.contains(&PublishScope::Private) {
                let ns = private::namespace(self.audience, authority.key);
                if self
                    .namespaces
                    .get(&ns)
                    .is_some_and(|n| n.owner != authority.key)
                {
                    return Err(RegistryError::Denied);
                }
                self.namespaces.entry(ns).or_insert_with(|| Namespace {
                    owner: authority.key,
                    sites: BTreeSet::new(),
                });
            }
        }
        Ok(())
    }
    pub(super) fn public_authority(&self, caller: Key, site: &SiteId) -> bool {
        !self.frozen.contains(&CellIdentity(caller))
            && self.authorities.iter().any(|a| {
                a.key == caller
                    && a.scopes
                        .iter()
                        .any(|s| s.covers(&PublishScope::Site(site.clone())))
            })
    }
    pub(super) fn site_url(&self, site: &SiteId, visibility: Visibility) -> Result<Url> {
        let url = match visibility {
            Visibility::Public => format!(
                "{}/sites/{}/{}/",
                self.public_base
                    .as_ref()
                    .ok_or_else(|| RegistryError::InvalidPolicy(
                        "Public hosting is not configured".into()
                    ))?
                    .as_str()
                    .trim_end_matches('/'),
                site.namespace.0,
                site.name
            ),
            Visibility::Private => {
                let config = self.private_hosting.as_ref().ok_or_else(|| RegistryError::InvalidPolicy("Private hosting is not configured; ask the operator to connect a private publisher".into()))?;
                format!(
                    "https://{}.{}/",
                    private::label(self.resource_id(site)),
                    config.domain
                )
            }
        };
        url.parse()
            .map_err(|e: crate::Invalid| RegistryError::InvalidState(e.to_string()))
    }
    fn check_read_proof(&self, proof: &SignedRequest, site: &SiteId) -> Result<()> {
        let request = proof.verify()?;
        let now = crate::publish::transport::unix_time()?;
        if request.audience != self.audience
            || request.expires <= now
            || request.expires.saturating_sub(now) > crate::publish::transport::MAX_LIFETIME_SECS
            || !matches!(request.command, Command::Verify { site: ref target } if target == site)
        {
            return Err(RegistryError::Denied);
        }
        self.check_site(request.caller, site)
    }
}
impl Registry {
    pub fn record_verification(
        &self,
        verification: &crate::publish::verify::Verification,
    ) -> Result<()> {
        self.transaction(|s| {
            let h = s
                .publications
                .get_mut(&Hash::of(&crate::canon::bytes(
                    &verification.publication.site,
                )))
                .ok_or(RegistryError::UnknownSite)?;
            if h.active.and_then(|v| h.versions.get(&v)) != Some(&verification.publication) {
                return Err(RegistryError::InvalidPolicy(
                    "Site changed during verification".into(),
                ));
            }
            h.verification = Some(verification.clone());
            Ok(((), true))
        })
    }

    pub fn configure_private(&self, config: private::Config) -> Result<()> {
        config.validate().map_err(RegistryError::InvalidPolicy)?;
        self.transaction(|s| {
            if s.publications.values().any(|h| {
                h.versions
                    .values()
                    .any(|r| r.visibility == Visibility::Private)
            }) && s.private_hosting.as_ref().map(|c| &c.domain) != Some(&config.domain)
            {
                return Err(RegistryError::InvalidPolicy(
                    "Cannot change private site origins after publishing".into(),
                ));
            }
            if s.public_base.as_ref().is_some_and(|base| {
                base.host() == config.domain
                    || base
                        .host()
                        .as_str()
                        .ends_with(&format!(".{}", config.domain))
            }) {
                return Err(RegistryError::InvalidPolicy(
                    "Public and private sites must use separate origins".into(),
                ));
            }
            s.private_hosting = Some(config);
            Ok(((), true))
        })
    }
    pub fn private_file(
        &self,
        host: &str,
        path: &str,
        user: Option<&str>,
        proof: Option<&SignedRequest>,
    ) -> Result<(PublicationReceipt, Vec<u8>)> {
        artifact::check_path(path).map_err(|_| RegistryError::UnknownSite)?;
        self.transaction(|s| {
            let config = s.private_hosting.as_ref().ok_or(RegistryError::Denied)?;
            let history = s
                .publications
                .values()
                .find(|h| {
                    format!(
                        "{}.{}",
                        private::label(s.resource_id(&h.site)),
                        config.domain
                    ) == host
                })
                .ok_or(RegistryError::UnknownSite)?;
            if let Some(proof) = proof {
                s.check_read_proof(proof, &history.site)?;
            } else if !user.is_some_and(|id| config.users.contains(id)) {
                return Err(RegistryError::Denied);
            }
            let receipt = history
                .active
                .and_then(|v| history.versions.get(&v))
                .filter(|r| r.visibility == Visibility::Private)
                .ok_or(RegistryError::UnknownSite)?
                .clone();
            let artifact = artifact::load(&self.directory, receipt.artifact)?;
            let file = artifact
                .files
                .into_iter()
                .find(|f| f.path == path)
                .ok_or(RegistryError::UnknownSite)?;
            Ok(((receipt, file.bytes), false))
        })
    }
    /// Authenticated snapshot for external verification. No network under lock.
    pub fn verification_snapshot(
        &self,
        proof: &SignedRequest,
    ) -> Result<(PublicationReceipt, Artifact)> {
        self.transaction(|s| {
            let request = proof.verify()?;
            let Command::Verify { site } = request.command else {
                return Err(RegistryError::Denied);
            };
            s.check_read_proof(proof, &site)?;
            let history = s
                .publications
                .get(&Hash::of(&crate::canon::bytes(&site)))
                .ok_or(RegistryError::UnknownSite)?;
            let receipt = history
                .active
                .and_then(|v| history.versions.get(&v))
                .ok_or(RegistryError::UnknownSite)?
                .clone();
            // Only operator-configured destinations, never a caller-supplied URL.
            if receipt.url != s.site_url(&site, receipt.visibility)? {
                return Err(RegistryError::Denied);
            }
            let artifact = artifact::load(&self.directory, receipt.artifact)?;
            Ok(((receipt, artifact), false))
        })
    }
}

impl Registry {
    pub fn reconcile_publishing(
        &self,
        provider: ProviderRef,
        endpoint: Url,
        cells: Vec<PolicyCell>,
    ) -> Result<BTreeMap<Name, Discovery>> {
        let tree = Tree::new(cells.iter().map(|c| (c.cell.clone(), c.parent.clone())))
            .map_err(|e| RegistryError::InvalidPolicy(e.to_string()))?;
        let pins: BTreeMap<_, _> = cells.iter().map(|c| (c.cell.name.clone(), c.key)).collect();
        if endpoint.as_str().trim_end_matches('/')[8..].contains(['/', '?', '#']) {
            return Err(RegistryError::InvalidPolicy(
                "Publisher endpoint must be an HTTPS origin".into(),
            ));
        }
        self.transaction(|s| {
            if provider.0 != s.audience {
                return Err(RegistryError::Denied);
            }
            let next = s.policy(&tree, &pins)?;
            let changed = crate::canon::bytes(&next) != crate::canon::bytes(&s.authorities);
            // Reconciliation must not restore an attachment the operator detached
            // while the same grant remained in the tree.
            let mut detached = BTreeSet::new();
            for record in s.resources.values() {
                for authority in &s.authorities {
                    if let Ok(site) = s.site_for_resource(record.id) {
                        if authority
                            .scopes
                            .iter()
                            .any(|scope| scope.covers(&PublishScope::Site(site.clone())))
                            && !record.attachments.iter().any(|a| a.cell.0 == authority.key)
                        {
                            detached.insert((record.id, authority.key.to_string()));
                        }
                    }
                }
            }
            s.authorities = next;
            s.provision_private_namespaces()?;
            // Do not reattach explicitly detached resources on every heartbeat.
            if changed {
                s.refresh_attachments();
                for record in s.resources.values_mut() {
                    record
                        .attachments
                        .retain(|a| !detached.contains(&(record.id, a.cell.0.to_string())));
                }
            }
            let inventory = s.inventory();
            let mut discovery = BTreeMap::new();
            for policy in &cells {
                let private = policy
                    .cell
                    .grants
                    .contains(&Grant::Publish(PublishScope::Private));
                let scope = policy.cell.grants.iter().find_map(|g| match g {
                    Grant::Publish(scope) => Some(scope),
                    _ => None,
                });
                let Some(scope) = scope else {
                    discovery.insert(
                        policy.cell.name.clone(),
                        Discovery::unavailable("Website publishing is not permitted for this cell"),
                    );
                    continue;
                };
                if !policy
                    .cell
                    .grants
                    .iter()
                    .flat_map(Grant::hosts)
                    .any(|host| host == endpoint.host())
                {
                    discovery.insert(
                        policy.cell.name.clone(),
                        Discovery::unavailable(format!(
                            "Allow {} in this cell and its parents to connect the publisher",
                            endpoint.host()
                        )),
                    );
                    continue;
                }
                if private && s.private_hosting.is_none() {
                    discovery.insert(
                        policy.cell.name.clone(),
                        Discovery::unavailable("The operator must configure private hosting"),
                    );
                    continue;
                }
                let namespace = if private {
                    crate::publish::private::namespace(s.audience, policy.key)
                } else {
                    match scope {
                        PublishScope::Namespace(ns) => ns.clone(),
                        PublishScope::Site(site) => site.namespace.clone(),
                        PublishScope::Private => unreachable!(),
                    }
                };
                let sites = inventory
                    .sites
                    .iter()
                    .filter(|site| s.check_site(policy.key, &site.publication.site).is_ok())
                    .cloned()
                    .collect();
                discovery.insert(
                    policy.cell.name.clone(),
                    Discovery {
                        config: Some(crate::publish::bridge::Config {
                            socket: "/run/cell-publish/bridge.sock".into(),
                            namespace,
                            endpoint: endpoint.clone(),
                            audience: s.audience,
                        }),
                        sites,
                        error: None,
                    },
                );
            }
            Ok((discovery, changed))
        })
    }
}
