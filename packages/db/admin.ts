import { blake3Hex } from './index.js'

/**
 * @mdrv/db/admin — token-guarded maintenance RPC for owner apps.
 *
 * The maintenance daemon (@mdrv/db-maintenance) never opens engines itself
 * while an owner process is up; it calls this endpoint instead, so backups
 * go through the engine (persist → VACUUM INTO → manifest) with the file
 * lock held by the one process that owns it.
 *
 * Mount it on your Bun.serve fetch (mdrv mode only):
 *
 *   import { createMdrvDbAdmin } from '@mdrv/db/admin'
 *   const admin = createMdrvDbAdmin(engine, { token: process.env.MDRV_DB_ADMIN_TOKEN! })
 *   // in fetch: if (url.pathname === '/mdrv/rpc') return admin(req)
 *
 * Security: the token holder may trigger backups to ANY directory and prune
 * the WAL — treat MDRV_DB_ADMIN_TOKEN like a database password. Bind the owner
 * to localhost when the RPC is enabled.
 */

export interface AdminEngine {
	status: unknown
	backup(destDir: string): Promise<string>
	verify(): Promise<string>
	checkpoint(compact?: boolean): Promise<string>
}

export interface MdrvDbAdminOptions {
	token: string
}

export interface ManifestEntry {
	path: string
	bytes: number
	blake3: string
}

export interface BackupManifest {
	name: string
	created_at: number
	applied_lsn: number | null
	entry_schema: number | null
	mode?: 'online' | 'offline'
	files: ManifestEntry[]
}

function json(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' },
	})
}

function maybeParse(value: string): unknown {
	try {
		return JSON.parse(value)
	} catch {
		return value
	}
}

/** Handler for POST /mdrv/rpc. Returns undefined for any other request. */
export function createMdrvDbAdmin(
	engine: AdminEngine,
	opts: MdrvDbAdminOptions,
): (req: Request) => Promise<Response> {
	return async (req: Request): Promise<Response> => {
		const url = new URL(req.url)
		if (url.pathname !== '/mdrv/rpc' || req.method !== 'POST') {
			return json({ ok: false, error: 'not found' }, 404)
		}
		if (req.headers.get('x-mdrv-token') !== opts.token) {
			return json({ ok: false, error: 'unauthorized' }, 401)
		}
		let body: { method?: string; params?: Record<string, unknown> }
		try {
			body = (await req.json()) as typeof body
		} catch {
			return json({ ok: false, error: 'invalid JSON body' }, 400)
		}
		const params = body.params ?? {}
		try {
			switch (body.method) {
				case 'status':
					return json({ ok: true, result: engine.status })
				case 'backup': {
					const dest = params.dest_dir
					if (typeof dest !== 'string' || !dest) {
						return json({ ok: false, error: 'params.dest_dir (string) required' }, 400)
					}
					return json({ ok: true, result: maybeParse(await engine.backup(dest)) })
				}
				case 'verify':
					return json({ ok: true, result: maybeParse(await engine.verify()) })
				case 'checkpoint':
					return json({
						ok: true,
						result: maybeParse(await engine.checkpoint(params.compact === true)),
					})
				default:
					return json({
						ok: false,
						error: `unknown method ${JSON.stringify(body.method)}`,
					}, 400)
			}
		} catch (err) {
			return json({ ok: false, error: String(err) }, 500)
		}
	}
}

export interface BackupVerifyReport {
	ok: boolean
	name: string | null
	checked: number
	bytes: number
	anomalies: string[]
}

/**
 * Offline backup verification: re-hash every file against manifest.json.
 * Works without the engine (no lock needed) — used by the maintenance
 * daemon when the owner is down, and as a post-restore sanity check.
 */
export async function verifyBackupDir(dir: string): Promise<BackupVerifyReport> {
	const anomalies: string[] = []
	const manifestPath = `${dir}/manifest.json`
	const file = Bun.file(manifestPath)
	if (!(await file.exists())) {
		return { ok: false, name: null, checked: 0, bytes: 0, anomalies: ['manifest.json missing'] }
	}
	let manifest: BackupManifest
	try {
		manifest = await file.json()
	} catch (err) {
		return {
			ok: false,
			name: null,
			checked: 0,
			bytes: 0,
			anomalies: [`manifest.json unreadable: ${err}`],
		}
	}
	let checked = 0
	let bytes = 0
	for (const entry of manifest.files) {
		const p = `${dir}/${entry.path}`
		const f = Bun.file(p)
		if (!(await f.exists())) {
			anomalies.push(`missing: ${entry.path}`)
			continue
		}
		const data = new Uint8Array(await f.arrayBuffer())
		const hex = blake3Hex(data).slice(0, 32)
		if (hex !== entry.blake3) anomalies.push(`checksum mismatch: ${entry.path}`)
		if (data.byteLength !== entry.bytes) {
			anomalies.push(`size mismatch: ${entry.path} (${data.byteLength} != ${entry.bytes})`)
		}
		checked++
		bytes += data.byteLength
	}
	return { ok: anomalies.length === 0, name: manifest.name ?? null, checked, bytes, anomalies }
}
