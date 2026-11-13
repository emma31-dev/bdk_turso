CREATE TABLE IF NOT EXISTS block (
    height INTEGER PRIMARY KEY NOT NULL,
    hash TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tx (
    txid TEXT NOT NULL PRIMARY KEY,
    tx BLOB,
    first_seen INTEGER,
    last_seen INTEGER,
    last_evicted INTEGER
);

CREATE TABLE IF NOT EXISTS txout (
    txid TEXT NOT NULL,
    vout INTEGER NOT NULL,
    value INTEGER NOT NULL,
    script BLOB NOT NULL,
    PRIMARY KEY (txid, vout)
);

CREATE TABLE IF NOT EXISTS anchor (
    block_height INTEGER NOT NULL,
    block_hash TEXT NOT NULL,
    txid TEXT NOT NULL,
    confirmation_time INTEGER NOT NULL,
    PRIMARY KEY (block_height, block_hash, txid)
);

CREATE INDEX IF NOT EXISTS block_hash_idx ON block(hash);
CREATE INDEX IF NOT EXISTS txout_script_idx ON txout(script);
CREATE INDEX IF NOT EXISTS anchor_txid_idx ON anchor(txid);
CREATE INDEX IF NOT EXISTS anchor_block_hash_idx ON anchor(block_hash);
