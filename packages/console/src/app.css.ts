import { createTheme, globalStyle, style, styleVariants } from '@vanilla-extract/css'

export const [themeClass, vars] = createTheme({
	color: {
		bg: '#0b0e14',
		panel: '#11141b',
		border: '#1f2430',
		text: '#cdd6f4',
		muted: '#6c7086',
		ok: '#a6e3a1',
		warn: '#f9e2af',
		error: '#f38ba8',
	},
	font: {
		mono: "ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace",
	},
})

globalStyle('html, body', {
	margin: 0,
	background: vars.color.bg,
	color: vars.color.text,
	fontFamily: vars.font.mono,
	fontSize: '13px',
	lineHeight: 1.5,
})

export const page = style({
	maxWidth: '1100px',
	margin: '0 auto',
	padding: '20px 16px 64px',
})

export const topbar = style({
	display: 'flex',
	alignItems: 'baseline',
	justifyContent: 'space-between',
	gap: '12px',
	borderBottom: `1px solid ${vars.color.border}`,
	paddingBottom: '10px',
	marginBottom: '16px',
})

export const heading = style({
	margin: 0,
	fontSize: '15px',
	fontWeight: 600,
	letterSpacing: '0.02em',
})

export const panel = style({
	background: vars.color.panel,
	border: `1px solid ${vars.color.border}`,
	borderRadius: '8px',
	padding: '14px 16px',
	marginBottom: '16px',
	overflowX: 'auto',
})

export const table = style({
	width: '100%',
	borderCollapse: 'collapse',
	fontSize: '12.5px',
})

globalStyle(`${table} th`, {
	textAlign: 'left',
	fontWeight: 500,
	color: vars.color.muted,
	padding: '4px 10px 6px',
	borderBottom: `1px solid ${vars.color.border}`,
	whiteSpace: 'nowrap',
})

globalStyle(`${table} td`, {
	padding: '6px 10px',
	borderBottom: `1px solid ${vars.color.border}`,
	whiteSpace: 'nowrap',
})

globalStyle(`${table} tbody tr:hover`, { background: '#151926' })

export const rowActive = style({ background: '#151926' })

export const detailCell = style({
	color: vars.color.muted,
	maxWidth: '320px',
	overflow: 'hidden',
	textOverflow: 'ellipsis',
})

export const badge = style({
	display: 'inline-block',
	padding: '0 8px',
	borderRadius: '999px',
	border: '1px solid transparent',
	fontSize: '11px',
	lineHeight: '1.7',
	whiteSpace: 'nowrap',
})

export const badgeVariant = styleVariants({
	ok: { color: vars.color.ok, borderColor: vars.color.ok, background: 'rgba(166, 227, 161, 0.08)' },
	skipped: { color: vars.color.warn, borderColor: vars.color.warn, background: 'rgba(249, 226, 175, 0.08)' },
	running: { color: vars.color.warn, borderColor: vars.color.warn, background: 'rgba(249, 226, 175, 0.08)' },
	error: { color: vars.color.error, borderColor: vars.color.error, background: 'rgba(243, 139, 168, 0.08)' },
	never: { color: vars.color.muted, borderColor: vars.color.border },
})

export const button = style({
	font: 'inherit',
	fontSize: '12.5px',
	color: vars.color.text,
	background: '#181d29',
	border: `1px solid ${vars.color.border}`,
	borderRadius: '6px',
	padding: '5px 12px',
	cursor: 'pointer',
	selectors: {
		'&:hover:not(:disabled)': { borderColor: vars.color.muted },
		'&:disabled': { opacity: 0.45, cursor: 'default' },
	},
})

export const buttonPrimary = style({
	color: vars.color.ok,
	borderColor: vars.color.ok,
})

export const input = style({
	font: 'inherit',
	color: vars.color.text,
	background: vars.color.bg,
	border: `1px solid ${vars.color.border}`,
	borderRadius: '6px',
	padding: '6px 10px',
	width: '100%',
	boxSizing: 'border-box',
	selectors: { '&:focus': { outline: 'none', borderColor: vars.color.muted } },
})

export const label = style({
	display: 'block',
	color: vars.color.muted,
	fontSize: '12px',
	margin: '10px 0 4px',
})

export const row = style({
	display: 'flex',
	alignItems: 'center',
	gap: '10px',
	flexWrap: 'wrap',
	marginTop: '10px',
})

export const metaGrid = style({
	display: 'grid',
	gridTemplateColumns: 'max-content 1fr',
	gap: '2px 16px',
	margin: '10px 0',
	fontSize: '12.5px',
})

globalStyle(`${metaGrid} dt`, { color: vars.color.muted })
globalStyle(`${metaGrid} dd`, { margin: 0 })

export const logList = style({
	listStyle: 'none',
	margin: 0,
	padding: 0,
	maxHeight: '420px',
	overflowY: 'auto',
	fontSize: '12px',
})

globalStyle(`${logList} li`, {
	display: 'flex',
	gap: '10px',
	alignItems: 'baseline',
	padding: '3px 0',
	borderBottom: `1px solid ${vars.color.border}`,
})

export const muted = style({ color: vars.color.muted })
export const okText = style({ color: vars.color.ok })
export const warnText = style({ color: vars.color.warn })
export const errorText = style({ color: vars.color.error })
export const footer = style({ color: vars.color.muted, fontSize: '12px' })
