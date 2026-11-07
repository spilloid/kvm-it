//! Setting a workflow's profile values (its `[[setup]]` list) in its text: the app's setup form and `kvmit workflow
//! new|set` both come here. Only those `[config]` values change (and, for a new organisation name, its `[tenants]`
//! entry); comments and layout are kept, and the result is checked to differ from the original in nothing else.
use crate::workflow::Workflow;
use std::collections::BTreeMap;

/// `text` with these setup values set. Refused: a key that is not a setup value, a value its field does not allow, or
/// any change beyond them (checked by parsing the result).
pub fn apply(text: &str, values: &BTreeMap<String, String>) -> Result<String, String> {
    let wf = Workflow::parse(text)?;
    let mut expected = wf.clone();
    for (k, v) in values {
        let Some(field) = wf.setup.iter().find(|f| &f.key == k) else {
            let known: Vec<&str> = wf.setup.iter().map(|f| f.key.as_str()).collect();
            return Err(match known.is_empty() {
                true => "this workflow has no setup values: edit its file instead".into(),
                false => format!("{k:?} is not a setup value of this workflow (it has: {})", known.join(", ")),
            });
        };
        field.check(v)?;
    }
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    for (k, v) in values {
        if k == "tenant" && !wf.tenants.is_empty() && !wf.tenants.contains_key(v) {
            // A new organisation: the configured one is renamed, and its picture becomes `tenant-<name>.png` beside the
            // old one (saved by the user, like every reference screen).
            let old = &wf.config["tenant"];
            // Beside the old picture, in its folder however it was written (`/` or `\`).
            let picture = match wf.tenants[old].rfind(['/', '\\']) {
                Some(i) => format!("{}tenant-{v}.png", &wf.tenants[old][..=i]),
                None => format!("tenant-{v}.png"),
            };
            // Never the picture of another tenant or of a screen: two names for one picture would make the tenant
            // check tie (and refuse) and saving one would change the other. Compared as Windows does: any case, either
            // slash.
            // Compared by their parts: `./`, doubled and back slashes, and case do not make another file.
            let parts = |p: &str| p.split(['/', '\\']).filter(|c| !c.is_empty() && *c != ".").map(str::to_ascii_lowercase).collect::<Vec<_>>();
            // A path with `..` cannot be compared by its parts: refused, as it never loads anyway.
            let same = |a: &str| parts(a).iter().any(|c| c == "..") || parts(a) == parts(&picture);
            let taken = wf.tenants.iter().filter(|(t, _)| *t != old).map(|(_, p)| p).chain(wf.states.iter().flat_map(|s| &s.detect.screens));
            if let Some(p) = taken.into_iter().find(|p| same(p)) {
                return Err(format!("the picture {picture} would be the same as {p}, already used by this workflow: choose another name"));
            }
            let table = doc.get_mut("tenants").and_then(|t| t.as_table_like_mut()).ok_or("no [tenants] table")?;
            let key_decor = table.get_key_value(old).map(|(key, _)| key.leaf_decor().clone());
            let value_decor = table.get(old).and_then(|i| i.as_value()).map(|v| v.decor().clone());
            table.remove(old);
            let mut value = toml_edit::Value::from(picture.as_str());
            if let Some(d) = value_decor {
                *value.decor_mut() = d;
            }
            table.insert(v, toml_edit::Item::Value(value));
            if let (Some(decor), Some(mut key)) = (key_decor, table.key_mut(v)) {
                *key.leaf_decor_mut() = decor;
            }
            expected.tenants.remove(old);
            expected.tenants.insert(v.clone(), picture);
        }
        let config = doc.get_mut("config").and_then(|t| t.as_table_like_mut()).ok_or("no [config] table")?;
        let item = config.get_mut(k).ok_or_else(|| format!("{k:?} is not in [config]"))?;
        let decor = item.as_value().map(|v| v.decor().clone());
        let mut value = toml_edit::Value::from(v.as_str());
        if let Some(d) = decor {
            *value.decor_mut() = d;
        }
        *item = toml_edit::Item::Value(value);
        expected.config.insert(k.clone(), v.clone());
    }
    let mut out = doc.to_string();
    // The writer ends new lines with LF; a file written with CRLF (Windows editors) keeps CRLF throughout.
    if text.contains("\r\n") {
        out = out.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    let got = Workflow::parse(&out).map_err(|e| format!("the changed workflow does not parse: {e}"))?;
    if got != expected {
        return Err("changing the setup values would change more than them; nothing was changed".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WF: &str = r#"workflow_format = 1
name = "w"
start = ["A"]
match = "structure"

[config]
# how it enrols
mode = "one"   # keep this comment
tenant = "contoso"
note = "x"

[tenants]
# the password page, with branding
contoso = "pics/tenant-contoso.png"
other = "pics/other.png"

[[setup]]
key = "mode"
label = "Mode"
choices = [{ value = "one", label = "The first" }, "two"]

[[setup]]
key = "tenant"
label = "Organisation"

[[states]]
name = "A"
detect = { screen = "a.png" }
verify_tenant = true
terminal = true
"#;

    fn set(pairs: &[(&str, &str)]) -> Result<String, String> {
        apply(WF, &pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    #[test]
    fn setup_fields_are_parsed_in_file_order() {
        let wf = Workflow::parse(WF).unwrap();
        let keys: Vec<&str> = wf.setup.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, ["mode", "tenant"]);
        assert_eq!(wf.setup[0].choices[0].label, "The first");
        assert_eq!(wf.setup[0].choices[1].label, "two");
        assert!(wf.setup[1].choices.is_empty());
    }

    #[test]
    fn a_choice_is_set_and_comments_are_kept() {
        let out = set(&[("mode", "two")]).unwrap();
        assert!(out.contains("mode = \"two\"   # keep this comment"), "{out}");
        assert!(out.contains("# how it enrols"));
        assert_eq!(Workflow::parse(&out).unwrap().config["mode"], "two");
        assert_eq!(out.replace("mode = \"two\"", "mode = \"one\""), WF, "nothing else changed");
    }

    #[test]
    fn a_new_organisation_renames_the_configured_tenant() {
        let out = set(&[("tenant", "fabrikam")]).unwrap();
        let wf = Workflow::parse(&out).unwrap();
        assert_eq!(wf.config["tenant"], "fabrikam");
        assert_eq!(wf.tenants.get("fabrikam").map(String::as_str), Some("pics/tenant-fabrikam.png"));
        assert!(!wf.tenants.contains_key("contoso"));
        assert_eq!(wf.tenants["other"], "pics/other.png", "other allowed tenants are kept");
        assert!(out.contains("# the password page, with branding\nfabrikam = "), "{out}");
        // A picture written with backslashes keeps its folder; a comment after the old picture is kept.
        let win = WF.replace("contoso = \"pics/tenant-contoso.png\"", "contoso = 'pics\\tenant-contoso.png' # mine");
        let out = apply(&win, &[("tenant".to_string(), "fabrikam".to_string())].into()).unwrap();
        assert_eq!(Workflow::parse(&out).unwrap().tenants["fabrikam"], "pics\\tenant-fabrikam.png");
        assert!(out.contains("tenant-fabrikam.png\" # mine") || out.contains("tenant-fabrikam.png' # mine"), "{out}");
        // Choosing a tenant already listed only selects it.
        let out = set(&[("tenant", "other")]).unwrap();
        let wf = Workflow::parse(&out).unwrap();
        assert_eq!((wf.config["tenant"].as_str(), wf.tenants.len()), ("other", 2));
    }

    #[test]
    fn a_renamed_tenant_never_shares_a_picture() {
        let clash = WF.replace("other = \"pics/other.png\"", "other = \"pics/Tenant-Fabrikam.png\"");
        let e = apply(&clash, &[("tenant".to_string(), "fabrikam".to_string())].into()).unwrap_err();
        assert!(e.contains("already used"), "{e}");
        let screen = WF.replace("screen = \"a.png\"", "screen = \"pics\\\\tenant-fabrikam.png\"");
        assert!(apply(&screen, &[("tenant".to_string(), "fabrikam".to_string())].into()).unwrap_err().contains("already used"));
        for alias in ["./pics/tenant-fabrikam.png", "pics//tenant-fabrikam.png", "pics/./TENANT-fabrikam.png"] {
            let w = WF.replace("other = \"pics/other.png\"", &format!("other = \"{alias}\""));
            assert!(apply(&w, &[("tenant".to_string(), "fabrikam".to_string())].into()).unwrap_err().contains("already used"), "{alias}");
        }
    }

    #[test]
    fn a_crlf_file_stays_crlf() {
        let crlf = WF.replace('\n', "\r\n");
        let out = apply(&crlf, &[("mode".to_string(), "two".to_string())].into()).unwrap();
        assert_eq!(out, crlf.replace("mode = \"one\"", "mode = \"two\""));
        let out = apply(&crlf, &[("tenant".to_string(), "fabrikam".to_string())].into()).unwrap();
        assert!(!out.replace("\r\n", "").contains('\n'), "no bare LF: {out:?}");
    }

    #[test]
    fn only_setup_values_with_allowed_values_can_be_set() {
        assert!(set(&[("note", "y")]).unwrap_err().contains("not a setup value"));
        assert!(set(&[("mode", "three")]).unwrap_err().contains("not one of one, two"));
        for bad in ["", "a b", "../x", "fab.rikam", "x\ny"] {
            assert!(set(&[("tenant", bad)]).is_err(), "{bad:?}");
        }
        let none = WF.split("[[setup]]").next().unwrap().to_string() + "[[states]]" + WF.split("[[states]]").nth(1).unwrap();
        let e = apply(&none, &[("mode".to_string(), "two".to_string())].into()).unwrap_err();
        assert!(e.contains("no setup values"), "{e}");
    }

    #[test]
    fn secrets_are_listed_in_the_order_they_are_typed() {
        let wf = Workflow::parse(
            r#"workflow_format = 1
start = ["A"]
match = "luma"
[secrets]
a_password = { prompt = "Password" }
b_unused = { prompt = "Unused" }
z_user = { prompt = "User" }
[[states]]
name = "A"
detect = { screen = "a.png" }
next = ["B"]
actions = [ { secret_text = "{{z_user}}" } ]
[[states]]
name = "B"
detect = { screen = "b.png" }
next = ["C"]
actions = [ { repeat = { count = 1, steps = [ { secret_text = "{{ a_password }}{{z_user}}" } ] } } ]
[[states]]
name = "C"
detect = { screen = "c.png" }
terminal = true
"#,
        )
        .unwrap();
        let names: Vec<&str> = wf.secrets_in_order().iter().map(|(k, _)| *k).collect();
        assert_eq!(names, ["z_user", "a_password", "b_unused"]);
    }

    #[test]
    fn a_bad_setup_list_is_refused_at_parse() {
        let with = |s: &str| Workflow::parse(&WF.replace("[[states]]", &format!("{s}\n[[states]]")));
        assert!(with("[[setup]]\nkey = \"nope\"\nlabel = \"N\"\n").unwrap_err().contains("not in [config]"));
        assert!(with("[[setup]]\nkey = \"mode\"\nlabel = \"M\"\n").unwrap_err().contains("twice"));
        assert!(with("[[setup]]\nkey = \"note\"\nlabel = \"\"\n").unwrap_err().contains("label"));
        assert!(with("[[setup]]\nkey = \"note\"\nlabel = \"N\"\nchoices = [\"a\", \"b\"]\n").unwrap_err().contains("does not fit"));
        assert!(with("[[setup]]\nkey = \"note\"\nlabel = \"N\"\nchoices = [\"x\", \"x\"]\n").unwrap_err().contains("twice"));
        assert!(with("[[setup]]\nkey = \"note\"\nlabel = \"N\"\nfoo = 1\n").is_err(), "unknown fields");
        let t = WF.replace("key = \"tenant\"\nlabel = \"Organisation\"", "key = \"tenant\"\nlabel = \"O\"\nchoices = [\"contoso\"]");
        assert!(Workflow::parse(&t).unwrap_err().contains("takes no choices"));
    }
}
