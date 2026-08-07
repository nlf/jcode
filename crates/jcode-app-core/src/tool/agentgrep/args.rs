use super::*;

struct ResolvedSearchScope {
    root: Option<String>,
    glob: Option<String>,
}

/// How a single-file scope should be expressed to the search engine.
///
/// The two modes need opposite things from a file scope, because they consume
/// the *relative* path differently.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FileScopeStyle {
    /// Root at the file. The walker yields exactly that entry and nothing else
    /// is descended. The relative path collapses to the empty string, which
    /// `grep` does not mind because it matches on file contents.
    Content,
    /// Root at the parent, with the file name as a glob. `find` ranks files by
    /// their path text, so an empty relative path carries no evidence and the
    /// file is dropped before it can be returned. The parent root keeps the
    /// name, and the glob keeps the walk's results to that one file.
    Name,
}

fn resolved_search_scope(
    ctx: &ToolContext,
    path: Option<&str>,
    file: Option<&str>,
    glob: Option<&str>,
    file_scope: FileScopeStyle,
) -> ResolvedSearchScope {
    // `file` scopes grep/find to one exact file when `path` is absent.
    let path = path.or(file);
    let Some(path) = path else {
        return ResolvedSearchScope {
            root: None,
            glob: normalized_agentgrep_glob_owned(glob),
        };
    };

    let resolved = resolve_path_arg(ctx, path);
    if resolved.is_file() {
        return match file_scope {
            // Scope to the file itself. Rooting at the parent and passing the
            // file name as a glob reads like scoping but is not: a glob filters
            // which files are reported, while the walk still descends the whole
            // parent tree. Pointing grep at `~/NOTES.md` therefore crawled all
            // of `$HOME`, hit macOS TCC-protected directories, and failed with
            // ripgrep exit 2 before the filter could ever apply.
            //
            // A file root also keeps the search off the ripgrep path, which
            // cannot express it: the engine spawns `rg` with
            // `current_dir(root)`, and a file is not a directory. The walker
            // handles a file root directly and yields exactly that one entry.
            FileScopeStyle::Content => ResolvedSearchScope {
                root: Some(resolved.display().to_string()),
                glob: None,
            },
            FileScopeStyle::Name => ResolvedSearchScope {
                root: Some(
                    resolved
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .display()
                        .to_string(),
                ),
                glob: resolved
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            },
        };
    }

    ResolvedSearchScope {
        root: Some(resolved.display().to_string()),
        glob: normalized_agentgrep_glob_owned(glob),
    }
}

pub(super) fn build_grep_args(params: &AgentGrepInput, ctx: &ToolContext) -> Result<GrepArgs> {
    let query = params
        .query
        .clone()
        .ok_or_else(|| anyhow::anyhow!("agentgrep grep requires 'query'"))?;
    let scope = resolved_search_scope(
        ctx,
        params.path.as_deref(),
        params.file.as_deref(),
        params.glob.as_deref(),
        FileScopeStyle::Content,
    );
    Ok(GrepArgs {
        query,
        regex: params.regex.unwrap_or(false),
        file_type: params.file_type.clone(),
        json: false,
        paths_only: params.paths_only.unwrap_or(false),
        hidden: params.hidden.unwrap_or(false),
        no_ignore: params.no_ignore.unwrap_or(false),
        path: scope.root,
        glob: scope.glob,
    })
}

pub(super) fn build_find_args(params: &AgentGrepInput, ctx: &ToolContext) -> Result<FindArgs> {
    let query = params.query.as_deref().unwrap_or_default();
    if query.trim().is_empty()
        && params.path.as_deref().is_none_or(str::is_empty)
        && params.file.as_deref().is_none_or(str::is_empty)
        && normalized_agentgrep_glob(params.glob.as_deref()).is_none()
        && params.file_type.as_deref().is_none_or(str::is_empty)
    {
        return Err(anyhow::anyhow!(
            "agentgrep find requires 'query' unless path, glob, or type narrows the search"
        ));
    }
    let scope = resolved_search_scope(
        ctx,
        params.path.as_deref(),
        params.file.as_deref(),
        params.glob.as_deref(),
        FileScopeStyle::Name,
    );
    Ok(FindArgs {
        query_parts: query.split_whitespace().map(ToOwned::to_owned).collect(),
        file_type: params.file_type.clone(),
        json: false,
        paths_only: params.paths_only.unwrap_or(false),
        debug_score: params.debug_score.unwrap_or(false),
        max_files: params.max_files.unwrap_or(10),
        hidden: params.hidden.unwrap_or(false),
        no_ignore: params.no_ignore.unwrap_or(false),
        path: scope.root,
        glob: scope.glob,
    })
}

pub(super) fn build_outline_args(
    params: &AgentGrepInput,
    ctx: &ToolContext,
    context_json_path: Option<&Path>,
) -> Result<OutlineArgs> {
    // Agents sometimes point `path` at the file itself, either instead of or
    // in addition to `file`. Treat a file-valued `path` as the outline target
    // so the file argument is not joined onto it (for example,
    // ".../todo.rs/.../todo.rs").
    if let Some(path) = params.path.as_deref() {
        let resolved = resolve_path_arg(ctx, path);
        if resolved.is_file() {
            return Ok(OutlineArgs {
                file: resolved.display().to_string(),
                json: false,
                max_items: None,
                path: None,
                context_json: context_json_path.map(|path| path.display().to_string()),
            });
        }
    }

    let file = outline_file_arg(params)?;
    Ok(OutlineArgs {
        file,
        json: false,
        max_items: None,
        path: resolved_root_string(ctx, params.path.as_deref()),
        context_json: context_json_path.map(|path| path.display().to_string()),
    })
}

