use std::cell::RefCell;
use std::collections::HashSet;
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::{mpsc, OnceLock};

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

struct SessionEngine {
    machine: Machine,
    stdout_buffer: Rc<RefCell<Vec<u8>>>,
    stderr_buffer: Rc<RefCell<Vec<u8>>>,
    stdout_offset: usize,
    stderr_offset: usize,
    knowledge_blocks: Vec<String>,
    load_mode_armed: bool,
}

enum WorkerRequest {
    Run {
        query: String,
        reply: mpsc::Sender<Result<PrologResponse, String>>,
    },
}

static PROLOG_WORKER: OnceLock<mpsc::Sender<WorkerRequest>> = OnceLock::new();

fn prolog_worker() -> &'static mpsc::Sender<WorkerRequest> {
    PROLOG_WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<WorkerRequest>();

        std::thread::Builder::new()
            .name("scryer-prolog-worker".to_string())
            .spawn(move || prolog_worker_loop(rx))
            .expect("failed to start scryer prolog worker");

        tx
    })
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

fn build_show_message(knowledge_blocks: &[String]) -> String {
    let unique_blocks = dedupe_preserving_order(knowledge_blocks.iter().cloned());

    if unique_blocks.is_empty() {
        return "No rules loaded.".to_string();
    }

    let mut section = String::from("Loaded rules and facts:\n\n");
    section.push_str(&unique_blocks.join("\n\n"));
    section
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

fn dedupe_preserving_order<I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut seen = HashSet::new();
    let mut unique_values = Vec::new();

    for value in values {
        if seen.insert(value.clone()) {
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
            let source = cursor.get_ref();
            let mut buf = stdout_buffer.borrow_mut();
            let old_len = buf.len();
            if source.len() > old_len {
                buf.extend_from_slice(&source[old_len..]);
            }
        })
    };

    let stderr_capture: Box<dyn FnMut(&mut Cursor<Vec<u8>>)> = {
        let stderr_buffer = Rc::clone(&stderr_buffer);

        Box::new(move |cursor: &mut Cursor<Vec<u8>>| {
            let source = cursor.get_ref();
            let mut buf = stderr_buffer.borrow_mut();
            let old_len = buf.len();
            if source.len() > old_len {
                buf.extend_from_slice(&source[old_len..]);
            }
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

fn format_atom_to(atom: &str, buf: &mut String) {
    let is_bare_atom = atom
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && atom.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        });

    if is_bare_atom {
        buf.push_str(atom);
    } else {
        buf.push('\'');
        for c in atom.chars() {
            if c == '\'' {
                buf.push_str("''");
            } else {
                buf.push(c);
            }
        }
        buf.push('\'');
    }
}

fn format_term_to(term: &Term, buf: &mut String) {
    match term {
        Term::Integer(value) => buf.push_str(&value.to_string()),
        Term::Rational(value) => buf.push_str(&value.to_string()),
        Term::Float(value) => buf.push_str(&value.to_string()),
        Term::Atom(value) => format_atom_to(value, buf),
        Term::String(value) => {
            buf.push('"');
            buf.push_str(&value.escape_default().to_string());
            buf.push('"');
        }
        Term::List(items) => {
            buf.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    buf.push_str(", ");
                }
                format_term_to(item, buf);
            }
            buf.push(']');
        }
        Term::Compound(functor, arguments) => {
            if arguments.is_empty() {
                format_atom_to(functor, buf);
            } else {
                format_atom_to(functor, buf);
                buf.push('(');
                for (i, arg) in arguments.iter().enumerate() {
                    if i > 0 {
                        buf.push_str(", ");
                    }
                    format_term_to(arg, buf);
                }
                buf.push(')');
            }
        }
        Term::Var(value) => buf.push_str(value),
        _ => buf.push_str(&format!("{:?}", term)),
    }
}

fn format_leaf_answer(answer: Result<LeafAnswer, Term>) -> (Option<String>, bool) {
    match answer {
        Ok(LeafAnswer::True) => (Some("true.".to_string()), true),
        Ok(LeafAnswer::False) => (Some("false.".to_string()), true),
        Ok(LeafAnswer::Exception(term)) => {
            let mut buf = String::from("Exception: ");
            format_term_to(&term, &mut buf);
            (Some(buf), false)
        }
        Ok(LeafAnswer::LeafAnswer { bindings, .. }) => {
            if bindings.is_empty() {
                (Some("true.".to_string()), true)
            } else {
                let mut buf = String::with_capacity(128);
                for (i, (name, value)) in bindings.iter().enumerate() {
                    if i > 0 {
                        buf.push_str(", ");
                    }
                    buf.push_str(name);
                    buf.push_str(" = ");
                    format_term_to(value, &mut buf);
                }
                (Some(buf), true)
            }
        }
        Err(term) => {
            let mut buf = String::from("Error: ");
            format_term_to(&term, &mut buf);
            (Some(buf), false)
        }
    }
}

