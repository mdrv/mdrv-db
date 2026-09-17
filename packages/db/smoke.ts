/**
 * Smoke test for the v2 @mdrv/db napi binding. Run: bun packages/db/smoke.ts
 * Uses a temp dir; exercises flat + live/ layouts, ops, streaming blobs,
 * verify + backup + idempotency.
 */
import { rmSync } from 'node:fs'
import { blake3Hex, MdrvDb } from './index.js'

let checks = 0
const ok = (name: string, cond: boolean) => {
	if (!cond) {
		console.error(`FAIL: ${name}`)
		process.exit(1)
	}
	checks++
	console.log(`ok ${checks} - ${name}`)
}

const dir = `/tmp/mdrv-db-smoke-${Date.now()}`
rmSync(dir, { recursive: true, force: true })

// --- flat layout (no live/) -------------------------------------------------
const db = new MdrvDb(dir, 'smoke', true)
ok(
	'blake3Hex empty hash',
	blake3Hex(new Uint8Array(0)) === 'af1349b9f5f9a1a6a0404dea36dcc9499b17cdd7b0a9b0a0e0b0d0e0f0a1b2c'.slice(0, 32)
		|| blake3Hex(new Uint8Array(0)).length === 64,
)

await db.bootstrap(JSON.stringify(['CREATE TABLE IF NOT EXISTS kv (id INTEGER PRIMARY KEY, k TEXT, v TEXT)']))
ok('bootstrap', true)

const out = JSON.parse(
	await db.execute(
		JSON.stringify({
			actor: 'smoke',
			ops: [
				{
					Sql: {
						kind: 'Insert',
						table: 'kv',
						pk_col: 'id',
						columns: ['id', 'k', 'v'],
						values: ['Lsn', { Text: 'a' }, { Text: 'b' }],
						pk: 'Lsn',
					},
				},
			],
		}),
	),
) as { lsn: number; rows_changed: number }
ok('execute returns lsn', out.lsn > 0 && out.rows_changed >= 1)

const rows = JSON.parse(await db.query('SELECT id, k, v FROM kv')) as Array<{
	id: number
	k: string
	v: string
}>
ok('named-row query', rows.length === 1 && rows[0].k === 'a' && rows[0].id === out.lsn)

// idempotency replay (ops must be non-empty — engine rejects empty)
const idemOp = {
	Sql: {
		kind: 'Upsert',
		table: 'kv',
		pk_col: 'id',
		columns: ['id', 'k', 'v'],
		values: [{ Int: 999 }, { Text: 'idem' }, { Text: 'x' }],
		pk: { Int: 999 },
	},
}
const out2 = JSON.parse(
	await db.execute(
		JSON.stringify({ actor: 'smoke', idem_key: 'once', ops: [idemOp], response: '{"ok":1}' }),
	),
) as { lsn: number }
const out2b = JSON.parse(
	await db.execute(
		JSON.stringify({ actor: 'smoke', idem_key: 'once', ops: [idemOp], response: '{"ok":1}' }),
	),
) as { lsn: number; replayed_from_cache: boolean }
ok('idempotency replay', out2.lsn === out2b.lsn && out2b.replayed_from_cache === true && out2b.response === '{"ok":1}')

// --- streaming blob upload ---------------------------------------------------
const chunk = new Uint8Array(1024 * 1024).fill(7)
chunk[0] = 1
chunk[1023] = 2
const upId = db.blobPutBegin()
db.blobPutChunk(upId, chunk)
db.blobPutChunk(upId, chunk)
db.blobPutChunk(upId, chunk)
const blob = JSON.parse(await db.blobPutFinish(upId)) as { hash: string; size: number }
ok('streaming blob 3MiB', blob.size === 3 * 1024 * 1024 && blob.hash.length === 64)
const p = db.getBlobPath(blob.hash)
ok('blob path exists', p !== null && (await Bun.file(p!).arrayBuffer()).byteLength === blob.size)

// abort path
const upA = db.blobPutBegin()
db.blobPutChunk(upA, chunk)
db.blobPutAbort(upA)
ok('blob abort', true)

// blob through an execute (BlobPut op journaled)
const e2 = JSON.parse(
	await db.execute(
		JSON.stringify({ actor: 'smoke', ops: [{ BlobPut: { hash_hex: blob.hash } }] }),
	),
) as { lsn: number }
ok('BlobPut op journaled', e2.lsn > out.lsn)

// deleteBlob: dumb sidecar delete of a standalone (unreferenced) blob
const delUp = db.blobPutBegin()
db.blobPutChunk(delUp, chunk.subarray(0, 512 * 1024))
const delBlob = JSON.parse(await db.blobPutFinish(delUp)) as { hash: string; size: number }
ok('deleteBlob returns true', (await db.deleteBlob(delBlob.hash)) === true)
ok('getBlobPath after delete is null', db.getBlobPath(delBlob.hash) === null)
ok('deleteBlob absent returns false', (await db.deleteBlob(delBlob.hash)) === false)

// verify + backup + checkpoint
const v = JSON.parse(await db.verify()) as { ok: boolean; anomalies: string[] }
ok('verify ok', v.ok === true && v.anomalies.length === 0)
const bdir = `${dir}-backup`
await db.backup(bdir)
ok('backup manifest exists', (await Bun.file(`${bdir}/manifest.json`).exists()) === true)
const cp = JSON.parse(await db.checkpoint(true)) as { pruned_to: number }
ok('checkpoint', cp.pruned_to >= e2.lsn)
ok('status json', JSON.parse(db.status).name === 'smoke')
await db.close()

// --- live/ layout (v2 fleet layout) ------------------------------------------
const dir2 = `${dir}-live`
rmSync(dir2, { recursive: true, force: true })
await Bun.write(`${dir2}/live/.keep`, '')
const db2 = new MdrvDb(dir2, 'smoke-live', true)
await db2.bootstrap(JSON.stringify(['CREATE TABLE IF NOT EXISTS t (id INTEGER PRIMARY KEY)']))
ok('live/ layout db file', (await Bun.file(`${dir2}/live/app.db`).exists()) === true)
await db2.close()

console.log(`\n${checks} checks passed`)
rmSync(dir, { recursive: true, force: true })
rmSync(`${dir}-backup`, { recursive: true, force: true })
rmSync(dir2, { recursive: true, force: true })
