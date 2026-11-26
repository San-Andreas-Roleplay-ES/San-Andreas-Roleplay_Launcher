/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./frontend/**/*.{vue,ts,tsx}"],
  darkMode: "class",
  theme: {
    extend: {
      screens: {
        // Short windows (down to the 720 px minimum): the home tightens its
        // spacing so every row fits without scrolling.
        short: { raw: "(max-height: 900px)" },
      },
    },
  },
  plugins: [],
}
