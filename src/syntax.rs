use std::path::Path;

use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::buffer::TextBuffer;

pub fn resolve_syntax<'a>(syntax_set: &'a SyntaxSet, buffer: &TextBuffer) -> &'a SyntaxReference {
    if let Some(path) = buffer.path.as_deref() {
        if let Ok(Some(syntax)) = syntax_set.find_syntax_for_file(path) {
            return syntax;
        }

        if let Some(syntax) = resolve_by_filename(syntax_set, path) {
            return syntax;
        }

        if let Some(syntax) = resolve_by_extension_alias(syntax_set, path) {
            return syntax;
        }
    }

    let first_line = buffer.line_text(0);
    if let Some(syntax) = resolve_by_shebang(syntax_set, &first_line) {
        return syntax;
    }
    if let Some(syntax) = syntax_set.find_syntax_by_first_line(&first_line) {
        return syntax;
    }

    syntax_set.find_syntax_plain_text()
}

fn resolve_by_filename<'a>(syntax_set: &'a SyntaxSet, path: &Path) -> Option<&'a SyntaxReference> {
    let file_name = path.file_name()?.to_string_lossy().to_ascii_lowercase();

    if file_name.starts_with(".env") {
        return find_by_extension(syntax_set, &["sh", "bash"]);
    }

    if file_name.starts_with("dockerfile") || file_name == "containerfile" {
        return find_by_name_or_extension(syntax_set, &["Dockerfile"], &["docker", "dockerfile"]);
    }

    match file_name.as_str() {
        "makefile" | "gnumakefile" | "bsdmakefile" | "justfile" => {
            find_by_name_or_extension(syntax_set, &["Makefile"], &["make"])
        }
        "cmakelists.txt" => find_by_name_or_extension(syntax_set, &["CMake"], &["cmake"]),
        "jenkinsfile" => find_by_extension(syntax_set, &["groovy"]),
        "gemfile" | "rakefile" | "guardfile" | "podfile" | "brewfile" | "fastfile" => {
            find_by_extension(syntax_set, &["rb", "ruby"])
        }
        ".bashrc" | ".bash_profile" | ".bash_logout" | ".profile" | ".zshrc" | ".zprofile"
        | ".zlogin" | ".zlogout" | ".envrc" => find_by_extension(syntax_set, &["sh", "bash"]),
        ".editorconfig" | ".npmrc" | ".yarnrc" | ".tool-versions" => {
            find_by_extension(syntax_set, &["ini"])
        }
        _ => None,
    }
}

fn resolve_by_extension_alias<'a>(
    syntax_set: &'a SyntaxSet,
    path: &Path,
) -> Option<&'a SyntaxReference> {
    let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let alias = match extension.as_str() {
        "jsonc" | "json5" | "webmanifest" | "har" | "geojson" => "json",
        "code-workspace" => "json",
        "mdx" | "markdown" => "md",
        "mjs" | "cjs" => "js",
        "mts" | "cts" => "ts",
        "astro" | "svelte" | "vue" | "hbs" | "handlebars" | "mustache" => "html",
        "csproj" | "fsproj" | "vbproj" | "props" | "targets" | "xaml" | "svg" | "plist" => "xml",
        "conf" | "cfg" => "ini",
        "kts" => "kt",
        "csx" | "cake" => "cs",
        "gradle" => "groovy",
        "psm1" | "psd1" => "ps1",
        "zsh" | "bash" | "fish" => "sh",
        "pyi" => "py",
        _ => return None,
    };

    find_by_extension(syntax_set, &[alias])
}

fn resolve_by_shebang<'a>(
    syntax_set: &'a SyntaxSet,
    first_line: &str,
) -> Option<&'a SyntaxReference> {
    if !first_line.starts_with("#!") {
        return None;
    }

    let line = first_line.to_ascii_lowercase();
    if line.contains("python") {
        return find_by_extension(syntax_set, &["py"]);
    }
    if line.contains("pwsh") || line.contains("powershell") {
        return find_by_extension(syntax_set, &["ps1"]);
    }
    if line.contains("node")
        || line.contains("deno")
        || line.contains("bun")
        || line.contains("tsx")
    {
        return find_by_extension(syntax_set, &["js"]);
    }
    if line.contains("ruby") {
        return find_by_extension(syntax_set, &["rb"]);
    }
    if line.contains("perl") {
        return find_by_extension(syntax_set, &["pl"]);
    }
    if line.contains("php") {
        return find_by_extension(syntax_set, &["php"]);
    }
    if line.contains("bash") || line.contains("sh") || line.contains("zsh") || line.contains("fish")
    {
        return find_by_extension(syntax_set, &["sh", "bash"]);
    }

    None
}

fn find_by_name_or_extension<'a>(
    syntax_set: &'a SyntaxSet,
    names: &[&str],
    extensions: &[&str],
) -> Option<&'a SyntaxReference> {
    for name in names {
        if let Some(syntax) = syntax_set.find_syntax_by_name(name) {
            return Some(syntax);
        }
    }
    find_by_extension(syntax_set, extensions)
}

fn find_by_extension<'a>(
    syntax_set: &'a SyntaxSet,
    extensions: &[&str],
) -> Option<&'a SyntaxReference> {
    for extension in extensions {
        if let Some(syntax) = syntax_set.find_syntax_by_extension(extension) {
            return Some(syntax);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use syntect::parsing::SyntaxSet;

    use crate::buffer::TextBuffer;

    use super::resolve_syntax;

    #[test]
    fn maps_code_workspace_to_json() {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let buffer = TextBuffer::with_text(
            Some(PathBuf::from("project.code-workspace")),
            "{ \"folders\": [] }",
        );

        let syntax = resolve_syntax(&syntax_set, &buffer);
        let expected = syntax_set.find_syntax_by_extension("json").unwrap();

        assert_eq!(syntax.name, expected.name);
    }

    #[test]
    fn maps_csproj_to_xml() {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let buffer = TextBuffer::with_text(
            Some(PathBuf::from("app.csproj")),
            "<Project Sdk=\"Microsoft.NET.Sdk\" />",
        );

        let syntax = resolve_syntax(&syntax_set, &buffer);
        let expected = syntax_set.find_syntax_by_extension("xml").unwrap();

        assert_eq!(syntax.name, expected.name);
    }

    #[test]
    fn uses_shebang_for_extensionless_scripts() {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let buffer = TextBuffer::with_text(None, "#!/usr/bin/env python\nprint('hi')\n");

        let syntax = resolve_syntax(&syntax_set, &buffer);
        let expected = syntax_set.find_syntax_by_extension("py").unwrap();

        assert_eq!(syntax.name, expected.name);
    }
}
