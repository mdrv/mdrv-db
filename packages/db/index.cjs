// @mdrv/db — native addon loader (CommonJS; see index.js for ESM)
const path = require('node:path')
const fs = require('node:fs')

const candidates = [
	`mdrv-db.${process.platform}-${process.arch}-gnu.node`,
	`mdrv-db.${process.platform}-${process.arch}.node`,
	'libmdrv_db.so',
]

for (const name of candidates) {
	const p = path.join(__dirname, name)
	if (fs.existsSync(p)) {
		module.exports = require(p)
		break
	}
}
