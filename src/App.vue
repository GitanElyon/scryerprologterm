<script lang="ts">
import AppLogic from "./App";

export default AppLogic;
</script>

<template>
  <main class="page">
    <section class="terminal">
      <div ref="terminal" class="output" aria-live="polite">
        <div v-for="entry in entries" :key="entry.id" class="line" :class="`line-${entry.kind}`">
          <pre>{{ entry.text }}</pre>
        </div>
      </div>

      <form class="inputRow" @submit.prevent="runQuery">
        <span class="prompt" aria-hidden="true">?-</span>
        <textarea
          ref="composer"
          autofocus
          v-model="query"
          @keydown="handleComposerKeydown"
          @input="handleComposerInput"
          :disabled="running"
          placeholder=":help to show help menu"
          autocomplete="off"
          spellcheck="false"
          rows="1"
        />
      </form>
    </section>
  </main>
</template>
