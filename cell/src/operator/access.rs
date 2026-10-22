//! Browser access is either local, or authenticated by the exe.dev ingress on
//! a dedicated trusted VM. Neither mode permits a public TCP listener. A local
//! process on that VM can impersonate the ingress, so agents must live elsewhere.
use axum::{
    extract::Request,
    http::{HeaderMap, Method},
};
use serde::Deserialize;
use std::{collections::BTreeSet, path::Path};

#[derive(Default)]
pub enum Access {
    #[default]
    Local,
    ExeProxy(ExeProxy),
}

pub struct ExeProxy {
    origin: String,
    host: crate::Host,
    users: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProxyFile {
    origin: String,
    users: BTreeSet<String>,
}

impl Access {
    pub fn exe_proxy(path: &Path) -> crate::apply::Fallible<Self> {
        Self::parse_proxy(&std::fs::read(path)?)
    }

    fn parse_proxy(bytes: &[u8]) -> crate::apply::Fallible<Self> {
        let config: ProxyFile = serde_json::from_slice(bytes)?;
        // One canonical HTTPS origin: no credentials, port, path, query or
        // fragment. Custom domains work; alternate ingress ports do not.
        let host: crate::Host = config
            .origin
            .strip_prefix("https://")
            .ok_or("exe proxy origin must be https://HOST")?
            .parse()?;
        if config.users.is_empty()
            || config.users.iter().any(|user| {
                user.is_empty()
                    || !user
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
        {
            return Err("exe proxy users must contain stable user IDs".into());
        }
        Ok(Self::ExeProxy(ExeProxy {
            origin: config.origin,
            host,
            users: config.users,
        }))
    }

    pub(super) fn origin(&self, authority: &str) -> String {
        match self {
            Self::Local => format!("http://{authority}"),
            Self::ExeProxy(proxy) => proxy.origin.clone(),
        }
    }

    pub(super) fn allows(&self, request: &Request, authority: &str) -> bool {
        // Login redirects and links from other sites must be able to open the
        // application shell. This exception cannot reach an API or mutate state.
        let navigation = request.method() == Method::GET
            && request.uri().path() == "/"
            && single(request.headers(), "sec-fetch-mode") == Some("navigate")
            && single(request.headers(), "sec-fetch-dest") == Some("document");
        self.allows_headers(request.headers(), authority, navigation)
    }

    fn allows_headers(&self, headers: &HeaderMap, authority: &str, navigation: bool) -> bool {
        let origin = self.origin(authority);
        if !optional(headers, "origin", |v| v == origin)
            || !optional(headers, "sec-fetch-site", |v| {
                matches!(v, "same-origin" | "none")
                    || (navigation && matches!(v, "cross-site" | "same-site"))
            })
        {
            return false;
        }
        match self {
            Self::Local => single(headers, "host") == Some(authority),
            Self::ExeProxy(proxy) => {
                single(headers, "host") == Some(proxy.host.as_str())
                    && single(headers, "x-forwarded-host") == Some(proxy.host.as_str())
                    && single(headers, "x-forwarded-proto") == Some("https")
                    && single(headers, "x-exedev-userid").is_some_and(|id| proxy.users.contains(id))
            }
        }
    }
}

fn single<'a>(headers: &'a HeaderMap, key: &str) -> Option<&'a str> {
    let mut values = headers.get_all(key).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        None
    } else {
        Some(value)
    }
}

fn optional(headers: &HeaderMap, key: &str, check: impl FnOnce(&str) -> bool) -> bool {
    !headers.contains_key(key) || single(headers, key).is_some_and(check)
}

