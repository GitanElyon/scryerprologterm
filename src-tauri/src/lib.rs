use std::cell::RefCell;
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use scryer_prolog::{LeafAnswer, Machine, MachineBuilder, StreamConfig, Term};

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

fn prolog_panic_response() -> PrologExecution {
    PrologExecution {
        response: PrologResponse {
            stdout: String::new(),
            stderr: "Error: Scryer Prolog panicked while executing the current submission."
                .to_string(),
        },
        success: false,
    }
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

fn make_capturing_streams() -> (StreamConfig, Rc<RefCell<Vec<u8>>>, Rc<RefCell<Vec<u8>>>) {
    let stdout_buffer = Rc::new(RefCell::new(Vec::new()));
    let stderr_buffer = Rc::new(RefCell::new(Vec::new()));

    let stdout_capture: Box<dyn FnMut(&mut Cursor<Vec<u8>>)> = {
        let stdout_buffer = Rc::clone(&stdout_buffer);

        Box::new(move |cursor: &mut Cursor<Vec<u8>>| {
            *stdout_buffer.borrow_mut() = cursor.get_ref().clone();
        })
    };

    let stderr_capture: Box<dyn FnMut(&mut Cursor<Vec<u8>>)> = {
        let stderr_buffer = Rc::clone(&stderr_buffer);

        Box::new(move |cursor: &mut Cursor<Vec<u8>>| {
            *stderr_buffer.borrow_mut() = cursor.get_ref().clone();
        })
    };

    let (_, streams) = StreamConfig::from_callbacks(Some(stdout_capture), Some(stderr_capture));
    (streams, stdout_buffer, stderr_buffer)
}

fn capture_buffer_delta(buffer: &Rc<RefCell<Vec<u8>>>, offset: &mut usize) -> String {
    let borrowed = buffer.borrow();
    let slice = borrowed.get(*offset..).unwrap_or(&[]);
    *offset = borrowed.len();
    String::from_utf8_lossy(slice).into_owned()
}

fn append_output(target: &mut String, chunk: &str) {
    if chunk.is_empty() {
        return;
    }

    if !target.is_empty() && !target.ends_with('\n') {
        target.push('\n');
    }

    target.push_str(chunk);
}

fn format_atom(atom: &str) -> String {
    let is_bare_atom = atom
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && atom.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        });

    if is_bare_atom {
        atom.to_string()
    } else {
        format!("'{}'", atom.replace('\'', "''"))
    }
}

fn format_term(term: &Term) -> String {
    match term {
        Term::Integer(value) => value.to_string(),
        Term::Rational(value) => value.to_string(),
        Term::Float(value) => value.to_string(),
        Term::Atom(value) => format_atom(value),
        Term::String(value) => format!("\"{}\"", value.escape_default()),
        Term::List(items) => {
            let formatted_items = items.iter().map(format_term).collect::<Vec<_>>().join(", ");
            format!("[{}]", formatted_items)
        }
        Term::Compound(functor, arguments) => {
            let formatted_arguments = arguments
                .iter()
                .map(format_term)
                .collect::<Vec<_>>()
                .join(", ");

            if formatted_arguments.is_empty() {
                format_atom(functor)
            } else {
                format!("{}({})", format_atom(functor), formatted_arguments)
            }
        }
        Term::Var(value) => value.clone(),
        _ => format!("{:?}", term),
    }
}

fn format_leaf_answer(answer: Result<LeafAnswer, Term>) -> (Option<String>, bool) {
    match answer {
        Ok(LeafAnswer::True) => (Some("true.".to_string()), true),
        Ok(LeafAnswer::False) => (Some("false.".to_string()), true),
        Ok(LeafAnswer::Exception(term)) => (
            Some(format!("Exception: {}", format_term(&term))),
            false,
        ),
        Ok(LeafAnswer::LeafAnswer { bindings, .. }) => {
            if bindings.is_empty() {
                (Some("true.".to_string()), true)
            } else {
                let formatted_bindings = bindings
                    .iter()
                    .map(|(name, value)| format!("{} = {}", name, format_term(value)))
                    .collect::<Vec<_>>()
                    .join(", ");

                (Some(formatted_bindings), true)
            }
        }
        Err(term) => (Some(format!("Error: {}", format_term(&term))), false),
    }
}

