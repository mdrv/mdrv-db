import pandacss from '@pandacss/vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { defineConfig } from 'vite'

export default defineConfig({
	// svelte first, pandacss second (same order idea as the official sandbox)
	plugins: [svelte(), pandacss({ transform: true })],
	build: {
		outDir: 'dist',
		// dist/index.html holds a committed placeholder ("build missing") —
		// the real build must replace it wholesale.
		emptyOutDir: true,
	},
})