/// The cell receives neither browser credentials nor the supervisor's trusted
/// ingress identity. These headers cannot become an alternate cell authority.
pub(super) fn strip_browser_headers(headers: &mut HeaderMap) {
    let keys: Vec<_> = headers
        .keys()
        .filter(|key| {
            let key = key.as_str();
            matches!(
                key,
                "origin"
                    | "connection"
                    | "proxy-authorization"
                    | "cookie"
                    | "authorization"
                    | "forwarded"
            ) || key.starts_with("sec-fetch-")
                || key.starts_with("x-forwarded-")
                || key.starts_with("x-exedev-")
        })
        .cloned()
        .collect();
    for key in keys {
        headers.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosted() -> Access {
        Access::parse_proxy(br#"{"origin":"https://supervisor.example","users":["usr_fixture"]}"#)
            .unwrap()
    }
    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (key, value) in [
            ("host", "supervisor.example"),
            ("x-forwarded-host", "supervisor.example"),
            ("x-forwarded-proto", "https"),
            ("x-exedev-userid", "usr_fixture"),
            ("origin", "https://supervisor.example"),
            ("sec-fetch-site", "same-origin"),
        ] {
            headers.insert(key, value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn hosted_requires_identity_and_exact_ingress_headers() {
        let access = hosted();
        assert!(access.allows_headers(&headers(), "127.0.0.1:8000", false));
        for (key, wrong) in [
            ("host", "evil.example"),
            ("x-forwarded-host", "evil.example"),
            ("x-forwarded-proto", "http"),
            ("x-exedev-userid", "usr_other"),
        ] {
            let mut h = headers();
            h.remove(key);
            assert!(
                !access.allows_headers(&h, "127.0.0.1:8000", false),
                "missing {key}"
            );
            h.insert(key, wrong.parse().unwrap());
            assert!(
                !access.allows_headers(&h, "127.0.0.1:8000", false),
                "wrong {key}"
            );
        }
    }

    #[test]
    fn both_modes_reject_csrf_and_ambiguous_headers() {
        for (access, mut good, authority) in [
            (hosted(), headers(), "127.0.0.1:8000"),
            (Access::Local, HeaderMap::new(), "127.0.0.1:8000"),
        ] {
            if matches!(access, Access::Local) {
                good.insert("host", authority.parse().unwrap());
            }
            assert!(access.allows_headers(&good, authority, false));
            for (key, value) in [
                ("origin", "https://evil.example"),
                ("origin", "null"),
                ("sec-fetch-site", "cross-site"),
                ("sec-fetch-site", "same-site"),
            ] {
                let mut bad = good.clone();
                bad.insert(key, value.parse().unwrap());
                assert!(!access.allows_headers(&bad, authority, false));
            }
            for key in good.keys() {
                let mut bad = good.clone();
                bad.append(key.clone(), good[key].clone());
                assert!(
                    !access.allows_headers(&bad, authority, false),
                    "duplicate {key}"
                );
            }
            good.remove("origin");
            good.remove("sec-fetch-site");
            assert!(
                access.allows_headers(&good, authority, false),
                "navigation and non-browser clients"
            );
        }
        let mut h = headers();
        h.insert("host", "127.0.0.1:8000".parse().unwrap());
        assert!(
            !Access::Local.allows_headers(&h, "127.0.0.1:8000", false),
            "forwarded headers never enable hosted mode"
        );
    }

    #[test]
    fn proxy_configuration_fails_closed() {
        for origin in [
            "http://supervisor.example",
            "https://supervisor.example/",
            "https://supervisor.example:443",
            "https://a@supervisor.example",
            "https://supervisor.example?x",
            "https://127.0.0.1",
            "https://SUPERVISOR.example",
        ] {
            let bytes =
                serde_json::to_vec(&serde_json::json!({"origin":origin,"users":["usr_fixture"]}))
                    .unwrap();
            assert!(Access::parse_proxy(&bytes).is_err(), "{origin}");
        }
        for users in [
            vec![],
            vec![""],
            vec!["usr fixture"],
            vec!["usr_fixture,usr_other"],
        ] {
            let bytes = serde_json::to_vec(
                &serde_json::json!({"origin":"https://supervisor.example","users":users}),
            )
            .unwrap();
            assert!(Access::parse_proxy(&bytes).is_err());
        }
        assert!(Access::parse_proxy(
            br#"{"origin":"https://supervisor.example","users":["usr_fixture"],"typo":true}"#
        )
        .is_err());
    }

    #[test]
    fn cells_do_not_receive_browser_or_ingress_credentials() {
        let mut h = headers();
        for key in [
            "cookie",
            "authorization",
            "proxy-authorization",
            "forwarded",
            "x-exedev-email",
            "x-exedev-authorization",
            "x-forwarded-for",
            "sec-fetch-mode",
        ] {
            h.insert(key, "private".parse().unwrap());
        }
        h.insert("content-type", "application/json".parse().unwrap());
        strip_browser_headers(&mut h);
        assert_eq!(h.len(), 2);
        assert!(h.contains_key("host") && h.contains_key("content-type"));
    }
}
