<script lang="ts">
// Shared by every LogoMark: a logo mounted shortly after another one (the
// skeleton -> content swap of a page) joins the same intro instead of
// restarting it, so the build-up plays once and never stutters.
const REJOIN_MS = 6000
/** When the intro is over: every intro keyframe has settled by then. */
const INTRO_MS = 3200
let introStartedAt = -Infinity

function introElapsed(play: boolean): number {
  if (!play) return INTRO_MS
  const now = performance.now()
  if (now - introStartedAt > REJOIN_MS) introStartedAt = now
  return Math.round(now - introStartedAt)
}
</script>

<script setup lang="ts">
import { onBeforeUnmount, ref } from "vue"
import panelLeft from "@/assets/logo/panel-left.svg"
import panelCenter from "@/assets/logo/panel-center.svg"
import panelRight from "@/assets/logo/panel-right.svg"
import plane from "@/assets/logo/plane.svg"
import sa from "@/assets/logo/sa.svg"
import roleplay from "@/assets/logo/roleplay.svg"
import star from "@/assets/logo/star.svg"
import silhouette from "@/assets/logo/silhouette.svg"

const props = withDefaults(
  defineProps<{
    size?: "sm" | "md" | "lg" | "xl"
    /** Play the build-up (base "SA" -> Role Play + stars). Off = finished logo. */
    intro?: boolean
    /** Hovering the logo shrinks it away and plays the intro again. */
    replayOnHover?: boolean
  }>(),
  {
    size: "lg",
    intro: true,
    replayOnHover: false,
  },
)

const SIZES = {
  sm: "h-12 w-12",
  md: "h-24 w-24",
  lg: "h-32 w-32",
  xl: "h-44 w-44",
} as const

type Box = readonly [left: number, top: number, width: number, height: number]

// Each piece is an SVG drawn on the 1024-unit logo canvas; boxes are % of it.
// The side panels are whole quads: their inner edge hides under the center one.
function at([left, top, width, height]: Box) {
  return { left: `${left}%`, top: `${top}%`, width: `${width}%`, height: `${height}%` }
}

const LAYOUT = {
  panelLeft: at([0.195, 7.227, 38.965, 85.156]),
  panelCenter: at([24.023, 2.441, 50.195, 94.238]),
  panelRight: at([70.02, 10.156, 29.59, 78.125]),
  plane: at([40.234, 9.375, 20.41, 6.445]),
  sa: at([14.551, 13.672, 69.238, 55.957]),
  roleplay: at([6.738, 51.562, 86.328, 29.102]),
}

// One star drawing, six placements (left to right, rising).
const STARS = (
  [
    [23.828, 75.879],
    [32.31, 74.507],
    [40.77, 73.145],
    [49.23, 71.813],
    [57.697, 70.444],
    [66.185, 69.173],
  ] as const
).map(([left, top], i) => ({ ...at([left, top, 9.277, 9.961]), "--i": i }))

// Inner edge of the center panel's frame: the plane flies inside it.
const SKY = { clipPath: "polygon(37.81% 5.08%, 71.82% 7.68%, 71.71% 89.86%, 26.78% 93.93%)" }
const SHINE = { maskImage: `url(${silhouette})`, WebkitMaskImage: `url(${silhouette})` }

/** The shrink-away before a replayed intro (matches .logo-out). */
const OUT_MS = 260

// How far into the intro this instance starts (--t). A replay sets it to 0
// and bumps `run`, which rebuilds the pieces and so restarts every animation.
const elapsed = ref(introElapsed(props.intro))
const run = ref(0)
const leaving = ref(false)
let startedAt = performance.now() - elapsed.value
let timer = 0

function replay() {
  // Not while the intro is still playing: hovering in and out would restart it.
  if (leaving.value || performance.now() - startedAt < INTRO_MS) return
  leaving.value = true
  timer = window.setTimeout(() => {
    leaving.value = false
    elapsed.value = 0
    startedAt = performance.now()
    run.value++
  }, OUT_MS)
}

function onHover() {
  if (props.replayOnHover) replay()
}

onBeforeUnmount(() => window.clearTimeout(timer))
</script>

