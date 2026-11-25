<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, useTemplateRef } from "vue"
import spray1Url from "@/assets/tag/spray_1.aac?url"
import spray2Url from "@/assets/tag/spray_2.aac?url"
import graffiti1Url from "@/assets/tag/graffiti_1.png"
import graffiti2Url from "@/assets/tag/graffiti_2.png"

/** Spot size in CSS px (h-24 w-24). */
const SIZE = 96
/** Canvas pixels per CSS px. */
const SCALE = 2
const RES = SIZE * SCALE
/** Brush radius in CSS px; the aiming ring (h-6 w-6) matches it. */
const BRUSH = 12
/** Dab spacing along a stroke, in CSS px. */
const STEP = 3
/** Faster strokes than this (px/s) lay proportionally less paint, like a real can. */
const FULL_PAINT_SPEED = 600
/** Canvas alpha (0-255) from which a pixel counts as painted (~60 % opaque). */
const SOLID = 150
/** Share of the tag's pixels that must be painted. */
const COVERAGE_TARGET = 0.9
const CHECK_EVERY_MS = 120

const sprayed = ref(false)
const hovering = ref(false)
const spraying = ref(false)
const cursor = ref({ x: 0, y: 0 })
const spotRef = useTemplateRef<HTMLDivElement>("spotRef")
const canvasRef = useTemplateRef<HTMLCanvasElement>("canvasRef")

const aiming = computed(() => (hovering.value || spraying.value) && !sprayed.value)

const graffiti2MaskUrl = `url(${graffiti2Url})`

const spray1 = new Audio(spray1Url)
spray1.loop = true
spray1.volume = 0.6

const spray2 = new Audio(spray2Url)
spray2.volume = 0.8

/** Canvas pixel indices inside the Grove tag: coverage only counts those. */
let target: Uint32Array | null = null
let brush: HTMLCanvasElement | null = null
let last = { x: 0, y: 0 }
let lastTick = 0
let lastCheck = 0
let frame = 0

function getCtx() {
  return canvasRef.value?.getContext("2d", { willReadFrequently: true }) ?? null
}

function makeBrush() {
  const r = BRUSH * SCALE
  const c = document.createElement("canvas")
  c.width = c.height = r * 2
  const ctx = c.getContext("2d")
  if (!ctx) return null
  const gradient = ctx.createRadialGradient(r, r, 0, r, r, r)
  // Light on purpose: one loose pass leaves the tag patchy; it takes a few
  // seconds of scrubbing (or two careful sweeps) to cover it.
  gradient.addColorStop(0, "rgba(74, 222, 128, 0.22)")
  gradient.addColorStop(1, "rgba(74, 222, 128, 0)")
  ctx.fillStyle = gradient
  ctx.fillRect(0, 0, r * 2, r * 2)
  return c
}

async function loadTarget() {
  const img = new Image()
  img.src = graffiti2Url
  await img.decode()
  const c = document.createElement("canvas")
  c.width = c.height = RES
  const ctx = c.getContext("2d", { willReadFrequently: true })
  if (!ctx) return
  // Same fit as the CSS mask (the tag is square, so "contain" fills the spot).
  ctx.drawImage(img, 0, 0, RES, RES)
  const { data } = ctx.getImageData(0, 0, RES, RES)
  const inside: number[] = []
  for (let i = 0; i < RES * RES; i++) {
    if (data[i * 4 + 3] > 128) inside.push(i)
  }
  target = Uint32Array.from(inside)
}

/** One dab centred at (x, y) in CSS px. It may sit outside the spot: the part
 *  that still overlaps the wall gets painted, which is how the edges fill. */
function dab(ctx: CanvasRenderingContext2D, x: number, y: number, strength: number) {
  if (!brush || x < -BRUSH || y < -BRUSH || x > SIZE + BRUSH || y > SIZE + BRUSH) return
  ctx.globalAlpha = strength
  ctx.drawImage(brush, (x - BRUSH) * SCALE, (y - BRUSH) * SCALE)
}

