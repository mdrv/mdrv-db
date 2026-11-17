import { defineConfig } from '@pandacss/dev'

// Single fixed dark theme; colors stay literal in src/app.css.ts because
// semanticTokens do not resolve inside css() utilities on 2.5.0 (guide ch. 07).
export default defineConfig({
	presets: [],
	preflight: true,
	include: ['./src/**/*.{ts,svelte}'],
	outdir: 'styled-system',
	globalCss: {
		'html, body': {
			margin: '0',
			background: '#0b0e14',
			color: '#cdd6f4',
			fontFamily:
				"ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace",
			fontSize: '13px',
			lineHeight: '1.5',
		},
	},
})