fn flush_machine_output(machine: &mut Machine) {
    let mut flush_query = machine.run_query("flush_output.");
    let _ = flush_query.next();
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

fn run_scryer(knowledge_blocks: &[String], goals: &[String]) -> PrologExecution {
    match catch_unwind(AssertUnwindSafe(|| run_scryer_inner(knowledge_blocks, goals))) {
        Ok(execution) => execution,
        Err(_) => prolog_panic_response(),
    }
}

fn run_scryer_inner(knowledge_blocks: &[String], goals: &[String]) -> PrologExecution {
    let (streams, stdout_buffer, stderr_buffer) = make_capturing_streams();
    let mut machine = MachineBuilder::default().with_streams(streams).build();
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut stdout_offset = 0;
    let mut stderr_offset = 0;
    let mut success = true;

    if !knowledge_blocks.is_empty() {
        let consult_program = build_program_text(knowledge_blocks);
        let consult_result = catch_unwind(AssertUnwindSafe(|| {
            machine.consult_module_string("user", consult_program);
        }));

        if consult_result.is_err() {
            return PrologExecution {
                response: PrologResponse {
                    stdout: String::new(),
                    stderr: "Scryer Prolog panicked while loading the current session knowledge."
                        .to_string(),
                },
                success: false,
            };
        }

        flush_machine_output(&mut machine);
        append_output(
            &mut stdout,
            &capture_buffer_delta(&stdout_buffer, &mut stdout_offset),
        );
        append_output(
            &mut stderr,
            &capture_buffer_delta(&stderr_buffer, &mut stderr_offset),
        );
    }

    for goal in goals {
        let query_text = format!("once(({goal})).");
        let query_result = catch_unwind(AssertUnwindSafe(|| {
            let mut query = machine.run_query(query_text);
            query.next().unwrap_or(Ok(LeafAnswer::False))
        }));

        let formatted_result = match query_result {
            Ok(result) => format_leaf_answer(result),
            Err(_) => (
                Some(format!("Error: Scryer Prolog panicked while evaluating: {goal}")),
                false,
            ),
        };

        flush_machine_output(&mut machine);

        append_output(
            &mut stdout,
            &capture_buffer_delta(&stdout_buffer, &mut stdout_offset),
        );
        append_output(
            &mut stderr,
            &capture_buffer_delta(&stderr_buffer, &mut stderr_offset),
        );

        if let Some(output_text) = formatted_result.0 {
            if output_text.starts_with("Error:") || output_text.starts_with("Exception:") {
                append_output(&mut stderr, &output_text);
                success = false;
            } else {
                append_output(&mut stdout, &output_text);

                if !formatted_result.1 {
                    success = false;
                }
            }
        } else {
            success = false;
        }
    }

    PrologExecution {
        response: PrologResponse {
            stdout: stdout.trim().to_string(),
            stderr: stderr.trim().to_string(),
        },
        success,
    }
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
    let should_load_knowledge = is_force_load_mode
        || load_mode_armed
        || looks_like_knowledge_block(&knowledge_candidate);

    let knowledge_only_submission = should_load_knowledge && !has_inline_queries;

    if knowledge_only_submission {
        if !has_knowledge {
            return Ok(PrologResponse {
                stdout: "No clauses found to load.".to_string(),
                stderr: String::new(),
            });
        }

        let mut execution = run_scryer(&[knowledge_candidate.clone()], &[]);
        if execution.success {
            let mut state = session_state()
                .lock()
                .map_err(|_| "Failed to acquire session lock".to_string())?;
            state.knowledge_blocks.push(knowledge_candidate.clone());
            state.load_mode_armed = false;
            record_successful_inputs(&mut state, vec![knowledge_candidate.clone()]);

            let loaded_message = "Clauses loaded successfully.";

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
    if should_load_knowledge && has_knowledge {
        candidate_knowledge.push(knowledge_candidate.clone());
    }

    let execution = run_scryer(&candidate_knowledge, &goals);

    if execution.success && should_load_knowledge && has_knowledge {
        let mut state = session_state()
            .lock()
            .map_err(|_| "Failed to acquire session lock".to_string())?;
        state.knowledge_blocks.push(knowledge_candidate.clone());
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
        normalize_goal, parse_load_command, run_prolog_query, session_state, split_inline_queries,
    };

    fn reset_session_state() {
        let mut state = session_state().lock().expect("session lock");
        *state = Default::default();
    }

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

    #[test]
    fn bare_equality_queries_stay_out_of_loaded_knowledge() {
        reset_session_state();

        let _response = run_prolog_query("1=1".to_string()).expect("bare equality should run");
        let state = session_state().lock().expect("session lock");

        assert!(state.knowledge_blocks.is_empty());
    }

    #[test]
    fn malformed_queries_do_not_panic() {
        reset_session_state();

        let _ = run_prolog_query("foo(".to_string());
        let state = session_state().lock().expect("session lock");

        assert!(state.knowledge_blocks.is_empty());
    }
}
