//! HTML to Markdown (for the MCP server's answers and NotebookLM sources), like markdownify with
//! ATX headings.

pub fn md(html: &str) -> String {
    if html.trim().is_empty() {
        return String::new();
    }
    let converter = htmd::HtmlToMarkdown::builder()
        .options(htmd::options::Options { heading_style: htmd::options::HeadingStyle::Atx, ..Default::default() })
        .skip_tags(vec!["script", "style"])
        .build();
    converter.convert(html).unwrap_or_default().trim().to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn converts() {
        let out = super::md("<h2>Welcome</h2><p>Hi <strong>there</strong> <a href=\"https://x.edu\">link</a></p><ul><li>a</li></ul>");
        assert!(out.starts_with("## Welcome"), "{out}");
        assert!(out.contains("**there**"));
        assert!(out.contains("[link](https://x.edu)"));
    }
}
