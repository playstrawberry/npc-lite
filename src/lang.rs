//! The plugin's texts in English, French, Spanish and Turkish (`lang/<code>.lang`, `key=text`
//! lines; `{0}`, `{1}` and `{2}` are filled in). Any other language reads English.

const FILES: [(&str, &str); 4] = [
    ("en", include_str!("../lang/en.lang")),
    ("fr", include_str!("../lang/fr.lang")),
    ("es", include_str!("../lang/es.lang")),
    ("tr", include_str!("../lang/tr.lang")),
];

/// The language of a locale written as Minecraft or the server writes it (`fr_fr`, `FrFr`,
/// `tr-tr`).
#[must_use]
pub fn language(locale: &str) -> &'static str {
    let code = locale.get(..2).unwrap_or("").to_ascii_lowercase();
    FILES
        .iter()
        .map(|(c, _)| *c)
        .find(|c| *c == code)
        .unwrap_or("en")
}

fn lookup<'a>(file: &'a str, key: &str) -> Option<&'a str> {
    file.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim())
}

/// The text of `key` in `language`, with `args` in place of `{0}`, `{1}`, ...; English when the
/// language lacks the key, the key itself when English lacks it too.
#[must_use]
pub fn text(language: &str, key: &str, args: &[&str]) -> String {
    let file = |code: &str| FILES.iter().find(|(c, _)| *c == code).map(|(_, f)| *f);
    let raw = file(language)
        .and_then(|f| lookup(f, key))
        .or_else(|| file("en").and_then(|f| lookup(f, key)))
        .unwrap_or(key);
    // One pass, so a value that contains `{1}` is never filled in again.
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let slot = tail.get(1..2).and_then(|d| d.parse::<usize>().ok());
        match slot {
            Some(i) if tail.get(2..3) == Some("}") && i < args.len() => {
                out.push_str(args[i]);
                rest = &tail[3..];
            }
            _ => {
                out.push('{');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(file: &str) -> Vec<&str> {
        file.lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim()))
            .collect()
    }

    #[test]
    fn every_language_has_every_key_with_the_same_placeholders() {
        let english = FILES[0].1;
        let english_keys = keys(english);
        assert!(english_keys.len() > 50);
        for (code, file) in FILES {
            assert_eq!(
                keys(file),
                english_keys,
                "{code}: the same keys in the same order"
            );
            for key in &english_keys {
                let found = lookup(file, key).unwrap();
                assert!(!found.is_empty(), "{code} {key} is empty");
                let want = lookup(english, key).unwrap();
                for slot in ["{0}", "{1}", "{2}", "{3}"] {
                    assert_eq!(
                        found.contains(slot),
                        want.contains(slot),
                        "{code} {key} {slot}"
                    );
                }
            }
        }
    }

    #[test]
    fn locales_pick_their_language_and_others_read_english() {
        assert_eq!(language("fr_fr"), "fr");
        assert_eq!(language("FrCa"), "fr");
        assert_eq!(language("es_mx"), "es");
        assert_eq!(language("tr-tr"), "tr");
        assert_eq!(language("de_de"), "en");
        assert_eq!(language(""), "en");
        assert_eq!(text("en", "created", &["guide"]), "Created NPC guide");
        assert_ne!(
            text("tr", "created", &["guide"]),
            text("en", "created", &["guide"])
        );
        assert_eq!(text("en", "no_such_key", &[]), "no_such_key");
        // A value with braces is put in as it is.
        assert_eq!(text("en", "created", &["{1}"]), "Created NPC {1}");
    }
}
