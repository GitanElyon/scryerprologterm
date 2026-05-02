import "./App.css";

import { nextTick, onMounted, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";

type EntryKind = "system" | "input" | "output" | "error";

interface TerminalEntry {
  id: number;
  kind: EntryKind;
  text: string;
}

interface PrologResponse {
  stdout: string;
  stderr: string;
}

export default {
  name: "App",
  setup() {
    const query = ref("");
    const running = ref(false);
    const terminal = ref<HTMLElement | null>(null);
    const composer = ref<HTMLTextAreaElement | null>(null);
    const defaultComposerHeight = ref(0);
    const entries = ref<TerminalEntry[]>([]);
    let nextEntryId = 1;

    function scrollToBottom() {
      nextTick(() => {
        if (terminal.value) {
          terminal.value.scrollTop = terminal.value.scrollHeight;
        }
      });
    }

    function resizeComposer() {
      if (!composer.value) {
        return;
      }

      if (!defaultComposerHeight.value) {
        defaultComposerHeight.value = composer.value.scrollHeight;
      }

      composer.value.style.height = "auto";
      const maxHeight = 220;
      const nextHeight = Math.min(composer.value.scrollHeight, maxHeight);
      const minimumHeight = defaultComposerHeight.value || nextHeight;
      composer.value.style.height = `${Math.max(nextHeight, minimumHeight)}px`;
      composer.value.style.overflowY = composer.value.scrollHeight > maxHeight ? "auto" : "hidden";
    }

    function resetComposerHeight() {
      if (!composer.value) {
        return;
      }

      const minimumHeight = defaultComposerHeight.value || composer.value.scrollHeight;
      composer.value.style.height = `${minimumHeight}px`;
      composer.value.style.overflowY = "hidden";
    }

    function clearScreen() {
      entries.value = [];
      nextEntryId = 1;

      if (terminal.value) {
        terminal.value.scrollTop = 0;
      }
    }

    function pushEntry(kind: EntryKind, text: string) {
      entries.value.push({ id: nextEntryId, kind, text });
      nextEntryId += 1;
      scrollToBottom();
    }

    function handleComposerInput() {
      resizeComposer();
    }

    function handleComposerKeydown(event: KeyboardEvent) {
      if (event.key !== "Enter") {
        return;
      }

      if (event.ctrlKey || event.metaKey || event.shiftKey || event.isComposing) {
        return;
      }

      event.preventDefault();
      void runQuery();
    }

    async function runQuery() {
      const rawQuery = query.value.trim();
      if (!rawQuery || running.value) {
        return;
      }

      if (rawQuery.toLowerCase() === ":clear") {
        clearScreen();
        query.value = "";
        resetComposerHeight();
        return;
      }

      running.value = true;

      const normalizedForDisplay = rawQuery.trimStart().startsWith("?-")
        ? rawQuery.trimStart().slice(2).trimStart()
        : rawQuery;
      const renderedInput = `?- ${normalizedForDisplay}`;

      pushEntry("input", renderedInput);
      query.value = "";
      resetComposerHeight();

      try {
        const response = await invoke<PrologResponse>("run_prolog_query", {
          query: rawQuery,
        });

        if (response.stdout) {
          pushEntry("output", response.stdout);
        }

        if (response.stderr) {
          pushEntry("error", response.stderr);
        }

        if (!response.stdout && !response.stderr) {
          pushEntry("system", "(No output)");
        }
      } catch (error) {
        pushEntry("error", String(error));
      } finally {
        running.value = false;
      }
    }

    watch(query, resizeComposer);

    onMounted(async () => {
      resizeComposer();
      resetComposerHeight();

      pushEntry("system", "Scryer Prolog Terminal - Type :help for available commands");
      // Send a ping query to warm up the engine
      await invoke("run_prolog_query", { query: ":boot" }).catch(() => {});
    });

    return {
      composer,
      entries,
      handleComposerInput,
      handleComposerKeydown,
      query,
      runQuery,
      running,
      terminal,
      resetComposerHeight,
    };
  },
};