import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    // Release root filesystems contain recursive symlinks and are not app source.
    watch: { ignored: ["**/artifacts/**"] },
  },
  build: { target: "es2022" },
});
