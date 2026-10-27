//! The standard licence texts a Store writes beside a model whose repository holds no licence
//! file (format spec 4.5, `license_text: "template"`): MIT and Apache-2.0, the licences of the
//! Model2Vec models in use. The Apache-2.0 text is the crate's own `LICENSE-APACHE`.

/// The standard MIT licence text, with the copyright line left to the model card.
const MIT: &str = "MIT License

Copyright (c) the copyright holders named in the model card (README.md)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
";

/// The standard Apache License 2.0 text.
const APACHE_2_0: &str = include_str!("../../../LICENSE-APACHE");

/// The licence file a Store writes for a model under the SPDX licence `license` when its
/// repository holds none: a note saying where the text comes from, then the standard text.
/// `None` for a licence without a bundled template.
pub(super) fn template(license: &str, repo: Option<&str>) -> Option<String> {
    let text = match license.to_ascii_lowercase().as_str() {
        "mit" => MIT,
        "apache-2.0" => APACHE_2_0,
        _ => return None,
    };
    let source = match repo {
        Some(repo) => format!("the Hugging Face repository {repo}"),
        None => "the model's repository".to_string(),
    };
    Some(format!(
        "This file was written by Graphersal, not taken from the model's repository: {source} \
         holds no licence file, and its model card (README.md) names the licence {license}. \
         Below is the standard text of that licence; the copyright holders are those the model \
         card names.\n\n---\n\n{text}"
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn templates_exist_for_mit_and_apache() {
        let mit = super::template("MIT", Some("o/n")).unwrap();
        assert!(mit.contains("o/n") && mit.contains("Permission is hereby granted"));
        let apache = super::template("apache-2.0", None).unwrap();
        assert!(apache.contains("Apache License") && apache.contains("Version 2.0"));
        assert!(super::template("cc-by-nc-4.0", None).is_none());
    }
}
