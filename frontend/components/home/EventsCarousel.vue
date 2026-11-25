<script setup lang="ts">
import EventCard from "./EventCard.vue"
import type { ServerEvent } from "@/api/public"

defineProps<{
  events: ServerEvent[]
  loading: boolean
  error: string | null
}>()

defineEmits<{
  open: [event: ServerEvent]
}>()
</script>

<template>
  <section class="w-full">
    <div v-if="loading" class="flex justify-center gap-3 overflow-hidden">
      <div
        v-for="i in 5"
        :key="i"
        class="shrink-0 animate-pulse rounded-xl bg-white/[0.05]"
        style="height: clamp(150px, 21.6vh, 240px); aspect-ratio: 9 / 16"
      />
    </div>

    <div v-else class="events-scroll overflow-x-auto pb-2">
      <div v-if="events.length > 0" class="mx-auto flex w-fit snap-x snap-mandatory gap-3">
        <EventCard
          v-for="event in events"
          :key="event.id"
          :event="event"
          @open="(e) => $emit('open', e)"
        />
      </div>
      <div
        v-else
        class="flex min-h-36 w-full flex-col items-center justify-center rounded-xl border border-white/10 bg-white/[0.03] px-6 text-center"
        aria-live="polite"
      >
        <i
          class="pi mb-2 text-lg"
          :class="error ? 'pi-exclamation-circle text-amber-200/60' : 'pi-calendar text-white/30'"
        />
        <p class="text-xs text-white/50">
          {{ error ?? "No hay eventos publicados por ahora." }}
        </p>
      </div>
    </div>
  </section>
</template>

<style scoped>
.events-scroll {
  scrollbar-width: thin;
  scrollbar-color: rgba(255, 255, 255, 0.15) transparent;
}
.events-scroll::-webkit-scrollbar {
  height: 4px;
}
.events-scroll::-webkit-scrollbar-track {
  background: transparent;
}
.events-scroll::-webkit-scrollbar-thumb {
  background: rgba(255, 255, 255, 0.15);
  border-radius: 999px;
}
.events-scroll::-webkit-scrollbar-thumb:hover {
  background: rgba(255, 255, 255, 0.3);
}
</style>
