import { configureSync, getConsoleSink, getLogger } from '@logtape/logtape'
import { mount } from 'svelte'
import './panda.css'
import App from './App.svelte'

// LogTape: minimal console sink; configureSync keeps this TLA-free for
// older build targets. Silence logtape's own meta logger except errors.
configureSync({
	sinks: { console: getConsoleSink() },
	loggers: [
		{ category: ['logtape', 'meta'], lowestLevel: 'error', sinks: ['console'] },
		{ category: 'mdrv-db', lowestLevel: 'info', sinks: ['console'] },
	],
})

const log = getLogger(['mdrv-db', 'console'])
self.addEventListener('error', (event) => {
	log.error`uncaught error: ${event.message}`
})
self.addEventListener('unhandledrejection', (event) => {
	log.error`unhandled rejection: ${String(event.reason)}`
})

const target = document.getElementById('app')
if (target) mount(App, { target })
