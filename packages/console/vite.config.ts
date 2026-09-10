import { vanillaExtractPlugin } from '@vanilla-extract/vite-plugin'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { defineConfig } from 'vite'

export default defineConfig({
	plugins: [vanillaExtractPlugin(), svelte()],
	build: {
		outDir: 'dist',
		// dist/index.html holds a committed placeholder ("build missing") —
		// the real build must replace it wholesale.
		emptyOutDir: true,
	},
})
