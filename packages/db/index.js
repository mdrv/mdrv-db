// @mdrv/db — native addon loader (ESM; see index.cjs for require())
import { existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'

const here = new URL('.', import.meta.url)

const candidates = [
	`mdrv-db.${process.platform}-${process.arch}-gnu.node`,
	`mdrv-db.${process.platform}-${process.arch}.node`,
	'libmdrv_db.so',
]

let addon = undefined
for (const name of candidates) {
	const p = fileURLToPath(new URL(name, here))
	if (existsSync(p)) {
		addon = createRequire(import.meta.url)(p)
		break
	}
}

if (!addon) {
	throw new Error(`@mdrv/db: no native addon found for ${process.platform}-${process.arch}`)
}

export const Mdrv = addon.Mdrv
export const blake3Hex = addon.blake3Hex
export default addon
