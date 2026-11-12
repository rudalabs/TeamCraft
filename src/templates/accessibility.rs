//! Accessibility checks of rendered pages (WCAG 2.2, the parts a static
//! check can decide). Run over every page `every_template_renders` renders
//! (`every_rendered_page_is_accessible`):
//!
//! * every form control (`input` except hidden/submit/button/reset, `select`,
//!   `textarea`) has a label: `<label for>`, an enclosing `<label>`,
//!   `aria-label` or `aria-labelledby`;
//! * every button, link and `input type=submit|button|reset|image` has an
//!   accessible name: text, `aria-label`, `aria-labelledby`, `title`, an
//!   `img` with a non-empty `alt` or an `svg` with a `<title>` inside;
//! * every `img` has an `alt` attribute (empty for decorative pictures);
//! * no `tabindex` above 0;
//! * `<html lang>` on a full page;
//! * headings do not skip a level on the way down (h2 → h4);
//! * every `<th>` has a `scope` and a name (text – a visually hidden one
//!   for action columns – or `aria-label`/`title`);
//! * a full page with a `<main>` starts with a skip link: its first link
//!   points to the id of `<main>`.
//!
//! A small tokenizer is enough for HTML the templates produce: tags with
//! quoted attributes, comments, raw text in `script`/`style`.

use std::collections::{BTreeMap, BTreeSet};

/// One tag of the page.
#[derive(Debug)]
struct Tag {
    name: String,
    closing: bool,
    attrs: BTreeMap<String, String>,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }

    fn has_name_attr(&self) -> bool {
        ["aria-label", "aria-labelledby", "title"]
            .iter()
            .any(|a| self.attr(a).is_some_and(|v| !v.trim().is_empty()))
    }
}

enum Token {
    Tag(Tag),
    Text(String),
}

fn parse_attrs(s: &str) -> BTreeMap<String, String> {
    let mut attrs = BTreeMap::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        if start == i {
            i += 1;
            continue;
        }
        let name = s[start..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let vs = i;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                value = s[vs..i].to_string();
                i += 1;
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = s[vs..i].to_string();
            }
        }
        attrs.insert(name, value);
    }
    attrs
}

fn tokenize(html: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        if lt > 0 {
            out.push(Token::Text(rest[..lt].to_string()));
        }
        rest = &rest[lt..];
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |e| &rest[e + 3..]);
            continue;
        }
        if rest.starts_with("<!") {
            rest = rest.find('>').map_or("", |e| &rest[e + 1..]);
            continue;
        }
        // the end of the tag: the first '>' outside quotes
        let b = rest.as_bytes();
        let mut i = 1;
        let mut quote = 0u8;
        while i < b.len() {
            match b[i] {
                q @ (b'"' | b'\'') if quote == 0 => quote = q,
                q if q == quote => quote = 0,
                b'>' if quote == 0 => break,
                _ => {}
            }
            i += 1;
        }
        let inner = &rest[1..i.min(rest.len())];
        rest = rest.get(i + 1..).unwrap_or("");
        let closing = inner.starts_with('/');
        let inner = inner.trim_start_matches('/');
        let name_end = inner
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(inner.len());
        let name = inner[..name_end].to_ascii_lowercase();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            out.push(Token::Text(format!("<{inner}>")));
            continue;
        }
        let attrs = parse_attrs(&inner[name_end..]);
        let raw = !closing && (name == "script" || name == "style");
        out.push(Token::Tag(Tag {
            name: name.clone(),
            closing,
            attrs,
        }));
        if raw {
            let end = format!("</{name}");
            let e = rest.to_ascii_lowercase().find(&end).unwrap_or(rest.len());
            rest = &rest[e..];
        }
    }
    if !rest.is_empty() {
        out.push(Token::Text(rest.to_string()));
    }
    out
}

/// An element whose accessible name is collected until it closes.
struct Open {
    tag: &'static str,
    what: String,
    named: bool,
}

const NO_LABEL_TYPES: &[&str] = &["hidden", "submit", "button", "reset", "image"];

