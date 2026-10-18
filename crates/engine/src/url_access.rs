//! Control internet access from PDFs (Acrobat: Preferences ▸ Trust Manager ▸ Internet Access
//! from PDF Files). A [`UrlPolicy`] says whether an address a document asks for (a link's URI
//! action, a Submit Form action, a script's `app.launchURL` or `this.submitForm`) may be
//! reached: every web site, none, or a custom list of hosts each allowed, blocked or asked
//! about, with a default for hosts not on the list. [`check_url`] answers for one address.
//!
//! Only web addresses (`http:`, `https:`) are governed by host rules. An email address
//! (`mailto:`) opens the user's email app rather than reaching a site, so it is reported
//! separately ([`UrlKind::Mail`]) and always asks. Every other scheme is refused
//! ([`UrlKind::Refused`]), for the reasons [`crate::links::document_url`] gives.
//!
//! Hosts are compared in one normal form ([`normalize_host`]): lowercase ASCII, no trailing
//! dot, internationalised names as their IDNA (`xn--`) form, IP literals in canonical form
//! (`[::1]`, `127.0.0.1` for `0x7f.1`), and without the port and any `user:password@` prefix.
//! A rule for a name also covers its subdomains unless it is exact; a rule for an IP literal
//! matches only that address.

use serde::{Deserialize, Serialize};

use crate::Session;

/// The most host rules a policy keeps.
pub const MAX_RULES: usize = 1000;

/// The longest host name accepted (RFC 1035: 253 characters without the trailing dot).
pub const MAX_HOST: usize = 253;

/// The longest text read as a host (a pasted address with a long path is cut before this).
const MAX_HOST_INPUT: usize = 4096;

/// What the policy does with every web site.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UrlMode {
    /// Every web site may be reached without asking.
    AllowAll,
    /// No web site may be reached.
    BlockAll,
    /// The host rules decide, then [`UrlPolicy::default_action`].
    #[default]
    Custom,
}

impl UrlMode {
    pub fn id(self) -> &'static str {
        match self {
            Self::AllowAll => "allow-all",
            Self::BlockAll => "block-all",
            Self::Custom => "custom",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [Self::AllowAll, Self::BlockAll, Self::Custom].into_iter().find(|m| m.id() == id)
    }
}

/// What a host rule (or the custom default) does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostAction {
    Allow,
    Block,
    /// Ask the user each time.
    #[default]
    Ask,
}

impl HostAction {
    pub fn id(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Block => "block",
            Self::Ask => "ask",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [Self::Allow, Self::Block, Self::Ask].into_iter().find(|a| a.id() == id)
    }

    /// How restrictive it is, to settle rules that match equally well (block wins).
    fn strictness(self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::Ask => 1,
            Self::Block => 2,
        }
    }

    fn decision(self) -> UrlDecision {
        match self {
            Self::Allow => UrlDecision::Allow,
            Self::Block => UrlDecision::Block,
            Self::Ask => UrlDecision::Prompt,
        }
    }
}

fn yes() -> bool {
    true
}

/// One host in the custom list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRule {
    /// The host in normal form ([`normalize_host`]).
    pub host: String,
    pub action: HostAction,
    /// Also match every subdomain of `host` (never for an IP literal).
    #[serde(default = "yes")]
    pub subdomains: bool,
}

impl HostRule {
    /// A rule for `host` as a user typed it: a name, `*.name` (the name and its subdomains), an
    /// IP literal, or a whole address whose host is used. `subdomains` is ignored for an IP
    /// literal, and forced on by a `*.` prefix.
    pub fn new(host: &str, action: HostAction, subdomains: bool) -> Result<Self, String> {
        let trimmed = host.trim();
        let (wildcard, rest) = match trimmed.strip_prefix("*.") {
            Some(rest) => (true, rest),
            None => (false, trimmed),
        };
        let host = normalize_host(rest)?;
        let subdomains = (subdomains || wildcard) && !is_ip_literal(&host);
        Ok(Self { host, action, subdomains })
    }

