//! The tool schema advertised to the model.

/// The tool schema advertised to the model.
pub fn tools_spec() -> serde_json::Value {
    serde_json::json!([
        tool("bash", "Run a shell command in the working directory and return stdout, stderr and exit code. Use for git, builds, tests, searching, installing, etc. Set background=true for long-running commands (servers, builds): it returns a job id immediately; poll with bash_output.", serde_json::json!({
            "type":"object",
            "properties":{
                "command":{"type":"string","description":"Shell command to run."},
                "timeout":{"type":"integer","description":"Seconds (default 120). Ignored for background jobs."},
                "background":{"type":"boolean","description":"Run detached; returns a job id."}
            },
            "required":["command"]
        })),
        tool("bash_output","Read output a background bash job has produced since the last bash_output call, plus its run status.", serde_json::json!({
            "type":"object",
            "properties":{"id":{"type":"integer","description":"Job id from bash."}},
            "required":["id"]
        })),
        tool("bash_kill","Kill a background bash job (its whole process group).", serde_json::json!({
            "type":"object",
            "properties":{"id":{"type":"integer","description":"Job id from bash."}},
            "required":["id"]
        })),
        tool("read_file","Read a UTF-8 text file, optionally a 1-based inclusive line range. Each line comes back as `<line number><TAB><content>`: everything after the tab is the file's exact text, indentation included. Never copy the number or the tab into edit_file's old_text.", serde_json::json!({
            "type":"object",
            "properties":{
                "path":{"type":"string"},
                "start_line":{"type":"integer"},
                "end_line":{"type":"integer"}
            },
            "required":["path"]
        })),
        tool("write_file","Create or overwrite a file with the given content. Creates parent dirs.", serde_json::json!({
            "type":"object",
            "properties":{"path":{"type":"string"},"content":{"type":"string"}},
            "required":["path","content"]
        })),
        tool("edit_file","Replace an exact substring in a file. old_text must appear exactly once; copy it from read_file's output after the tab, with the file's exact indentation. Use for surgical edits.", serde_json::json!({
            "type":"object",
            "properties":{"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}},
            "required":["path","old_text","new_text"]
        })),
        tool("multi_edit","Apply a batch of edits across one or more files in a single approval and commit. Edits apply in order (so several edits to the same file compose); each old_text must be unique in its file at the time it runs. All-or-nothing: if any edit can't be located, none are applied.", serde_json::json!({
            "type":"object",
            "properties":{
                "path":{"type":"string","description":"Default file for edits that don't name their own path."},
                "edits":{"type":"array","items":{
                    "type":"object",
                    "properties":{
                        "path":{"type":"string","description":"File to edit; defaults to the previous edit's path."},
                        "old_text":{"type":"string"},
                        "new_text":{"type":"string"}
                    },
                    "required":["old_text","new_text"]
                }}
            },
            "required":["edits"]
        })),
        tool("list_files","List entries in a directory (non-recursive).", serde_json::json!({
            "type":"object",
            "properties":{"path":{"type":"string","description":"default '.'"}}
        })),
        tool("grep","Search file contents for a regex pattern under a path. Returns matching path:line: text. Skips .git/target/node_modules and binaries.", serde_json::json!({
            "type":"object",
            "properties":{
                "pattern":{"type":"string","description":"Rust regex."},
                "path":{"type":"string","description":"file or dir, default '.'"},
                "ignore_case":{"type":"boolean"}
            },
            "required":["pattern"]
        })),
        tool("glob","Find files matching a glob pattern (e.g. src/**/*.rs). Returns relative paths.", serde_json::json!({
            "type":"object",
            "properties":{"pattern":{"type":"string"}},
            "required":["pattern"]
        })),
        tool("remember","Persist a one-line note into long-term memory (~/config/settings/haikucode/memory.md) for future sessions. The note belongs to the current working directory and is shown only in sessions there, unless global is true. Write it so it stands alone: say what the project is, not which tool built it.", serde_json::json!({
            "type":"object",
            "properties":{
                "note":{"type":"string"},
                "global":{"type":"boolean","description":"true only for facts about the user that hold in every project (default false)"}
            },
            "required":["note"]
        })),
        tool("recall","Retrieve memories; with 'query' returns only matching lines, else all.", serde_json::json!({
            "type":"object",
            "properties":{"query":{"type":"string"}}
        })),
        tool("web_fetch","Fetch a URL over HTTP(S) and return its content as readable text (HTML is stripped to text). Use for documentation, APIs, and reference pages.", serde_json::json!({
            "type":"object",
            "properties":{"url":{"type":"string","description":"http:// or https:// URL"}},
            "required":["url"]
        })),
        tool("web_search","Search the web (DuckDuckGo) and return the top results as title, URL and snippet. Follow up with web_fetch to read a promising result.", serde_json::json!({
            "type":"object",
            "properties":{"query":{"type":"string"}},
            "required":["query"]
        })),
        tool("view_image","Load an image file (png/jpg/gif/webp) from disk into the conversation so you can see it. Use for screenshots, diagrams, and mockups.", serde_json::json!({
            "type":"object",
            "properties":{"path":{"type":"string"}},
            "required":["path"]
        })),
        tool("todo","Maintain a visible plan for multi-step tasks. Pass the FULL list every time (it replaces the previous plan; it is shown to the user). Keep items short; mark exactly one item in_progress while working on it.", serde_json::json!({
            "type":"object",
            "properties":{"items":{"type":"array","items":{
                "type":"object",
                "properties":{
                    "text":{"type":"string"},
                    "status":{"type":"string","enum":["pending","in_progress","completed"]}
                },
                "required":["text"]
            }}},
            "required":["items"]
        })),
        tool("ask_user","Ask the user ONE question and wait for their typed answer. Use only when blocked on a decision or missing information you cannot determine yourself.", serde_json::json!({
            "type":"object",
            "properties":{"question":{"type":"string"}},
            "required":["question"]
        })),
        tool("task","Delegate a self-contained sub-task to a fresh sub-agent that has its own context and the same tools. Sub-agents run one at a time, not in parallel; use one to keep a big sub-task's intermediate steps out of your context. Give complete instructions; you only get back the sub-agent's final report.", serde_json::json!({
            "type":"object",
            "properties":{
                "description":{"type":"string","description":"Short label for the sub-task (a few words)."},
                "prompt":{"type":"string","description":"Full instructions for the sub-agent, including everything it needs to know."}
            },
            "required":["prompt"]
        })),
    ])
}

fn tool(name: &str, desc: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type":"function",
        "function":{"name":name,"description":desc,"parameters":params}
    })
}