/// The accessibility problems of one rendered page.
pub fn check(page: &str, html: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut report = |what: String| problems.push(format!("{page}: {what}"));
    let tokens = tokenize(html);

    let mut label_for = BTreeSet::new();
    let mut controls: Vec<(String, String)> = Vec::new(); // (id, description)
    let mut in_label = 0usize;
    let mut open: Vec<Open> = Vec::new();
    let mut last_heading = 0u8;
    // the first link of the page and the id of <main> (skip link)
    let mut first_link: Option<String> = None;
    let mut main_id: Option<Option<String>> = None;
    let full_page = html
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("<!doctype");

    for token in &tokens {
        let tag = match token {
            Token::Text(t) => {
                let text = t.trim();
                if !text.is_empty() && !text.starts_with('<') {
                    for o in &mut open {
                        o.named = true;
                    }
                }
                continue;
            }
            Token::Tag(tag) => tag,
        };
        let n = tag.name.as_str();
        if tag.closing {
            match n {
                "label" => in_label = in_label.saturating_sub(1),
                "a" | "button" | "th" => {
                    if let Some(pos) = open.iter().rposition(|o| o.tag == n) {
                        let o = open.remove(pos);
                        if !o.named {
                            report(format!("{} has no accessible name", o.what));
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if let Some(t) = tag.attr("tabindex") {
            if t.trim().parse::<i32>().is_ok_and(|v| v > 0) {
                report(format!("<{n}> with tabindex={t}"));
            }
        }
        match n {
            "html" => {
                if tag.attr("lang").is_none_or(|l| l.trim().is_empty()) {
                    report("<html> without lang".into());
                }
            }
            "label" => {
                if let Some(f) = tag.attr("for") {
                    label_for.insert(f.to_string());
                }
                in_label += 1;
            }
            "input" | "select" | "textarea" => {
                let ty = tag.attr("type").unwrap_or("text").to_ascii_lowercase();
                let desc = describe(tag);
                if n == "input" && NO_LABEL_TYPES.contains(&ty.as_str()) {
                    let named = tag.has_name_attr()
                        || match ty.as_str() {
                            "image" => tag.attr("alt").is_some_and(|a| !a.trim().is_empty()),
                            "hidden" => true,
                            _ => tag.attr("value").is_some_and(|v| !v.trim().is_empty()),
                        };
                    if !named {
                        report(format!("{desc} has no accessible name"));
                    }
                } else if in_label == 0
                    && !tag
                        .attr("aria-label")
                        .or(tag.attr("aria-labelledby"))
                        .is_some_and(|v| !v.trim().is_empty())
                {
                    match tag.attr("id") {
                        Some(id) => controls.push((id.to_string(), desc)),
                        None => report(format!("{desc} has no label")),
                    }
                }
            }
            "a" | "button" => {
                if n == "a" && first_link.is_none() {
                    if let Some(href) = tag.attr("href") {
                        first_link = Some(href.to_string());
                    }
                }
                // a link without href is no control (an anchor or a placeholder)
                if n == "button" || tag.attr("href").is_some() {
                    open.push(Open {
                        tag: if n == "a" { "a" } else { "button" },
                        what: describe(tag),
                        named: tag.has_name_attr(),
                    });
                }
            }
            "img" => match tag.attr("alt") {
                None => report(format!("{} without alt", describe(tag))),
                Some(alt) if !alt.trim().is_empty() => {
                    for o in &mut open {
                        o.named = true;
                    }
                }
                Some(_) => {}
            },
            "svg" => {
                if tag.has_name_attr() {
                    for o in &mut open {
                        o.named = true;
                    }
                }
            }
            "th" => {
                if tag.attr("scope").is_none_or(|s| s.trim().is_empty()) {
                    report("<th> without scope".into());
                }
                open.push(Open {
                    tag: "th",
                    what: describe(tag),
                    named: tag.has_name_attr(),
                });
            }
            "main" if main_id.is_none() => {
                main_id = Some(tag.attr("id").map(str::to_string));
            }
            h if h.len() == 2 && h.starts_with('h') => {
                if let Some(level) = h[1..].parse::<u8>().ok().filter(|l| (1..=6).contains(l)) {
                    if last_heading > 0 && level > last_heading + 1 {
                        report(format!(
                            "heading <{h}> after <h{last_heading}> skips a level"
                        ));
                    }
                    last_heading = level;
                }
            }
            _ => {}
        }
    }
    for (id, desc) in controls {
        if !label_for.contains(&id) {
            report(format!("{desc} has no label"));
        }
    }
    for o in open {
        report(format!("{} is not closed", o.what));
    }
    if full_page {
        if let Some(id) = main_id {
            let target = id.map(|id| format!("#{id}"));
            if target.is_none() || first_link != target {
                report("no skip link to <main> as the first link of the page".into());
            }
        }
    }
    if full_page
        && !tokens
            .iter()
            .any(|t| matches!(t, Token::Tag(t) if t.name == "html"))
    {
        report("full page without <html>".into());
    }
    problems
}

fn describe(tag: &Tag) -> String {
    let mut s = format!("<{}", tag.name);
    for key in ["type", "name", "id", "class", "href", "src"] {
        if let Some(v) = tag.attr(key) {
            let v: String = v.chars().take(40).collect();
            s.push_str(&format!(" {key}=\"{v}\""));
        }
    }
    s.push('>');
    s
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn the_checks_find_what_they_promise() {
        let bad = r#"<!DOCTYPE html><html><body>
            <h1>T</h1><h3>skipped</h3>
            <input name="q"><select id="s"></select><textarea id="t"></textarea>
            <button class="x"></button><a href="/a"><span class="icon"></span></a>
            <img src="/x.png"><div tabindex="2"></div>
            <main id="main">
            <table><tr><th>Name</th><th scope="col"></th></tr></table>
            <input type="submit">
            </main>
        </body></html>"#;
        let p = check("bad", bad);
        let has = |part: &str| p.iter().any(|m| m.contains(part));
        assert!(has("<html> without lang"), "{p:#?}");
        assert!(has("<h3> after <h1>"), "{p:#?}");
        assert!(has(r#"<input name="q"> has no label"#), "{p:#?}");
        assert!(has(r#"<select id="s"> has no label"#), "{p:#?}");
        assert!(has(r#"<textarea id="t"> has no label"#), "{p:#?}");
        assert!(
            has(r#"<button class="x"> has no accessible name"#),
            "{p:#?}"
        );
        assert!(has(r#"<a href="/a"> has no accessible name"#), "{p:#?}");
        assert!(has("without alt"), "{p:#?}");
        assert!(has("tabindex=2"), "{p:#?}");
        assert!(has("<th> without scope"), "{p:#?}");
        assert!(
            has(r#"<input type="submit"> has no accessible name"#),
            "{p:#?}"
        );
        assert!(has("<th> has no accessible name"), "{p:#?}");
        assert!(has("no skip link to <main>"), "{p:#?}");
        assert_eq!(p.len(), 13, "{p:#?}");
        // a skip link to another target does not count either
        let elsewhere = r##"<!DOCTYPE html><html lang="en"><body>
            <a href="#top">Top</a><main id="main"><h1>T</h1></main></body></html>"##;
        assert_eq!(check("elsewhere", elsewhere).len(), 1);

        let good = r##"<!DOCTYPE html><html lang="en"><body>
            <a class="skip-link" href="#main">Skip to the main content</a>
            <main id="main">
            <h1>T</h1><h2>a</h2><h3>b</h3><h2>c</h2>
            <label for="q">Search</label><input id="q" name="q">
            <label>Name <input name="n"></label>
            <select aria-label="State"></select>
            <textarea aria-labelledby="h"></textarea>
            <input type="hidden" name="csrf"><input type="submit" value="Save">
            <button>Save</button><button aria-label="Close"><svg></svg></button>
            <a href="/a" title="Home"><span class="icon"></span></a>
            <a href="/b"><img src="/b.png" alt="Logo"></a><img src="/d.png" alt="">
            <a name="anchor"></a><div tabindex="0"></div><span tabindex="-1"></span>
            <table><tr><th scope="col">Name</th>
              <th scope="col"><span class="visually-hidden">Actions</span></th>
              <th scope="col" aria-label="Select"></th></tr></table>
            <script>if (a < b && "<button>") {}</script>
            </main>
        </body></html>"##;
        assert_eq!(check("good", good), Vec::<String>::new());
    }
}
