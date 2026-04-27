use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(serde::Serialize)]
struct PrologResponse {
    stdout: String,
    stderr: String,
}

struct PrologExecution {
    response: PrologResponse,
    success: bool,
}

#[derive(Default)]
struct SessionState {
    knowledge_blocks: Vec<String>,
    successful_inputs: Vec<String>,
    load_mode_armed: bool,
}

static SESSION_STATE: OnceLock<Mutex<SessionState>> = OnceLock::new();

fn session_state() -> &'static Mutex<SessionState> {
    SESSION_STATE.get_or_init(|| Mutex::new(SessionState::default()))
}

fn normalize_goal(raw_goal: &str) -> Option<String> {
    let goal = raw_goal.trim().trim_start_matches("?-").trim();
    if goal.is_empty() {
        return None;
    }

    Some(goal.trim_end_matches('.').trim().to_string())
}

fn split_inline_queries(raw_input: &str) -> (String, Vec<String>) {
    let mut knowledge_lines = Vec::new();
    let mut queries = Vec::new();

    for line in raw_input.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("?-") {
            if let Some(goal) = normalize_goal(trimmed) {
                queries.push(goal);
            }
        } else {
            knowledge_lines.push(line);
        }
    }

    (knowledge_lines.join("\n"), queries)
}

fn has_non_comment_content(raw_input: &str) -> bool {
    raw_input.lines().any(|line| {
        let trimmed = line.trim();
        !trimmed.is_empty() && !trimmed.starts_with('%')
    })
}

fn looks_like_knowledge_block(raw_input: &str) -> bool {
    if raw_input.contains("?-") {
        return false;
    }

    if raw_input.contains(":-") {
        return true;
    }

    let statement_lines = raw_input
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('%')
        })
        .count();

    statement_lines >= 2
}

fn build_program_text(knowledge_blocks: &[String]) -> String {
    let mut parts = Vec::new();
    for block in knowledge_blocks {
        let trimmed = block.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed);
        }
    }

    if parts.is_empty() {
        "% empty knowledge base\n".to_string()
    } else {
        format!("{}\n", parts.join("\n\n"))
    }
}

fn build_help_message() -> String {
    [
        "Scryer Terminal Help",
        "",
        "Commands:",
        "  :help              Show this help menu",
        "  :show              Show loaded rules and facts",
        "  :reset             Clear loaded clauses",
        "  :clear             Clear the screen",
        "  :load <clauses>    Force load mode for clauses/rules",
        "  :load              Arm load mode for the next submission",
        "",
        "Usage:",
        "  - Enter sends the current input.",
        "  - Ctrl+Enter inserts a newline.",
        "  - Paste clauses/rules only to load them.",
        "  - Include ?- lines in a pasted block to load and query in one run.",
    ]
    .join("\n")
}

fn build_show_message_with_inputs(
    knowledge_blocks: &[String],
    successful_inputs: &[String],
) -> String {
    let unique_blocks = dedupe_preserving_order(knowledge_blocks.iter().cloned());
    let unique_inputs = dedupe_preserving_order(successful_inputs.iter().cloned());

    if unique_blocks.is_empty() && unique_inputs.is_empty() {
        return "No rules loaded.".to_string();
    }

    let mut sections = Vec::new();

    if !unique_blocks.is_empty() {
        let mut section = String::from("Loaded rules and facts:\n\n");
        section.push_str(&unique_blocks.join("\n\n"));
        sections.push(section);
    }

    sections.join("\n\n")
}

fn dedupe_preserving_order<I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut unique_values = Vec::new();

    for value in values {
        if !unique_values.contains(&value) {
            unique_values.push(value);
        }
    }

    unique_values
}

fn record_successful_inputs(state: &mut SessionState, inputs: Vec<String>) {
    for input in inputs {
        if !state.successful_inputs.contains(&input) {
            state.successful_inputs.push(input);
        }
    }
}

fn parse_load_command(raw_input: &str) -> Option<String> {
    let trimmed = raw_input.trim();
    let remainder = trimmed.strip_prefix(":load")?;

    if remainder.is_empty() {
        return Some(String::new());
    }

    if remainder.chars().next().is_some_and(char::is_whitespace) {
        return Some(remainder.trim().to_string());
    }

    None
}

fn create_temp_program_path() -> Result<PathBuf, String> {
    let mut path = std::env::temp_dir();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("Failed to compute timestamp for temp file: {error}"))?
        .as_nanos();

    path.push(format!(
        "scryerprologterm-{}-{timestamp}.pl",
        std::process::id()
    ));

    Ok(path)
}

