# scryerprologterm Documentation

## Overview

scryerprologterm is a desktop Prolog terminal for Scryer Prolog. It is meant for quick interactive work rather than full project management. The interface behaves like a lightweight REPL with a session-aware knowledge base.

The app is built from two cooperating pieces:

- A Vue frontend that renders the terminal output, input composer, and command feedback.
- A Rust Tauri backend that launches Scryer Prolog, manages temporary source files, and keeps session state in memory.

## Core Behavior

The app supports three primary workflows:

1. Run a one-off goal.
2. Load a block of facts and rules into the current session.
3. Mix rules and queries in the same pasted block.

The backend does not keep a long-running Prolog interpreter alive. Instead, each submission starts a fresh `scryer-prolog` process with the current session knowledge preloaded from memory. This keeps execution isolated while still giving the appearance of a persistent terminal session.

## Input Model

The composer is intentionally minimal:

- Enter submits the current input.
- Ctrl+Enter inserts a newline.
- The composer starts as a single line and expands when multi-line text is entered or pasted.
- The visible prompt is `?-`.
- The input row stays anchored to the bottom of the screen.
- The transcript auto-scrolls to the newest output after each submission.

The user-facing placeholder is `:help to show help menu`.

## Commands

### `:help`

Shows the built-in command reference.

### `:show`

Displays the current session state. It includes:

- Loaded facts and rules.
- Successful inputs already seen in the session.

The command is deduplicated so repeated submissions are only listed once.

### `:reset`

Clears the session state:

- Loaded knowledge blocks.
- Successful-input history.
- Any armed load mode.

This does not clear the visible transcript.

### `:clear`

Clears only the on-screen transcript.

This does not change the session knowledge base.

### `:load`

`:load` can be used in two ways:

- `:load` by itself arms the next submission as a load-only block.
- `:load <clauses>` immediately loads the given clauses.

This is useful when you want to paste facts/rules first and query them later.

## Submission Rules

The backend classifies a submission in a few steps:

- If it is a command like `:help`, `:show`, `:reset`, or `:clear`, it is handled immediately.
- If it begins with `:load`, it is handled as load mode.
- If it contains embedded `?-` lines, those lines are treated as queries and the remaining lines are treated as knowledge to load.
- If it looks like a block of facts or rules, it is treated as a load-only submission.
- Otherwise it is treated as a query.

## How Queries Execute

Query execution uses this pattern:

1. Build a temporary Prolog source file from the current session knowledge.
2. Launch `scryer-prolog` with that file.
3. Pipe one or more goals to stdin as `once((Goal)).`
4. Send `halt.` so the process exits cleanly.

Wrapping goals with `once/1` avoids non-deterministic backtracking prompts and keeps the terminal responsive.

## How Knowledge Is Stored

The session stores two pieces of memory:

- Loaded knowledge blocks.
- Successful inputs.

The session state is updated only when the relevant load or query run succeeds.

Important limitation:

- The state is in memory only. Restarting the app clears it.

## Example Session

1. Paste and submit:

```prolog
bird(sparrow).
bird(eagle).
bird(penguin).
bird(ostrich).

can_fly(sparrow).
can_fly(eagle).
cannot_fly(penguin).
cannot_fly(ostrich).

flies(X) :-
    bird(X),
    can_fly(X).

does_not_fly(X) :-
    bird(X),
    cannot_fly(X).
```

2. Run a query:

```prolog
can_the_bird_fly(sparrow).
```

3. Check what is loaded:

```prolog
:show
```

4. Clear the session state if needed:

```prolog
:reset
```

5. Clear only the visible terminal output:

```prolog
:clear
```

## Project Structure

- [src/App.vue](src/App.vue) defines the terminal layout.
- [src/App.ts](src/App.ts) contains the Vue state and command handling.
- [src/App.css](src/App.css) defines the terminal styling and composer layout.
- [src-tauri/src/lib.rs](src-tauri/src/lib.rs) contains the Prolog command bridge and session state.

## Development Notes

- The app expects `scryer-prolog` to be available in the Nix dev shell.
- `nix develop` should provide the full frontend and backend toolchain.
- `npm tauri dev` runs the full desktop app during development.

## Current Design Goals

- Keep the interface minimal and terminal-like.
- Preserve a clear distinction between screen output and session state.
- Make pasted Prolog blocks easy to run without extra wrapping syntax.