fn flush_machine_output(machine: &mut Machine) {
    let mut flush_query = machine.run_query("flush_output.");
    let _ = flush_query.next();
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

impl SessionEngine {
    fn new() -> Self {
        let (streams, stdout_buffer, stderr_buffer) = make_capturing_streams();

        Self {
            machine: MachineBuilder::default().with_streams(streams).build(),
            stdout_buffer,
            stderr_buffer,
            stdout_offset: 0,
            stderr_offset: 0,
            knowledge_blocks: Vec::new(),
            load_mode_armed: false,
        }
    }

    fn rebuild_machine(&mut self) {
        let load_mode_armed = self.load_mode_armed;
        let knowledge_blocks = std::mem::take(&mut self.knowledge_blocks);

        *self = Self::new();
        self.load_mode_armed = load_mode_armed;

        for block in &knowledge_blocks {
            let _ = self.run_scryer(std::slice::from_ref(block), &[]);
        }

        self.knowledge_blocks = knowledge_blocks;
        self.load_mode_armed = load_mode_armed;
    }

    fn run_scryer(&mut self, knowledge_blocks: &[String], goals: &[String]) -> PrologExecution {
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut success = true;

        if !knowledge_blocks.is_empty() {
            for block in knowledge_blocks {
                let consult_result = catch_unwind(AssertUnwindSafe(|| {
                    self.machine.consult_module_string("user", block.clone());
                }));

                if consult_result.is_err() {
                    return PrologExecution {
                        response: PrologResponse {
                            stdout: String::new(),
                            stderr: "Scryer Prolog panicked while loading the current submission."
                                .to_string(),
                        },
                        success: false,
                    };
                }

                flush_machine_output(&mut self.machine);
                append_output(
                    &mut stdout,
                    &capture_buffer_delta(&self.stdout_buffer, &mut self.stdout_offset),
                );
                append_output(
                    &mut stderr,
                    &capture_buffer_delta(&self.stderr_buffer, &mut self.stderr_offset),
                );
            }
        }

        for goal in goals {
            let query_text = format!("once(({goal})).");
            let query_result = catch_unwind(AssertUnwindSafe(|| {
                let mut query = self.machine.run_query(query_text);
                query.next().unwrap_or(Ok(LeafAnswer::False))
            }));

            let formatted_result = match query_result {
                Ok(result) => format_leaf_answer(result),
                Err(_) => (
                    Some(format!("Error: Scryer Prolog panicked while evaluating: {goal}")),
                    false,
                ),
            };

            flush_machine_output(&mut self.machine);

            append_output(
                &mut stdout,
                &capture_buffer_delta(&self.stdout_buffer, &mut self.stdout_offset),
            );
            append_output(
                &mut stderr,
                &capture_buffer_delta(&self.stderr_buffer, &mut self.stderr_offset),
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

    fn handle_query(&mut self, query: String) -> Result<PrologResponse, String> {
        let raw_input = query.trim();
        if raw_input.is_empty() {
            return Err("Query is empty".to_string());
        }

        if raw_input.eq_ignore_ascii_case(":boot") {
            return Ok(PrologResponse {
                stdout: String::new(),
                stderr: String::new(),
            });
        }

        if raw_input.eq_ignore_ascii_case(":help") {
            return Ok(PrologResponse {
                stdout: build_help_message(),
                stderr: String::new(),
            });
        }

        if raw_input.eq_ignore_ascii_case(":show") {
            return Ok(PrologResponse {
                stdout: build_show_message(&self.knowledge_blocks),
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
            *self = Self::new();

            return Ok(PrologResponse {
                stdout: "Knowledge base cleared.".to_string(),
                stderr: String::new(),
            });
        }

        let force_load_mode = parse_load_command(raw_input);

        if let Some(load_payload) = force_load_mode {
            if load_payload.is_empty() {
                self.load_mode_armed = true;

                return Ok(PrologResponse {
                    stdout: "Load mode armed. Submit clauses next, or use `:load <clauses>` directly.".to_string(),
                    stderr: String::new(),
                });
            }

            return self.execute_submission(&load_payload, true);
        }

        self.execute_submission(raw_input, false)
    }

    fn execute_submission(&mut self, input: &str, is_force_load_mode: bool) -> Result<PrologResponse, String> {
        let load_mode_armed = self.load_mode_armed;

        let (knowledge_text, inline_queries) = split_inline_queries(input);
        let knowledge_candidate = knowledge_text.trim();
        let has_knowledge = has_non_comment_content(knowledge_candidate);
        let has_inline_queries = !inline_queries.is_empty();
        let should_load_knowledge = is_force_load_mode
            || load_mode_armed
            || looks_like_knowledge_block(knowledge_candidate);

        let knowledge_only_submission = should_load_knowledge && !has_inline_queries;

        if knowledge_only_submission {
            if !has_knowledge {
                return Ok(PrologResponse {
                    stdout: "No clauses found to load.".to_string(),
                    stderr: String::new(),
                });
            }

            let knowledge_owned = knowledge_candidate.to_string();
            let mut execution = self.run_scryer(std::slice::from_ref(&knowledge_owned), &[]);
            if execution.success {
                self.knowledge_blocks.push(knowledge_owned);
                self.load_mode_armed = false;

                let loaded_message = "Clauses loaded successfully.";

                if execution.response.stdout.is_empty() {
                    execution.response.stdout = loaded_message.to_string();
                } else {
                    execution.response.stdout =
                        format!("{}\n{}", execution.response.stdout, loaded_message);
                }
            } else {
                self.rebuild_machine();
            }

            return Ok(execution.response);
        }

        let goals = if has_inline_queries {
            inline_queries
        } else {
            vec![normalize_goal(input).ok_or_else(|| "Query is empty".to_string())?]
        };

        let execution = if should_load_knowledge && has_knowledge {
            self.run_scryer(&[knowledge_candidate.to_string()], &goals)
        } else {
            self.run_scryer(&[], &goals)
        };

        if execution.success && should_load_knowledge && has_knowledge {
            self.knowledge_blocks.push(knowledge_candidate.to_string());
            self.load_mode_armed = false;
        }

        if !execution.success && should_load_knowledge && has_knowledge {
            self.rebuild_machine();
        }

        Ok(execution.response)
    }
}

fn prolog_worker_loop(receiver: mpsc::Receiver<WorkerRequest>) {
    let mut engine = SessionEngine::new();

    while let Ok(request) = receiver.recv() {
        match request {
            WorkerRequest::Run { query, reply } => {
                let result = catch_unwind(AssertUnwindSafe(|| engine.handle_query(query)));
                let response = match result {
                    Ok(response) => response,
                    Err(_) => {
                        engine = SessionEngine::new();
                        Ok(prolog_panic_response().response)
                    }
                };

                let _ = reply.send(response);
            }
        }
    }
}

#[tauri::command]
fn run_prolog_query(query: String) -> Result<PrologResponse, String> {
    let (reply_tx, reply_rx) = mpsc::channel();

    prolog_worker()
        .send(WorkerRequest::Run {
            query,
            reply: reply_tx,
        })
        .map_err(|_| "Failed to send query to the Prolog worker".to_string())?;

    reply_rx
        .recv()
        .map_err(|_| "Failed to receive a response from the Prolog worker".to_string())?
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
        build_help_message, build_show_message, looks_like_knowledge_block, normalize_goal,
        parse_load_command, run_prolog_query, split_inline_queries,
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
        let message = build_show_message(&[
            "bird(sparrow).".to_string(),
            "bird(sparrow).".to_string(),
            "flies(X) :- bird(X).".to_string(),
        ]);
        assert!(message.contains("bird(sparrow)."));
        assert!(message.contains("flies(X) :- bird(X)."));
        assert_eq!(message.matches("bird(sparrow)." ).count(), 1);
    }

    #[test]
    fn bare_equality_queries_do_not_load_rules() {
        let _ = run_prolog_query(":reset".to_string()).expect("reset should succeed");

        let _ = run_prolog_query("1=1".to_string()).expect("bare equality should run");
        let show = run_prolog_query(":show".to_string()).expect("show should succeed");

        assert!(show.stdout.contains("No rules loaded."));
    }

    #[test]
    fn malformed_queries_do_not_panic() {
        let _ = run_prolog_query(":reset".to_string()).expect("reset should succeed");

        let response = run_prolog_query("foo(".to_string()).expect("malformed input should be contained");

        assert!(!response.stderr.is_empty());
    }
}
