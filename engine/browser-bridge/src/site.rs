//! Site permission policy for browser navigation.

use url::Url;

/// Whether the agent may load a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteDecision {
    Allowed,
    Blocked,
    /// Not covered by the lists: the user should be asked.
    Ask,
}

/// Decide whether `url` may be loaded.
///
/// Patterns match the host, optionally with a port and/or scheme:
/// `example.com` (exact host, any port), `*.example.com` (the domain and all
/// subdomains), `localhost:3000`, `127.0.0.1:*`, `https://docs.rs`, `*`.
/// A path in a pattern is ignored. `blocked` wins over `allowed`.
///
/// Defaults: loopback hosts (`localhost`, `*.localhost`, `127.0.0.0/8`,
/// `::1`), `file://` and `about:blank` are allowed; browser-internal schemes
/// (`chrome:`, `edge:`, `devtools:`, `javascript:`, ...) are blocked; other
/// hosts are `Ask`.
pub fn site_decision(url: &str, allowed: &[String], blocked: &[String]) -> SiteDecision {
    let Ok(parsed) = Url::parse(url.trim()) else {
        return SiteDecision::Blocked;
    };
    let scheme = parsed.scheme().to_ascii_lowercase();
    match scheme.as_str() {
        "http" | "https" => {}
        "blob" => {
            // blob:https://host/uuid -> decide on the embedded origin.
            return match Url::parse(parsed.path()) {
                Ok(inner) if matches!(inner.scheme(), "http" | "https") => {
                    site_decision(inner.as_str(), allowed, blocked)
                }
                _ => SiteDecision::Blocked,
            };
        }
        "file" => {
            let file_blocked = blocked
                .iter()
                .any(|p| matches!(p.trim().to_ascii_lowercase().as_str(), "file:" | "file://" | "file://*"));
            return if file_blocked { SiteDecision::Blocked } else { SiteDecision::Allowed };
        }
        "about" => {
            return if matches!(parsed.path(), "blank" | "srcdoc") {
                SiteDecision::Allowed
            } else {
                SiteDecision::Blocked
            };
        }
        "data" => {
            let listed = |list: &[String]| list.iter().any(|p| p.trim().eq_ignore_ascii_case("data:"));
            return if listed(blocked) {
                SiteDecision::Blocked
            } else if listed(allowed) {
                SiteDecision::Allowed
            } else {
                SiteDecision::Ask
            };
        }
        _ => return SiteDecision::Blocked,
    }

    let Some(host) = parsed.host_str() else {
        return SiteDecision::Blocked;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_ascii_lowercase();
    let port = parsed.port_or_known_default();

    if blocked.iter().any(|p| pattern_matches(p, &scheme, &host, port)) {
        return SiteDecision::Blocked;
    }
    if allowed.iter().any(|p| pattern_matches(p, &scheme, &host, port)) || is_loopback(&host) {
        return SiteDecision::Allowed;
    }
    SiteDecision::Ask
}

/// Host of a URL for messages (`example.com:8080`), or the URL itself.
pub(crate) fn display_host(url: &str) -> String {
    match Url::parse(url) {
        Ok(u) => match (u.host_str(), u.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            _ => u.scheme().to_string() + ":",
        },
        Err(_) => url.to_string(),
    }
}

fn is_loopback(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") || host == "::1" {
        return true;
    }
    host.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
}

fn pattern_matches(pattern: &str, scheme: &str, host: &str, port: Option<u16>) -> bool {
    let mut p = pattern.trim().to_ascii_lowercase();
    if p.is_empty() {
        return false;
    }
    if p == "*" {
        return true;
    }
    if let Some((s, rest)) = p.split_once("://") {
        if s != "*" && s != scheme {
            return false;
        }
        p = rest.to_string();
    }
    // Drop any path.
    if let Some(i) = p.find('/') {
        p.truncate(i);
    }
    let (host_pat, port_pat) = split_host_port(&p);
    if let Some(port_pat) = port_pat {
        if port_pat != "*" && port_pat.parse::<u16>().ok() != port {
            return false;
        }
    }
    let host_pat = host_pat.trim_start_matches('[').trim_end_matches(']').trim_end_matches('.');
    if let Some(domain) = host_pat.strip_prefix("*.") {
        if !domain.contains('*') {
            return host == domain || host.ends_with(&format!(".{domain}"));
        }
    }
    glob_match(host_pat, host)
}

