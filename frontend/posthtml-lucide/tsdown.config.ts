import { defineConfig } from 'tsdown'

export default defineConfig({
  entry: ['index.js'],
  format: ['esm', 'cjs'],
  cwd: './lib',
  outDir: '../dist',
})