<template>
  <div
    class="logo relative shrink-0"
    :class="SIZES[size]"
    :style="{ '--t': `${elapsed}ms` }"
    role="img"
    aria-label="San Andreas Roleplay"
    @mouseenter="onHover"
  >
    <div :key="run" class="absolute inset-0" :class="{ 'logo-out': leaving }">
      <div
        class="logo-shade pointer-events-none absolute -inset-[14%] translate-y-[6%] bg-[radial-gradient(closest-side,rgba(0,0,0,0.55),transparent)]"
      />

      <div class="logo-stage absolute inset-0">
        <img :src="panelLeft" alt="" class="logo-side-left absolute" :style="LAYOUT.panelLeft" />
        <img :src="panelRight" alt="" class="logo-side-right absolute" :style="LAYOUT.panelRight" />

        <div class="logo-center absolute inset-0">
          <img :src="panelCenter" alt="" class="absolute" :style="LAYOUT.panelCenter" />
          <div class="absolute inset-0" :style="SKY">
            <img :src="plane" alt="" class="logo-plane absolute" :style="LAYOUT.plane" />
          </div>
        </div>

        <div class="logo-rise absolute" :style="LAYOUT.sa">
          <img :src="sa" alt="" class="logo-stamp h-full w-full" />
        </div>

        <div class="logo-write absolute" :style="LAYOUT.roleplay">
          <img :src="roleplay" alt="" class="logo-slide h-full w-full" />
        </div>

        <span v-for="(place, i) in STARS" :key="i" class="logo-star absolute" :style="place">
          <img :src="star" alt="" class="h-full w-full" />
        </span>

        <div
          class="logo-shine pointer-events-none absolute inset-0 overflow-hidden"
          :style="SHINE"
        />
      </div>
    </div>
  </div>
</template>

<style scoped>
/*
 * Timeline (ms; every delay is shifted by --t, the time this instance joined
 * the intro, so a remount continues where the previous logo was):
 *     0  center panel drops in        300/360  side panels slide out from behind it
 *   640  "SA" stamps down (the base logo) and shakes the whole mark
 *  1500  "SA" rises to make room      1700  "Role Play" is written in
 *  2150  the six stars pop, one by one
 *  then, in a loop: a shine sweep, a wanted-level twinkle, the plane crossing the sky.
 * The resting styles (no animation) are the finished logo.
 */
.logo-star::after {
  content: "";
  position: absolute;
  inset: -55%;
  border-radius: 9999px;
  background: radial-gradient(
    closest-side,
    rgb(255 246 220 / 0.95),
    rgb(251 191 36 / 0.4) 45%,
    transparent
  );
  opacity: 0;
  pointer-events: none;
}

.logo-shine::before {
  content: "";
  position: absolute;
  inset: 0;
  background: linear-gradient(
    105deg,
    transparent 38%,
    rgb(255 255 255 / 0.45) 50%,
    transparent 62%
  );
  transform: translateX(-100%);
}

.logo-shine {
  mask-size: 100% 100%;
  -webkit-mask-size: 100% 100%;
}

/* No prefers-reduced-motion guard on purpose: Windows reports "reduce" whenever
   its animation effects are off, a common performance tweak among players, and
   the logo would then never animate. The motion is short and plays once. */
.logo-shade {
  animation: logo-fade 700ms ease-out calc(0ms - var(--t)) both;
}

.logo-center {
  transform-origin: 52% 60%;
  animation: logo-drop 560ms cubic-bezier(0.2, 0.7, 0.3, 1) calc(0ms - var(--t)) both;
}

.logo-side-left {
  transform-origin: 100% 85%;
  animation: logo-out-left 540ms cubic-bezier(0.2, 0.7, 0.3, 1) calc(300ms - var(--t)) both;
}

.logo-side-right {
  transform-origin: 0% 85%;
  animation: logo-out-right 540ms cubic-bezier(0.2, 0.7, 0.3, 1) calc(360ms - var(--t)) both;
}

.logo-plane {
  animation: logo-fly 24s linear calc(-4850ms - var(--t)) infinite;
}

.logo-stamp {
  animation: logo-stamp 460ms cubic-bezier(0.3, 0.6, 0.3, 1) calc(640ms - var(--t)) both;
}

.logo-stage {
  animation: logo-shake 320ms linear calc(840ms - var(--t));
}

.logo-rise {
  animation: logo-rise 600ms cubic-bezier(0.45, 0, 0.25, 1) calc(1500ms - var(--t)) both;
}

.logo-write {
  animation: logo-write 560ms cubic-bezier(0.3, 0.5, 0.2, 1) calc(1700ms - var(--t)) both;
}

.logo-slide {
  animation: logo-slide 680ms cubic-bezier(0.2, 0.8, 0.2, 1) calc(1700ms - var(--t)) both;
}

.logo-star {
  animation:
    logo-pop 420ms ease-out calc(2150ms + var(--i) * 90ms - var(--t)) both,
    logo-bump 9s ease-out calc(7600ms + var(--i) * 100ms - var(--t)) infinite;
}

.logo-star::after {
  animation:
    logo-flash 520ms ease-out calc(2150ms + var(--i) * 90ms - var(--t)) both,
    logo-twinkle 9s ease-out calc(7600ms + var(--i) * 100ms - var(--t)) infinite;
}

.logo-shine::before {
  animation: logo-shine 9s ease-in-out calc(3000ms - var(--t)) infinite;
}

