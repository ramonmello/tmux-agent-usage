//! Execute the actual published snippets, so edits cannot silently drift.

/// Every document that may hold a `<!-- test:NAME -->` block.
pub const DOCUMENTS: [(&str, &str); 5] = [
    ("README.md", include_str!("../../README.md")),
    ("CONTRIBUTING.md", include_str!("../../CONTRIBUTING.md")),
    (
        "docs/configuration.md",
        include_str!("../../docs/configuration.md"),
    ),
    (
        "docs/how-it-works.md",
        include_str!("../../docs/how-it-works.md"),
    ),
    (
        "docs/troubleshooting.md",
        include_str!("../../docs/troubleshooting.md"),
    ),
];

/// The block marked `<!-- test:NAME -->`, which must exist exactly once
/// across all documents and use the given fence language.
pub fn snippet(name: &str, language: &str) -> String {
    let marker = format!("<!-- test:{name} -->\n```{language}\n");
    let mut found = DOCUMENTS
        .iter()
        .filter_map(|(file, text)| text.split_once(&marker).map(|(_, rest)| (*file, rest)));
    let (file, rest) = found
        .next()
        .unwrap_or_else(|| panic!("missing documented snippet {name}"));
    assert!(
        found.next().is_none() && rest.split_once(&marker).is_none(),
        "documented snippet {name} appears more than once"
    );
    rest.split_once("\n```")
        .unwrap_or_else(|| panic!("unterminated snippet {name} in {file}"))
        .0
        .to_owned()
}
