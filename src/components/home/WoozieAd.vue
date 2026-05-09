<script setup lang="ts">
import { computed } from "vue"
import type { WoozieAlert } from "@/api/public"

const props = defineProps<{
  alert: WoozieAlert | null
}>()

defineEmits<{
  dismiss: []
}>()

const typeBorder: Record<WoozieAlert["type"], string> = {
  bonus: "border-amber-400/30",
  event: "border-orange-400/30",
  maintenance: "border-rose-400/30",
  announcement: "border-sky-400/30",
}

const typeGlow: Record<WoozieAlert["type"], string> = {
  bonus: "from-amber-400/10",
  event: "from-orange-400/10",
  maintenance: "from-rose-400/10",
  announcement: "from-sky-400/10",
}

const border = computed(() => props.alert ? typeBorder[props.alert.type] : "")
const glow = computed(() => props.alert ? typeGlow[props.alert.type] : "")
</script>

<template>
  <Transition name="woozie">
    <div
      v-if="alert"
      class="w-full overflow-hidden rounded-xl border bg-black/60 backdrop-blur-md"
      :class="border"
    >
      <!-- glow superior -->
      <div
        class="pointer-events-none absolute inset-x-0 top-0 h-px bg-gradient-to-r to-transparent"
        :class="glow"
        aria-hidden="true"
      />

      <div class="flex items-start gap-3 px-4 py-3">
        <!-- avatar de woozie -->
        <img
        src="@/assets/woozie.png"
        alt="Woozie"
        class="h-9 w-9 flex-shrink-0 rounded-full object-cover ring-1 ring-white/10"
        />

        <!-- contenido -->
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2">
            <span class="text-xs font-semibold text-amber-300/90">Woozie</span>
            <i
              class="pi text-[10px]"
            />
          </div>
          <p class="mt-0.5 text-[13px] leading-snug text-white/70">
            {{ alert.message }}
          </p>
        </div>

        <!-- cerrar -->
        <button
          type="button"
          class="flex h-6 w-6 flex-shrink-0 items-center justify-center rounded-md text-white/25 transition hover:bg-white/[0.06] hover:text-white/50"
          @click="$emit('dismiss')"
        >
          <i class="pi pi-times text-[10px]" />
        </button>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.woozie-enter-active {
  transition:
    opacity 0.35s ease,
    transform 0.35s cubic-bezier(0.16, 1, 0.3, 1),
    max-height 0.35s ease;
}

.woozie-leave-active {
  transition:
    opacity 0.2s ease,
    transform 0.2s ease,
    max-height 0.2s ease;
}

.woozie-enter-from {
  opacity: 0;
  transform: translateY(-8px) scale(0.97);
  max-height: 0;
}

.woozie-enter-to {
  max-height: 120px;
}

.woozie-leave-to {
  opacity: 0;
  transform: translateY(-4px);
  max-height: 0;
}
</style>