/* Replay on hover: the whole mark spins away before the intro builds it again. */
.logo-out {
  animation: logo-out 260ms cubic-bezier(0.55, 0, 0.75, 0) forwards;
}

@keyframes logo-out {
  to {
    transform: scale(0) rotate(-14deg);
  }
}

@keyframes logo-fade {
  from {
    opacity: 0;
  }
}

@keyframes logo-drop {
  0% {
    opacity: 0;
    transform: translateY(-14%) scale(1.15) rotate(-5deg);
  }
  40% {
    opacity: 1;
  }
  65% {
    transform: translateY(1.2%) scale(0.98) rotate(0.8deg);
  }
  85% {
    transform: translateY(-0.4%) rotate(-0.2deg);
  }
  100% {
    opacity: 1;
    transform: none;
  }
}

@keyframes logo-out-left {
  0% {
    opacity: 0;
    transform: translateX(40%) rotate(6deg);
  }
  25% {
    opacity: 1;
  }
  70% {
    transform: translateX(-4%) rotate(-1.2deg);
  }
  100% {
    opacity: 1;
    transform: none;
  }
}

@keyframes logo-out-right {
  0% {
    opacity: 0;
    transform: translateX(-40%) rotate(-6deg);
  }
  25% {
    opacity: 1;
  }
  70% {
    transform: translateX(4%) rotate(1.2deg);
  }
  100% {
    opacity: 1;
    transform: none;
  }
}

/* Enters from behind the left frame, sits in its artwork spot at ~33 %
   (lined up with the end of the intro) and leaves past the right frame. */
@keyframes logo-fly {
  0% {
    transform: translate(-120%, 30%);
  }
  75%,
  100% {
    transform: translate(155%, -40%);
  }
}

@keyframes logo-stamp {
  0% {
    opacity: 0;
    transform: scale(1.9) rotate(-12deg);
  }
  40% {
    opacity: 1;
    transform: scale(0.9) rotate(1.5deg);
  }
  65% {
    transform: scale(1.05) rotate(-0.6deg);
  }
  85% {
    transform: scale(0.99);
  }
  100% {
    opacity: 1;
    transform: none;
  }
}

@keyframes logo-shake {
  0%,
  100% {
    transform: none;
  }
  20% {
    transform: translate(-1.2%, 0.8%) rotate(-0.6deg);
  }
  40% {
    transform: translate(1%, -0.6%) rotate(0.5deg);
  }
  60% {
    transform: translate(-0.6%, 0.4%) rotate(-0.2deg);
  }
  80% {
    transform: translate(0.3%, -0.2%);
  }
}

/* The base logo has the "SA" 28 px right and 83 px lower (of 1024): that is
   where it lands, then it moves up into the final layout. */
@keyframes logo-rise {
  0% {
    transform: translate(3.95%, 14.49%);
  }
  18% {
    transform: translate(3.95%, 15.9%) scale(1.02, 0.97);
  }
  75% {
    transform: translate(-0.3%, -1.5%);
  }
  100% {
    transform: none;
  }
}

/* The visible band sweeps left to right; it starts empty (both edges at -40 %)
   and ends wider than the box so the slide's overshoot is never cut. */
@keyframes logo-write {
  0% {
    clip-path: inset(-40% 140% -40% -40%);
  }
  100% {
    clip-path: inset(-40% -40% -40% -40%);
  }
}

@keyframes logo-slide {
  0% {
    transform: translateX(-10%) skewX(-14deg);
  }
  70% {
    transform: translateX(1.5%) skewX(3deg);
  }
  100% {
    transform: none;
  }
}

@keyframes logo-pop {
  0% {
    opacity: 0;
    transform: scale(0.2) rotate(-140deg);
  }
  55% {
    opacity: 1;
    transform: scale(1.3) rotate(10deg);
  }
  78% {
    transform: scale(0.92) rotate(-3deg);
  }
  100% {
    opacity: 1;
    transform: none;
  }
}

@keyframes logo-flash {
  0% {
    opacity: 0;
    transform: scale(0.3);
  }
  30% {
    opacity: 1;
  }
  100% {
    opacity: 0;
    transform: scale(1.6);
  }
}

@keyframes logo-bump {
  0%,
  8%,
  100% {
    transform: none;
  }
  3% {
    transform: scale(1.15) rotate(-6deg);
  }
}

@keyframes logo-twinkle {
  0% {
    opacity: 0;
    transform: scale(0.4);
  }
  3% {
    opacity: 0.8;
    transform: scale(1.15);
  }
  8%,
  100% {
    opacity: 0;
    transform: scale(1.5);
  }
}

@keyframes logo-shine {
  0% {
    transform: translateX(-100%);
  }
  20%,
  100% {
    transform: translateX(100%);
  }
}
</style>