/// The built-in tool schema plus any MCP tools, as one array for the request.
/// The `skill` tool is only offered when skills are installed, so it costs no
/// tokens otherwise.
pub fn tools_spec_with(mcp: &[crate::mcp::McpTool]) -> serde_json::Value {
    let mut spec = tools_spec();
    if let Some(arr) = spec.as_array_mut() {
        if !crate::commands::discover_skills().is_empty() {
            arr.push(tool(
                "skill",
                "Load an installed skill by name (see the Skills list in your instructions): returns \
                 its full instructions and the paths of its files. Call it before starting a task \
                 that matches a skill's description, then follow it.",
                serde_json::json!({
                    "type":"object",
                    "properties":{"name":{"type":"string","description":"The skill's name, as listed."}},
                    "required":["name"]
                }),
            ));
        }
        for t in mcp {
            arr.push(tool(&t.full_name, &t.description, t.schema.clone()));
        }
    }
    spec
}

/// Tool schema for a sub-agent: same as the parent's but without `task` (no
/// recursive sub-agents) and without `ask_user` (sub-agents must work
/// autonomously; the parent handles user interaction).
pub fn tools_spec_subagent(mcp: &[crate::mcp::McpTool]) -> serde_json::Value {
    let mut spec = tools_spec_with(mcp);
    if let Some(arr) = spec.as_array_mut() {
        arr.retain(|t| {
            let name = t.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str());
            !matches!(name, Some("task") | Some("ask_user"))
        });
    }
    spec
}
