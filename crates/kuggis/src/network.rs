//! Pure policy decisions. No listener, DNS lookup or firewall is installed here.
use crate::config::{Destination, Egress, Endpoint, Host, Workspace};
use std::net::IpAddr;

pub struct Policy<'a> {
    workspace: &'a Workspace,
}

impl<'a> Policy<'a> {
    pub fn new(workspace: &'a Workspace) -> Self {
        Self { workspace }
    }

    pub fn endpoint(&self, host: &Host, port: u16) -> Option<&'a Endpoint> {
        self.workspace
            .resources()
            .iter()
            .flat_map(|resource| &resource.endpoints)
            .find(|endpoint| &endpoint.host == host && endpoint.port.get() == port)
    }

    pub fn allows_target(&self, host: &Host, port: u16) -> bool {
        port != 0
            && (self.workspace.egress() == Egress::Internet || self.endpoint(host, port).is_some())
    }

    /// A future connector must connect to one of these checked addresses, without
    /// resolving the name again. A mixed public/private DNS response fails closed.
    pub fn allows_resolved(&self, host: &Host, port: u16, addresses: &[IpAddr]) -> bool {
        self.allows_target(host, port)
            && !addresses.is_empty()
            && addresses.iter().all(|address| {
                match self.endpoint(host, port).map(|e| &e.destination) {
                    Some(Destination::Pinned { addresses: pins }) => pins.contains(address),
                    _ => public_address(*address),
                }
            })
    }
}

pub(crate) fn valid_pin(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let first = ip.octets()[0];
            // A deployment may explicitly grant a link-local platform service.
            // Public rules still reject it; pins never imply a general route.
            first != 0 && first < 224 && !ip.is_loopback()
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return valid_pin(ip.into());
            }
            !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast()
                && (ip.segments()[0] & 0xffc0) != 0xfe80
                // Translation and transition prefixes are not an escape hatch.
                && ((ip.segments()[0] & 0xfe00) == 0xfc00 || global_v6(ip))
        }
    }
}

/// Deliberately conservative. Private destinations require explicit pins.
pub fn public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let o = ip.octets();
            if !valid_pin(address) || ip.is_private() || ip.is_link_local() || ip.is_documentation()
            {
                return false;
            }
            !((o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19)))
        }
        IpAddr::V6(ip) => global_v6(ip),
    }
}

fn global_v6(ip: std::net::Ipv6Addr) -> bool {
    let s = ip.segments();
    (s[0] & 0xe000) == 0x2000
        && !(s[0] == 0x2001 && s[1] < 0x0200) // special-purpose /23
        && !(s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        && s[0] != 0x2002 // 6to4 can embed non-public IPv4
        && !(s[0] == 0x3fff && s[1] < 0x1000) // documentation /20
}