pub(super) fn build_smart_args_and_query(
    params: &AgentGrepInput,
    ctx: &ToolContext,
    context_json_path: Option<&Path>,
) -> Result<(SmartArgs, SmartQuery)> {
    let terms = trace_or_smart_terms_owned(params)?;
    let query = parse_smart_query(&terms).map_err(|err| {
        anyhow::anyhow!(
            "{}\n\ntrace queries use a small DSL. Example:\n  agentgrep trace subject:auth_status relation:rendered support:ui",
            err
        )
    })?;
    let scope = resolved_search_scope(
        ctx,
        params.path.as_deref(),
        params.file.as_deref(),
        params.glob.as_deref(),
        FileScopeStyle::Name,
    );

    let args = SmartArgs {
        terms,
        json: false,
        max_files: params.max_files.unwrap_or(5),
        max_regions: params.max_regions.unwrap_or(6),
        full_region: parse_full_region_mode(params.full_region.as_deref())?,
        debug_plan: params.debug_plan.unwrap_or(false),
        debug_score: params.debug_score.unwrap_or(false),
        paths_only: params.paths_only.unwrap_or(false),
        path: scope.root,
        file_type: params.file_type.clone(),
        glob: scope.glob,
        hidden: params.hidden.unwrap_or(false),
        no_ignore: params.no_ignore.unwrap_or(false),
        context_json: context_json_path.map(|path| path.display().to_string()),
    };

    Ok((args, query))
}

pub(super) fn trace_or_smart_terms_owned(params: &AgentGrepInput) -> Result<Vec<String>> {
    if let Some(terms) = params.terms.as_ref().filter(|terms| !terms.is_empty()) {
        return Ok(terms.clone());
    }

    if params.mode == "smart"
        && let Some(query) = params.query.as_deref()
    {
        let split_terms: Vec<String> = query
            .split_whitespace()
            .filter(|term| !term.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        if !split_terms.is_empty() {
            return Ok(split_terms);
        }
    }

    let field_hint = if params.mode == "smart" {
        "non-empty 'terms' or 'query'"
    } else {
        "non-empty 'terms'"
    };

    Err(anyhow::anyhow!(
        "agentgrep {} requires {}",
        params.mode,
        field_hint
    ))
}

fn outline_file_arg(params: &AgentGrepInput) -> Result<String> {
    params
        .file
        .clone()
        .or_else(|| params.query.clone())
        .or_else(|| {
            params
                .terms
                .as_ref()
                .and_then(|terms| terms.first().cloned())
        })
        .ok_or_else(|| {
            anyhow::anyhow!("agentgrep outline requires 'file' (or legacy 'query' / first term)")
        })
}

fn parse_full_region_mode(value: Option<&str>) -> Result<FullRegionMode> {
    match value.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(FullRegionMode::Auto),
        "always" => Ok(FullRegionMode::Always),
        "never" => Ok(FullRegionMode::Never),
        other => Err(anyhow::anyhow!(
            "agentgrep trace full_region must be one of: auto, always, never; got {other}"
        )),
    }
}

fn resolved_root_string(ctx: &ToolContext, path: Option<&str>) -> Option<String> {
    path.map(|path| resolve_path_arg(ctx, path).display().to_string())
}

pub(super) fn resolve_search_root(ctx: &ToolContext, path: Option<&str>) -> Result<PathBuf> {
    path.map(PathBuf::from)
        .or_else(|| ctx.working_dir.clone())
        .ok_or_else(|| anyhow::anyhow!("agentgrep requires a session working directory"))
}

pub(super) fn summarize_agentgrep_request(
    params: &AgentGrepInput,
    ctx: &ToolContext,
    context_json_path: Option<&Path>,
) -> String {
    let mut parts = vec![format!("mode={}", params.mode)];
    if let Some(query) = params.query.as_deref() {
        parts.push(format!("query={}", util::truncate_str(query, 80)));
    }
    if let Some(file) = params.file.as_deref() {
        parts.push(format!("file={file}"));
    }
    if let Some(terms) = params.terms.as_ref() {
        parts.push(format!(
            "terms={}",
            util::truncate_str(&terms.join(" "), 80)
        ));
    }
    if let Some(path) = resolved_root_string(ctx, params.path.as_deref()) {
        parts.push(format!("root={path}"));
    }
    if let Some(glob) = normalized_agentgrep_glob(params.glob.as_deref()) {
        parts.push(format!("glob={glob}"));
    }
    if let Some(file_type) = params.file_type.as_deref() {
        parts.push(format!("type={file_type}"));
    }
    if params.paths_only.unwrap_or(false) {
        parts.push("paths_only=true".to_string());
    }
    if context_json_path.is_some() {
        parts.push("context_json=true".to_string());
    }
    parts.join(" ")
}
