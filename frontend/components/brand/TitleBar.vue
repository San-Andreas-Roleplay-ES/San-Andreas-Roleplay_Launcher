<script setup lang="ts">
import { launcher } from "@/api/launcher"
import { useWindowState } from "@/composables/useWindowState"

const { isMaximized, toggleMaximize } = useWindowState()

function onMinimize() {
  void launcher.minimize()
}

function onClose() {
  void launcher.close()
}
</script>

<template>
  <header
    data-tauri-drag-region
    class="relative z-20 flex h-8 items-center justify-between border-b border-white/5 bg-zinc-950/80 pl-3 text-white/80"
  >
    <!--
      `data-tauri-drag-region` only applies to the element that receives the
      mousedown, not to its children, so every non-interactive piece of the bar
      carries it (the <img> is click-through via the global `pointer-events: none`).
      Tauri also toggles maximise on double-click there, so no handler is needed.
    -->
    <div data-tauri-drag-region class="flex h-full items-center gap-2">
      <img src="/logo-squared.png" alt="SARP" class="h-4 w-4 select-none" draggable="false" />
      <span data-tauri-drag-region class="text-[11px] font-medium tracking-wider text-white/70">
        San Andreas Roleplay · Launcher
      </span>
    </div>

    <div class="flex h-full">
      <button
        type="button"
        aria-label="Minimizar"
        class="flex h-full w-11 items-center justify-center text-white/70 transition-colors hover:bg-white/10 hover:text-white"
        @click="onMinimize"
      >
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <rect x="0" y="4.5" width="10" height="1" fill="currentColor" />
        </svg>
      </button>

      <button
        type="button"
        :aria-label="isMaximized ? 'Restaurar' : 'Maximizar'"
        :title="isMaximized ? 'Restaurar' : 'Maximizar'"
        class="flex h-full w-11 items-center justify-center text-white/70 transition-colors hover:bg-white/10 hover:text-white"
        @click="toggleMaximize"
      >
        <svg v-if="!isMaximized" width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <rect
            x="0.5"
            y="0.5"
            width="9"
            height="9"
            stroke="currentColor"
            stroke-width="1"
            fill="none"
          />
        </svg>
        <svg v-else width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <rect
            x="2.5"
            y="0.5"
            width="7"
            height="7"
            stroke="currentColor"
            stroke-width="1"
            fill="none"
          />
          <rect
            x="0.5"
            y="2.5"
            width="7"
            height="7"
            stroke="currentColor"
            stroke-width="1"
            fill="#09090b"
          />
        </svg>
      </button>

      <button
        type="button"
        aria-label="Cerrar"
        class="flex h-full w-11 items-center justify-center text-white/70 transition-colors hover:bg-red-600 hover:text-white"
        @click="onClose"
      >
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <path
            d="M1 1 L9 9 M9 1 L1 9"
            stroke="currentColor"
            stroke-width="1"
            stroke-linecap="square"
            fill="none"
          />
        </svg>
      </button>
    </div>
  </header>
</template>
