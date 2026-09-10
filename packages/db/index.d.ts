/** @mdrv/db — napi surface. Structured payloads travel as JSON strings. */

/**
 * query() returns an array of row OBJECTS keyed by column name:
 *   [{ id: 1, name: "alice" }, ...]  (empty array = no rows)
 /**
  * Op mirrors (serde externally-tagged):
  *   { "Sql": { kind: "Insert"|"Upsert"|"Delete"|"Update", table, pk_col, columns, values, pk } }
  *   { "BlobPut": { hash_hex } }   { "BlobDrop": { hash_hex } }
  * Values: "Null" | { "Int": n } | { "Real": f } | { "Text": s } | { "Blob": base64 } | "Lsn"
  *   "Lsn" resolves to the entry's LSN at apply time (deterministic row id).
  * Insert = INSERT OR IGNORE (callers detect rows_changed 0 as a conflict);
  * Upsert = INSERT OR REPLACE (full row); Update = partial SET by pk.
  */
export interface MutateRequest {
	actor: string
	ops: unknown[]
	idem_key?: string
	response?: string
}

export interface ExecuteOutcome {
	lsn: number
	rows_changed: number
	replayed_from_cache: boolean
	response?: string
}

export declare class Mdrv {
	/** Opens (or creates) the data dir; uses <dir>/live when it exists (v2 fleet layout). */
	constructor(dataDir: string, name: string, fsyncEachWrite?: boolean)
	execute(requestJson: string): Promise<string>
	query(sql: string, paramsJson?: string): Promise<string>
	bootstrap(statementsJson: string): Promise<void>
	/** Whole-buffer stage for Op.BlobPut. Stream large files instead. */
	putBlob(bytes: Uint8Array): Promise<string>
	/** Streaming upload for large blobs: begin → chunk… → finish (or abort). */
	blobPutBegin(): string
	blobPutChunk(id: string, bytes: Uint8Array): void
	blobPutFinish(id: string): Promise<string>
	blobPutAbort(id: string): void
	getBlobPath(hashHex: string): string | null
	report(level: number, event: string, dataJson?: string): void
	reportExport(sinceMs: number, limit?: number): Promise<string>
	backup(destDir: string): Promise<string>
	verify(): Promise<string>
	checkpoint(compact?: boolean): Promise<string>
	readonly status: string
	close(): Promise<void>
}

/** BLAKE3 of bytes as hex (Bun's CryptoHasher has no blake3). Pure, opens nothing. */
export declare function blake3Hex(bytes: Uint8Array): string
