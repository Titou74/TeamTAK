//! Names derived from user input: team slugs (the TAK group name), TAK usernames, e-mails.

/// Longest team slug. The slug is the `cn` of the LDAP group and the channel name in ATAK.
pub const MAX_SLUG_LEN: usize = 32;
/// Longest TAK username; it is also the CN of the client certificate (64 characters at most).
pub const MAX_USERNAME_LEN: usize = 48;

/// Group name prefixes kept for later phases (`evt-` is used by cross-team events).
pub const RESERVED_SLUG_PREFIXES: &[&str] = &["evt-"];
/// Group names TAK Server gives a meaning to.
pub const RESERVED_SLUGS: &[&str] = &["anon", "tak-admins", "admin", "administrators"];

fn fold_char(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => "a",
        'ç' | 'Ç' => "c",
        'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => "e",
        'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => "i",
        'ñ' | 'Ñ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' => "o",
        'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => "u",
        'ý' | 'ÿ' | 'Ý' => "y",
        'œ' | 'Œ' => "oe",
        'æ' | 'Æ' => "ae",
        'ß' => "ss",
        _ => return None,
    })
}

/// Lowercase ASCII letters, digits and single dashes; accents are folded, anything else
/// becomes a dash. Returns an empty string when nothing usable is left.
pub fn slugify(input: &str, max_len: usize) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in input.trim().chars() {
        let folded: String = match fold_char(c) {
            Some(s) => s.to_string(),
            None if c.is_ascii_alphanumeric() => c.to_ascii_lowercase().to_string(),
            None => String::new(),
        };
        if folded.is_empty() {
            dash = !out.is_empty();
        } else {
            if dash {
                out.push('-');
                dash = false;
            }
            out.push_str(&folded);
        }
    }
    if out.len() > max_len {
        out.truncate(max_len);
        while out.ends_with('-') {
            out.pop();
        }
    }
    out
}

/// A slug is accepted as a TAK group name: 2 to 32 characters, `[a-z0-9-]`, not starting or
/// ending with a dash, not reserved.
pub fn is_valid_slug(slug: &str) -> bool {
    let shape = (2..=MAX_SLUG_LEN).contains(&slug.len())
        && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--");
    shape && !RESERVED_SLUGS.contains(&slug) && !RESERVED_SLUG_PREFIXES.iter().any(|p| slug.starts_with(p))
}

/// The three ways a team hands out TAK access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    /// One TAK account for the whole team.
    Shared,
    /// One TAK account per member of the team.
    Individual,
    /// A TAK account for a person who has no account on the platform.
    External,
}

impl AccessKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AccessKind::Shared => "shared",
            AccessKind::Individual => "individual",
            AccessKind::External => "external",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "shared" => Some(AccessKind::Shared),
            "individual" => Some(AccessKind::Individual),
            "external" => Some(AccessKind::External),
            _ => None,
        }
    }
}

/// Builds the TAK username of an access. `hint` is the member's e-mail for an individual
/// access and the label typed by the team leader for an external one. `attempt` starts at 0
/// and is raised by the caller while the result is already taken.
pub fn access_username(team_slug: &str, kind: AccessKind, hint: &str, attempt: u32) -> String {
    let suffix = match kind {
        AccessKind::Shared => "shared".to_string(),
        AccessKind::Individual => {
            let local = hint.split('@').next().unwrap_or(hint);
            let s = slugify(local, 20);
            if s.is_empty() {
                "member".into()
            } else {
                s
            }
        }
        AccessKind::External => {
            let s = slugify(hint, 18);
            format!("x-{}", if s.is_empty() { "guest" } else { &s })
        }
    };
    let mut name = format!("{team_slug}-{suffix}");
    name.truncate(MAX_USERNAME_LEN - 4);
    while name.ends_with('-') {
        name.pop();
    }
    if attempt > 0 {
        name.push_str(&format!("-{}", attempt + 1));
    }
    name
}

/// Trims and lowercases an e-mail address and checks its general shape. Deliverability is
/// proven by the login code itself, so the check stays simple.
pub fn normalize_email(input: &str) -> Option<String> {
    let email = input.trim().to_lowercase();
    if email.len() > 254 || email.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (local, domain) = email.split_once('@')?;
    if local.is_empty() || local.len() > 64 || domain.contains('@') {
        return None;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty() && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    let local_ok = local.chars().all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~.-".contains(c));
    (domain_ok && local_ok).then_some(email)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_folds_accents_and_collapses_separators() {
        assert_eq!(slugify("  Équipe  Montagne n°2 ", 32), "equipe-montagne-n-2");
        assert_eq!(slugify("Sauveteurs_de l'Œil", 32), "sauveteurs-de-l-oeil");
        assert_eq!(slugify("---", 32), "");
        assert_eq!(slugify("日本", 32), "");
    }

    #[test]
    fn slug_is_truncated_without_trailing_dash() {
        let s = slugify("abcdefghij klmnopqrst uvwxyzabcd efgh", 21);
        assert_eq!(s, "abcdefghij-klmnopqrst");
        assert!(is_valid_slug(&s));
    }

    #[test]
    fn slug_validation() {
        assert!(is_valid_slug("alpha"));
        assert!(is_valid_slug("team-42"));
        assert!(!is_valid_slug("a"));
        assert!(!is_valid_slug("Alpha"));
        assert!(!is_valid_slug("-alpha"));
        assert!(!is_valid_slug("al--pha"));
        assert!(!is_valid_slug("evt-exercise"), "prefix kept for events");
        assert!(!is_valid_slug("tak-admins"));
        assert!(!is_valid_slug(&"a".repeat(33)));
    }

    #[test]
    fn usernames_by_kind() {
        assert_eq!(access_username("alpha", AccessKind::Shared, "", 0), "alpha-shared");
        assert_eq!(access_username("alpha", AccessKind::Individual, "Jean.Dupont@example.org", 0), "alpha-jean-dupont");
        assert_eq!(access_username("alpha", AccessKind::External, "Pilote Hélico", 0), "alpha-x-pilote-helico");
        assert_eq!(access_username("alpha", AccessKind::External, "Pilote Hélico", 1), "alpha-x-pilote-helico-2");
        assert_eq!(access_username("alpha", AccessKind::Individual, "@", 0), "alpha-member");
    }

    #[test]
    fn usernames_fit_a_certificate_cn() {
        let slug = "a".repeat(MAX_SLUG_LEN);
        for kind in [AccessKind::Shared, AccessKind::Individual, AccessKind::External] {
            let u = access_username(&slug, kind, &"x".repeat(80), 12);
            assert!(u.len() <= MAX_USERNAME_LEN, "{u}");
            assert!(u.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
        }
    }

    #[test]
    fn emails() {
        assert_eq!(normalize_email("  Jean.Dupont@Example.ORG "), Some("jean.dupont@example.org".into()));
        assert_eq!(normalize_email("a+tag@sub.example.fr"), Some("a+tag@sub.example.fr".into()));
        for bad in ["", "a", "a@", "@b.fr", "a@b", "a b@c.fr", "a@b..fr", "a@-b.fr", "a@b@c.fr", "é@b.fr"] {
            assert_eq!(normalize_email(bad), None, "{bad}");
        }
    }
}