fn run_scryer(knowledge_blocks: &[String], goals: &[String]) -> Result<PrologExecution, String> {
    let program_path = create_temp_program_path()?;
    let program_text = build_program_text(knowledge_blocks);

    fs::write(&program_path, program_text)
        .map_err(|error| format!("Failed to create temporary Prolog source file: {error}"))?;

    let program_path_text = program_path.to_string_lossy().to_string();

    let mut child = Command::new("scryer-prolog")
        .args(["-f", &program_path_text, "--no-add-history"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            let _ = fs::remove_file(&program_path);
            format!(
                "Failed to start scryer-prolog: {error}. Run `nix develop` to enter the dev shell."
            )
        })?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "Failed to open stdin for scryer-prolog".to_string())?;

        for goal in goals {
            writeln!(stdin, "once(({goal})).")
                .map_err(|error| format!("Failed to send goal to scryer-prolog: {error}"))?;
        }

        writeln!(stdin, "halt.")
            .map_err(|error| format!("Failed to send halt command to scryer-prolog: {error}"))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|error| format!("Failed while waiting for scryer-prolog: {error}"))?;

    let _ = fs::remove_file(&program_path);

    Ok(PrologExecution {
        response: PrologResponse {
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        },
        success: output.status.success(),
    })
}

#[tauri::command]
fn run_prolog_query(query: String) -> Result<PrologResponse, String> {
    let mut raw_input = query.trim().to_string();
    if raw_input.is_empty() {
        return Err("Query is empty".to_string());
    }

    if raw_input.eq_ignore_ascii_case(":help") {
        return Ok(PrologResponse {
            stdout: build_help_message(),
            stderr: String::new(),
        });
    }

    if raw_input.eq_ignore_ascii_case(":show") {
        let state = session_state()
            .lock()
            .map_err(|_| "Failed to acquire session lock".to_string())?;

        return Ok(PrologResponse {
            stdout: build_show_message_with_inputs(&state.knowledge_blocks, &state.successful_inputs),
            stderr: String::new(),
        });
    }

    if raw_input.eq_ignore_ascii_case(":clear") {
        return Ok(PrologResponse {
            stdout: String::new(),
            stderr: String::new(),
        });
    }

    if raw_input.eq_ignore_ascii_case(":reset") {
        let mut state = session_state()
            .lock()
            .map_err(|_| "Failed to acquire session lock".to_string())?;
        state.knowledge_blocks.clear();
        state.successful_inputs.clear();
        state.load_mode_armed = false;

        return Ok(PrologResponse {
            stdout: "Knowledge base cleared.".to_string(),
            stderr: String::new(),
        });
    }

    let force_load_mode = parse_load_command(&raw_input);
    let is_force_load_mode = force_load_mode.is_some();

    if let Some(load_payload) = force_load_mode {
        if load_payload.is_empty() {
            let mut state = session_state()
                .lock()
                .map_err(|_| "Failed to acquire session lock".to_string())?;
            state.load_mode_armed = true;

            return Ok(PrologResponse {
                stdout: "Load mode armed. Submit clauses next, or use `:load <clauses>` directly.".to_string(),
                stderr: String::new(),
            });
        }

        raw_input = load_payload;
    }

    let load_mode_armed = {
        let state = session_state()
            .lock()
            .map_err(|_| "Failed to acquire session lock".to_string())?;
        state.load_mode_armed
    };

    let (knowledge_text, inline_queries) = split_inline_queries(&raw_input);
    let knowledge_candidate = knowledge_text.trim().to_string();
    let has_knowledge = has_non_comment_content(&knowledge_candidate);
    let has_inline_queries = !inline_queries.is_empty();

    let knowledge_only_submission =
        is_force_load_mode || load_mode_armed || (!has_inline_queries && looks_like_knowledge_block(&raw_input));

    if knowledge_only_submission {
        let existing_knowledge = {
            let state = session_state()
                .lock()
                .map_err(|_| "Failed to acquire session lock".to_string())?;
            state.knowledge_blocks.clone()
        };

        let mut candidate_knowledge = existing_knowledge;
        if has_knowledge {
            candidate_knowledge.push(knowledge_candidate.clone());
        }

        let mut execution = run_scryer(&candidate_knowledge, &[])?;
        if execution.success {
            if has_knowledge {
                let mut state = session_state()
                    .lock()
                    .map_err(|_| "Failed to acquire session lock".to_string())?;
                state.knowledge_blocks = candidate_knowledge;
                state.load_mode_armed = false;
                record_successful_inputs(&mut state, vec![knowledge_candidate.clone()]);
            }

            let loaded_message = if has_knowledge {
                "Clauses loaded successfully."
            } else {
                "No clauses found to load."
            };

            if execution.response.stdout.is_empty() {
                execution.response.stdout = loaded_message.to_string();
            } else {
                execution.response.stdout =
                    format!("{}\n{}", execution.response.stdout, loaded_message);
            }
        }

        return Ok(execution.response);
    }

    let goals = if has_inline_queries {
        inline_queries
    } else {
        vec![normalize_goal(&raw_input).ok_or_else(|| "Query is empty".to_string())?]
    };

    let existing_knowledge = {
        let state = session_state()
            .lock()
            .map_err(|_| "Failed to acquire session lock".to_string())?;
        state.knowledge_blocks.clone()
    };

    let mut candidate_knowledge = existing_knowledge;
    if has_knowledge {
        candidate_knowledge.push(knowledge_candidate.clone());
    }

    let execution = run_scryer(&candidate_knowledge, &goals)?;

    if execution.success && has_knowledge {
        let mut state = session_state()
            .lock()
            .map_err(|_| "Failed to acquire session lock".to_string())?;
        state.knowledge_blocks = candidate_knowledge;
        state.load_mode_armed = false;
        record_successful_inputs(&mut state, vec![knowledge_candidate.clone()]);
    }

    if execution.success && !goals.is_empty() {
        let mut state = session_state()
            .lock()
            .map_err(|_| "Failed to acquire session lock".to_string())?;
        record_successful_inputs(
            &mut state,
            goals.iter().map(|goal| format!("?- {goal}")).collect(),
        );
    }

    Ok(execution.response)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![run_prolog_query])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{
        build_help_message, build_show_message_with_inputs, looks_like_knowledge_block,
        normalize_goal, parse_load_command, split_inline_queries,
    };

    #[test]
    fn normalizes_goal_prefix_and_trailing_dot() {
        assert_eq!(
            normalize_goal("?- can_the_bird_fly(sparrow)."),
            Some("can_the_bird_fly(sparrow)".to_string())
        );
    }

    #[test]
    fn detects_knowledge_blocks_by_structure() {
        let source = "bird(sparrow).\nbird(eagle).\nflies(X) :- bird(X), can_fly(X).";
        assert!(looks_like_knowledge_block(source));
        assert!(!looks_like_knowledge_block("can_the_bird_fly(sparrow)."));
    }

    #[test]
    fn separates_inline_query_lines_from_knowledge() {
        let input = "bird(sparrow).\n?- can_the_bird_fly(sparrow).\ncan_fly(sparrow).";
        let (knowledge, queries) = split_inline_queries(input);

        assert_eq!(knowledge, "bird(sparrow).\ncan_fly(sparrow).");
        assert_eq!(queries, vec!["can_the_bird_fly(sparrow)".to_string()]);
    }

    #[test]
    fn help_message_lists_core_commands() {
        let help = build_help_message();
        assert!(help.contains(":help"));
        assert!(help.contains(":reset"));
        assert!(help.contains(":load"));
    }

    #[test]
    fn parse_load_command_accepts_space_and_newline_payloads() {
        assert_eq!(
            parse_load_command(":load bird(sparrow)."),
            Some("bird(sparrow).".to_string())
        );
        assert_eq!(
            parse_load_command(":load\nbird(sparrow)."),
            Some("bird(sparrow).".to_string())
        );
    }

    #[test]
    fn parse_load_command_handles_bare_and_invalid_forms() {
        assert_eq!(parse_load_command(":load"), Some(String::new()));
        assert_eq!(parse_load_command(":loader"), None);
    }

    #[test]
    fn show_message_reports_loaded_rules() {
        let message = build_show_message_with_inputs(
            &["bird(sparrow).".to_string(), "bird(sparrow).".to_string(), "flies(X) :- bird(X).".to_string()],
            &["?- can_the_bird_fly(sparrow).".to_string(), "?- can_the_bird_fly(sparrow).".to_string()],
        );
        assert!(message.contains("bird(sparrow)."));
        assert!(message.contains("flies(X) :- bird(X)."));
        assert!(message.contains("Successful inputs:"));
        assert_eq!(message.matches("bird(sparrow)." ).count(), 1);
        assert_eq!(message.matches("?- can_the_bird_fly(sparrow)." ).count(), 1);
    }
}
