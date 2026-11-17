import { css } from './styled-system/css'

// Single fixed dark theme (was: vanilla-extract createTheme vars). Literal
// values, not semanticTokens: they do not resolve inside css() on 2.5.0.
const c = {
	bg: '#0b0e14',
	panel: '#11141b',
	border: '#1f2430',
	text: '#cdd6f4',
	muted: '#6c7086',
	ok: '#a6e3a1',
	warn: '#f9e2af',
	error: '#f38ba8',
}

const hoverRow = '#151926'
const warnVars = {
	color: c.warn,
	borderColor: c.warn,
	background: 'rgba(249, 227, 175, 0.08)',
}

export const page = css({
	maxWidth: '1100px',
	margin: '0 auto',
	padding: '20px 16px 64px',
})

export const topbar = css({
	display: 'flex',
	alignItems: 'baseline',
	justifyContent: 'space-between',
	gap: '12px',
	borderBottom: `1px solid ${c.border}`,
	paddingBottom: '10px',
	marginBottom: '16px',
})

export const heading = css({
	margin: '0',
	fontSize: '15px',
	fontWeight: 600,
	letterSpacing: '0.02em',
})

export const panel = css({
	background: c.panel,
	border: `1px solid ${c.border}`,
	borderRadius: '8px',
	padding: '14px 16px',
	marginBottom: '16px',
	overflowX: 'auto',
})

export const table = css({
	width: '100%',
	borderCollapse: 'collapse',
	fontSize: '12.5px',
	'& th': {
		textAlign: 'left',
		fontWeight: 500,
		color: c.muted,
		padding: '4px 10px 6px',
		borderBottom: `1px solid ${c.border}`,
		whiteSpace: 'nowrap',
	},
	'& td': {
		padding: '6px 10px',
		borderBottom: `1px solid ${c.border}`,
		whiteSpace: 'nowrap',
	},
	'& tbody tr:hover': { background: hoverRow },
})

export const rowActive = css({ background: hoverRow })

export const detailCell = css({
	color: c.muted,
	maxWidth: '320px',
	overflow: 'hidden',
	textOverflow: 'ellipsis',
})

// Variant state rides a data-tone attribute inside this one static class:
// atomic class ordering cannot resolve `${base} ${variant}` concatenations
// deterministically (guide ch. 10, pitfall 5).
export const badge = css({
	display: 'inline-block',
	padding: '0 8px',
	borderRadius: '999px',
	border: '1px solid transparent',
	fontSize: '11px',
	lineHeight: '1.7',
	whiteSpace: 'nowrap',
	'&[data-tone=ok]': {
		color: c.ok,
		borderColor: c.ok,
		background: 'rgba(166, 227, 161, 0.08)',
	},
	'&[data-tone=skipped]': warnVars,
	'&[data-tone=running]': warnVars,
	'&[data-tone=error]': {
		color: c.error,
		borderColor: c.error,
		background: 'rgba(243, 139, 168, 0.08)',
	},
	'&[data-tone=never]': { color: c.muted, borderColor: c.border },
})

export const button = css({
	font: 'inherit',
	fontSize: '12.5px',
	color: c.text,
	background: '#181d29',
	border: `1px solid ${c.border}`,
	borderRadius: '6px',
	padding: '5px 12px',
	cursor: 'pointer',
	'&:hover:not(:disabled)': { borderColor: c.muted },
	'&:disabled': { opacity: 0.45, cursor: 'default' },
	// Primary variant via attribute, not a second class (atomic ordering).
	'&[data-variant=primary]': { color: c.ok, borderColor: c.ok },
})

export const input = css({
	font: 'inherit',
	color: c.text,
	background: c.bg,
	border: `1px solid ${c.border}`,
	borderRadius: '6px',
	padding: '6px 10px',
	width: '100%',
	boxSizing: 'border-box',
	'&:focus': { outline: 'none', borderColor: c.muted },
})

export const label = css({
	display: 'block',
	color: c.muted,
	fontSize: '12px',
	margin: '10px 0 4px',
})

export const row = css({
	display: 'flex',
	alignItems: 'center',
	gap: '10px',
	flexWrap: 'wrap',
	marginTop: '10px',
})

export const metaGrid = css({
	display: 'grid',
	gridTemplateColumns: 'max-content 1fr',
	gap: '2px 16px',
	margin: '10px 0',
	fontSize: '12.5px',
	'& dt': { color: c.muted },
	'& dd': { margin: '0' },
})

export const logList = css({
	listStyle: 'none',
	margin: '0',
	padding: '0',
	maxHeight: '420px',
	overflowY: 'auto',
	fontSize: '12px',
	'& li': {
		display: 'flex',
		gap: '10px',
		alignItems: 'baseline',
		padding: '3px 0',
		borderBottom: `1px solid ${c.border}`,
	},
})

export const muted = css({ color: c.muted })
export const okText = css({ color: c.ok })
export const warnText = css({ color: c.warn })
export const errorText = css({ color: c.error })
export const footer = css({ color: c.muted, fontSize: '12px' })