    /// How well this rule matches `host` (normal form): `None`, or the rule's length (a longer
    /// rule is the more specific match).
    fn matches(&self, host: &str) -> Option<usize> {
        if host == self.host {
            return Some(self.host.len());
        }
        if !self.subdomains || is_ip_literal(&self.host) || is_ip_literal(host) {
            return None;
        }
        let parent = host.strip_suffix(self.host.as_str())?;
        parent.strip_suffix('.').filter(|label| !label.is_empty()).map(|_| self.host.len())
    }
}

/// Internet access from PDFs. The default asks about every web site, as PdfCraft always has.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UrlPolicy {
    pub mode: UrlMode,
    /// The custom list, in the order added. One rule per host; [`UrlPolicy::set_rule`] replaces.
    pub rules: Vec<HostRule>,
    /// What custom mode does with a host on no rule.
    pub default_action: HostAction,
}

impl UrlPolicy {
    /// Add `rule`, replacing any rule for the same host. Refused once [`MAX_RULES`] are kept.
    pub fn set_rule(&mut self, rule: HostRule) -> Result<(), String> {
        if let Some(old) = self.rules.iter_mut().find(|r| r.host == rule.host) {
            *old = rule;
            return Ok(());
        }
        if self.rules.len() >= MAX_RULES {
            return Err(format!("the list already holds {MAX_RULES} hosts, the most it keeps; remove some first"));
        }
        self.rules.push(rule);
        Ok(())
    }

    /// Remove the rule for `host` (as a user typed it). Returns whether one was there.
    pub fn remove_rule(&mut self, host: &str) -> Result<bool, String> {
        let wanted = HostRule::new(host, HostAction::Ask, false)?.host;
        let before = self.rules.len();
        self.rules.retain(|r| r.host != wanted);
        Ok(self.rules.len() != before)
    }

    /// The same policy rebuilt from untrusted data (a settings file): each rule's host
    /// normalised, rules that can't be read dropped, later rules for one host replacing earlier
    /// ones, at most [`MAX_RULES`]. Returns the policy and how many rules were dropped.
    pub fn sanitized(&self) -> (Self, usize) {
        let mut out = Self { mode: self.mode, rules: Vec::new(), default_action: self.default_action };
        let mut dropped = 0usize;
        for r in &self.rules {
            match HostRule::new(&r.host, r.action, r.subdomains) {
                Ok(rule) if out.set_rule(rule.clone()).is_ok() => {}
                _ => dropped = dropped.saturating_add(1),
            }
        }
        (out, dropped)
    }

    /// The rule that decides for `host` (normal form): the longest match, and the strictest
    /// action among equals.
    pub fn rule_for(&self, host: &str) -> Option<&HostRule> {
        self.rules.iter().filter_map(|r| r.matches(host).map(|len| ((len, r.action.strictness()), r))).max_by_key(|(key, _)| *key).map(|(_, r)| r)
    }
}

/// What may happen with an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UrlDecision {
    /// Reach it without asking.
    Allow,
    /// Don't reach it; say why.
    Block,
    /// Ask the user first.
    Prompt,
}

/// What kind of address was checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UrlKind {
    /// `http:` or `https:`: governed by the policy.
    Web,
    /// `mailto:`: opens the email app; not governed by host rules, always asks.
    Mail,
    /// Any other scheme, or an address PdfCraft never opens: always blocked.
    Refused,
}

/// The answer for one address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UrlCheck {
    pub decision: UrlDecision,
    pub kind: UrlKind,
    /// The address's scheme (lowercase), when it has one.
    pub scheme: Option<String>,
    /// The host in normal form, for a web address.
    pub host: Option<String>,
    /// The custom rule that decided, if one did.
    pub rule: Option<HostRule>,
    /// Why, in a sentence an agent or a notice can show.
    pub reason: String,
}