/// Split `host:port`, taking care of bracketed and bare IPv6 literals.
fn split_host_port(p: &str) -> (&str, Option<&str>) {
    if let Some(rest) = p.strip_prefix('[') {
        if let Some((h, tail)) = rest.split_once(']') {
            return (h, tail.strip_prefix(':'));
        }
    }
    if p.matches(':').count() > 1 {
        return (p, None); // bare IPv6, no port
    }
    match p.rsplit_once(':') {
        Some((h, port)) => (h, Some(port)),
        None => (p, None),
    }
}

/// `*` matches any run of characters, `?` one character.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use SiteDecision::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn defaults() {
        assert_eq!(site_decision("http://localhost:3000/x", &[], &[]), Allowed);
        assert_eq!(site_decision("http://app.localhost/", &[], &[]), Allowed);
        assert_eq!(site_decision("http://127.0.0.1:8080", &[], &[]), Allowed);
        assert_eq!(site_decision("http://127.1.2.3", &[], &[]), Allowed);
        assert_eq!(site_decision("http://[::1]:5173/", &[], &[]), Allowed);
        assert_eq!(site_decision("file:///C:/site/index.html", &[], &[]), Allowed);
        assert_eq!(site_decision("about:blank", &[], &[]), Allowed);
        assert_eq!(site_decision("https://example.com", &[], &[]), Ask);
        assert_eq!(site_decision("data:text/html,hi", &[], &[]), Ask);
        assert_eq!(site_decision("chrome://settings", &[], &[]), Blocked);
        assert_eq!(site_decision("edge://flags", &[], &[]), Blocked);
        assert_eq!(site_decision("javascript:alert(1)", &[], &[]), Blocked);
        assert_eq!(site_decision("about:version", &[], &[]), Blocked);
        assert_eq!(site_decision("not a url", &[], &[]), Blocked);
    }

    #[test]
    fn host_globs_and_ports() {
        let allowed = v(&["*.example.com", "docs.rs", "127.0.0.1:*", "localhost:3000", "https://secure.test"]);
        assert_eq!(site_decision("https://example.com/", &allowed, &[]), Allowed);
        assert_eq!(site_decision("https://a.b.example.com/", &allowed, &[]), Allowed);
        assert_eq!(site_decision("https://badexample.com/", &allowed, &[]), Ask);
        assert_eq!(site_decision("https://docs.rs/serde", &allowed, &[]), Allowed);
        assert_eq!(site_decision("https://www.docs.rs/", &allowed, &[]), Ask);
        assert_eq!(site_decision("https://secure.test/", &allowed, &[]), Allowed);
        assert_eq!(site_decision("http://secure.test/", &allowed, &[]), Ask);

        let ports = v(&["intranet:8080", "api-*.corp.dev"]);
        assert_eq!(site_decision("http://intranet:8080/", &ports, &[]), Allowed);
        assert_eq!(site_decision("http://intranet:9090/", &ports, &[]), Ask);
        assert_eq!(site_decision("https://api-eu.corp.dev/", &ports, &[]), Allowed);
        assert_eq!(site_decision("https://www.corp.dev/", &ports, &[]), Ask);
        assert_eq!(site_decision("https://anything.org", &v(&["*"]), &[]), Allowed);
    }

    #[test]
    fn blocked_wins() {
        let allowed = v(&["*.example.com"]);
        let blocked = v(&["ads.example.com", "localhost:6666", "file://"]);
        assert_eq!(site_decision("https://ads.example.com/x", &allowed, &blocked), Blocked);
        assert_eq!(site_decision("https://www.example.com/x", &allowed, &blocked), Allowed);
        assert_eq!(site_decision("http://localhost:6666/", &[], &blocked), Blocked);
        assert_eq!(site_decision("http://localhost:5555/", &[], &blocked), Allowed);
        assert_eq!(site_decision("file:///etc/passwd", &[], &blocked), Blocked);
        assert_eq!(site_decision("blob:https://ads.example.com/uuid", &allowed, &blocked), Blocked);
        assert_eq!(site_decision("blob:https://www.example.com/uuid", &allowed, &blocked), Allowed);
    }

    #[test]
    fn globbing() {
        assert!(glob_match("a*c", "abbbc"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(!glob_match("*.x", "y"));
        assert_eq!(display_host("http://localhost:3000/a"), "localhost:3000");
        assert_eq!(display_host("https://example.com/a"), "example.com");
    }
}
