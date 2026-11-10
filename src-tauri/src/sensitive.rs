//! FR-P03 / NFR-S03: sensitive-content detection done in memory, no persistence.
use once_cell::sync::Lazy;
use std::sync::Mutex;

struct Patterns {
    rules: Vec<(&'static str, regex_lite::Regex)>,
}

// regex-lite is a tiny, dependency-free regex engine (keeps binary small, no DB).
// (declared in Cargo.toml)
use regex_lite::Regex;

static PATTERNS: Lazy<Mutex<Option<Patterns>>> = Lazy::new(|| Mutex::new(None));

fn ensure_init() {
    let mut guard = PATTERNS.lock().unwrap();
    if guard.is_none() {
        let rules: Vec<(&'static str, Regex)> = vec![
            ("aws-access-key", Regex::new(r"AKIA[0-9A-Z]{16}").unwrap()),
            ("jwt", Regex::new(r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}").unwrap()),
            ("private-key", Regex::new(r"-----BEGIN (RSA |EC |OPENSSH |PGP |)?PRIVATE KEY( BLOCK)?-----").unwrap()),
            ("password-field", Regex::new(r"(?i)(password|passwd|pwd)\s*[:=]\s*\S+").unwrap()),
            ("slack-token", Regex::new(r"xox[baprs]-[A-Za-z0-9-]{10,}").unwrap()),
            ("generic-secret", Regex::new(r"(?i)(api[_-]?key|secret|token)\s*[:=]\s*[A-Za-z0-9_\-]{16,}").unwrap()),
        ];
        *guard = Some(Patterns { rules });
    }
}

/// Returns Some(rule_name) if the text looks like a credential/secret.
pub fn detect(text: &str) -> Option<&'static str> {
    if text.len() < 8 { return None; }
    ensure_init();
    let guard = PATTERNS.lock().unwrap();
    let p = guard.as_ref().unwrap();
    for (name, re) in &p.rules {
        if re.is_match(text) { return Some(name); }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_aws_key() {
        assert_eq!(detect("aws AKIAIOSFODNN7EXAMPLE key"), Some("aws-access-key"));
    }
    #[test]
    fn detects_jwt() {
        let jwt = "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert_eq!(detect(jwt), Some("jwt"));
    }
    #[test]
    fn detects_private_key() {
        assert_eq!(detect("-----BEGIN OPENSSH PRIVATE KEY-----\nabc"), Some("private-key"));
    }
    #[test]
    fn detects_password_assignment() {
        assert_eq!(detect("password: hUnTer2!"), Some("password-field"));
    }
    #[test]
    fn clean_text_passes() {
        assert_eq!(detect("下午三点开会，记得带电脑"), None);
        assert_eq!(detect("hello world"), None);
    }
}
