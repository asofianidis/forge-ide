//! Built-in catalog of well-known language servers and how to install them.
//!
//! Each entry records which files a server is useful for, the executable it
//! provides (so we can detect whether it is already installed), and one or more
//! install commands. Install commands go through the language's own toolchain
//! (rustup, npm, go, pipx, …) so servers land in the usual place and get
//! updated the usual way.

/// Operating system an install method applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Any,
    Windows,
    Mac,
}

impl Os {
    pub fn matches_current(self) -> bool {
        match self {
            Os::Any => true,
            Os::Windows => cfg!(windows),
            Os::Mac => cfg!(target_os = "macos"),
        }
    }
}

/// A command that installs a server, e.g. `npm install -g pyright`.
#[derive(Debug)]
pub struct InstallMethod {
    pub os: Os,
    /// Program to run (looked up on `PATH`).
    pub program: &'static str,
    pub args: &'static [&'static str],
}

impl InstallMethod {
    /// Human-readable command line, suitable for display or copying into a terminal.
    pub fn command_line(&self) -> String {
        std::iter::once(self.program)
            .chain(self.args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

const fn any(program: &'static str, args: &'static [&'static str]) -> InstallMethod {
    InstallMethod {
        os: Os::Any,
        program,
        args,
    }
}

const fn on(os: Os, program: &'static str, args: &'static [&'static str]) -> InstallMethod {
    InstallMethod { os, program, args }
}

#[derive(Debug)]
pub struct ServerSpec {
    /// Stable identifier, also used as the settings key.
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub languages: &'static [&'static str],
    /// Lowercase file extensions without the leading dot.
    pub extensions: &'static [&'static str],
    /// Exact file names the server applies to (e.g. `Dockerfile`).
    pub filenames: &'static [&'static str],
    /// Executable the server provides.
    pub command: &'static str,
    /// Arguments to start the server speaking LSP over stdio.
    pub launch_args: &'static [&'static str],
    /// If set, the executable must exit successfully with these arguments to
    /// count as installed. Needed for rustup proxies, which exist on `PATH`
    /// even when the component itself is missing.
    pub verify_args: Option<&'static [&'static str]>,
    pub homepage: &'static str,
    /// Install commands in order of preference. Servers without one for the
    /// current OS link to their homepage for manual installation.
    pub install: &'static [InstallMethod],
}

impl ServerSpec {
    /// Whether this server handles a file with the given name.
    pub fn handles_file(&self, file_name: &str) -> bool {
        if self.filenames.contains(&file_name) {
            return true;
        }
        file_name
            .rsplit_once('.')
            .is_some_and(|(_, ext)| self.handles_extension(ext))
    }

    pub fn handles_extension(&self, ext: &str) -> bool {
        self.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
    }

    /// Install methods applicable to the current OS, in order of preference.
    pub fn install_methods(&self) -> impl Iterator<Item = &InstallMethod> {
        self.install.iter().filter(|m| m.os.matches_current())
    }

    /// Case-insensitive match against name, id, languages and description.
    pub fn matches_query(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let contains = |s: &str| s.to_lowercase().contains(&query);
        contains(self.name)
            || contains(self.id)
            || contains(self.description)
            || self.languages.iter().any(|l| contains(l))
            || self.extensions.iter().any(|e| *e == query)
    }
}

/// All servers known to Forge, sorted by name.
pub fn servers() -> &'static [ServerSpec] {
    SERVERS
}

#[cfg(test)]
pub fn find(id: &str) -> Option<&'static ServerSpec> {
    SERVERS.iter().find(|s| s.id == id)
}

/// Servers that handle a file with the given name.
pub fn servers_for_file(file_name: &str) -> impl Iterator<Item = &'static ServerSpec> {
    SERVERS.iter().filter(move |s| s.handles_file(file_name))
}

const VSCODE_EXTRACTED: &[InstallMethod] = &[any(
    "npm",
    &["install", "-g", "vscode-langservers-extracted"],
)];