function tick(now: number) {
  if (!spraying.value) return
  const ctx = getCtx()
  if (ctx) {
    const dt = Math.max(now - lastTick, 1)
    const { x, y } = cursor.value
    const dist = Math.hypot(x - last.x, y - last.y)
    if (dist < 0.5) {
      // Holding still keeps spraying, at the same rate on any refresh rate.
      dab(ctx, x, y, Math.min(1, dt / 16.7))
    } else {
      const strength = Math.min(1, FULL_PAINT_SPEED / ((dist / dt) * 1000))
      const steps = Math.ceil(dist / STEP)
      for (let i = 1; i <= steps; i++) {
        const t = i / steps
        dab(ctx, last.x + (x - last.x) * t, last.y + (y - last.y) * t, strength)
      }
    }
    ctx.globalAlpha = 1
  }
  last = { ...cursor.value }
  lastTick = now
  if (now - lastCheck >= CHECK_EVERY_MS) {
    lastCheck = now
    checkCoverage()
  }
  if (spraying.value) frame = requestAnimationFrame(tick)
}

function checkCoverage() {
  const ctx = getCtx()
  if (!ctx || !target || sprayed.value) return
  const { data } = ctx.getImageData(0, 0, RES, RES)
  let painted = 0
  for (const i of target) {
    if (data[i * 4 + 3] >= SOLID) painted++
  }
  if (painted / target.length >= COVERAGE_TARGET) finish()
}

function moveCursor(e: PointerEvent) {
  const rect = spotRef.value?.getBoundingClientRect()
  if (!rect) return
  cursor.value = { x: e.clientX - rect.left, y: e.clientY - rect.top }
}

function onPointerDown(e: PointerEvent) {
  if (e.button !== 0 || sprayed.value || spraying.value) return
  // Capture keeps the whole stroke ours: leaving the spot only stops the paint
  // landing on it, the spray (sound, ring) goes on until the button is released.
  spotRef.value?.setPointerCapture(e.pointerId)
  moveCursor(e)
  last = { ...cursor.value }
  lastTick = lastCheck = performance.now()
  spraying.value = true
  spray1.currentTime = 0
  void spray1.play().catch(() => {})
  frame = requestAnimationFrame(tick)
}

function stop() {
  if (!spraying.value) return
  spraying.value = false
  cancelAnimationFrame(frame)
  spray1.pause()
}

function release() {
  if (!spraying.value) return
  stop()
  checkCoverage()
}

function finish() {
  stop()
  sprayed.value = true
  void spray2.play().catch(() => {})
}

onMounted(() => {
  brush = makeBrush()
  void loadTarget().catch(() => {})
  window.addEventListener("blur", stop)
})

onBeforeUnmount(() => {
  window.removeEventListener("blur", stop)
  stop()
  spray1.src = ""
  spray2.src = ""
})
</script>

<template>
  <div
    ref="spotRef"
    class="relative h-24 w-24"
    :class="{ 'cursor-none': aiming }"
    @pointerenter="hovering = true"
    @pointerleave="hovering = false"
    @pointermove="moveCursor"
    @pointerdown="onPointerDown"
    @pointerup="release"
    @pointercancel="stop"
    @lostpointercapture="release"
  >
    <img
      :src="graffiti1Url"
      alt=""
      class="pointer-events-none absolute inset-0 h-full w-full object-contain"
    />
    <canvas
      v-show="!sprayed"
      ref="canvasRef"
      :width="RES"
      :height="RES"
      class="spray-canvas pointer-events-none absolute inset-0 h-full w-full"
    />
    <img
      v-if="sprayed"
      :src="graffiti2Url"
      alt=""
      class="graffiti-2 pointer-events-none absolute inset-0 h-full w-full object-contain"
    />
    <span
      v-if="aiming"
      class="pointer-events-none absolute h-6 w-6 -translate-x-1/2 -translate-y-1/2 rounded-full border border-green-300/80 bg-green-300/30 mix-blend-screen"
      :style="{ left: `${cursor.x}px`, top: `${cursor.y}px` }"
    />
  </div>
</template>

<style scoped>
.spray-canvas {
  -webkit-mask-image: v-bind(graffiti2MaskUrl);
  -webkit-mask-repeat: no-repeat;
  -webkit-mask-position: center;
  -webkit-mask-size: contain;
  mask-image: v-bind(graffiti2MaskUrl);
  mask-repeat: no-repeat;
  mask-position: center;
  mask-size: contain;
}

.graffiti-2 {
  animation: graffiti-pop 0.35s ease-out;
}

@keyframes graffiti-pop {
  0% {
    opacity: 0;
    transform: scale(0.92);
  }
  100% {
    opacity: 1;
    transform: scale(1);
  }
}
</style>
