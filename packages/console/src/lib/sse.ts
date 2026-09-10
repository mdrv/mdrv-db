import type { FleetStatus } from './api'

/** Frames pushed by GET /api/events. */
export type FleetEvent =
	| FleetStatus
	| {
		type: 'job.finished'
		slug: string
		result: string
		detail: string | null
		last_run_ms: number | null
		next_run_ms: number | null
		last_backup: string | null
	}

/**
 * Subscribe to the daemon's SSE stream. Each frame's `data` is one JSON
 * object (see FleetEvent); comment pings are ignored by EventSource, which
 * also reconnects on its own. Returns an unsubscribe function.
 */
export function connectEvents(onEvent: (event: FleetEvent) => void): () => void {
	const source = new EventSource('/api/events')
	source.onmessage = (message) => {
		try {
			onEvent(JSON.parse(message.data) as FleetEvent)
		} catch {
			// malformed frame — drop it, the stream continues
		}
	}
	return () => source.close()
}