static SERVERS: &[ServerSpec] = &[
    ServerSpec {
        id: "bash-language-server",
        name: "Bash Language Server",
        description: "Completion, hover and diagnostics (via ShellCheck) for shell scripts.",
        languages: &["Bash", "Shell"],
        extensions: &["sh", "bash"],
        filenames: &[],
        command: "bash-language-server",
        launch_args: &["start"],
        verify_args: None,
        homepage: "https://github.com/bash-lsp/bash-language-server",
        install: &[any("npm", &["install", "-g", "bash-language-server"])],
    },
    ServerSpec {
        id: "clangd",
        name: "clangd",
        description: "The LLVM project's language server for C, C++ and Objective-C.",
        languages: &["C", "C++", "Objective-C"],
        extensions: &["c", "h", "cc", "cpp", "cxx", "hh", "hpp", "hxx", "m", "mm"],
        filenames: &[],
        command: "clangd",
        launch_args: &[],
        verify_args: None,
        homepage: "https://clangd.llvm.org/installation",
        install: &[
            on(
                Os::Windows,
                "winget",
                &["install", "--id", "LLVM.LLVM", "-e"],
            ),
            on(Os::Mac, "brew", &["install", "llvm"]),
        ],
    },
    ServerSpec {
        id: "csharp-ls",
        name: "csharp-ls",
        description: "Roslyn-based language server for C#.",
        languages: &["C#"],
        extensions: &["cs", "csx"],
        filenames: &[],
        command: "csharp-ls",
        launch_args: &[],
        verify_args: None,
        homepage: "https://github.com/razzmatazz/csharp-language-server",
        install: &[any("dotnet", &["tool", "install", "--global", "csharp-ls"])],
    },
    ServerSpec {
        id: "vscode-css-language-server",
        name: "CSS Language Server",
        description: "CSS, SCSS and Less support extracted from VS Code.",
        languages: &["CSS", "SCSS", "Less"],
        extensions: &["css", "scss", "less"],
        filenames: &[],
        command: "vscode-css-language-server",
        launch_args: &["--stdio"],
        verify_args: None,
        homepage: "https://github.com/hrsh7th/vscode-langservers-extracted",
        install: VSCODE_EXTRACTED,
    },
    ServerSpec {
        id: "dockerfile-language-server",
        name: "Dockerfile Language Server",
        description: "Completion, hover and linting for Dockerfiles.",
        languages: &["Dockerfile"],
        extensions: &["dockerfile"],
        filenames: &["Dockerfile", "Containerfile"],
        command: "docker-langserver",
        launch_args: &["--stdio"],
        verify_args: None,
        homepage: "https://github.com/rcjsuen/dockerfile-language-server",
        install: &[any(
            "npm",
            &["install", "-g", "dockerfile-language-server-nodejs"],
        )],
    },
    ServerSpec {
        id: "gopls",
        name: "gopls",
        description: "The official Go language server, maintained by the Go team.",
        languages: &["Go"],
        extensions: &["go"],
        filenames: &["go.mod", "go.work"],
        command: "gopls",
        launch_args: &[],
        verify_args: None,
        homepage: "https://pkg.go.dev/golang.org/x/tools/gopls",
        install: &[any("go", &["install", "golang.org/x/tools/gopls@latest"])],
    },
    ServerSpec {
        id: "haskell-language-server",
        name: "Haskell Language Server",
        description: "Official Haskell language server, installed through GHCup.",
        languages: &["Haskell"],
        extensions: &["hs", "lhs"],
        filenames: &[],
        command: "haskell-language-server-wrapper",
        launch_args: &["--lsp"],
        verify_args: None,
        homepage: "https://haskell-language-server.readthedocs.io",
        install: &[any("ghcup", &["install", "hls"])],
    },
    ServerSpec {
        id: "vscode-html-language-server",
        name: "HTML Language Server",
        description: "HTML support extracted from VS Code.",
        languages: &["HTML"],
        extensions: &["html", "htm"],
        filenames: &[],
        command: "vscode-html-language-server",
        launch_args: &["--stdio"],
        verify_args: None,
        homepage: "https://github.com/hrsh7th/vscode-langservers-extracted",
        install: VSCODE_EXTRACTED,
    },
    ServerSpec {
        id: "intelephense",
        name: "Intelephense",
        description: "Fast PHP language server (core features are free).",
        languages: &["PHP"],
        extensions: &["php"],
        filenames: &[],
        command: "intelephense",
        launch_args: &["--stdio"],
        verify_args: None,
        homepage: "https://intelephense.com",
        install: &[any("npm", &["install", "-g", "intelephense"])],
    },
    ServerSpec {
        id: "jdtls",
        name: "jdtls",
        description: "Eclipse JDT Language Server: full-featured Java support.",
        languages: &["Java"],
        extensions: &["java"],
        filenames: &[],
        command: "jdtls",
        launch_args: &[],
        verify_args: None,
        homepage: "https://github.com/eclipse-jdtls/eclipse.jdt.ls",
        install: &[on(Os::Mac, "brew", &["install", "jdtls"])],
    },
    ServerSpec {
        id: "vscode-json-language-server",
        name: "JSON Language Server",
        description: "JSON with schema validation, extracted from VS Code.",
        languages: &["JSON"],
        extensions: &["json", "jsonc"],
        filenames: &[],
        command: "vscode-json-language-server",
        launch_args: &["--stdio"],
        verify_args: None,
        homepage: "https://github.com/hrsh7th/vscode-langservers-extracted",
        install: VSCODE_EXTRACTED,
    },
    ServerSpec {
        id: "lua-language-server",
        name: "Lua Language Server",
        description: "Feature-rich Lua language server (LuaLS).",
        languages: &["Lua"],
        extensions: &["lua"],
        filenames: &[],
        command: "lua-language-server",
        launch_args: &[],
        verify_args: None,
        homepage: "https://luals.github.io/#install",
        install: &[on(Os::Mac, "brew", &["install", "lua-language-server"])],
    },
    ServerSpec {
        id: "marksman",
        name: "Marksman",
        description: "Markdown language server with links, references and TOC support.",
        languages: &["Markdown"],
        extensions: &["md", "markdown"],
        filenames: &[],
        command: "marksman",
        launch_args: &["server"],
        verify_args: None,
        homepage: "https://github.com/artempyanykh/marksman",
        install: &[on(Os::Mac, "brew", &["install", "marksman"])],
    },
    ServerSpec {
        id: "ocamllsp",
        name: "OCaml LSP",
        description: "Official OCaml language server, installed through opam.",
        languages: &["OCaml"],
        extensions: &["ml", "mli"],
        filenames: &[],
        command: "ocamllsp",
        launch_args: &[],
        verify_args: None,
        homepage: "https://github.com/ocaml/ocaml-lsp",
        install: &[any("opam", &["install", "-y", "ocaml-lsp-server"])],
    },
    ServerSpec {
        id: "pyright",
        name: "Pyright",
        description: "Fast static type checker and language server for Python.",
        languages: &["Python"],
        extensions: &["py", "pyi"],
        filenames: &[],
        command: "pyright-langserver",
        launch_args: &["--stdio"],
        verify_args: None,
        homepage: "https://github.com/microsoft/pyright",
        install: &[
            any("npm", &["install", "-g", "pyright"]),
            any("pipx", &["install", "pyright"]),
        ],
    },
    ServerSpec {
        id: "pylsp",
        name: "Python LSP Server",
        description: "Community Python language server with a plugin ecosystem.",
        languages: &["Python"],
        extensions: &["py", "pyi"],
        filenames: &[],
        command: "pylsp",
        launch_args: &[],
        verify_args: None,
        homepage: "https://github.com/python-lsp/python-lsp-server",
        install: &[any("pipx", &["install", "python-lsp-server"])],
    },
    ServerSpec {
        id: "ruby-lsp",
        name: "Ruby LSP",
        description: "Shopify's language server for Ruby.",
        languages: &["Ruby"],
        extensions: &["rb", "rake", "gemspec"],
        filenames: &["Gemfile", "Rakefile"],
        command: "ruby-lsp",
        launch_args: &[],
        verify_args: None,
        homepage: "https://shopify.github.io/ruby-lsp",
        install: &[any("gem", &["install", "ruby-lsp"])],
    },
    ServerSpec {
        id: "ruff",
        name: "Ruff",
        description: "Extremely fast Python linter and formatter with a built-in language server.",
        languages: &["Python"],
        extensions: &["py", "pyi"],
        filenames: &[],
        command: "ruff",
        launch_args: &["server"],
        verify_args: None,
        homepage: "https://docs.astral.sh/ruff/editors/",
        install: &[any("pipx", &["install", "ruff"])],
    },
    ServerSpec {
        id: "rust-analyzer",
        name: "rust-analyzer",
        description: "The official Rust language server: completion, inlay hints, refactors and more.",
        languages: &["Rust"],
        extensions: &["rs"],
        filenames: &[],
        command: "rust-analyzer",
        launch_args: &[],
        verify_args: Some(&["--version"]),
        homepage: "https://rust-analyzer.github.io",
        install: &[any("rustup", &["component", "add", "rust-analyzer"])],
    },
    ServerSpec {
        id: "svelte-language-server",
        name: "Svelte Language Server",
        description: "Language server for Svelte components.",
        languages: &["Svelte"],
        extensions: &["svelte"],
        filenames: &[],
        command: "svelteserver",
        launch_args: &["--stdio"],
        verify_args: None,
        homepage: "https://github.com/sveltejs/language-tools",
        install: &[any("npm", &["install", "-g", "svelte-language-server"])],
    },
    ServerSpec {
        id: "taplo",
        name: "Taplo",
        description: "TOML toolkit with formatting, validation and schema-aware completion.",
        languages: &["TOML"],
        extensions: &["toml"],
        filenames: &[],
        command: "taplo",
        launch_args: &["lsp", "stdio"],
        verify_args: None,
        homepage: "https://taplo.tamasfe.dev",
        install: &[any(
            "cargo",
            &["install", "taplo-cli", "--locked", "--features", "lsp"],
        )],
    },
    ServerSpec {
        id: "terraform-ls",
        name: "Terraform Language Server",
        description: "HashiCorp's official language server for Terraform.",
        languages: &["Terraform"],
        extensions: &["tf", "tfvars"],
        filenames: &[],
        command: "terraform-ls",
        launch_args: &["serve"],
        verify_args: None,
        homepage: "https://github.com/hashicorp/terraform-ls",
        install: &[on(
            Os::Mac,
            "brew",
            &["install", "hashicorp/tap/terraform-ls"],
        )],
    },
    ServerSpec {
        id: "typescript-language-server",
        name: "TypeScript Language Server",
        description: "TypeScript and JavaScript support backed by tsserver.",
        languages: &["TypeScript", "JavaScript"],
        extensions: &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"],
        filenames: &[],
        command: "typescript-language-server",
        launch_args: &["--stdio"],
        verify_args: None,
        homepage: "https://github.com/typescript-language-server/typescript-language-server",
        install: &[any(
            "npm",
            &["install", "-g", "typescript-language-server", "typescript"],
        )],
    },
    ServerSpec {
        id: "vue-language-server",
        name: "Vue Language Server",
        description: "Official language server for Vue single-file components.",
        languages: &["Vue"],
        extensions: &["vue"],
        filenames: &[],
        command: "vue-language-server",
        launch_args: &["--stdio"],
        verify_args: None,
        homepage: "https://github.com/vuejs/language-tools",
        install: &[any("npm", &["install", "-g", "@vue/language-server"])],
    },
    ServerSpec {
        id: "yaml-language-server",
        name: "YAML Language Server",
        description: "YAML validation and completion with JSON Schema support (Kubernetes, CI configs, …).",
        languages: &["YAML"],
        extensions: &["yaml", "yml"],
        filenames: &[],
        command: "yaml-language-server",
        launch_args: &["--stdio"],
        verify_args: None,
        homepage: "https://github.com/redhat-developer/yaml-language-server",
        install: &[any("npm", &["install", "-g", "yaml-language-server"])],
    },
    ServerSpec {
        id: "zls",
        name: "ZLS",
        description: "The Zig language server.",
        languages: &["Zig"],
        extensions: &["zig", "zon"],
        filenames: &[],
        command: "zls",
        launch_args: &[],
        verify_args: None,
        homepage: "https://github.com/zigtools/zls",
        install: &[on(Os::Mac, "brew", &["install", "zls"])],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique() {
        let mut seen = HashSet::new();
        for s in servers() {
            assert!(seen.insert(s.id), "duplicate id {}", s.id);
        }
    }

    #[test]
    fn entries_are_well_formed() {
        for s in servers() {
            assert!(!s.name.is_empty() && !s.command.is_empty(), "{}", s.id);
            assert!(!s.languages.is_empty(), "{} has no languages", s.id);
            assert!(s.homepage.starts_with("https://"), "{}", s.id);
            for ext in s.extensions {
                assert!(
                    !ext.starts_with('.') && *ext == ext.to_lowercase(),
                    "{}: bad extension {ext:?}",
                    s.id
                );
            }
        }
    }

    #[test]
    fn sorted_by_name() {
        let names: Vec<String> = servers().iter().map(|s| s.name.to_lowercase()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn matches_files() {
        let ids = |f: &str| servers_for_file(f).map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(ids("main.rs"), ["rust-analyzer"]);
        assert_eq!(ids("Dockerfile"), ["dockerfile-language-server"]);
        assert!(ids("app.TSX").contains(&"typescript-language-server"));
        assert!(ids("script.py").contains(&"pyright"));
        assert!(ids("README").is_empty());
    }

    #[test]
    fn query_matching() {
        let rust = find("rust-analyzer").unwrap();
        assert!(rust.matches_query(""));
        assert!(rust.matches_query("RUST"));
        assert!(rust.matches_query("rs"));
        assert!(!rust.matches_query("python"));
    }

    #[test]
    fn command_line_formatting() {
        let m = &find("gopls").unwrap().install[0];
        assert_eq!(
            m.command_line(),
            "go install golang.org/x/tools/gopls@latest"
        );
    }
}
