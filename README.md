# scryerprologterm

A compact desktop Scryer Prolog terminal built with Tauri + Vue.

## What It Does

This app gives you a small terminal-like interface for running Scryer Prolog goals and loading short knowledge bases directly from the UI. You can type a query, paste a block of rules, or mix both in one submission.

The app keeps an in-memory session knowledge base while it is open. That means you can load facts and rules once, then run later queries against the same session without re-pasting everything.

The backend embeds the `scryer-prolog` crate directly, so queries run inside the app process instead of shelling out to an external `scryer-prolog` binary.

## Run

1. Enter the Nix development shell:

```bash
nix develop
```

2. Install dependencies (first run only):

```bash
npm install
```

3. Start the app:

```bash
npm run tauri dev
```

## Quick Use

- Enter sends the current input.
- Ctrl+Enter inserts a newline.
- `:help` shows the command menu.
- `:show` prints the currently loaded rules.
- `:reset` clears the loaded session state.
- `:clear` clears the screen output only.
- `:load` arms the next submission as a load-only block.
- `:load ...` loads clauses immediately.

## Examples

- Simple query:

```prolog
1 = 1.
```

- Load and query in one paste:

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

can_the_bird_fly(Bird) :-
	(flies(Bird) -> write(Bird), write(' can fly.');
	 does_not_fly(Bird) -> write(Bird), write(' cannot fly.');
	 write(Bird), write(' is not a known bird.')).

?- can_the_bird_fly(sparrow).
?- can_the_bird_fly(penguin).
```

## Command Reference

The textbox treats `?- goal.` as a normal Prolog query. Queries are wrapped with `once((Goal))` before execution so the UI does not hang waiting for more answers.

`:load` behaves as a staging command. If you submit `:load` by itself, the next submission is treated as a load-only block. If you submit `:load <clauses>`, those clauses are loaded immediately.

`:show` displays the loaded rules currently in the session knowledge base.

`:reset` clears the session knowledge base.

`:clear` only clears the on-screen transcript.

## Notes

- The terminal keeps an in-memory session knowledge base and reuses it on later queries.
- The app keeps one warm embedded Scryer Prolog machine alive per session and loads new clauses into it incrementally.
- Query execution wraps goals as `once((Goal))` to avoid interactive backtracking hangs.
- `nix develop` still provides the frontend and Rust/Tauri toolchain, but the Prolog engine now comes from the Rust crate dependency.
