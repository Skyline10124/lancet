// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `fs.search` API 工具组件。

wit_bindgen::generate!({
    world: "fs-search",
    path: "wit",
    generate_all,
});

use exports::lancet::fs_search::fs::{Guest, SearchHit};
use globset::{GlobBuilder, GlobMatcher};
use lancet::host::handles::ReadDir;
use regex::Regex;

struct FsSearch;

impl Guest for FsSearch {
    fn search(
        dir: &ReadDir,
        pattern: String,
        glob: Option<String>,
    ) -> Result<Vec<SearchHit>, String> {
        let re =
            Regex::new(&pattern).map_err(|err| format!("正则表达式 `{pattern}` 无效：{err}"))?;
        let glob_matcher = match glob.as_deref() {
            Some(pat) => Some(
                GlobBuilder::new(pat)
                    .literal_separator(true)
                    .build()
                    .map_err(|err| format!("glob 模式 `{pat}` 无效：{err}"))?
                    .compile_matcher(),
            ),
            None => None,
        };

        let mut hits = Vec::new();
        search_dir(dir, "", &re, glob_matcher.as_ref(), &mut hits)?;
        hits.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.line_no.cmp(&b.line_no)));
        Ok(hits)
    }
}

fn search_dir(
    dir: &ReadDir,
    rel_dir: &str,
    re: &Regex,
    glob_matcher: Option<&GlobMatcher>,
    hits: &mut Vec<SearchHit>,
) -> Result<(), String> {
    let query = if rel_dir.is_empty() { "." } else { rel_dir };
    let Some(entries) = dir.entries(query)? else {
        return Ok(());
    };

    for entry in entries {
        let rel_path = if rel_dir.is_empty() {
            entry.name
        } else {
            format!("{rel_dir}/{}", entry.name)
        };

        if entry.is_dir {
            if !entry.is_link {
                search_dir(dir, &rel_path, re, glob_matcher, hits)?;
            }
            continue;
        }

        if let Some(matcher) = glob_matcher
            && !matcher.is_match(&rel_path)
        {
            continue;
        }

        let Some(content) = dir.read(&rel_path)? else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            if re.is_match(line) {
                hits.push(SearchHit {
                    path: rel_path.clone(),
                    line_no: (idx + 1) as u32,
                    text: line.to_owned(),
                });
            }
        }
    }

    Ok(())
}

export!(FsSearch);
