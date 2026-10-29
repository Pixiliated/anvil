//! The CLI and the MCP server must offer the same commands with the same arguments: the CLI's
//! `a-b` is the tool `a_b`, positionals and options are parameters, and required positionals
//! are required parameters. `--json` is CLI-only (tools always answer in JSON).

use anvil_mcp::AnvilServer;
use anvil_ops::{CLI_ONLY_OPTIONS, COMMANDS};
use std::collections::BTreeSet;

fn snake(s: &str) -> String {
    s.replace('-', "_")
}

#[test]
fn every_cli_command_has_an_mcp_tool_and_back() {
    let tools = AnvilServer::tools();
    let tool_names: BTreeSet<String> = tools.iter().map(|t| t.name.to_string()).collect();
    let command_names: BTreeSet<String> = COMMANDS.iter().map(|c| snake(c.name)).collect();
    assert_eq!(tool_names, command_names, "commands and tools differ");

    let mut problems = Vec::new();
    for c in COMMANDS {
        let tool = tools.iter().find(|t| t.name == snake(c.name)).unwrap();
        let schema = serde_json::Value::Object((*tool.input_schema).clone());
        let props: BTreeSet<String> =
            schema["properties"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
        let required: BTreeSet<String> = schema["required"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
            .unwrap_or_default();

        let mut want: BTreeSet<String> = c.positionals.iter().map(|p| snake(p.trim_end_matches('?'))).collect();
        want.extend(c.options.iter().filter(|(n, _, _)| !CLI_ONLY_OPTIONS.contains(n)).map(|(n, _, _)| snake(n)));
        let want_required: BTreeSet<String> =
            c.positionals.iter().filter(|p| !p.ends_with('?')).map(|p| snake(p)).collect();
        if props != want {
            problems.push(format!("{}: tool parameters {props:?}, CLI arguments {want:?}", c.name));
        }
        if required != want_required {
            problems.push(format!("{}: tool requires {required:?}, CLI requires {want_required:?}", c.name));
        }
        if tool.description.as_deref().unwrap_or("").is_empty() {
            problems.push(format!("{}: the tool has no description", c.name));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