impl UrlCheck {
    fn refused(scheme: Option<String>, reason: String) -> Self {
        Self { decision: UrlDecision::Block, kind: UrlKind::Refused, scheme, host: None, rule: None, reason }
    }
}

fn is_ip_literal(host: &str) -> bool {
    host.starts_with('[') || host.parse::<std::net::Ipv4Addr>().is_ok()
}

/// A host in normal form, from a name, an IP literal (`[::1]` or bare `::1`), `host:port`, or a
/// whole address (`https://user@Host.Example.:8443/path`). Errors say what was wrong.
pub fn normalize_host(input: &str) -> Result<String, String> {
    let text = input.trim();
    if text.is_empty() {
        return Err("the host is empty".into());
    }
    if text.len() > MAX_HOST_INPUT {
        return Err(format!("the host is longer than {MAX_HOST_INPUT} bytes"));
    }
    // A whole address: keep only what follows the scheme.
    let rest = match text.find("://") {
        Some(i) => text.get(i.saturating_add(3)..).unwrap_or_default(),
        None => text.strip_prefix("//").unwrap_or(text),
    };
    let authority = rest.split(['/', '\\', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(inner) = host_port.strip_prefix('[') {
        let (inside, after) = inner.split_once(']').ok_or_else(|| format!("{host_port:?} has no closing ]"))?;
        check_port(after)?;
        format!("[{inside}]")
    } else if host_port.matches(':').count() > 1 {
        // A bare IPv6 address (no port can be told apart without brackets).
        format!("[{host_port}]")
    } else {
        match host_port.split_once(':') {
            Some((h, port)) => {
                check_port(&format!(":{port}"))?;
                h.to_string()
            }
            None => host_port.to_string(),
        }
    };
    let host = host.strip_suffix('.').unwrap_or(&host);
    if host.is_empty() {
        return Err(format!("{text:?} has no host"));
    }
    if host.contains('*') {
        return Err(format!("{text:?}: a wildcard is allowed only as a leading \"*.\""));
    }
    let parsed = url::Host::parse(host).map_err(|e| format!("{host:?} isn't a valid host name or IP address ({e})"))?;
    let normal = match parsed {
        url::Host::Domain(d) => d,
        url::Host::Ipv4(a) => a.to_string(),
        url::Host::Ipv6(a) => format!("[{a}]"),
    };
    if normal.is_empty() || normal.len() > MAX_HOST || normal.split('.').any(str::is_empty) {
        return Err(format!("{host:?} isn't a valid host name"));
    }
    Ok(normal)
}

/// `""` or `:` followed by a port number (0–65535).
fn check_port(after: &str) -> Result<(), String> {
    if after.is_empty() {
        return Ok(());
    }
    let digits = after.strip_prefix(':').ok_or_else(|| format!("unexpected {after:?} after the host"))?;
    if digits.is_empty() {
        return Ok(());
    }
    if !digits.bytes().all(|b| b.is_ascii_digit()) || digits.parse::<u16>().is_err() {
        return Err(format!("{digits:?} isn't a port number"));
    }
    Ok(())
}

/// Whether `policy` lets a document reach `url`, and why.
pub fn check_url(policy: &UrlPolicy, url: &str) -> UrlCheck {
    let scheme = url.trim().split_once(':').map(|(s, _)| s.to_ascii_lowercase()).filter(|s| !s.is_empty());
    let url = match crate::links::document_url(url) {
        Ok(u) => u,
        Err(e) => {
            return UrlCheck::refused(
                scheme,
                format!("PdfCraft never opens this address from a document: {e}. Only web (http, https) and email (mailto) addresses are allowed."),
            );
        }
    };
    let Ok(parsed) = url::Url::parse(&url) else {
        return UrlCheck::refused(scheme, "the address isn't a complete web or email address".into());
    };
    let scheme = Some(parsed.scheme().to_string());
    if parsed.scheme() == "mailto" {
        return UrlCheck {
            decision: UrlDecision::Prompt,
            kind: UrlKind::Mail,
            scheme,
            host: None,
            rule: None,
            reason: "an email address opens your email app; Internet access settings apply only to web sites, so PdfCraft asks first".into(),
        };
    }
    let host = match parsed.host_str().map(normalize_host) {
        Some(Ok(h)) => h,
        Some(Err(e)) => return UrlCheck::refused(scheme, format!("the address's host can't be read: {e}")),
        None => return UrlCheck::refused(scheme, "the address has no host".into()),
    };
    let web =
        |decision, rule, reason: String| UrlCheck { decision, kind: UrlKind::Web, scheme: scheme.clone(), host: Some(host.clone()), rule, reason };
    match policy.mode {
        UrlMode::AllowAll => web(UrlDecision::Allow, None, "Internet access settings allow every web site".into()),
        UrlMode::BlockAll => web(UrlDecision::Block, None, format!("Internet access settings block every web site, including {host}")),
        UrlMode::Custom => match policy.rule_for(&host) {
            Some(rule) => {
                let how = match rule.action {
                    HostAction::Allow => "allow",
                    HostAction::Block => "block",
                    HostAction::Ask => "ask about",
                };
                let which = if rule.host == host { rule.host.clone() } else { format!("{} and its subdomains", rule.host) };
                web(rule.action.decision(), Some(rule.clone()), format!("Internet access settings {how} {which}"))
            }
            None => {
                let reason = match policy.default_action {
                    HostAction::Allow => format!("{host} isn't on the list, and other web sites are allowed"),
                    HostAction::Block => format!("{host} isn't on the list, and other web sites are blocked"),
                    HostAction::Ask => format!("{host} isn't on the list, so PdfCraft asks first"),
                };
                web(policy.default_action.decision(), None, reason)
            }
        },
    }
}

impl Session {
    /// Internet access from PDFs (Preferences ▸ Trust Manager).
    pub fn url_policy(&self) -> &UrlPolicy {
        &self.url_policy
    }

    /// Replace the policy. Its rules are normalised; returns how many could not be read and were
    /// dropped (a hand-edited settings file).
    pub fn set_url_policy(&mut self, policy: &UrlPolicy) -> usize {
        let (policy, dropped) = policy.sanitized();
        self.url_policy = policy;
        dropped
    }

    /// Whether a document may reach `url` under the current policy.
    pub fn check_url(&self, url: &str) -> UrlCheck {
        check_url(&self.url_policy, url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(rules: &[(&str, HostAction, bool)], default_action: HostAction) -> UrlPolicy {
        let mut p = UrlPolicy { mode: UrlMode::Custom, rules: Vec::new(), default_action };
        for (h, a, s) in rules {
            p.set_rule(HostRule::new(h, *a, *s).unwrap()).unwrap();
        }
        p
    }

    #[test]
    fn hosts_normalise_case_dots_ports_userinfo_and_idna() {
        for (input, want) in [
            ("Example.COM", "example.com"),
            ("example.com.", "example.com"),
            ("  example.com:8443 ", "example.com"),
            ("https://user:pw@Docs.Example.com.:443/path?q#f", "docs.example.com"),
            ("//example.com/x", "example.com"),
            ("BÜCHER.de", "xn--bcher-kva.de"),
            ("xn--bcher-kva.de", "xn--bcher-kva.de"),
            ("127.0.0.1:80", "127.0.0.1"),
            ("0x7f.1", "127.0.0.1"),
            ("[::1]:8080", "[::1]"),
            ("::1", "[::1]"),
            ("[0:0:0:0:0:0:0:1]", "[::1]"),
            ("http://[2001:DB8::1]/", "[2001:db8::1]"),
        ] {
            assert_eq!(normalize_host(input).as_deref(), Ok(want), "{input}");
        }
        for bad in ["", "   ", "example..com", "exa mple.com", "a.*.com", "example.com:99999", "example.com:8x", "[::1", "https://", "."] {
            assert!(normalize_host(bad).is_err(), "{bad:?} -> {:?}", normalize_host(bad));
        }
        assert!(normalize_host(&"a".repeat(MAX_HOST_INPUT + 1)).is_err());
        assert!(normalize_host(&format!("{}.com", "a.".repeat(130))).is_err(), "longer than 253");
    }

    #[test]
    fn rules_match_subdomains_unless_exact_and_never_by_suffix_alone() {
        let p = custom(&[("example.com", HostAction::Allow, true), ("exact.org", HostAction::Allow, false)], HostAction::Block);
        let d = |u: &str| check_url(&p, u).decision;
        assert_eq!(d("https://example.com/"), UrlDecision::Allow);
        assert_eq!(d("https://a.b.EXAMPLE.com./x"), UrlDecision::Allow);
        assert_eq!(d("https://badexample.com/"), UrlDecision::Block, "suffix without a dot");
        assert_eq!(d("https://example.com.evil.net/"), UrlDecision::Block);
        assert_eq!(d("https://exact.org/"), UrlDecision::Allow);
        assert_eq!(d("https://www.exact.org/"), UrlDecision::Block, "exact rule");
        // The user-info prefix doesn't decide: the real host does.
        assert_eq!(d("https://example.com@evil.net/"), UrlDecision::Block);
        assert_eq!(check_url(&p, "https://example.com@evil.net/").host.as_deref(), Some("evil.net"));
        // The port doesn't matter.
        assert_eq!(d("http://example.com:8080/"), UrlDecision::Allow);
    }

    #[test]
    fn the_most_specific_rule_wins_and_block_breaks_ties() {
        let p = custom(
            &[("example.com", HostAction::Allow, true), ("ads.example.com", HostAction::Block, true), ("*.cdn.example.com", HostAction::Ask, false)],
            HostAction::Allow,
        );
        let c = check_url(&p, "https://x.ads.example.com/");
        assert_eq!((c.decision, c.rule.map(|r| r.host)), (UrlDecision::Block, Some("ads.example.com".into())));
        assert_eq!(check_url(&p, "https://img.cdn.example.com/").decision, UrlDecision::Prompt, "*. forces subdomains");
        assert_eq!(check_url(&p, "https://www.example.com/").decision, UrlDecision::Allow);
        // Equal matches: the stricter action decides, wherever it is in the list.
        let q = UrlPolicy {
            rules: vec![
                HostRule { host: "example.com".into(), action: HostAction::Allow, subdomains: true },
                HostRule { host: "example.com".into(), action: HostAction::Block, subdomains: true },
            ],
            ..Default::default()
        };
        assert_eq!(check_url(&q, "https://example.com/").decision, UrlDecision::Block);
    }

    #[test]
    fn ip_literals_match_only_themselves() {
        let p = custom(
            &[("127.0.0.1", HostAction::Allow, true), ("[::1]", HostAction::Allow, true), ("0.0.1", HostAction::Block, true)],
            HostAction::Block,
        );
        assert!(p.rules.iter().all(|r| !r.subdomains));
        assert_eq!(check_url(&p, "http://127.0.0.1:9000/").decision, UrlDecision::Allow);
        assert_eq!(check_url(&p, "http://0x7f.0.0.1/").decision, UrlDecision::Allow, "the same address in another form");
        assert_eq!(check_url(&p, "http://[::1]/").decision, UrlDecision::Allow);
        assert_eq!(check_url(&p, "http://10.0.0.1/").decision, UrlDecision::Block);
    }

    #[test]
    fn the_three_modes() {
        let rules = [("example.com", HostAction::Block, true)];
        let mut p = custom(&rules, HostAction::Ask);
        assert_eq!(check_url(&p, "https://other.net/").decision, UrlDecision::Prompt);
        assert_eq!(check_url(&p, "https://example.com/").decision, UrlDecision::Block);
        p.mode = UrlMode::AllowAll;
        assert_eq!(check_url(&p, "https://example.com/").decision, UrlDecision::Allow, "rules apply only in custom mode");
        p.mode = UrlMode::BlockAll;
        let c = check_url(&p, "https://other.net/");
        assert_eq!((c.decision, c.kind), (UrlDecision::Block, UrlKind::Web));
        assert!(c.reason.contains("other.net"), "{}", c.reason);
        // The default asks, as PdfCraft always did.
        assert_eq!(check_url(&UrlPolicy::default(), "https://example.com/").decision, UrlDecision::Prompt);
    }

    #[test]
    fn other_schemes_are_refused_and_email_is_reported_apart() {
        let p = UrlPolicy { mode: UrlMode::AllowAll, ..Default::default() };
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
            "smb://host/share",
            "ms-settings:",
            "",
            "   ",
            "https://",
            "relative/path",
        ] {
            let c = check_url(&p, bad);
            assert_eq!((c.decision, c.kind), (UrlDecision::Block, UrlKind::Refused), "{bad:?}");
        }
        assert_eq!(check_url(&p, "FILE:///x").scheme.as_deref(), Some("file"));
        let m = check_url(&UrlPolicy { mode: UrlMode::BlockAll, ..Default::default() }, "mailto:forms@example.org");
        assert_eq!((m.decision, m.kind, m.host), (UrlDecision::Prompt, UrlKind::Mail, None));
        assert_eq!(check_url(&p, "mailto:a@example.org?attach=/etc/passwd").kind, UrlKind::Refused);
    }

    #[test]
    fn rules_from_settings_are_normalised_deduplicated_and_capped() {
        let p = UrlPolicy {
            rules: vec![
                HostRule { host: "Example.COM.".into(), action: HostAction::Allow, subdomains: true },
                HostRule { host: "not a host".into(), action: HostAction::Allow, subdomains: true },
                HostRule { host: "example.com".into(), action: HostAction::Block, subdomains: false },
                HostRule { host: "10.0.0.1".into(), action: HostAction::Allow, subdomains: true },
            ],
            ..Default::default()
        };
        let (s, dropped) = p.sanitized();
        assert_eq!(dropped, 1);
        assert_eq!(
            s.rules,
            vec![
                HostRule { host: "example.com".into(), action: HostAction::Block, subdomains: false },
                HostRule { host: "10.0.0.1".into(), action: HostAction::Allow, subdomains: false },
            ]
        );
        let mut big = UrlPolicy::default();
        for i in 0..MAX_RULES {
            big.set_rule(HostRule::new(&format!("h{i}.example"), HostAction::Allow, true).unwrap()).unwrap();
        }
        assert!(big.set_rule(HostRule::new("one-more.example", HostAction::Allow, true).unwrap()).is_err());
        assert!(big.set_rule(HostRule::new("h0.example", HostAction::Block, true).unwrap()).is_ok(), "replacing still works");
        assert!(big.remove_rule("H0.Example.").unwrap());
        assert!(!big.remove_rule("h0.example").unwrap());
        // Serde round trip, and a hostile settings value reads as the default.
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"mode\":\"custom\"") && json.contains("\"default_action\":\"ask\""), "{json}");
        assert_eq!(serde_json::from_str::<UrlPolicy>(&json).unwrap(), s);
        assert_eq!(serde_json::from_str::<UrlPolicy>("{}").unwrap(), UrlPolicy::default());
        assert!(serde_json::from_str::<UrlPolicy>("{\"mode\":\"nope\"}").is_err());
    }

    #[test]
    fn the_session_keeps_the_policy() {
        let mut s = Session::new();
        assert_eq!(s.check_url("https://example.com/").decision, UrlDecision::Prompt);
        let p = UrlPolicy { mode: UrlMode::BlockAll, ..Default::default() };
        assert_eq!(s.set_url_policy(&p), 0);
        assert_eq!(s.url_policy().mode, UrlMode::BlockAll);
        assert_eq!(s.check_url("https://example.com/").decision, UrlDecision::Block);
    }
}
