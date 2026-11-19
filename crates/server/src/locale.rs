//! Languages of the platform and the texts of its e-mails.
//!
//! The texts are compiled in from `web/src/locales/<language>/email.json` (see build.rs).
//! The list of languages is the set of folders that hold such a file: adding a language to
//! the UI adds it here too.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::Value;

include!(concat!(env!("OUT_DIR"), "/email_locales.rs"));

/// Default and fallback language.
pub const DEFAULT_LANGUAGE: &str = "en";

fn catalogs() -> &'static BTreeMap<&'static str, Value> {
    static CATALOGS: OnceLock<BTreeMap<&'static str, Value>> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        EMAIL_LOCALES
            .iter()
            .map(|(language, json)| {
                (*language, serde_json::from_str(json).unwrap_or_else(|e| panic!("email.json of {language}: {e}")))
            })
            .collect()
    })
}

/// Languages the platform supports, in alphabetical order.
pub fn languages() -> Vec<&'static str> {
    catalogs().keys().copied().collect()
}

/// The language itself when supported, the default one otherwise.
pub fn resolve(language: &str) -> &'static str {
    catalogs().keys().copied().find(|l| *l == language).unwrap_or(DEFAULT_LANGUAGE)
}

pub fn is_supported(language: &str) -> bool {
    catalogs().contains_key(language)
}

fn lookup(language: &str, path: &[&str]) -> Option<&'static str> {
    let mut node = catalogs().get(language)?;
    for key in path {
        node = node.get(key)?;
    }
    node.as_str()
}

/// Text at `path` in `language`, falling back to English.
fn text(language: &str, path: &[&str]) -> &'static str {
    lookup(language, path)
        .or_else(|| lookup(DEFAULT_LANGUAGE, path))
        .unwrap_or_else(|| panic!("e-mail text {} missing in the default language", path.join(".")))
}

fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in vars {
        out = out.replace(&format!("{{{{{name}}}}}"), value);
    }
    out
}

/// The e-mails the platform sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    LoginCode,
    Invitation,
    TeamRequest,
    TeamApproved,
    TeamRejected,
}

impl Template {
    #[cfg_attr(not(test), allow(dead_code))]
    pub const ALL: &'static [Template] =
        &[Template::LoginCode, Template::Invitation, Template::TeamRequest, Template::TeamApproved, Template::TeamRejected];

    pub fn key(self) -> &'static str {
        match self {
            Template::LoginCode => "login_code",
            Template::Invitation => "invitation",
            Template::TeamRequest => "team_request",
            Template::TeamApproved => "team_approved",
            Template::TeamRejected => "team_rejected",
        }
    }
}

pub struct Rendered {
    pub subject: String,
    pub body: String,
}

/// Renders an e-mail in `language` (English when the language is unknown). `vars` replace
/// the `{{name}}` placeholders; `url` is also used by the signature.
pub fn render(template: Template, language: &str, vars: &[(&str, &str)], url: &str) -> Rendered {
    let language = resolve(language);
    // A subject is a single line whatever the variables hold.
    let subject = fill(text(language, &[template.key(), "subject"]), vars).replace(['\r', '\n'], " ");
    let body = fill(text(language, &[template.key(), "body"]), vars);
    let signature = fill(text(language, &["signature"]), &[("url", url)]);
    Rendered { subject, body: format!("{body}\n\n-- \n{signature}\n") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(s: &str) -> BTreeSet<String> {
        s.split("{{").skip(1).filter_map(|p| p.split_once("}}")).map(|(name, _)| name.to_string()).collect()
    }

    fn leaves(prefix: &str, v: &Value, out: &mut BTreeMap<String, String>) {
        match v {
            Value::Object(map) => {
                for (k, v) in map {
                    let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                    leaves(&key, v, out);
                }
            }
            Value::String(s) => {
                out.insert(prefix.to_string(), s.clone());
            }
            other => panic!("{prefix}: unexpected value {other}"),
        }
    }

    #[test]
    fn english_and_french_are_present() {
        assert!(is_supported("en") && is_supported("fr"));
        assert_eq!(resolve("fr"), "fr");
        assert_eq!(resolve("xx"), "en");
    }

    #[test]
    fn every_language_has_the_same_keys_and_placeholders() {
        let mut reference = BTreeMap::new();
        leaves("", &catalogs()["en"], &mut reference);
        for (language, catalog) in catalogs() {
            let mut texts = BTreeMap::new();
            leaves("", catalog, &mut texts);
            assert_eq!(
                texts.keys().collect::<Vec<_>>(),
                reference.keys().collect::<Vec<_>>(),
                "keys of {language}/email.json differ from en"
            );
            for (key, value) in &texts {
                assert_eq!(placeholders(value), placeholders(&reference[key]), "placeholders of {key} in {language}");
            }
        }
    }

    #[test]
    fn every_template_has_its_texts() {
        for template in Template::ALL {
            for language in languages() {
                assert!(lookup(language, &[template.key(), "subject"]).is_some(), "{language} {}", template.key());
                assert!(lookup(language, &[template.key(), "body"]).is_some(), "{language} {}", template.key());
            }
        }
    }

    #[test]
    fn login_code_is_rendered_in_the_recipient_language() {
        let vars = [("code", "123456"), ("minutes", "10")];
        let en = render(Template::LoginCode, "en", &vars, "https://tak.example.org");
        let fr = render(Template::LoginCode, "fr", &vars, "https://tak.example.org");
        assert_eq!(en.subject, "Your sign-in code: 123456");
        assert_eq!(fr.subject, "Votre code de connexion : 123456");
        assert!(fr.body.contains("valable 10 minutes"));
        assert!(en.body.contains("https://tak.example.org"));
        assert!(!en.body.contains("{{") && !fr.body.contains("{{"));
        let fallback = render(Template::LoginCode, "de", &vars, "u");
        assert_eq!(fallback.subject, en.subject);
    }

    #[test]
    fn subjects_stay_on_one_line() {
        let r = render(Template::TeamRequest, "en", &[("team", "A\r\nBcc: x"), ("requester", "r"), ("message", "m")], "u");
        assert!(!r.subject.contains('\n') && !r.subject.contains('\r'));
    }
}
